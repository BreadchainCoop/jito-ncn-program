use jito_bytemuck::{AccountDeserialize, Discriminator};
use jito_jsm_core::{
    loader::{load_system_account, load_system_program},
    slot_toggle::SlotToggleState,
};
use jito_restaking_core::{ncn::Ncn, ncn_operator_state::NcnOperatorState, operator::Operator};
use ncn_program_core::{
    account_payer::AccountPayer,
    config::Config,
    constants::{G1_COMPRESSED_POINT_SIZE, G2_COMPRESSED_POINT_SIZE, MAX_OPERATORS},
    error::NCNProgramError,
    g1_point::G1Point,
    g2_point::{G2CompressedPoint, G2Point},
    loaders::load_ncn_epoch,
    ncn_operator_account::NCNOperatorAccount,
    snapshot::{OperatorSnapshot, Snapshot},
    utils::pop_message_digest,
};
use solana_program::{
    account_info::AccountInfo,
    entrypoint::ProgramResult,
    msg,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvar::{clock::Clock, Sysvar},
};

/// Registers an operator by creating a new PDA account with BLS key verification.
///
/// ### Parameters:
/// - `g1_pubkey`: G1 public key in compressed format (32 bytes)
/// - `g2_pubkey`: G2 public key in compressed format (64 bytes)
/// - `signature`: BLS signature of the G1 pubkey signed by the G2 private key (64 bytes uncompressed G1 point)
///
/// ### Accounts:
/// 1. `[]` config: NCN configuration account
/// 2. `[writable]` ncn_operator_account: The ncn operator account PDA account to create
/// 3. `[]` ncn: The NCN account
/// 4. `[]` operator: The operator to register
/// 5. `[signer]` operator_admin: The operator admin that must sign
/// 6. `[]` ncn_operator_state: The connection between NCN and operator
/// 7. `[writable]` snapshot: Snapshot account containing operator snapshots
/// 8. `[]` restaking_config: Restaking configuration account
/// 9. `[writable, signer]` account_payer: Account paying for the initialization
/// 10. `[]` system_program: Solana System Program
pub fn process_register_operator(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    g1_pubkey: [u8; G1_COMPRESSED_POINT_SIZE],
    g2_pubkey: [u8; G2_COMPRESSED_POINT_SIZE],
    signature: [u8; 64],
    ip_address: [u8; 4],
    port: u16,
) -> ProgramResult {
    let [config, ncn_operator_account, ncn, operator, operator_admin, ncn_operator_state, snapshot, restaking_config, account_payer, system_program] =
        accounts
    else {
        msg!("Error: Not enough account keys provided");
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    Config::load(program_id, config, ncn.key, false)?;
    load_system_account(ncn_operator_account, true)?;
    Ncn::load(&jito_restaking_program::id(), ncn, false)?;
    Operator::load(&jito_restaking_program::id(), operator, false)?;
    AccountPayer::load(program_id, account_payer, ncn.key, true)?;
    load_system_program(system_program)?;
    Snapshot::load(program_id, snapshot, ncn.key, true)?;
    NcnOperatorState::load(
        &jito_restaking_program::id(),
        ncn_operator_state,
        ncn,
        operator,
        false,
    )?;

    // Verify that the operator_admin is authorized to register this operator
    {
        let operator_data = operator.data.borrow();
        let operator_account = Operator::try_from_slice_unchecked(&operator_data)?;

        if operator_account.admin.ne(operator_admin.key) {
            msg!("Error: Operator admin is not authorized to register this operator");
            return Err(ProgramError::InvalidAccountData);
        }

        if !operator_admin.is_signer {
            msg!("Error: Operator admin must sign the transaction");
            return Err(ProgramError::MissingRequiredSignature);
        }
    }

    let current_slot = Clock::get()?.slot;
    let (_, ncn_epoch_length) = load_ncn_epoch(restaking_config, current_slot, None)?;

    // Check if the operator <> NCN connection is active
    let is_active = {
        let ncn_operator_state_data = ncn_operator_state.data.borrow();
        let ncn_operator_state_account =
            NcnOperatorState::try_from_slice_unchecked(&ncn_operator_state_data)?;

        let ncn_operator_state = ncn_operator_state_account
            .ncn_opt_in_state
            .state(current_slot, ncn_epoch_length)?;

        let operator_ncn_state = ncn_operator_state_account
            .operator_opt_in_state
            .state(current_slot, ncn_epoch_length)?;

        matches!(
            ncn_operator_state,
            SlotToggleState::Active | SlotToggleState::WarmUp
        ) && matches!(
            operator_ncn_state,
            SlotToggleState::Active | SlotToggleState::WarmUp
        )
    };

    if !is_active {
        msg!("Error: Operator <> NCN connection is not active");
        return Err(ProgramError::from(
            NCNProgramError::OperatorNcnConnectionNotActive,
        ));
    }

    // Verify BLS proof-of-possession: signature over the domain-tagged digest
    // binding (ncn, operator, g1_pubkey), signed by the G2 private key
    {
        let signature = G1Point::from(signature);
        let g2_compressed = G2CompressedPoint::from(g2_pubkey);
        let g2_point = G2Point::try_from(g2_compressed)
            .map_err(|_| NCNProgramError::G2PointDecompressionError)?;

        let pop_digest = pop_message_digest(ncn.key, operator.key, &g1_pubkey);
        g2_point
            .verify_operator_registeration(signature, g1_pubkey, &pop_digest)
            .map_err(|_| NCNProgramError::BLSVerificationError)?;

        msg!("BLS proof-of-possession verification successful");
    }

    // Verify the ncn operator account PDA is correct
    let (ncn_operator_account_pda, ncn_operator_account_bump, mut ncn_operator_account_seeds) =
        NCNOperatorAccount::find_program_address(program_id, ncn.key, operator.key);
    ncn_operator_account_seeds.push(vec![ncn_operator_account_bump]);

    if ncn_operator_account_pda != *ncn_operator_account.key {
        msg!("Error: Invalid ncn operator account PDA");
        return Err(ProgramError::InvalidSeeds);
    }

    // Create the ncn operator account account
    AccountPayer::pay_and_create_account(
        program_id,
        ncn.key,
        account_payer,
        ncn_operator_account,
        system_program,
        program_id,
        NCNOperatorAccount::SIZE,
        &ncn_operator_account_seeds,
    )?;

    let clock = Clock::get()?;
    let slot = clock.slot;

    let operator_index = {
        let operator_data = operator.data.borrow();
        let operator_account = Operator::try_from_slice_unchecked(&operator_data)?;
        operator_account.index()
    };

    // Check if operator index is valid
    let ncn_operator_index = {
        let ncn_operator_state_data = ncn_operator_state.data.borrow();
        let ncn_operator_state =
            NcnOperatorState::try_from_slice_unchecked(&ncn_operator_state_data)?;

        let ncn_operator_index = ncn_operator_state.index();

        if ncn_operator_index >= MAX_OPERATORS as u64 {
            msg!(
                "Error: Operator index is out of bounds. Index: {},",
                ncn_operator_index,
            );
            return Err(NCNProgramError::OperatorIsNotInSnapshot.into());
        }

        ncn_operator_index
    };

    // Initialize the ncn operator account account
    let mut ncn_operator_account_data = ncn_operator_account.try_borrow_mut_data()?;
    ncn_operator_account_data[0] = NCNOperatorAccount::DISCRIMINATOR;
    let ncn_operator_account_account =
        NCNOperatorAccount::try_from_slice_unchecked_mut(&mut ncn_operator_account_data)?;

    ncn_operator_account_account.initialize(
        ncn.key,
        operator.key,
        &g1_pubkey,
        &g2_pubkey,
        ncn_operator_index,
        slot,
        ip_address,
        port,
        ncn_operator_account_bump,
    );

    // Create operator snapshot and add it to the snapshot
    let operator_snapshot = OperatorSnapshot::new(
        operator.key,
        current_slot,
        is_active,
        ncn_operator_index,
        operator_index,
        g1_pubkey,
    )?;

    let mut snapshot_data = snapshot.try_borrow_mut_data()?;
    let snapshot_account = Snapshot::try_from_slice_unchecked_mut(&mut snapshot_data)?;

    // Add the operator snapshot to the snapshot
    snapshot_account.add_operator_snapshot(operator_snapshot, slot)?;

    snapshot_account.register_operator_g1_pubkey(&g1_pubkey)?;

    msg!(
        "Operator registered successfully with index {}",
        ncn_operator_index
    );

    Ok(())
}
