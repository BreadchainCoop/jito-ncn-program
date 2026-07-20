use core::fmt;
use std::mem::size_of;

use bytemuck::{Pod, Zeroable};
use jito_bytemuck::{
    types::{PodU16, PodU64},
    AccountDeserialize, Discriminator,
};
use shank::{ShankAccount, ShankType};
use solana_program::{account_info::AccountInfo, program_error::ProgramError, pubkey::Pubkey};

use crate::{
    constants::{MAX_ST_MINTS, MAX_VAULTS},
    discriminators::Discriminators,
    error::NCNProgramError,
    loaders::check_load,
};

/// Neutral vault weight: contributes its full delegation to stake weight.
pub const FULL_WEIGHT_BPS: u16 = 10_000;

#[derive(Debug, Clone, Copy, Zeroable, ShankType, Pod)]
#[repr(C)]
pub struct StMintEntry {
    /// The supported token ( ST ) mint
    st_mint: Pubkey,

    /// Weight applied to delegations of vaults holding this mint, in bps of
    /// the delegated amount (docs/INTERFACES.md par.3): stake contribution =
    /// delegation * weight_bps / 10_000. Weights need not sum to 10_000
    /// across mints.
    weight_bps: PodU16,

    // Either a switchboard feed or a weight must be set
    /// The switchboard feed for the mint
    reserve_switchboard_feed: [u8; 32],
}

impl StMintEntry {
    pub fn new(st_mint: &Pubkey, weight_bps: u16) -> Self {
        Self {
            st_mint: *st_mint,
            weight_bps: PodU16::from(weight_bps),
            reserve_switchboard_feed: [0; 32],
        }
    }

    pub const fn st_mint(&self) -> &Pubkey {
        &self.st_mint
    }

    pub fn weight_bps(&self) -> u16 {
        self.weight_bps.into()
    }

    pub fn is_empty(&self) -> bool {
        self.st_mint().eq(&Pubkey::default())
    }
}

impl Default for StMintEntry {
    fn default() -> Self {
        Self::new(&Pubkey::default(), 0)
    }
}

#[derive(Debug, Clone, Copy, Zeroable, ShankType, Pod)]
#[repr(C)]
pub struct VaultEntry {
    /// The vault account
    vault: Pubkey,
    /// The supported token ( ST ) mint of the vault
    st_mint: Pubkey,
    /// The index of the vault in respect to the NCN account
    vault_index: PodU64,
    /// The slot the vault was registered
    slot_registered: PodU64,
}

impl VaultEntry {
    pub const EMPTY_VAULT_INDEX: u64 = u64::MAX;
    pub const EMPTY_SLOT_REGISTERED: u64 = u64::MAX;

    pub fn new(vault: &Pubkey, st_mint: &Pubkey, vault_index: u64, slot_registered: u64) -> Self {
        Self {
            vault: *vault,
            st_mint: *st_mint,
            vault_index: PodU64::from(vault_index),
            slot_registered: PodU64::from(slot_registered),
        }
    }

    pub const fn vault(&self) -> &Pubkey {
        &self.vault
    }

    pub const fn st_mint(&self) -> &Pubkey {
        &self.st_mint
    }

    pub fn vault_index(&self) -> u64 {
        self.vault_index.into()
    }

    pub fn slot_registered(&self) -> u64 {
        self.slot_registered.into()
    }

    pub fn is_empty(&self) -> bool {
        self.slot_registered() == u64::MAX
    }
}

impl Default for VaultEntry {
    fn default() -> Self {
        Self::new(
            &Pubkey::default(),
            &Pubkey::default(),
            Self::EMPTY_VAULT_INDEX,
            Self::EMPTY_SLOT_REGISTERED,
        )
    }
}

#[derive(Debug, Clone, Copy, Zeroable, Pod, AccountDeserialize, ShankAccount)]
#[repr(C)]
pub struct VaultRegistry {
    /// The NCN the vault registry is associated with
    pub ncn: Pubkey,
    /// The bump seed for the PDA
    pub bump: u8,
    /// The list of supported token ( ST ) mints
    pub st_mint_list: [StMintEntry; 16],
    /// The list of vaults
    pub vault_list: [VaultEntry; 16],
}

impl Discriminator for VaultRegistry {
    const DISCRIMINATOR: u8 = Discriminators::VaultRegistry as u8;
}

impl VaultRegistry {
    const VAULT_REGISTRY_SEED: &'static [u8] = b"vault_registry";
    pub const SIZE: usize = 8 + size_of::<Self>();

    pub fn new(ncn: &Pubkey, bump: u8) -> Self {
        Self {
            ncn: *ncn,
            bump,
            st_mint_list: [StMintEntry::default(); MAX_ST_MINTS],
            vault_list: [VaultEntry::default(); MAX_VAULTS],
        }
    }

    pub fn initialize(&mut self, ncn: &Pubkey, bump: u8) {
        // Initializes field by field to avoid overflowing stack
        self.ncn = *ncn;
        self.bump = bump;
        self.st_mint_list = [StMintEntry::default(); MAX_ST_MINTS];
        self.vault_list = [VaultEntry::default(); MAX_VAULTS];
    }

    pub fn seeds(ncn: &Pubkey) -> Vec<Vec<u8>> {
        Vec::from_iter(
            [Self::VAULT_REGISTRY_SEED.to_vec(), ncn.to_bytes().to_vec()]
                .iter()
                .cloned(),
        )
    }

    pub fn find_program_address(program_id: &Pubkey, ncn: &Pubkey) -> (Pubkey, u8, Vec<Vec<u8>>) {
        let seeds = Self::seeds(ncn);
        let seeds_iter: Vec<_> = seeds.iter().map(|s| s.as_slice()).collect();
        let (address, bump) = Pubkey::find_program_address(&seeds_iter, program_id);
        (address, bump, seeds)
    }

    pub fn load(
        program_id: &Pubkey,
        account: &AccountInfo,
        ncn: &Pubkey,
        expect_writable: bool,
    ) -> Result<(), ProgramError> {
        let expected_pda = Self::find_program_address(program_id, ncn).0;
        check_load(
            program_id,
            account,
            &expected_pda,
            Some(Self::DISCRIMINATOR),
            expect_writable,
        )
    }

    pub fn has_st_mint(&self, mint: &Pubkey) -> bool {
        self.st_mint_list.iter().any(|m| m.st_mint.eq(mint))
    }

    pub fn check_st_mint_entry(_entry: &StMintEntry) -> Result<(), ProgramError> {
        Ok(())
    }

    pub fn register_st_mint(
        &mut self,
        st_mint: &Pubkey,
        weight_bps: u16,
    ) -> Result<(), ProgramError> {
        if weight_bps == 0 || weight_bps > FULL_WEIGHT_BPS {
            return Err(NCNProgramError::InvalidWeightBps.into());
        }

        // Check if mint is already in the list
        if self.st_mint_list.iter().any(|m| m.st_mint.eq(st_mint)) {
            return Err(NCNProgramError::MintInTable.into());
        }

        // Insert at the first empty slot
        let mint_entry = self
            .st_mint_list
            .iter_mut()
            .find(|m| m.st_mint == StMintEntry::default().st_mint)
            .ok_or(NCNProgramError::VaultRegistryListFull)?;

        let new_mint_entry = StMintEntry::new(st_mint, weight_bps);

        Self::check_st_mint_entry(&new_mint_entry)?;

        *mint_entry = new_mint_entry;

        Ok(())
    }

    pub fn set_st_mint(&mut self, st_mint: &Pubkey) -> Result<(), ProgramError> {
        let mint_entry = self
            .st_mint_list
            .iter_mut()
            .find(|m| m.st_mint.eq(st_mint))
            .ok_or(NCNProgramError::MintEntryNotFound)?;

        let updated_mint_entry = *mint_entry;

        Self::check_st_mint_entry(&updated_mint_entry)?;

        *mint_entry = updated_mint_entry;

        Ok(())
    }

    pub fn register_vault(
        &mut self,
        vault: &Pubkey,
        st_mint: &Pubkey,
        vault_index: u64,
        current_slot: u64,
    ) -> Result<(), ProgramError> {
        // Check if (mint, vault_index) is already in the list
        if self.vault_list.iter().any(|m| m.vault.eq(vault)) {
            return Ok(());
        }

        // Insert at the first empty slot
        let mint_entry = self
            .vault_list
            .iter_mut()
            .find(|m| m.st_mint == VaultEntry::default().st_mint)
            .ok_or(NCNProgramError::VaultRegistryListFull)?;

        *mint_entry = VaultEntry::new(vault, st_mint, vault_index, current_slot);
        Ok(())
    }

    pub const fn get_vault_entries(&self) -> &[VaultEntry; MAX_VAULTS] {
        &self.vault_list
    }

    /// Position of the vault's entry within `vault_list` — the stable key the
    /// snapshot uses for per-vault stake contributions.
    pub fn get_vault_registry_slot(&self, vault: &Pubkey) -> Result<usize, NCNProgramError> {
        self.vault_list
            .iter()
            .position(|entry| !entry.is_empty() && entry.vault().eq(vault))
            .ok_or(NCNProgramError::VaultNotInRegistry)
    }

    /// The weight (bps of delegation) applied to a vault's delegations: the
    /// weight of the vault's supported mint.
    pub fn get_vault_weight_bps(&self, vault: &Pubkey) -> Result<u16, NCNProgramError> {
        let entry = self
            .vault_list
            .iter()
            .find(|entry| !entry.is_empty() && entry.vault().eq(vault))
            .ok_or(NCNProgramError::VaultNotInRegistry)?;

        let mint_entry = self
            .st_mint_list
            .iter()
            .find(|m| m.st_mint().eq(entry.st_mint()))
            .ok_or(NCNProgramError::MintEntryNotFound)?;

        Ok(mint_entry.weight_bps())
    }

    pub fn vault_count(&self) -> u64 {
        self.vault_list.iter().filter(|m| !m.is_empty()).count() as u64
    }

    pub fn get_valid_vault_entries(&self) -> Vec<VaultEntry> {
        self.vault_list
            .iter()
            .filter(|m| !m.is_empty())
            .copied()
            .collect()
    }

    pub fn get_valid_mint_entries(&self) -> Vec<StMintEntry> {
        self.st_mint_list
            .iter()
            .filter(|m| !m.is_empty())
            .copied()
            .collect()
    }

    pub const fn get_mint_entries(&self) -> &[StMintEntry; MAX_ST_MINTS] {
        &self.st_mint_list
    }

    pub fn st_mint_count(&self) -> usize {
        self.st_mint_list.iter().filter(|m| !m.is_empty()).count()
    }

    pub fn get_mint_entry(&self, st_mint: &Pubkey) -> Result<StMintEntry, ProgramError> {
        let mint_entry = self
            .st_mint_list
            .iter()
            .find(|m| m.st_mint().eq(st_mint))
            .ok_or(NCNProgramError::MintEntryNotFound)?;

        Ok(*mint_entry)
    }
}

#[rustfmt::skip]
impl fmt::Display for VaultRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "\n\n----------- Vault Registry -------------")?;
        writeln!(f, "  NCN:                          {}", self.ncn)?;
        writeln!(f, "  ST Mints:                     ")?;
        for mint in self.get_valid_mint_entries() {
            writeln!(f, "    Mint:                       {}", mint.st_mint())?;
        }
        writeln!(f, "  Vaults:                     ")?;
        for vault in self.get_valid_vault_entries() {
            writeln!(f, "    Vault:                      {}", vault.vault())?;
            writeln!(f, "      Mint:                     {}", vault.st_mint())?;
            writeln!(f, "      Index:                    {}", vault.vault_index())?;
            writeln!(f, "      Slot Registered:          {}\n", vault.slot_registered())?;
        }


        Ok(())
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn test_len() {
        use std::mem::size_of;

        let expected_total = size_of::<Pubkey>() // ncn
            + 1 // bump
            + size_of::<[StMintEntry; MAX_ST_MINTS]>() // st_mint_list
            + size_of::<[VaultEntry; MAX_VAULTS]>(); // vault_list

        assert_eq!(size_of::<VaultRegistry>(), expected_total);

        let vault_registry = VaultRegistry::new(&Pubkey::default(), 0);
        assert_eq!(vault_registry.vault_list.len(), MAX_VAULTS);
    }

    #[test]
    fn test_add_mint() {
        let mut vault_registry = VaultRegistry::new(&Pubkey::default(), 0);
        let mint = Pubkey::new_unique();

        // Test 1: Initial registration should succeed
        assert_eq!(vault_registry.get_valid_mint_entries().len(), 0);
        vault_registry
            .register_st_mint(&mint, FULL_WEIGHT_BPS)
            .unwrap();
        assert_eq!(vault_registry.get_valid_mint_entries().len(), 1);

        // Test 2: Trying to add the same mint should fail
        let result = vault_registry.register_st_mint(&mint, FULL_WEIGHT_BPS);
        assert!(result.is_err());
        assert_eq!(vault_registry.get_valid_mint_entries().len(), 1);

        // Test 3: Invalid weights are rejected
        let some_mint = Pubkey::new_unique();
        assert!(vault_registry.register_st_mint(&some_mint, 0).is_err());
        assert!(vault_registry
            .register_st_mint(&some_mint, FULL_WEIGHT_BPS + 1)
            .is_err());

        // Test 4: The registry holds exactly MAX_ST_MINTS entries
        for _ in 1..MAX_ST_MINTS {
            vault_registry
                .register_st_mint(&Pubkey::new_unique(), 5_000)
                .unwrap();
        }
        assert_eq!(vault_registry.get_valid_mint_entries().len(), MAX_ST_MINTS);

        // Test 5: Attempting to add to a full list should fail
        let overflow_mint = Pubkey::new_unique();
        let result = vault_registry.register_st_mint(&overflow_mint, FULL_WEIGHT_BPS);
        assert!(result.is_err());
        assert_eq!(vault_registry.get_valid_mint_entries().len(), MAX_ST_MINTS);

        // Test 6: has_st_mint should work correctly
        assert!(vault_registry.has_st_mint(&mint));
        assert!(!vault_registry.has_st_mint(&overflow_mint));

        // Test 7: weight is recorded
        let mut fresh_registry = VaultRegistry::new(&Pubkey::default(), 0);
        let mint_with_weight = Pubkey::new_unique();
        fresh_registry
            .register_st_mint(&mint_with_weight, 2_500)
            .unwrap();
        assert_eq!(
            fresh_registry
                .get_mint_entry(&mint_with_weight)
                .unwrap()
                .weight_bps(),
            2_500
        );
    }

    #[test]
    fn test_set_st_mint() {
        let mut vault_registry = VaultRegistry::new(&Pubkey::default(), 0);
        let mint = Pubkey::new_unique();

        // First register a mint to update
        vault_registry
            .register_st_mint(&mint, FULL_WEIGHT_BPS)
            .unwrap();

        // Test 1: Verify initial state
        let entry = vault_registry.get_mint_entry(&mint).unwrap();
        assert_eq!(entry.st_mint(), &mint);
    }
}
