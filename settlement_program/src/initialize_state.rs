use jito_bytemuck::{AccountDeserialize, Discriminator};
use jito_jsm_core::loader::load_system_program;
use jito_restaking_core::ncn::Ncn;
use settlement_core::{instruction::InitializeStateArgs, state::GkState};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint::ProgramResult,
    msg,
    pubkey::Pubkey,
};

use crate::system::create_pda_account;

/// Creates the per-app settlement state PDA (INTERFACES.md §4), payer-funded.
///
/// ### Accounts:
/// 1. `[writable]` state: `[b"gk_state", ncn, app_id]` PDA, must not exist
/// 2. `[]` ncn: the jito-restaking NCN this consumer settles against
/// 3. `[writable, signer]` payer: funds rent
/// 4. `[]` system_program
pub fn process_initialize_state(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    args: &InitializeStateArgs,
) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let state = next_account_info(account_info_iter)?;
    let ncn = next_account_info(account_info_iter)?;
    let payer = next_account_info(account_info_iter)?;
    let system_program = next_account_info(account_info_iter)?;

    load_system_program(system_program)?;
    Ncn::load(&jito_restaking_program::id(), ncn, false)?;
    jito_jsm_core::loader::load_signer(payer, true)?;

    let bump = GkState::load_uninitialized(program_id, state, ncn.key, &args.app_id)?;

    let seeds = GkState::seeds(ncn.key, &args.app_id);
    let mut seeds_with_bump: Vec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
    let bump_slice = [bump];
    seeds_with_bump.push(&bump_slice);

    create_pda_account(
        payer,
        state,
        system_program,
        program_id,
        GkState::SIZE,
        GkState::SIZE,
        &seeds_with_bump,
    )?;

    let mut state_data = state.try_borrow_mut_data()?;
    state_data
        .first_mut()
        .map(|b| *b = GkState::DISCRIMINATOR)
        .ok_or(solana_program::program_error::ProgramError::AccountDataTooSmall)?;
    let state_account = GkState::try_from_slice_unchecked_mut(&mut state_data)?;
    *state_account = GkState::new(
        ncn.key,
        args.app_id,
        args.sim_profile_id,
        args.env_commitment,
        bump,
    );

    msg!(
        "Initialized gk_state for ncn {} app_id {:?} at {}",
        ncn.key,
        &args.app_id[..4],
        state.key
    );

    Ok(())
}
