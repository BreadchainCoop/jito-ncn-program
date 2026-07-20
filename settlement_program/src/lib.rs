//! gaskiller-settlement program (docs/INTERFACES.md §4, PARITY-PLAN.md §3.5).
//!
//! Native no-Anchor program. Instruction data is
//! `8-byte discriminator ‖ borsh(args)` (see `settlement_core::instruction`);
//! any other leading 8 bytes are treated as an event self-CPI, which is only
//! accepted when the program's event-authority PDA signs (i.e. when the
//! program invoked itself via `invoke_signed`) — the hand-rolled `emit_cpi`
//! analog.

mod close_buffer;
mod emit_event;
mod initialize_state;
mod settle;
mod system;
mod verify;
mod write_buffer;

use borsh::BorshDeserialize;
use settlement_core::instruction::{
    CloseBufferArgs, InitializeStateArgs, SettleArgs, WriteBufferArgs, CLOSE_BUFFER_DISCRIMINATOR,
    INITIALIZE_STATE_DISCRIMINATOR, SETTLE_DISCRIMINATOR, WRITE_BUFFER_DISCRIMINATOR,
};
use solana_program::{
    account_info::AccountInfo, declare_id, entrypoint::ProgramResult, msg,
    program_error::ProgramError, pubkey::Pubkey,
};
#[cfg(not(feature = "no-entrypoint"))]
use solana_security_txt::security_txt;

use crate::{
    close_buffer::process_close_buffer, emit_event::process_emit_event,
    initialize_state::process_initialize_state, settle::process_settle,
    write_buffer::process_write_buffer,
};

declare_id!("6XTdBk798fEpM2VPBXpkLPw4zJJLvASaiyHaEmj9Ripx");

#[cfg(not(feature = "no-entrypoint"))]
security_txt! {
    // Required fields
    name: "gaskiller-settlement",
    project_url: "https://github.com/BreadchainCoop/jito-ncn-program",
    contacts: "email:team@breadchain.xyz",
    policy: "https://github.com/BreadchainCoop/jito-ncn-program",
    // Optional Fields
    preferred_languages: "en",
    source_code: "https://github.com/BreadchainCoop/jito-ncn-program"
}

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process_instruction);

pub fn process_instruction(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    if *program_id != id() {
        return Err(ProgramError::IncorrectProgramId);
    }

    let (discriminator, args) = instruction_data
        .split_at_checked(8)
        .ok_or(ProgramError::InvalidInstructionData)?;
    let discriminator: [u8; 8] = discriminator
        .try_into()
        .map_err(|_| ProgramError::InvalidInstructionData)?;

    match discriminator {
        INITIALIZE_STATE_DISCRIMINATOR => {
            msg!("Instruction: InitializeState");
            let args = InitializeStateArgs::try_from_slice(args)?;
            process_initialize_state(program_id, accounts, &args)
        }
        WRITE_BUFFER_DISCRIMINATOR => {
            msg!("Instruction: WriteBuffer");
            let args = WriteBufferArgs::try_from_slice(args)?;
            process_write_buffer(program_id, accounts, &args)
        }
        SETTLE_DISCRIMINATOR => {
            msg!("Instruction: Settle");
            let args = SettleArgs::try_from_slice(args)?;
            process_settle(program_id, accounts, &args)
        }
        CLOSE_BUFFER_DISCRIMINATOR => {
            msg!("Instruction: CloseBuffer");
            let args = CloseBufferArgs::try_from_slice(args)?;
            process_close_buffer(program_id, accounts, &args)
        }
        // Any other discriminator is an event self-CPI: a no-op branch that
        // logs, gated on the event-authority PDA signature.
        _ => process_emit_event(program_id, accounts, instruction_data),
    }
}
