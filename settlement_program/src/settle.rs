use borsh::BorshDeserialize;
use jito_bytemuck::AccountDeserialize;
use settlement_core::{
    buffer::{buffer_content_len, find_buffer_program_address},
    error::SettlementError,
    instruction::{find_event_authority, SettleArgs, EVENT_AUTHORITY_SEED, SETTLE_DISCRIMINATOR},
    payload::{
        event_cpi_data, StateUpdate, StoryMeta, MAX_EVENT_CPI_DATA_LEN, STORY_META_DISCRIMINANT,
    },
    state::GkState,
};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    instruction::{AccountMeta, Instruction},
    msg,
    program::invoke_signed,
    program_error::ProgramError,
    pubkey::Pubkey,
};

use crate::verify::verify_certificate_inline;

/// The `verifyAndUpdate` analog (INTERFACES.md §4): recompute the payload
/// digest, verify the NCN certificate inline (D-VERIFY option B), apply
/// exactly one commitment write, emit events via self-CPI, bump the
/// consumer-local transition counter.
///
/// ### Accounts:
/// 1. `[writable]` state: settlement state PDA
/// 2. `[]` ncn_config: NCN program Config PDA
/// 3. `[]` ncn: the NCN account
/// 4. `[]` snapshot: NCN snapshot PDA
/// 5. `[]` restaking_config: jito-restaking Config
/// 6. `[]` event_authority: `[b"gk_event_authority"]` PDA (self-CPI signer)
/// 7. `[]` settlement_program: this program (self-CPI target)
/// 8. `[]` buffer (optional): required when a story_meta event is present
pub fn process_settle(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    args: &SettleArgs,
) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let state = next_account_info(account_info_iter)?;
    let ncn_config = next_account_info(account_info_iter)?;
    let ncn = next_account_info(account_info_iter)?;
    let snapshot = next_account_info(account_info_iter)?;
    let restaking_config = next_account_info(account_info_iter)?;
    let event_authority = next_account_info(account_info_iter)?;
    let settlement_program = next_account_info(account_info_iter)?;
    let buffer = next_account_info(account_info_iter).ok();

    GkState::load(program_id, state, true)?;

    let payload = &args.payload;

    // The payload must bind THIS instruction (bytes4-selector analog)...
    if payload.ix_discriminator.ne(&SETTLE_DISCRIMINATOR) {
        msg!("Payload ix_discriminator does not bind the settle instruction");
        return Err(SettlementError::DigestMismatch.into());
    }
    // ...and THIS state PDA (address(this) analog).
    if payload.state_pda.ne(&state.key.to_bytes()) {
        msg!("Payload state_pda does not match the state account");
        return Err(SettlementError::InvalidStatePda.into());
    }

    // Consumer-local replay nonce (StateTracker analog).
    let transition_count = {
        let state_data = state.data.borrow();
        GkState::try_from_slice_unchecked(&state_data)?.transition_count()
    };
    if payload.transition_index != transition_count {
        msg!(
            "Invalid transition index: payload {}, state {}",
            payload.transition_index,
            transition_count
        );
        return Err(SettlementError::InvalidTransitionIndex.into());
    }

    // Recompute the certified digest from the payload.
    let digest = payload.digest().map_err(ProgramError::from)?;

    // Verify the aggregated BLS certificate (stateless, read-only).
    verify_certificate_inline(
        ncn_config,
        ncn,
        snapshot,
        restaking_config,
        &digest,
        &args.aggregated_g2,
        &args.aggregated_signature,
        &args.operators_signature_bitmap,
        args.expected_generation,
    )?;

    // ---- Validate the diff (all checks before any effect) ----

    let (expected_event_authority, event_authority_bump) = find_event_authority(program_id);
    if event_authority.key.ne(&expected_event_authority) {
        msg!("Wrong event authority account");
        return Err(SettlementError::EventAuthorityNotSigner.into());
    }
    if settlement_program.key.ne(program_id) {
        msg!("Wrong settlement program account");
        return Err(ProgramError::IncorrectProgramId);
    }

    let mut commitment_root: Option<[u8; 32]> = None;
    for update in payload.updates.iter() {
        match update {
            StateUpdate::Store { data } => {
                if commitment_root.is_some() {
                    msg!("Payload has more than one Store update");
                    return Err(SettlementError::MultipleStore.into());
                }
                commitment_root = Some(*data);
            }
            StateUpdate::Event {
                discriminant,
                payload: event_payload,
            } => {
                let data_len = event_payload.len().saturating_add(8);
                if data_len > MAX_EVENT_CPI_DATA_LEN {
                    msg!("Event data {} exceeds the 10 KiB self-CPI cap", data_len);
                    return Err(SettlementError::EventTooLarge.into());
                }
                if discriminant.eq(&STORY_META_DISCRIMINANT) {
                    verify_story_meta(
                        program_id,
                        state.key,
                        payload.transition_index,
                        event_payload,
                        buffer,
                    )?;
                }
            }
        }
    }
    let commitment_root = commitment_root.ok_or(SettlementError::MissingStore)?;

    // ---- Apply: exactly one commitment write + event self-CPIs + bump ----

    for update in payload.updates.iter() {
        if let StateUpdate::Event {
            discriminant,
            payload: event_payload,
        } = update
        {
            let ix = Instruction {
                program_id: *program_id,
                accounts: vec![AccountMeta::new_readonly(*event_authority.key, true)],
                data: event_cpi_data(discriminant, event_payload),
            };
            invoke_signed(
                &ix,
                &[event_authority.clone(), settlement_program.clone()],
                &[&[EVENT_AUTHORITY_SEED, &[event_authority_bump]]],
            )?;
        }
    }

    let mut state_data = state.try_borrow_mut_data()?;
    let state_account = GkState::try_from_slice_unchecked_mut(&mut state_data)?;
    state_account
        .apply_settle(commitment_root)
        .map_err(ProgramError::from)?;

    msg!(
        "Settled transition {}: commitment_root updated, transition_count now {}",
        payload.transition_index,
        state_account.transition_count()
    );

    Ok(())
}

/// Validates a story_meta event against the staged buffer account:
/// the buffer must be the PDA for (state, transition_index), match the
/// event's recorded buffer pubkey, and hash to `story_sha256` over
/// `data[..len]`.
fn verify_story_meta(
    program_id: &Pubkey,
    state_key: &Pubkey,
    transition_index: u64,
    event_payload: &[u8],
    buffer: Option<&AccountInfo>,
) -> ProgramResult {
    let meta = StoryMeta::try_from_slice(event_payload)
        .map_err(|_| SettlementError::MalformedStoryMeta)?;

    let buffer = buffer.ok_or(SettlementError::MissingBufferAccount)?;
    if buffer.owner.ne(program_id) {
        return Err(ProgramError::InvalidAccountOwner);
    }

    let (expected_buffer, _, _) =
        find_buffer_program_address(program_id, state_key, transition_index);
    if buffer.key.ne(&expected_buffer) || meta.buffer.ne(buffer.key) {
        msg!(
            "Buffer mismatch: account {}, derived {}, event {}",
            buffer.key,
            expected_buffer,
            meta.buffer
        );
        return Err(SettlementError::BufferKeyMismatch.into());
    }

    let data = buffer.data.borrow();
    let content_len = buffer_content_len(data.len()).map_err(ProgramError::from)?;
    let len = meta.len as usize;
    if len > content_len {
        msg!("story len {} exceeds buffer content {}", len, content_len);
        return Err(SettlementError::InvalidBufferBounds.into());
    }
    let content = data
        .get(..len)
        .ok_or(SettlementError::InvalidBufferBounds)?;
    let hash = solana_nostd_sha256::hashv(&[content]);
    if hash.ne(&meta.story_sha256) {
        msg!("Buffer hash mismatch");
        return Err(SettlementError::BufferHashMismatch.into());
    }

    msg!(
        "story_meta verified: {} bytes staged in {}",
        len,
        buffer.key
    );
    Ok(())
}
