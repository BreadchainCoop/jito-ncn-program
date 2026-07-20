use jito_jsm_core::loader::{load_signer, load_system_program};
use settlement_core::{
    buffer::{
        buffer_content_len, buffer_seeds, find_buffer_program_address, read_buffer_trailer,
        write_buffer_trailer, BUFFER_TRAILER_LEN, MAX_BUFFER_CONTENT_LEN,
    },
    error::SettlementError,
    instruction::WriteBufferArgs,
    state::GkState,
};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    entrypoint::MAX_PERMITTED_DATA_INCREASE,
    msg,
    program_error::ProgramError,
    pubkey::Pubkey,
};

use crate::system::create_pda_account;

/// Permissionless create-if-needed + append into a transition's buffer PDA.
///
/// The buffer PDA is `[b"gk_buffer", state, transition_index_le]`. On first
/// write the account is created (payer-funded, rent for the FULL
/// `max_size + trailer` target) at up to 10,240 bytes (the CPI allocation
/// cap); later writes grow it toward the target as needed, relocating the
/// trailer. Content is written at `data[offset..offset + bytes.len()]`.
///
/// ### Accounts:
/// 1. `[]` state: settlement state PDA (defines the current transition floor)
/// 2. `[writable]` buffer: the buffer PDA
/// 3. `[writable, signer]` payer: funds rent on creation
/// 4. `[]` system_program
pub fn process_write_buffer(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    args: &WriteBufferArgs,
) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let state = next_account_info(account_info_iter)?;
    let buffer = next_account_info(account_info_iter)?;
    let payer = next_account_info(account_info_iter)?;
    let system_program = next_account_info(account_info_iter)?;

    load_system_program(system_program)?;
    load_signer(payer, true)?;
    GkState::load(program_id, state, false)?;

    // Only current-or-future transitions may be staged.
    {
        let state_data = state.data.borrow();
        let state_account =
            <GkState as jito_bytemuck::AccountDeserialize>::try_from_slice_unchecked(&state_data)?;
        if args.transition_index < state_account.transition_count() {
            msg!(
                "Buffer transition {} already settled",
                args.transition_index
            );
            return Err(SettlementError::InvalidTransitionIndex.into());
        }
    }

    let (buffer_pda, bump, _) =
        find_buffer_program_address(program_id, state.key, args.transition_index);
    if buffer.key.ne(&buffer_pda) {
        msg!("Buffer account is not at the expected PDA");
        return Err(SettlementError::BufferKeyMismatch.into());
    }

    // Create on first write.
    if buffer.data_is_empty() && buffer.owner.ne(program_id) {
        let max_content = args.max_size as usize;
        if max_content > MAX_BUFFER_CONTENT_LEN {
            return Err(SettlementError::InvalidBufferBounds.into());
        }
        let target_total = max_content
            .checked_add(BUFFER_TRAILER_LEN)
            .ok_or(SettlementError::ArithmeticOverflow)?;
        let initial_total = target_total.min(MAX_PERMITTED_DATA_INCREASE);

        let seeds = buffer_seeds(state.key, args.transition_index);
        let mut seeds_with_bump: Vec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
        let bump_slice = [bump];
        seeds_with_bump.push(&bump_slice);

        create_pda_account(
            payer,
            buffer,
            system_program,
            program_id,
            initial_total,
            target_total,
            &seeds_with_bump,
        )?;

        let mut data = buffer.try_borrow_mut_data()?;
        write_buffer_trailer(&mut data, payer.key, args.max_size).map_err(ProgramError::from)?;
    } else if buffer.owner.ne(program_id) {
        return Err(ProgramError::InvalidAccountOwner);
    }

    // Grow toward the recorded target if this write needs more room.
    let (recorded_payer, max_content) = {
        let data = buffer.data.borrow();
        read_buffer_trailer(&data).map_err(ProgramError::from)?
    };
    let write_end = (args.offset as usize)
        .checked_add(args.bytes.len())
        .ok_or(SettlementError::ArithmeticOverflow)?;
    if write_end > max_content as usize {
        msg!(
            "Write [{}, {}) exceeds max content {}",
            args.offset,
            write_end,
            max_content
        );
        return Err(SettlementError::InvalidBufferBounds.into());
    }

    let current_total = buffer.data_len();
    let needed_total = write_end
        .checked_add(BUFFER_TRAILER_LEN)
        .ok_or(SettlementError::ArithmeticOverflow)?;
    if needed_total > current_total {
        let grown_total = current_total
            .checked_add(MAX_PERMITTED_DATA_INCREASE)
            .ok_or(SettlementError::ArithmeticOverflow)?
            .min(
                (max_content as usize)
                    .checked_add(BUFFER_TRAILER_LEN)
                    .ok_or(SettlementError::ArithmeticOverflow)?,
            );
        if needed_total > grown_total {
            // One instruction can only grow 10,240 bytes: the writer must
            // append (roughly) sequentially.
            msg!(
                "Write needs {} bytes but one call can only grow to {}",
                needed_total,
                grown_total
            );
            return Err(SettlementError::InvalidBufferBounds.into());
        }
        buffer.realloc(grown_total, false)?;
        let mut data = buffer.try_borrow_mut_data()?;
        // The old trailer bytes are now content: zero them, then rewrite the
        // trailer at the new end.
        let old_trailer_start = buffer_content_len(current_total).map_err(ProgramError::from)?;
        data.get_mut(old_trailer_start..current_total)
            .ok_or(SettlementError::InvalidBufferBounds)?
            .fill(0);
        write_buffer_trailer(&mut data, &recorded_payer, max_content)
            .map_err(ProgramError::from)?;
    }

    // Append the chunk.
    let mut data = buffer.try_borrow_mut_data()?;
    let content_len = buffer_content_len(data.len()).map_err(ProgramError::from)?;
    if write_end > content_len {
        return Err(SettlementError::InvalidBufferBounds.into());
    }
    data.get_mut(args.offset as usize..write_end)
        .ok_or(SettlementError::InvalidBufferBounds)?
        .copy_from_slice(&args.bytes);

    msg!(
        "Buffer {} wrote {} bytes at offset {}",
        buffer.key,
        args.bytes.len(),
        args.offset
    );

    Ok(())
}
