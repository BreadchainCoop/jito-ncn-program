//! The per-consumer-app settlement state PDA (docs/INTERFACES.md §4).

use std::mem::size_of;

use bytemuck::{Pod, Zeroable};
use jito_bytemuck::{types::PodU64, AccountDeserialize, Discriminator};
use ncn_program_core::loaders::check_load;
use shank::ShankAccount;
use solana_program::{
    account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey, pubkey::PubkeyError,
};

use crate::error::SettlementError;

/// Account discriminators owned by the gaskiller-settlement program.
/// (Buffer accounts carry NO discriminator: their content must start at data
/// offset 0 so `sha256(buffer.data[..len])` hashes the raw story bytes.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettlementDiscriminators {
    GkState = 0x60,
}

/// Settlement state PDA, seeds `[b"gk_state", ncn, app_id]`.
///
/// Byte layout (jito-bytemuck style, 8-byte discriminator header then the
/// `#[repr(C)]` Pod struct — every field is byte-aligned so there is no
/// padding): discriminator, ncn, app_id, commitment_root, transition_count,
/// sim_profile_id, env_commitment, bump — the §4 field order.
#[derive(Debug, Clone, Copy, Zeroable, Pod, AccountDeserialize, ShankAccount)]
#[repr(C)]
pub struct GkState {
    /// The NCN whose certificates settle into this state.
    ncn: Pubkey,
    /// Consumer application id (32 opaque bytes).
    app_id: [u8; 32],
    /// The single commitment slot (the *_ROOT_SLOT analog).
    commitment_root: [u8; 32],
    /// Consumer-local transition counter — the replay nonce bound into every
    /// payload digest (StateTracker analog).
    transition_count: PodU64,
    /// Versioned simulation profile id pin.
    sim_profile_id: [u8; 32],
    /// Execution environment (overlay manifest) commitment pin.
    env_commitment: [u8; 32],
    /// PDA bump.
    bump: u8,
}

impl Discriminator for GkState {
    const DISCRIMINATOR: u8 = SettlementDiscriminators::GkState as u8;
}

impl GkState {
    pub const GK_STATE_SEED: &'static [u8] = b"gk_state";
    pub const SIZE: usize = 8 + size_of::<Self>();

    pub fn new(
        ncn: &Pubkey,
        app_id: [u8; 32],
        sim_profile_id: [u8; 32],
        env_commitment: [u8; 32],
        bump: u8,
    ) -> Self {
        Self {
            ncn: *ncn,
            app_id,
            commitment_root: [0; 32],
            transition_count: PodU64::from(0),
            sim_profile_id,
            env_commitment,
            bump,
        }
    }

    pub fn seeds(ncn: &Pubkey, app_id: &[u8; 32]) -> Vec<Vec<u8>> {
        vec![
            Self::GK_STATE_SEED.to_vec(),
            ncn.to_bytes().to_vec(),
            app_id.to_vec(),
        ]
    }

    pub fn find_program_address(
        program_id: &Pubkey,
        ncn: &Pubkey,
        app_id: &[u8; 32],
    ) -> (Pubkey, u8, Vec<Vec<u8>>) {
        let seeds = Self::seeds(ncn, app_id);
        let (address, bump) = Pubkey::find_program_address(
            &seeds.iter().map(|s| s.as_slice()).collect::<Vec<_>>(),
            program_id,
        );
        (address, bump, seeds)
    }

    /// Recreates this account's PDA from its stored fields (cheap: uses the
    /// stored bump, one `create_program_address`).
    pub fn pda(&self, program_id: &Pubkey) -> Result<Pubkey, PubkeyError> {
        Pubkey::create_program_address(
            &[
                Self::GK_STATE_SEED,
                self.ncn.as_ref(),
                &self.app_id,
                &[self.bump],
            ],
            program_id,
        )
    }

    /// Validates owner + discriminator (+ writability), then checks the
    /// account key against the PDA derived from the account's own stored
    /// fields — so a settle does not need `ncn`/`app_id` as inputs.
    pub fn load(
        program_id: &Pubkey,
        account: &AccountInfo,
        expect_writable: bool,
    ) -> Result<(), ProgramError> {
        check_load(
            program_id,
            account,
            account.key,
            Some(Self::DISCRIMINATOR),
            expect_writable,
        )?;
        let data = account.data.borrow();
        let state = Self::try_from_slice_unchecked(&data)?;
        let expected = state
            .pda(program_id)
            .map_err(|_| ProgramError::InvalidSeeds)?;
        if account.key.ne(&expected) {
            return Err(ProgramError::InvalidSeeds);
        }
        Ok(())
    }

    /// Validates the PDA against explicit `ncn` + `app_id` (initialize path,
    /// where the account does not exist yet).
    pub fn load_uninitialized(
        program_id: &Pubkey,
        account: &AccountInfo,
        ncn: &Pubkey,
        app_id: &[u8; 32],
    ) -> Result<u8, ProgramError> {
        let (expected, bump, _) = Self::find_program_address(program_id, ncn, app_id);
        if account.key.ne(&expected) {
            return Err(ProgramError::InvalidSeeds);
        }
        if !account.data_is_empty() {
            return Err(ProgramError::AccountAlreadyInitialized);
        }
        Ok(bump)
    }

    pub const fn ncn(&self) -> &Pubkey {
        &self.ncn
    }

    pub const fn app_id(&self) -> &[u8; 32] {
        &self.app_id
    }

    pub const fn commitment_root(&self) -> &[u8; 32] {
        &self.commitment_root
    }

    pub fn transition_count(&self) -> u64 {
        self.transition_count.into()
    }

    pub const fn sim_profile_id(&self) -> &[u8; 32] {
        &self.sim_profile_id
    }

    pub const fn env_commitment(&self) -> &[u8; 32] {
        &self.env_commitment
    }

    pub const fn bump(&self) -> u8 {
        self.bump
    }

    /// Applies the single settle write: new commitment root + counter bump.
    pub fn apply_settle(&mut self, commitment_root: [u8; 32]) -> Result<(), SettlementError> {
        self.commitment_root = commitment_root;
        let next = self
            .transition_count()
            .checked_add(1)
            .ok_or(SettlementError::ArithmeticOverflow)?;
        self.transition_count = PodU64::from(next);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gk_state_size_and_layout() {
        // §4 layout: 5 pubkey-sized fields + u64 + bump, byte-aligned.
        let expected = size_of::<Pubkey>() // ncn
            + 32 // app_id
            + 32 // commitment_root
            + size_of::<PodU64>() // transition_count
            + 32 // sim_profile_id
            + 32 // env_commitment
            + 1; // bump
        assert_eq!(size_of::<GkState>(), expected);
        assert_eq!(GkState::SIZE, 8 + expected);
    }

    #[test]
    fn gk_state_new_and_apply() {
        let ncn = Pubkey::new_unique();
        let mut state = GkState::new(&ncn, [1; 32], [2; 32], [3; 32], 254);
        assert_eq!(state.ncn(), &ncn);
        assert_eq!(state.app_id(), &[1; 32]);
        assert_eq!(state.commitment_root(), &[0; 32]);
        assert_eq!(state.transition_count(), 0);
        assert_eq!(state.sim_profile_id(), &[2; 32]);
        assert_eq!(state.env_commitment(), &[3; 32]);
        assert_eq!(state.bump(), 254);

        state.apply_settle([9; 32]).unwrap();
        assert_eq!(state.commitment_root(), &[9; 32]);
        assert_eq!(state.transition_count(), 1);
    }

    #[test]
    fn gk_state_pda_roundtrip() {
        let program_id = Pubkey::new_unique();
        let ncn = Pubkey::new_unique();
        let app_id = [7u8; 32];
        let (pda, bump, seeds) = GkState::find_program_address(&program_id, &ncn, &app_id);
        assert_eq!(seeds[0], b"gk_state".to_vec());
        assert_eq!(seeds[1], ncn.to_bytes().to_vec());
        assert_eq!(seeds[2], app_id.to_vec());

        let state = GkState::new(&ncn, app_id, [0; 32], [0; 32], bump);
        assert_eq!(state.pda(&program_id).unwrap(), pda);
    }
}
