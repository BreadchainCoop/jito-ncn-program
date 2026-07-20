use settlement_core::{
    error::SettlementError, instruction::find_event_authority, payload::MAX_EVENT_CPI_DATA_LEN,
};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    msg,
    pubkey::Pubkey,
};

/// The event self-CPI no-op branch (the hand-rolled `emit_cpi` analog).
///
/// Instruction data is `event discriminant (8) ‖ event payload`; the event
/// lands in the transaction's inner-instruction record, the non-truncatable
/// channel indexers read. The branch only logs — but it requires the
/// program's event-authority PDA as a signer, which only the program itself
/// can produce via `invoke_signed`, so third parties cannot forge events.
///
/// ### Accounts:
/// 1. `[signer]` event_authority: `[b"gk_event_authority"]` PDA
pub fn process_emit_event(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let event_authority = next_account_info(account_info_iter)?;

    let (expected_authority, _) = find_event_authority(program_id);
    if event_authority.key.ne(&expected_authority) || !event_authority.is_signer {
        msg!("Event branch invoked without the event authority signature");
        return Err(SettlementError::EventAuthorityNotSigner.into());
    }

    if instruction_data.len() > MAX_EVENT_CPI_DATA_LEN {
        return Err(SettlementError::EventTooLarge.into());
    }

    let discriminant = instruction_data.get(..8).unwrap_or_default();
    msg!(
        "gk_event discriminant={:?} payload_len={}",
        discriminant,
        instruction_data.len().saturating_sub(8)
    );

    Ok(())
}
