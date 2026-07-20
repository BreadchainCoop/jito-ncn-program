//! Small system-program helpers for payer-funded PDA accounts.

use solana_program::{
    account_info::AccountInfo, entrypoint::ProgramResult, program::invoke, program::invoke_signed,
    rent::Rent, system_instruction, sysvar::Sysvar,
};

/// Creates a PDA-owned account funded by a transaction-signer payer.
///
/// `space` must be <= MAX_PERMITTED_DATA_INCREASE (10,240) — CPI-created
/// accounts cannot be larger; bigger accounts are grown afterwards.
/// `rent_space` is the size the rent deposit is computed for (pre-funding
/// later growth in one transfer).
pub fn create_pda_account<'a, 'info>(
    payer: &'a AccountInfo<'info>,
    new_account: &'a AccountInfo<'info>,
    system_program: &'a AccountInfo<'info>,
    owner: &solana_program::pubkey::Pubkey,
    space: usize,
    rent_space: usize,
    seeds_with_bump: &[&[u8]],
) -> ProgramResult {
    let rent = Rent::get()?;
    let required_lamports = rent
        .minimum_balance(rent_space)
        .saturating_sub(new_account.lamports());

    if required_lamports > 0 {
        invoke(
            &system_instruction::transfer(payer.key, new_account.key, required_lamports),
            &[payer.clone(), new_account.clone(), system_program.clone()],
        )?;
    }

    invoke_signed(
        &system_instruction::allocate(new_account.key, space as u64),
        &[new_account.clone(), system_program.clone()],
        &[seeds_with_bump],
    )?;

    invoke_signed(
        &system_instruction::assign(new_account.key, owner),
        &[new_account.clone(), system_program.clone()],
        &[seeds_with_bump],
    )
}
