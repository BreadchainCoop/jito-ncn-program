use jito_bytemuck::AccountDeserialize;
use jito_jsm_core::loader::load_signer;
use jito_restaking_core::{ncn::Ncn, operator::Operator};
use ncn_program_core::{config::Config, error::NCNProgramError, snapshot::Snapshot};
use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, msg, program_error::ProgramError,
    pubkey::Pubkey,
};

/// Removes an operator from the snapshot (docs/INTERFACES.md §3).
///
/// Tombstones the operator's index slot, subtracts its G1 key from the running
/// APK, and bumps the snapshot generation (invalidating any in-flight
/// certificate assembled against the previous operator set).
///
/// ### Authorization: the signer must be EITHER the NCN admin OR this
/// operator's admin.
///
/// ### Accounts:
/// 1. `[]` config: NCN configuration account
/// 2. `[]` ncn: The NCN account
/// 3. `[]` operator: The operator being removed
/// 4. `[signer]` admin: NCN admin or the operator's admin
/// 5. `[writable]` snapshot: Snapshot account containing operator snapshots
pub fn process_remove_operator(program_id: &Pubkey, accounts: &[AccountInfo]) -> ProgramResult {
    let [config, ncn, operator, admin, snapshot] = accounts else {
        msg!("Error: Not enough account keys provided");
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    Config::load(program_id, config, ncn.key, false)?;
    Ncn::load(&jito_restaking_program::id(), ncn, false)?;
    Operator::load(&jito_restaking_program::id(), operator, false)?;
    load_signer(admin, false)?;
    Snapshot::load(program_id, snapshot, ncn.key, true)?;

    // Authorization: admin must be the NCN admin or the operator's admin.
    {
        let ncn_admin = {
            let ncn_data = ncn.data.borrow();
            Ncn::try_from_slice_unchecked(&ncn_data)?.admin
        };
        let operator_admin = {
            let operator_data = operator.data.borrow();
            Operator::try_from_slice_unchecked(&operator_data)?.admin
        };

        if admin.key != &ncn_admin && admin.key != &operator_admin {
            msg!("Error: signer is neither the NCN admin nor the operator admin");
            return Err(NCNProgramError::CannotRemoveOperator.into());
        }
    }

    let mut snapshot_data = snapshot.try_borrow_mut_data()?;
    let snapshot_account = Snapshot::try_from_slice_unchecked_mut(&mut snapshot_data)?;

    let removed_index = snapshot_account.remove_operator_snapshot(operator.key)?;

    msg!(
        "Operator {} removed (index {} tombstoned); snapshot generation is now {}",
        operator.key,
        removed_index,
        snapshot_account.generation()
    );

    Ok(())
}
