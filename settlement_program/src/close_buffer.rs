use jito_bytemuck::AccountDeserialize;
use jito_jsm_core::loader::load_signer;
use settlement_core::{
    buffer::{find_buffer_program_address, read_buffer_trailer},
    error::SettlementError,
    instruction::CloseBufferArgs,
    state::GkState,
};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    msg,
    program_error::ProgramError,
    pubkey::Pubkey,
    system_program,
};

/// Closes a settled transition's buffer and refunds rent to the recorded
/// payer (retain-until-indexed: callable by the payer any time AFTER settle).
///
/// ### Accounts:
/// 1. `[]` state: settlement state PDA
/// 2. `[writable]` buffer: the buffer PDA to close
/// 3. `[writable, signer]` payer: the rent payer recorded at buffer creation
pub fn process_close_buffer(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    args: &CloseBufferArgs,
) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let state = next_account_info(account_info_iter)?;
    let buffer = next_account_info(account_info_iter)?;
    let payer = next_account_info(account_info_iter)?;

    GkState::load(program_id, state, false)?;
    load_signer(payer, true)?;

    if buffer.owner.ne(program_id) {
        return Err(ProgramError::InvalidAccountOwner);
    }

    let (expected_buffer, _, _) =
        find_buffer_program_address(program_id, state.key, args.transition_index);
    if buffer.key.ne(&expected_buffer) {
        return Err(SettlementError::BufferKeyMismatch.into());
    }

    // Retain-until-indexed policy: only closable once the transition settled.
    let transition_count = {
        let state_data = state.data.borrow();
        GkState::try_from_slice_unchecked(&state_data)?.transition_count()
    };
    if args.transition_index >= transition_count {
        msg!(
            "Buffer transition {} not settled yet (count {})",
            args.transition_index,
            transition_count
        );
        return Err(SettlementError::BufferNotSettled.into());
    }

    let recorded_payer = {
        let data = buffer.data.borrow();
        read_buffer_trailer(&data).map_err(ProgramError::from)?.0
    };
    if recorded_payer.ne(payer.key) {
        msg!("Close authority {} is not the recorded payer", payer.key);
        return Err(SettlementError::InvalidBufferPayer.into());
    }

    // Refund rent and reclaim the account.
    let refund = buffer.lamports();
    **payer.lamports.borrow_mut() = payer
        .lamports()
        .checked_add(refund)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    **buffer.lamports.borrow_mut() = 0;
    buffer.assign(&system_program::id());
    buffer.realloc(0, false)?;

    msg!(
        "Closed buffer {} for transition {}, refunded {} lamports to {}",
        buffer.key,
        args.transition_index,
        refund,
        payer.key
    );

    Ok(())
}
