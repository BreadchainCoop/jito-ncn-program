use jito_bytemuck::AccountDeserialize;
use jito_restaking_core::{
    config::Config, ncn::Ncn, ncn_operator_state::NcnOperatorState,
    ncn_vault_ticket::NcnVaultTicket, operator::Operator,
};
use jito_vault_core::{
    vault::Vault, vault_ncn_ticket::VaultNcnTicket,
    vault_operator_delegation::VaultOperatorDelegation,
};
use ncn_program_core::{
    config::Config as NcnConfig,
    error::NCNProgramError,
    loaders::load_ncn_epoch,
    snapshot::{OperatorSnapshot, Snapshot},
    stake_weight::StakeWeights,
    vault_registry::{VaultRegistry, FULL_WEIGHT_BPS},
};
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult, msg,
    program_error::ProgramError, pubkey::Pubkey, sysvar::Sysvar,
};

/// Records the delegation between a vault and an operator at a specific epoch.
///
/// ### Parameters:
/// - `epoch`: The target epoch
///
/// ### Accounts:
/// 1. `[]` ncn_config: NCN configuration account
/// 2. `[]` restaking_config: Restaking configuration account
/// 3. `[]` ncn: The NCN account
/// 4. `[]` operator: The operator account
/// 5. `[]` vault: The vault account
/// 6. `[]` vault_ncn_ticket: The vault NCN ticket
/// 7. `[]` ncn_vault_ticket: The NCN vault ticket
/// 8. `[]` ncn_operator_state: The connection between NCN and operator
/// 9. `[]` vault_operator_delegation: The delegation between vault and operator
/// 10. `[writable]` snapshot: Snapshot account containing operator snapshots
/// 11. `[]` vault_registry: Vault registry (weights + per-vault registry slots)
pub fn process_snapshot_vault_operator_delegation(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
) -> ProgramResult {
    let [ncn_config, restaking_config, ncn, operator, vault, vault_ncn_ticket, ncn_vault_ticket, ncn_operator_state, vault_operator_delegation, snapshot, vault_registry] =
        accounts
    else {
        msg!("Error: Not enough account keys provided");
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    NcnConfig::load(program_id, ncn_config, ncn.key, false)?;
    Config::load(&jito_restaking_program::id(), restaking_config, false)?;
    Ncn::load(&jito_restaking_program::id(), ncn, false)?;
    Operator::load(&jito_restaking_program::id(), operator, false)?;
    Vault::load(&jito_vault_program::id(), vault, false)?;
    Snapshot::load(program_id, snapshot, ncn.key, true)?;
    VaultRegistry::load(program_id, vault_registry, ncn.key, false)?;

    NcnVaultTicket::load(
        &jito_restaking_program::id(),
        ncn_vault_ticket,
        ncn,
        vault,
        false,
    )?;

    if !vault_ncn_ticket.data_is_empty() {
        VaultNcnTicket::load(
            &jito_vault_program::id(),
            vault_ncn_ticket,
            vault,
            ncn,
            false,
        )?;
    }

    if !vault_operator_delegation.data_is_empty() {
        VaultOperatorDelegation::load(
            &jito_vault_program::id(),
            vault_operator_delegation,
            vault,
            operator,
            false,
        )?;
    }
    VaultOperatorDelegation::load(
        &jito_vault_program::id(),
        vault_operator_delegation,
        vault,
        operator,
        false,
    )?;

    let current_slot = Clock::get()?.slot;

    let (_, ncn_epoch_length) = load_ncn_epoch(restaking_config, current_slot, None)?;

    // check vault is up to date
    let vault_needs_update = {
        let vault_data = vault.data.borrow();
        let vault_account = Vault::try_from_slice_unchecked(&vault_data)?;

        vault_account.is_update_needed(current_slot, ncn_epoch_length)?
    };
    if vault_needs_update {
        msg!("Error: Vault is not up to date");
        return Err(NCNProgramError::VaultNeedsUpdate.into());
    }

    let mut snapshot_data = snapshot.try_borrow_mut_data()?;
    let snapshot_account = Snapshot::try_from_slice_unchecked_mut(&mut snapshot_data)?;
    let operator_snapshot = *snapshot_account
        .find_operator_snapshot(operator.key)
        .ok_or_else(|| {
            msg!(
                "Error: Operator snapshot not found for operator: {}",
                operator.key
            );
            NCNProgramError::OperatorIsNotInSnapshot
        })?;

    let mut cloned_operator_snapshot = operator_snapshot;

    // Check if operator has valid BN128 G1 pubkey and determine active status
    let is_vault_ncn_connection_active = {
        let ncn_vault_okay = {
            let ncn_vault_ticket_data = ncn_vault_ticket.data.borrow();
            let ncn_vault_ticket_account =
                NcnVaultTicket::try_from_slice_unchecked(&ncn_vault_ticket_data)?;

            // If the NCN removes a vault, it should immediately be barred from the snapshot
            ncn_vault_ticket_account
                .state
                .is_active(current_slot, ncn_epoch_length)?
        };

        let vault_ncn_okay = {
            if vault_ncn_ticket.data_is_empty() {
                false
            } else {
                let vault_ncn_ticket_data = vault_ncn_ticket.data.borrow();
                let vault_ncn_ticket_account =
                    VaultNcnTicket::try_from_slice_unchecked(&vault_ncn_ticket_data)?;

                // If a vault removes itself from the ncn, it should still be able to participate
                // until it is finished cooling down - this is so the operators with delegation
                // from this vault can still participate
                vault_ncn_ticket_account
                    .state
                    .is_active_or_cooldown(current_slot, ncn_epoch_length)?
            }
        };

        let delegation_dne = vault_operator_delegation.data_is_empty();

        vault_ncn_okay && ncn_vault_okay && !delegation_dne
    };

    let is_operator_ncn_connection_active = {
        let ncn_operator_state_data = ncn_operator_state.data.borrow();
        let ncn_operator_state_account =
            NcnOperatorState::try_from_slice_unchecked(&ncn_operator_state_data)?;

        // If the NCN removes an operator, it should immediately be barred from the snapshot
        let ncn_operator_okay = ncn_operator_state_account
            .ncn_opt_in_state
            .is_active(current_slot, ncn_epoch_length)?;

        // If the operator removes itself from the ncn, it should still be able to participate
        // while it is cooling down
        let operator_ncn_okay = ncn_operator_state_account
            .operator_opt_in_state
            .is_active_or_cooldown(current_slot, ncn_epoch_length)?;

        ncn_operator_okay && operator_ncn_okay
    };

    let is_active = is_operator_ncn_connection_active && is_vault_ncn_connection_active;

    // Resolve the vault's registry slot (per-vault contribution key) and its
    // weight: contribution = delegation * weight_bps / 10_000
    // (docs/INTERFACES.md par.3)
    let (registry_slot, weight_bps) = {
        let vault_registry_data = vault_registry.data.borrow();
        let vault_registry_account = VaultRegistry::try_from_slice_unchecked(&vault_registry_data)?;
        (
            vault_registry_account.get_vault_registry_slot(vault.key)? as u64,
            vault_registry_account.get_vault_weight_bps(vault.key)?,
        )
    };

    let (total_stake_weight, next_epoch_stake_weight) = if is_active {
        let vault_operator_delegation_data = vault_operator_delegation.data.borrow();
        let vault_operator_delegation_account =
            VaultOperatorDelegation::try_from_slice_unchecked(&vault_operator_delegation_data)?;

        OperatorSnapshot::calculate_stake_weights(vault_operator_delegation_account)?
    } else {
        (0u128, 0u128)
    };

    // Scale the raw delegation by the vault's weight
    let apply_weight = |raw: u128| -> Result<u128, ProgramError> {
        raw.checked_mul(weight_bps as u128)
            .and_then(|scaled| scaled.checked_div(FULL_WEIGHT_BPS as u128))
            .ok_or_else(|| NCNProgramError::ArithmeticOverflow.into())
    };
    let total_stake_weight = apply_weight(total_stake_weight)?;
    let next_epoch_stake_weight = apply_weight(next_epoch_stake_weight)?;

    // Increment vault operator delegation and check if finalized
    let this_epoch_stake_weight = StakeWeights::snapshot(total_stake_weight)?;
    let next_epoch_stake_weight = StakeWeights::snapshot(next_epoch_stake_weight)?;
    let _ncn_operator_index = {
        cloned_operator_snapshot.snapshot_vault_operator_delegation(
            current_slot,
            registry_slot,
            &this_epoch_stake_weight,
            &next_epoch_stake_weight,
            snapshot_account.minimum_stake(),
        )?;

        cloned_operator_snapshot.ncn_operator_index()
    };

    snapshot_account.update_operator_snapshot(
        cloned_operator_snapshot.ncn_operator_index() as usize,
        &cloned_operator_snapshot,
    );

    // The snapshot-level freshness marker tracks the latest crank; the
    // VerifyCertificate slot-freshness window measures against it.
    snapshot_account.set_last_snapshot_slot(current_slot);

    Ok(())
}
