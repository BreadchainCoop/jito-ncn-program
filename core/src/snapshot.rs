use core::fmt;
use std::mem::size_of;

use bytemuck::{Pod, Zeroable};
use jito_bytemuck::{
    types::{PodBool, PodU64},
    AccountDeserialize, Discriminator,
};
use jito_vault_core::vault_operator_delegation::VaultOperatorDelegation;
use shank::{ShankAccount, ShankType};
use solana_bn254::compression::prelude::alt_bn128_g1_decompress;
use solana_program::{account_info::AccountInfo, msg, program_error::ProgramError, pubkey::Pubkey};
use spl_math::precise_number::PreciseNumber;

use crate::{
    constants::{G1_COMPRESSED_POINT_SIZE, MAX_OPERATORS, MAX_VAULTS},
    discriminators::Discriminators,
    error::NCNProgramError,
    g1_point::{G1CompressedPoint, G1Point},
    loaders::check_load,
    stake_weight::StakeWeights,
};

// PDA'd ["snapshot", NCN]
#[derive(Debug, Clone, Copy, Zeroable, Pod, AccountDeserialize, ShankAccount)]
#[repr(C)]
pub struct Snapshot {
    /// The NCN this snapshot is for
    ncn: Pubkey,
    /// Bump seed for the PDA
    bump: u8,
    /// Slot Snapshot was created
    slot_created: PodU64,
    /// Keeps track of the number of completed operator registration through `snapshot_vault_operator_delegation` and `initialize_operator_snapshot`
    operators_registered: PodU64,
    /// Keeps track of the number of valid operator vault delegations
    operators_can_vote_count: PodU64,
    /// total Operators G1 Pubkey aggregated stake weights
    total_aggregated_g1_pubkey: [u8; 32],
    /// Array of operator snapshots
    operator_snapshots: [OperatorSnapshot; 256],
    /// Minimum stake weight required to vote
    minimum_stake: StakeWeights,

    last_snapshot_slot: PodU64, // Track the last slot when the snapshot was taken
}

impl Discriminator for Snapshot {
    const DISCRIMINATOR: u8 = Discriminators::Snapshot as u8;
}

impl Snapshot {
    const SNAPSHOT_SEED: &'static [u8] = b"snapshot";
    pub const SIZE: usize = 8 + size_of::<Self>();

    pub fn new(ncn: &Pubkey, bump: u8, current_slot: u64, minimum_stake: StakeWeights) -> Self {
        Self {
            ncn: *ncn,
            slot_created: PodU64::from(current_slot),
            last_snapshot_slot: PodU64::from(0),
            bump,
            operators_registered: PodU64::from(0),
            operators_can_vote_count: PodU64::from(0),
            total_aggregated_g1_pubkey: [0; G1_COMPRESSED_POINT_SIZE],
            operator_snapshots: [OperatorSnapshot::default(); 256],
            minimum_stake,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn initialize(
        &mut self,
        ncn: &Pubkey,
        bump: u8,
        current_slot: u64,
        minimum_stake: StakeWeights,
    ) {
        // Initializes field by field to avoid overflowing stack
        self.ncn = *ncn;
        self.slot_created = PodU64::from(current_slot);
        self.last_snapshot_slot = PodU64::from(0);
        self.bump = bump;
        self.operators_registered = PodU64::from(0);
        self.operators_can_vote_count = PodU64::from(0);
        self.total_aggregated_g1_pubkey = [0; G1_COMPRESSED_POINT_SIZE];
        let default_operator_snapshot = OperatorSnapshot::default();
        self.operator_snapshots = [default_operator_snapshot; 256];
        self.minimum_stake = minimum_stake;
    }

    pub fn seeds(ncn: &Pubkey) -> Vec<Vec<u8>> {
        Vec::from_iter(
            [Self::SNAPSHOT_SEED.to_vec(), ncn.to_bytes().to_vec()]
                .iter()
                .cloned(),
        )
    }

    pub fn find_program_address(program_id: &Pubkey, ncn: &Pubkey) -> (Pubkey, u8, Vec<Vec<u8>>) {
        let seeds = Self::seeds(ncn);
        let seeds_iter: Vec<_> = seeds.iter().map(|s| s.as_slice()).collect();
        let (pda, bump) = Pubkey::find_program_address(&seeds_iter, program_id);
        (pda, bump, seeds)
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

    pub fn load_to_close(
        program_id: &Pubkey,
        account_to_close: &AccountInfo,
        ncn: &Pubkey,
    ) -> Result<(), ProgramError> {
        Self::load(program_id, account_to_close, ncn, true)
    }

    pub fn operators_registered(&self) -> u64 {
        self.operators_registered.into()
    }

    pub fn operators_can_vote_count(&self) -> u64 {
        self.operators_can_vote_count.into()
    }

    pub const fn total_aggregated_g1_pubkey(&self) -> [u8; G1_COMPRESSED_POINT_SIZE] {
        self.total_aggregated_g1_pubkey
    }

    pub fn last_snapshot_slot(&self) -> u64 {
        self.last_snapshot_slot.into()
    }

    pub fn minimum_stake(&self) -> &StakeWeights {
        &self.minimum_stake
    }

    fn increment_operator_registration(
        &mut self,
        current_slot: u64,
    ) -> Result<(), NCNProgramError> {
        self.operators_registered = PodU64::from(
            self.operators_registered()
                .checked_add(1)
                .ok_or(NCNProgramError::ArithmeticOverflow)?,
        );

        self.last_snapshot_slot = PodU64::from(current_slot);

        Ok(())
    }

    /// Adds a G1 pubkey to the total aggregated pubkey
    pub fn add_g1_pubkey_to_total_agg(
        &mut self,
        g1_pubkey: &[u8; G1_COMPRESSED_POINT_SIZE],
    ) -> Result<(), NCNProgramError> {
        alt_bn128_g1_decompress(g1_pubkey).map_err(|_| NCNProgramError::InvalidG1Pubkey)?;
        if self.total_aggregated_g1_pubkey == [0u8; G1_COMPRESSED_POINT_SIZE] {
            self.total_aggregated_g1_pubkey = *g1_pubkey;
        } else {
            let total_aggregated_g1_pubkey_point =
                G1Point::try_from(&G1CompressedPoint(self.total_aggregated_g1_pubkey))?;
            let pk_point = G1Point::try_from(&G1CompressedPoint(*g1_pubkey))?;
            let new_point = total_aggregated_g1_pubkey_point + pk_point;
            let compressed = G1CompressedPoint::try_from(new_point)?;
            self.total_aggregated_g1_pubkey = compressed.0;
        }
        Ok(())
    }

    /// Subtracts a G1 pubkey from the total aggregated pubkey
    pub fn subtract_g1_pubkey_from_total_agg(
        &mut self,
        g1_pubkey: &[u8; G1_COMPRESSED_POINT_SIZE],
    ) -> Result<(), NCNProgramError> {
        alt_bn128_g1_decompress(g1_pubkey).map_err(|_| NCNProgramError::InvalidG1Pubkey)?;
        let total_aggregated_g1_pubkey_point =
            G1Point::try_from(&G1CompressedPoint(self.total_aggregated_g1_pubkey))?;
        let pk_point = G1Point::try_from(&G1CompressedPoint(*g1_pubkey))?;
        let new_point = total_aggregated_g1_pubkey_point + pk_point.negate();
        let compressed = G1CompressedPoint::try_from(new_point)?;
        self.total_aggregated_g1_pubkey = compressed.0;
        Ok(())
    }

    pub fn register_operator_g1_pubkey(
        &mut self,
        operator_g1_pubkey: &[u8; G1_COMPRESSED_POINT_SIZE],
    ) -> Result<(), NCNProgramError> {
        self.add_g1_pubkey_to_total_agg(operator_g1_pubkey)
    }

    pub fn update_g1_operator_pubkey(
        &mut self,
        operator_g1_pubkey: &[u8; G1_COMPRESSED_POINT_SIZE],
        operator: &Pubkey,
    ) -> Result<(), NCNProgramError> {
        // First, get the old G1 pubkey if the operator exists
        let operator_registerd = self.operators_registered();

        match operator_registerd {
            0 => {
                msg!("No operators registered in snapshot");
                return Err(NCNProgramError::NoOperatorsRegistered);
            }
            1 => {
                self.total_aggregated_g1_pubkey = *operator_g1_pubkey;
                // Now update the operator's pubkey
                if let Some(operator_snapshot) = self.find_mut_operator_snapshot(operator) {
                    operator_snapshot.update_g1_pubkey(&operator_g1_pubkey);
                }
            }
            count if count < MAX_OPERATORS as u64 => {
                let old_g1_pubkey = self
                    .find_operator_snapshot(operator)
                    .map(|operator_snapshot| operator_snapshot.g1_pubkey());

                if let Some(old_pubkey) = old_g1_pubkey {
                    // Subtract the old pubkey from the total aggregated pubkey
                    self.subtract_g1_pubkey_from_total_agg(&old_pubkey)?;

                    // Now update the operator's pubkey
                    if let Some(operator_snapshot) = self.find_mut_operator_snapshot(operator) {
                        operator_snapshot.update_g1_pubkey(&operator_g1_pubkey);
                    }

                    // Add the new pubkey to the total aggregated pubkey
                    self.add_g1_pubkey_to_total_agg(operator_g1_pubkey)?;
                } else {
                    msg!("Operator snapshot not found for operator: {}", operator);
                }
            }
            _ => {
                msg!("Operator count is not valid: {}", operator_registerd);
                return Err(NCNProgramError::InvalidOperatorCount);
            }
        }

        Ok(())
    }

    pub fn operator_snapshots(&self) -> &[OperatorSnapshot] {
        &self.operator_snapshots
    }

    /// Get an operator snapshot by operator index
    pub fn get_operator_snapshot(&self, ncn_operator_index: u64) -> Option<&OperatorSnapshot> {
        if ncn_operator_index >= self.operators_registered() {
            return None;
        }
        let snapshot = &self.operator_snapshots[ncn_operator_index as usize];
        Some(snapshot)
    }

    /// Get a mutable operator snapshot by operator index
    pub fn get_mut_operator_snapshot(
        &mut self,
        ncn_operator_index: u64,
    ) -> Option<&mut OperatorSnapshot> {
        if ncn_operator_index >= self.operators_registered() {
            return None;
        }
        let snapshot = &mut self.operator_snapshots[ncn_operator_index as usize];
        Some(snapshot)
    }

    /// Find an operator snapshot by operator pubkey
    pub fn find_operator_snapshot(&self, operator: &Pubkey) -> Option<&OperatorSnapshot> {
        self.operator_snapshots.iter().find(|snapshot| {
            snapshot.operator() == operator && snapshot.ncn_operator_index() != u64::MAX
        })
    }

    /// Find a mutable operator snapshot by operator pubkey
    pub fn find_mut_operator_snapshot(
        &mut self,
        operator: &Pubkey,
    ) -> Option<&mut OperatorSnapshot> {
        self.operator_snapshots.iter_mut().find(|snapshot| {
            snapshot.operator() == operator && snapshot.ncn_operator_index() != u64::MAX
        })
    }

    /// Add a new operator snapshot to the array
    pub fn add_operator_snapshot(
        &mut self,
        operator_snapshot: OperatorSnapshot,
        slot: u64,
    ) -> Result<(), NCNProgramError> {
        let operator_index = operator_snapshot.ncn_operator_index();
        if operator_index >= MAX_OPERATORS as u64 {
            return Err(NCNProgramError::TooManyVaultOperatorDelegations);
        }

        // Check if slot is already occupied
        if self.operator_snapshots[operator_index as usize].ncn_operator_index() != u64::MAX {
            return Err(NCNProgramError::DuplicateVaultOperatorDelegation);
        }

        self.operator_snapshots[operator_index as usize] = operator_snapshot;

        self.increment_operator_registration(slot)?;
        Ok(())
    }

    /// Get all active operator snapshots
    pub fn get_active_operator_snapshots(&self) -> Vec<&OperatorSnapshot> {
        self.operator_snapshots
            .iter()
            .filter(|snapshot| snapshot.ncn_operator_index() != u64::MAX && snapshot.is_active())
            .collect()
    }

    /// Update an operator snapshot in the array
    pub fn update_operator_snapshot(
        &mut self,
        operator_index: usize,
        operator_snapshot: &OperatorSnapshot,
    ) {
        self.operator_snapshots[operator_index] = *operator_snapshot;
    }
}

// Operator snapshot entry within Snapshot
#[derive(Debug, Clone, Copy, Zeroable, Pod, ShankType)]
#[repr(C)]
pub struct OperatorSnapshot {
    operator: Pubkey,

    g1_pubkey: [u8; 32], // G1 compressed pubkey

    slot_created: PodU64,
    last_snapshot_slot: PodU64,

    is_active: PodBool,

    ncn_operator_index: PodU64,

    operator_index: PodU64,

    has_minimum_stake: PodBool,
    has_minimum_stake_next_epoch: PodBool,

    stake_weight: StakeWeights,
    next_epoch_stake_weight: StakeWeights,
}

impl Default for OperatorSnapshot {
    fn default() -> Self {
        Self {
            operator: Pubkey::default(),
            g1_pubkey: [0; G1_COMPRESSED_POINT_SIZE],
            slot_created: PodU64::from(0),
            last_snapshot_slot: PodU64::from(0),
            is_active: PodBool::from(false),
            ncn_operator_index: PodU64::from(u64::MAX),
            operator_index: PodU64::from(u64::MAX),
            has_minimum_stake: PodBool::from(false),
            has_minimum_stake_next_epoch: PodBool::from(false),
            stake_weight: StakeWeights::default(),
            next_epoch_stake_weight: StakeWeights::default(),
        }
    }
}

impl OperatorSnapshot {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        operator: &Pubkey,
        current_slot: u64,
        is_active: bool,
        ncn_operator_index: u64,
        operator_index: u64,
        g1_pubkey: [u8; G1_COMPRESSED_POINT_SIZE],
    ) -> Result<Self, NCNProgramError> {
        Ok(Self {
            operator: *operator,
            slot_created: PodU64::from(current_slot),
            last_snapshot_slot: PodU64::from(0),
            is_active: PodBool::from(is_active),
            ncn_operator_index: PodU64::from(ncn_operator_index),
            operator_index: PodU64::from(operator_index),
            g1_pubkey,
            has_minimum_stake: PodBool::from(false),
            has_minimum_stake_next_epoch: PodBool::from(false),
            stake_weight: StakeWeights::default(),
            next_epoch_stake_weight: StakeWeights::default(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn initialize(
        &mut self,
        operator: &Pubkey,
        current_slot: u64,
        is_active: bool,
        ncn_operator_index: u64,
        operator_index: u64,
        g1_pubkey: [u8; G1_COMPRESSED_POINT_SIZE],
        vault_operator_delegation_count: u64,
    ) -> Result<(), NCNProgramError> {
        if vault_operator_delegation_count > MAX_VAULTS as u64 {
            return Err(NCNProgramError::TooManyVaultOperatorDelegations);
        }

        // Initializes field by field to avoid overflowing stack
        self.operator = *operator;
        self.slot_created = PodU64::from(current_slot);
        self.last_snapshot_slot = PodU64::from(0);
        self.is_active = PodBool::from(is_active);
        self.ncn_operator_index = PodU64::from(ncn_operator_index);
        self.operator_index = PodU64::from(operator_index);
        self.g1_pubkey = g1_pubkey;
        self.has_minimum_stake = PodBool::from(false);
        self.has_minimum_stake_next_epoch = PodBool::from(false);
        self.stake_weight = StakeWeights::default();

        Ok(())
    }

    pub fn ncn_operator_index(&self) -> u64 {
        self.ncn_operator_index.into()
    }

    pub fn is_active(&self) -> bool {
        self.is_active.into()
    }

    pub fn g1_pubkey(&self) -> [u8; G1_COMPRESSED_POINT_SIZE] {
        self.g1_pubkey
    }

    pub fn slot_created(&self) -> u64 {
        self.slot_created.into()
    }

    pub fn last_snapshot_slot(&self) -> u64 {
        self.last_snapshot_slot.into()
    }

    pub fn have_valid_bn128_g1_pubkey(&self) -> bool {
        self.g1_pubkey != [0u8; 32]
    }

    pub const fn operator(&self) -> &Pubkey {
        &self.operator
    }

    pub fn has_minimum_stake(&self) -> bool {
        self.has_minimum_stake.into()
    }

    pub fn has_minimum_stake_now(
        &self,
        current_epoch: u64,
        snapshot_epoch: u64,
    ) -> Result<bool, NCNProgramError> {
        let epoch_diff = current_epoch - snapshot_epoch;
        match epoch_diff {
            0 => Ok(self.has_minimum_stake.into()),
            1 => Ok(self.has_minimum_stake_next_epoch.into()),
            _ => {
                msg!("Operator snapshot is outdated: {}", self.operator());
                Err(NCNProgramError::OperatorSnapshotOutdated)
            }
        }
    }

    pub fn has_minimum_stake_next_epoch(&self) -> bool {
        self.has_minimum_stake_next_epoch.into()
    }

    pub fn stake_weight(&self) -> &StakeWeights {
        &self.stake_weight
    }

    pub fn next_epoch_stake_weight(&self) -> &StakeWeights {
        &self.next_epoch_stake_weight
    }

    pub fn set_has_minimum_stake_this_epoch(&mut self, has_minimum_stake: bool) {
        self.has_minimum_stake = PodBool::from(has_minimum_stake);
    }

    pub fn set_has_minimum_stake_next_epoch(&mut self, has_minimum_stake: bool) {
        self.has_minimum_stake_next_epoch = PodBool::from(has_minimum_stake);
    }

    pub fn set_stake_weight(&mut self, stake_weight_so_far: &StakeWeights) {
        self.stake_weight = *stake_weight_so_far;
    }

    pub fn set_next_epoch_stake_weight(&mut self, next_epoch_stake_weight: &StakeWeights) {
        self.next_epoch_stake_weight = *next_epoch_stake_weight;
    }

    pub fn snapshot_vault_operator_delegation(
        &mut self,
        current_slot: u64,
        stake_weights: &StakeWeights,
        next_epoch_stake_weights: &StakeWeights,
        minimum_stake: &StakeWeights,
    ) -> Result<(), NCNProgramError> {
        if !self.is_active() {
            return Err(NCNProgramError::OperatorSnapshotIsNotActive);
        }

        self.set_stake_weight(stake_weights);
        self.set_next_epoch_stake_weight(next_epoch_stake_weights);

        self.set_has_minimum_stake_this_epoch(
            self.stake_weight().stake_weight() >= minimum_stake.stake_weight(),
        );

        self.set_has_minimum_stake_next_epoch(
            self.next_epoch_stake_weight().stake_weight() >= minimum_stake.stake_weight(),
        );

        self.last_snapshot_slot = PodU64::from(current_slot);
        Ok(())
    }

    pub fn calculate_stake_weights(
        vault_operator_delegation: &VaultOperatorDelegation,
    ) -> Result<(u128, u128), ProgramError> {
        let total_security = vault_operator_delegation
            .delegation_state
            .total_security()?;

        let cooling_down_amount = vault_operator_delegation
            .delegation_state
            .cooling_down_amount();

        let precise_total_security = PreciseNumber::new(total_security as u128)
            .ok_or(NCNProgramError::NewPreciseNumberError)?;
        let precies_cooling_down_amount = PreciseNumber::new(cooling_down_amount as u128)
            .ok_or(NCNProgramError::NewPreciseNumberError)?;
        let precise_next_epoch_securites = precise_total_security
            .checked_sub(&precies_cooling_down_amount)
            .ok_or(NCNProgramError::ArithmeticUnderflowError)?;

        let total_stake_weight = precise_total_security
            .to_imprecise()
            .ok_or(NCNProgramError::CastToImpreciseNumberError)?;
        let next_epoch_stake_weight = precise_next_epoch_securites
            .to_imprecise()
            .ok_or(NCNProgramError::CastToImpreciseNumberError)?;

        Ok((total_stake_weight, next_epoch_stake_weight))
    }

    pub fn update_g1_pubkey(&mut self, g1_pubkey: &[u8; G1_COMPRESSED_POINT_SIZE]) {
        self.g1_pubkey = *g1_pubkey;
    }
}

#[derive(Debug, Clone, Copy, Zeroable, Pod, ShankType)]
#[repr(C)]
pub struct VaultOperatorStakeWeight {
    vault: Pubkey,
    vault_index: PodU64,
    stake_weight: StakeWeights,
}

impl Default for VaultOperatorStakeWeight {
    fn default() -> Self {
        Self {
            vault: Pubkey::default(),
            vault_index: PodU64::from(u64::MAX),
            stake_weight: StakeWeights::default(),
        }
    }
}

impl VaultOperatorStakeWeight {
    pub fn new(vault: &Pubkey, vault_index: u64, stake_weight: &StakeWeights) -> Self {
        Self {
            vault: *vault,
            vault_index: PodU64::from(vault_index),
            stake_weight: *stake_weight,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.vault_index() == u64::MAX
    }

    pub fn vault_index(&self) -> u64 {
        self.vault_index.into()
    }

    pub const fn stake_weights(&self) -> &StakeWeights {
        &self.stake_weight
    }

    pub const fn vault(&self) -> &Pubkey {
        &self.vault
    }
}

#[rustfmt::skip]
impl fmt::Display for Snapshot {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
       writeln!(f, "\n\n----------- Snapshot -------------")?;
       writeln!(f, "  NCN:                          {}", self.ncn)?;
       writeln!(f, "  Bump:                         {}", self.bump)?;
       writeln!(f, "  Operators Registered:         {}", self.operators_registered())?;
       writeln!(f, "  Operators can vote:           {}", self.operators_can_vote_count())?;
       writeln!(f, "  Last Snapshot Slot:           {}", self.last_snapshot_slot())?;
       writeln!(f, "  Total Agg G1 Pubkey:          {:?}", self.total_aggregated_g1_pubkey())?;
       writeln!(f, "  Minimum Stake Weight:         {:?}", self.minimum_stake())?;
       writeln!(f, "  operators snapshots:")?;
       for operator_snapshot in self.operator_snapshots.iter() {
        if operator_snapshot.ncn_operator_index() != u64::MAX {
           writeln!(f, "{}", operator_snapshot)?;
        }
       }

       writeln!(f, "\n")?;
       Ok(())
   }
}

#[rustfmt::skip]
impl fmt::Display for OperatorSnapshot {
   fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
       writeln!(f, "\n\n----------- Operator Snapshot -------------")?;
       writeln!(f, "  Operator:                     {}", self.operator)?;
       writeln!(f, "  Is Active:                    {}", self.is_active())?;
       writeln!(f, "  NCN Operator Index:           {}", self.ncn_operator_index())?;
       writeln!(f, "  Slot Last Snapshoted:         {}", self.last_snapshot_slot())?;
       writeln!(f, "  G1 Pubkey:                    {:?}", self.g1_pubkey())?;
       writeln!(f, "  Has Minimum Stake Weight:     {}", self.has_minimum_stake())?;
       writeln!(f, "  Has Minimum next epoch:       {}", self.has_minimum_stake_next_epoch())?;
       writeln!(f, "  Stake Weight:                 {:?}", self.stake_weight())?;
       writeln!(f, "  Next Epoch Stake Weight:      {:?}", self.next_epoch_stake_weight())?;

       writeln!(f, "\n")?;
       Ok(())
   }
}

#[cfg(test)]
mod tests {
    use solana_program::msg;

    use super::*;

    #[test]
    fn test_operator_snapshot_size() {
        use std::mem::size_of;

        let expected_total = size_of::<Pubkey>() // operator
            + size_of::<[u8; G1_COMPRESSED_POINT_SIZE]>() // g1_pubkey
            + size_of::<PodU64>() // slot_created
            + size_of::<PodU64>() // slot_last_snapshoted
            + size_of::<PodBool>() // is_active
            + size_of::<PodU64>() // ncn_operator_index
            + size_of::<PodU64>() // operator_index
            + size_of::<PodBool>() // has_minimum_stake
            + size_of::<PodBool>() // has_minimum_stake_next_epoch
            + size_of::<StakeWeights>() // stake_weight
            + size_of::<StakeWeights>(); // next_epoch_stake_weight

        assert_eq!(size_of::<OperatorSnapshot>(), expected_total);
    }

    #[test]
    fn test_snapshot_size() {
        use std::mem::size_of;

        msg!("Snapshot size: {:?}", size_of::<Snapshot>());

        let expected_total = size_of::<Pubkey>() // ncn
            + size_of::<u8>() // bump
            + size_of::<PodU64>() // slot_created
            + size_of::<PodU64>() // last_snapshot_slot
            + size_of::<PodU64>() // operators_registered
            + size_of::<PodU64>() // operators_can_vote_count
            + size_of::<[u8; G1_COMPRESSED_POINT_SIZE]>() // total_aggregated_g1_pubkey
            + size_of::<[OperatorSnapshot; 256]>() // operator_snapshots
            + size_of::<StakeWeights>(); // minimum_stake

        assert_eq!(size_of::<Snapshot>(), expected_total);
    }

    #[test]
    fn test_vault_operator_stake_weight_is_empty() {
        // Test default (should be empty)
        let default_weight = VaultOperatorStakeWeight::default();
        assert!(default_weight.is_empty());

        // Test non-empty case
        let non_empty_weight =
            VaultOperatorStakeWeight::new(&Pubkey::new_unique(), 1, &StakeWeights::default());
        assert!(!non_empty_weight.is_empty());
    }

    #[test]
    fn test_operator_snapshot_initialize_active_inactive() {
        let current_slot = 100;
        let g1_pubkey = G1CompressedPoint::from_random().0;

        // Create two operator snapshots - one for active and one for inactive
        let mut active_snapshot =
            OperatorSnapshot::new(&Pubkey::new_unique(), current_slot, true, 0, 0, g1_pubkey)
                .unwrap();

        let mut inactive_snapshot =
            OperatorSnapshot::new(&Pubkey::new_unique(), current_slot, false, 0, 0, g1_pubkey)
                .unwrap();

        // Initialize active snapshot
        active_snapshot
            .initialize(
                &Pubkey::new_unique(),
                current_slot,
                true, // is_active
                0,
                0,
                g1_pubkey,
                1,
            )
            .unwrap();

        // Initialize inactive snapshot
        inactive_snapshot
            .initialize(
                &Pubkey::new_unique(),
                current_slot,
                false, // not active
                0,
                0,
                g1_pubkey,
                1,
            )
            .unwrap();
    }

    #[test]
    fn test_add_g1_pubkey_to_total_aggregated_and_subtract() {
        // Create a snapshot
        let mut snapshot = Snapshot::new(
            &Pubkey::new_unique(),
            1,                    // bump
            100,                  // current_slot
            StakeWeights::new(1), // minimum_stake
        );
        // Initial aggregated is zero
        assert_eq!(
            snapshot.total_aggregated_g1_pubkey(),
            [0u8; G1_COMPRESSED_POINT_SIZE]
        );

        // Add first pubkey
        let pk1 = G1CompressedPoint::from_random().0;
        snapshot.add_g1_pubkey_to_total_agg(&pk1).unwrap();
        assert_eq!(snapshot.total_aggregated_g1_pubkey(), pk1);

        // Add second pubkey
        let pk2 = G1CompressedPoint::from_random().0;
        snapshot.add_g1_pubkey_to_total_agg(&pk2).unwrap();
        let after_add = snapshot.total_aggregated_g1_pubkey();
        assert_ne!(after_add, pk1);
        assert_ne!(after_add, pk2);

        // Subtract second pubkey
        snapshot.subtract_g1_pubkey_from_total_agg(&pk2).unwrap();
        let after_subtract = snapshot.total_aggregated_g1_pubkey();
        assert_eq!(after_subtract, pk1);

        // Subtract first pubkey, should give zero point (identity)
        snapshot.subtract_g1_pubkey_from_total_agg(&pk1).unwrap();
        let after_zero = snapshot.total_aggregated_g1_pubkey();
        assert_eq!(after_zero, G1CompressedPoint::default().0);
    }

    #[test]
    fn test_register_operator_g1_pubkey() {
        // Create a snapshot with default aggregated G1 pubkey (all zeros) using heap allocation
        let mut snapshot = Box::new(Snapshot::new(
            &Pubkey::new_unique(),
            1,                    // bump
            100,                  // current_slot
            StakeWeights::new(1), // minimum_stake
        ));

        // Verify initial state - total_aggregated_g1_pubkey should be all zeros
        assert_eq!(
            snapshot.total_aggregated_g1_pubkey(),
            [0u8; G1_COMPRESSED_POINT_SIZE]
        );

        // Generate a random G1 pubkey
        let operator_g1_pubkey = G1CompressedPoint::from_random().0;

        // Register the operator's G1 pubkey
        let result = snapshot.register_operator_g1_pubkey(&operator_g1_pubkey);
        assert!(result.is_ok());

        // Verify the aggregated G1 pubkey is no longer all zeros
        assert_ne!(
            snapshot.total_aggregated_g1_pubkey(),
            [0u8; G1_COMPRESSED_POINT_SIZE]
        );

        // The aggregated pubkey should now equal the first operator's pubkey
        // since we started with zero point (identity element for addition)
        assert_eq!(snapshot.total_aggregated_g1_pubkey(), operator_g1_pubkey);
    }

    #[test]
    fn test_register_multiple_operator_g1_pubkeys() {
        // Create a snapshot using heap allocation
        let mut snapshot = Box::new(Snapshot::new(
            &Pubkey::new_unique(),
            1,                    // bump
            100,                  // current_slot
            StakeWeights::new(1), // minimum_stake
        ));

        // Generate multiple random G1 pubkeys
        let operator1_g1_pubkey = G1CompressedPoint::from_random().0;
        let operator2_g1_pubkey = G1CompressedPoint::from_random().0;
        let operator3_g1_pubkey = G1CompressedPoint::from_random().0;

        // Register first operator's G1 pubkey
        snapshot
            .register_operator_g1_pubkey(&operator1_g1_pubkey)
            .unwrap();
        let after_first = snapshot.total_aggregated_g1_pubkey();

        // Register second operator's G1 pubkey
        snapshot
            .register_operator_g1_pubkey(&operator2_g1_pubkey)
            .unwrap();
        let after_second = snapshot.total_aggregated_g1_pubkey();

        // Register third operator's G1 pubkey
        snapshot
            .register_operator_g1_pubkey(&operator3_g1_pubkey)
            .unwrap();
        let after_third = snapshot.total_aggregated_g1_pubkey();

        // Verify that each registration changes the aggregated pubkey
        assert_ne!(after_first, [0u8; G1_COMPRESSED_POINT_SIZE]);
        assert_ne!(after_second, after_first);
        assert_ne!(after_third, after_second);

        // Manually compute the expected aggregated result to verify correctness
        let point1 = G1Point::try_from(&G1CompressedPoint(operator1_g1_pubkey)).unwrap();
        let point2 = G1Point::try_from(&G1CompressedPoint(operator2_g1_pubkey)).unwrap();
        let point3 = G1Point::try_from(&G1CompressedPoint(operator3_g1_pubkey)).unwrap();

        let expected_aggregate = point1 + point2 + point3;
        let expected_compressed = G1CompressedPoint::try_from(expected_aggregate).unwrap();

        assert_eq!(snapshot.total_aggregated_g1_pubkey(), expected_compressed.0);
    }

    #[test]
    fn test_operator_snapshot_g1_pubkey_storage() {
        // Generate a random G1 pubkey
        let g1_pubkey = G1CompressedPoint::from_random().0;

        // Create an operator snapshot with the G1 pubkey
        let operator_snapshot = OperatorSnapshot::new(
            &Pubkey::new_unique(),
            100,       // current_slot
            true,      // is_active
            0,         // ncn_operator_index
            0,         // operator_index
            g1_pubkey, // g1_pubkey
        )
        .unwrap();

        // Verify the G1 pubkey is stored correctly
        assert_eq!(operator_snapshot.g1_pubkey, g1_pubkey);
    }

    #[test]
    fn test_operator_snapshot_initialize_g1_pubkey() {
        // Generate two different G1 pubkeys
        let original_g1_pubkey = G1CompressedPoint::from_random().0;
        let new_g1_pubkey = G1CompressedPoint::from_random().0;

        // Create an operator snapshot with the original G1 pubkey
        let mut operator_snapshot = OperatorSnapshot::new(
            &Pubkey::new_unique(),
            100,                // current_slot
            true,               // is_active
            0,                  // ncn_operator_index
            0,                  // operator_index
            original_g1_pubkey, // original g1_pubkey
        )
        .unwrap();

        // Initialize with a new G1 pubkey
        operator_snapshot
            .initialize(
                &Pubkey::new_unique(),
                100,           // current_slot
                true,          // is_active
                0,             // ncn_operator_index
                0,             // operator_index
                new_g1_pubkey, // new g1_pubkey
                1,
            )
            .unwrap();

        // Verify the G1 pubkey was updated during initialization
        assert_eq!(operator_snapshot.g1_pubkey, new_g1_pubkey);
        assert_ne!(operator_snapshot.g1_pubkey, original_g1_pubkey);
    }

    #[test]
    fn test_snapshot_aggregation_order_independence() {
        // Test that G1 pubkey aggregation is order-independent (commutative)
        let operator1_g1_pubkey = G1CompressedPoint::from_random().0;
        let operator2_g1_pubkey = G1CompressedPoint::from_random().0;

        // Create two snapshots using heap allocation to avoid stack overflow
        let mut snapshot1 = Snapshot::new(
            &Pubkey::new_unique(),
            1,                    // bump
            100,                  // current_slot
            StakeWeights::new(1), // minimum_stake
        );

        // Register operators in different orders
        // Snapshot 1: operator1 first, then operator2
        snapshot1
            .register_operator_g1_pubkey(&operator1_g1_pubkey)
            .unwrap();
        snapshot1
            .register_operator_g1_pubkey(&operator2_g1_pubkey)
            .unwrap();

        let mut snapshot2 = Snapshot::new(
            &Pubkey::new_unique(),
            1,                    // bump
            100,                  // current_slot
            StakeWeights::new(1), // minimum_stake
        );
        // Snapshot 2: operator2 first, then operator1
        snapshot2
            .register_operator_g1_pubkey(&operator2_g1_pubkey)
            .unwrap();
        snapshot2
            .register_operator_g1_pubkey(&operator1_g1_pubkey)
            .unwrap();

        // Both should result in the same aggregated G1 pubkey
        assert_eq!(
            snapshot1.total_aggregated_g1_pubkey(),
            snapshot2.total_aggregated_g1_pubkey()
        );
    }

    #[test]
    fn test_snapshot_g1_pubkey_getter() {
        // Create a snapshot using heap allocation
        let mut snapshot = Box::new(Snapshot::new(
            &Pubkey::new_unique(),
            1,                    // bump
            100,                  // current_slot
            StakeWeights::new(1), // minimum_stake
        ));

        // Initially should be all zeros
        assert_eq!(
            snapshot.total_aggregated_g1_pubkey(),
            [0u8; G1_COMPRESSED_POINT_SIZE]
        );

        // Register an operator G1 pubkey
        let g1_pubkey = G1CompressedPoint::from_random().0;
        snapshot.register_operator_g1_pubkey(&g1_pubkey).unwrap();

        // Verify getter returns the updated value
        assert_eq!(snapshot.total_aggregated_g1_pubkey(), g1_pubkey);
    }

    #[test]
    fn test_snapshot_add_operator_snapshot() {
        // Create a snapshot using heap allocation
        let mut snapshot = Box::new(Snapshot::new(
            &Pubkey::new_unique(),
            1,                    // bump
            100,                  // current_slot
            StakeWeights::new(1), // minimum_stake
        ));

        // Create an operator snapshot
        let operator_pubkey = Pubkey::new_unique();
        let g1_pubkey = G1CompressedPoint::from_random().0;
        let operator_snapshot = OperatorSnapshot::new(
            &operator_pubkey,
            100,       // current_slot
            true,      // is_active
            0,         // ncn_operator_index
            0,         // operator_index
            g1_pubkey, // g1_pubkey
        )
        .unwrap();

        // Add the operator snapshot to the snapshot
        let result = snapshot.add_operator_snapshot(operator_snapshot, 100);
        assert!(result.is_ok());

        {
            // Verify the operator snapshot was added using its index
            let retrieved_snapshot = snapshot.get_operator_snapshot(0);
            let test = snapshot.operator_snapshots[0];
            assert!(retrieved_snapshot.is_some());
            assert_eq!(retrieved_snapshot.unwrap().operator(), &operator_pubkey);
        }

        {
            // Verify the operator snapshot was added using its id
            let retrieved_snapshot = snapshot.find_operator_snapshot(&operator_pubkey);
            assert!(retrieved_snapshot.is_some());
            assert_eq!(retrieved_snapshot.unwrap().operator(), &operator_pubkey);
        }
    }

    #[test]
    fn test_snapshot_find_operator_snapshot() {
        // Create a snapshot using heap allocation
        let mut snapshot = Box::new(Snapshot::new(
            &Pubkey::new_unique(),
            1,                    // bump
            100,                  // current_slot
            StakeWeights::new(1), // minimum_stake
        ));

        // Create operator snapshots
        let operator1_pubkey = Pubkey::new_unique();
        let operator2_pubkey = Pubkey::new_unique();
        let g1_pubkey_1 = G1CompressedPoint::from_random().0;
        let g1_pubkey_2 = G1CompressedPoint::from_random().0;

        let operator1_snapshot = OperatorSnapshot::new(
            &operator1_pubkey,
            100,         // current_slot
            true,        // is_active
            0,           // ncn_operator_index (index 0)
            0,           // operator_index
            g1_pubkey_1, // g1_pubkey
        )
        .unwrap();

        let operator2_snapshot = OperatorSnapshot::new(
            &operator2_pubkey,
            100,         // current_slot
            true,        // is_active
            1,           // ncn_operator_index (index 1)
            1,           // operator_index
            g1_pubkey_2, // g1_pubkey
        )
        .unwrap();

        // Add both operator snapshots
        snapshot
            .add_operator_snapshot(operator1_snapshot, 100)
            .unwrap();
        snapshot
            .add_operator_snapshot(operator2_snapshot, 110)
            .unwrap();

        // Find operator snapshots by pubkey
        let found1 = snapshot.find_operator_snapshot(&operator1_pubkey);
        let found2 = snapshot.find_operator_snapshot(&operator2_pubkey);
        let not_found = snapshot.find_operator_snapshot(&Pubkey::new_unique());

        assert!(found1.is_some());
        assert_eq!(found1.unwrap().operator(), &operator1_pubkey);

        assert!(found2.is_some());
        assert_eq!(found2.unwrap().operator(), &operator2_pubkey);

        assert!(not_found.is_none());
    }

    #[test]
    fn test_snapshot_add_operator_snapshot_duplicate() {
        // Create a snapshot using heap allocation
        let mut snapshot = Box::new(Snapshot::new(
            &Pubkey::new_unique(),
            1,                    // bump
            100,                  // current_slot
            StakeWeights::new(1), // minimum_stake
        ));

        // Create two operator snapshots with the same index
        let g1_pubkey = G1CompressedPoint::from_random().0;
        let operator1_snapshot = OperatorSnapshot::new(
            &Pubkey::new_unique(),
            100,       // current_slot
            true,      // is_active
            0,         // ncn_operator_index
            0,         // operator_index
            g1_pubkey, // g1_pubkey
        )
        .unwrap();

        let operator2_snapshot = OperatorSnapshot::new(
            &Pubkey::new_unique(),
            100,       // current_slot
            true,      // is_active
            0,         // ncn_operator_index (same as operator1)
            1,         // operator_index
            g1_pubkey, // g1_pubkey
        )
        .unwrap();

        // Add first operator snapshot - should succeed
        let result1 = snapshot.add_operator_snapshot(operator1_snapshot, 100);
        assert!(result1.is_ok());

        // Try to add second operator snapshot with same index - should fail
        let result2 = snapshot.add_operator_snapshot(operator2_snapshot, 100);
        assert!(result2.is_err());
        assert_eq!(
            result2.unwrap_err(),
            NCNProgramError::DuplicateVaultOperatorDelegation
        );
    }

    #[test]
    fn test_snapshot_get_active_operator_snapshots() {
        // Create a snapshot using heap allocation
        let mut snapshot = Box::new(Snapshot::new(
            &Pubkey::new_unique(),
            1,                    // bump
            100,                  // current_slot
            StakeWeights::new(1), // minimum_stake
        ));

        let g1_pubkey = G1CompressedPoint::from_random().0;

        // Create active operator snapshot
        let active_operator_snapshot = OperatorSnapshot::new(
            &Pubkey::new_unique(),
            100,       // current_slot
            true,      // is_active = true
            0,         // ncn_operator_index
            0,         // operator_index
            g1_pubkey, // g1_pubkey
        )
        .unwrap();

        // Create inactive operator snapshot
        let inactive_operator_snapshot = OperatorSnapshot::new(
            &Pubkey::new_unique(),
            100,       // current_slot
            false,     // is_active = false
            1,         // ncn_operator_index
            1,         // operator_index
            g1_pubkey, // g1_pubkey
        )
        .unwrap();

        // Add both operator snapshots
        snapshot
            .add_operator_snapshot(active_operator_snapshot, 100)
            .unwrap();
        snapshot
            .add_operator_snapshot(inactive_operator_snapshot, 100)
            .unwrap();

        // Get active operator snapshots
        let active_snapshots = snapshot.get_active_operator_snapshots();

        // Should only return the active one
        assert_eq!(active_snapshots.len(), 1);
        assert!(active_snapshots[0].is_active());
        assert_eq!(active_snapshots[0].ncn_operator_index(), 0);
    }

    #[test]
    fn test_snapshot_initialize() {
        let ncn = Pubkey::new_unique();
        let mut snapshot = Snapshot::new(
            &Pubkey::new_unique(),
            1,                    // bump
            100,                  // current_slot
            StakeWeights::new(1), // minimum_stake
        );

        snapshot.initialize(
            &ncn,
            2,                      // bump
            200,                    // current_slot
            StakeWeights::new(100), // minimum_stake
        );

        assert_eq!(snapshot.ncn, ncn);
        assert_eq!(snapshot.bump, 2);
        assert_eq!(u64::from(snapshot.slot_created), 200u64);
        assert_eq!(snapshot.minimum_stake().stake_weight(), 100);
    }

    #[test]
    fn test_snapshot_seeds_and_pda() {
        let ncn = Pubkey::new_unique();
        let program_id = Pubkey::new_unique();

        let seeds = Snapshot::seeds(&ncn);
        assert_eq!(seeds.len(), 2);
        assert_eq!(seeds[0], b"snapshot");
        assert_eq!(seeds[1], ncn.to_bytes().to_vec());

        let (pda, bump, returned_seeds) = Snapshot::find_program_address(&program_id, &ncn);
        assert_eq!(returned_seeds, seeds);
        assert!(bump > 0);
        assert!(pda != Pubkey::default());
    }

    #[test]
    fn test_snapshot_getters() {
        let ncn = Pubkey::new_unique();
        let snapshot = Snapshot::new(
            &ncn,
            3,                      // bump
            500,                    // current_slot
            StakeWeights::new(200), // minimum_stake
        );

        assert_eq!(snapshot.operators_registered(), 0);
        assert_eq!(snapshot.operators_can_vote_count(), 0);
        assert_eq!(snapshot.last_snapshot_slot(), 0);
        assert_eq!(snapshot.minimum_stake().stake_weight(), 200);
    }

    #[test]
    fn test_snapshot_increment_operator_registration() {
        let mut snapshot = Snapshot::new(
            &Pubkey::new_unique(),
            1,                    // bump
            100,                  // current_slot
            StakeWeights::new(1), // minimum_stake
        );

        // First increment - should succeed
        let result = snapshot.increment_operator_registration(150);
        assert!(result.is_ok());
        assert_eq!(snapshot.operators_registered(), 1);
        assert_eq!(snapshot.operators_can_vote_count(), 0); // operators_can_vote_count is not updated by this method

        // Second increment - should finalize
        let result = snapshot.increment_operator_registration(200);
        assert!(result.is_ok());
        assert_eq!(snapshot.operators_registered(), 2);
        assert_eq!(snapshot.operators_can_vote_count(), 0); // Still 0 - not updated by registration
        assert_eq!(snapshot.last_snapshot_slot(), 200);
    }

    #[test]
    fn test_snapshot_get_operator_snapshot() {
        let mut snapshot = Snapshot::new(
            &Pubkey::new_unique(),
            1,                    // bump
            100,                  // current_slot
            StakeWeights::new(1), // minimum_stake
        );

        // Test getting non-existent snapshot
        assert!(snapshot.get_operator_snapshot(0).is_none());
        assert!(snapshot.get_operator_snapshot(1).is_none());

        // Add an operator snapshot
        let operator_pubkey = Pubkey::new_unique();
        let g1_pubkey = G1CompressedPoint::from_random().0;
        let operator_snapshot = OperatorSnapshot::new(
            &operator_pubkey,
            100,       // current_slot
            true,      // is_active
            0,         // ncn_operator_index
            0,         // operator_index
            g1_pubkey, // g1_pubkey
        )
        .unwrap();

        snapshot
            .add_operator_snapshot(operator_snapshot, 100)
            .unwrap();

        // Test getting existing snapshot
        let retrieved = snapshot.get_operator_snapshot(0);
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().operator(), &operator_pubkey);

        // Test getting out of bounds
        assert!(snapshot.get_operator_snapshot(2).is_none());
    }

    #[test]
    fn test_snapshot_get_mut_operator_snapshot() {
        let mut snapshot = Snapshot::new(
            &Pubkey::new_unique(),
            1,                    // bump
            100,                  // current_slot
            StakeWeights::new(1), // minimum_stake
        );

        // Add an operator snapshot
        let operator_pubkey = Pubkey::new_unique();
        let g1_pubkey = G1CompressedPoint::from_random().0;
        let operator_snapshot = OperatorSnapshot::new(
            &operator_pubkey,
            100,       // current_slot
            true,      // is_active
            0,         // ncn_operator_index
            0,         // operator_index
            g1_pubkey, // g1_pubkey
        )
        .unwrap();

        snapshot
            .add_operator_snapshot(operator_snapshot, 100)
            .unwrap();

        // Test getting mutable reference
        let mut_snapshot = snapshot.get_mut_operator_snapshot(0);
        assert!(mut_snapshot.is_some());

        // Modify the snapshot
        if let Some(snapshot) = mut_snapshot {
            snapshot.set_has_minimum_stake_this_epoch(true);
            assert!(snapshot.has_minimum_stake());
        }

        // Verify the change persisted
        let retrieved = snapshot.get_operator_snapshot(0);
        assert!(retrieved.unwrap().has_minimum_stake());
    }

    #[test]
    fn test_snapshot_find_mut_operator_snapshot() {
        let mut snapshot = Snapshot::new(
            &Pubkey::new_unique(),
            1,                    // bump
            100,                  // current_slot
            StakeWeights::new(1), // minimum_stake
        );

        let operator_pubkey = Pubkey::new_unique();
        let g1_pubkey = G1CompressedPoint::from_random().0;
        let operator_snapshot = OperatorSnapshot::new(
            &operator_pubkey,
            100,       // current_slot
            true,      // is_active
            0,         // ncn_operator_index
            0,         // operator_index
            g1_pubkey, // g1_pubkey
        )
        .unwrap();

        snapshot
            .add_operator_snapshot(operator_snapshot, 100)
            .unwrap();

        // Test finding by pubkey
        let mut_found = snapshot.find_mut_operator_snapshot(&operator_pubkey);
        assert!(mut_found.is_some());

        // Test finding non-existent
        let not_found = snapshot.find_mut_operator_snapshot(&Pubkey::new_unique());
        assert!(not_found.is_none());
    }

    #[test]
    fn test_snapshot_add_operator_snapshot_index_overflow() {
        let mut snapshot = Snapshot::new(
            &Pubkey::new_unique(),
            1,                    // bump
            100,                  // current_slot
            StakeWeights::new(1), // minimum_stake
        );

        let g1_pubkey = G1CompressedPoint::from_random().0;
        let operator_snapshot = OperatorSnapshot::new(
            &Pubkey::new_unique(),
            100,                  // current_slot
            true,                 // is_active
            MAX_OPERATORS as u64, // ncn_operator_index (too large)
            0,                    // operator_index
            g1_pubkey,            // g1_pubkey
        )
        .unwrap();

        let result = snapshot.add_operator_snapshot(operator_snapshot, 100);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err(),
            NCNProgramError::TooManyVaultOperatorDelegations
        );
    }

    #[test]
    fn test_operator_snapshot_new() {
        let operator = Pubkey::new_unique();
        let g1_pubkey = G1CompressedPoint::from_random().0;

        let snapshot = OperatorSnapshot::new(
            &operator, 100,       // current_slot
            true,      // is_active
            5,         // ncn_operator_index
            10,        // operator_index
            g1_pubkey, // g1_pubkey
        )
        .unwrap();

        assert_eq!(snapshot.operator(), &operator);
        assert_eq!(u64::from(snapshot.slot_created), 100u64);
        assert_eq!(u64::from(snapshot.last_snapshot_slot), 0u64);
        assert!(snapshot.is_active());
        assert_eq!(snapshot.ncn_operator_index(), 5);
        assert_eq!(u64::from(snapshot.operator_index), 10u64);
        assert_eq!(snapshot.g1_pubkey(), g1_pubkey);
        assert!(!snapshot.has_minimum_stake());
        assert_eq!(snapshot.stake_weight().stake_weight(), 0);
    }

    #[test]
    fn test_operator_snapshot_initialize_validation() {
        let mut snapshot = OperatorSnapshot::default();

        // Test with too many vault operator delegations
        let result = snapshot.initialize(
            &Pubkey::new_unique(),
            100,                                // current_slot
            true,                               // is_active
            0,                                  // ncn_operator_index
            0,                                  // operator_index
            G1CompressedPoint::from_random().0, // g1_pubkey
            MAX_VAULTS as u64 + 1,              // too many delegations
        );

        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err(),
            NCNProgramError::TooManyVaultOperatorDelegations
        );
    }

    #[test]
    fn test_operator_snapshot_initialize_inactive() {
        let mut snapshot = OperatorSnapshot::default();

        let operator = Pubkey::new_unique();
        let g1_pubkey = G1CompressedPoint::from_random().0;

        // Use a smaller vault_operator_delegation_count that fits with MAX_VAULTS=1
        snapshot
            .initialize(
                &operator, 100,       // current_slot
                false,     // is_active = false
                0,         // ncn_operator_index
                0,         // operator_index
                g1_pubkey, // g1_pubkey
                1,         // vault_operator_delegation_count (reduced from 5 to 1)
            )
            .unwrap();

        assert_eq!(snapshot.operator(), &operator);
        assert!(!snapshot.is_active());
        assert_eq!(u64::from(snapshot.last_snapshot_slot), 0u64); // Should be finalized immediately
    }

    #[test]
    fn test_operator_snapshot_have_valid_bn128_g1_pubkey() {
        let mut snapshot = OperatorSnapshot::default();

        // Test with invalid pubkey (all zeros)
        assert!(!snapshot.have_valid_bn128_g1_pubkey());

        // Test with valid pubkey
        let valid_g1_pubkey = G1CompressedPoint::from_random().0;
        snapshot.g1_pubkey = valid_g1_pubkey;
        assert!(snapshot.have_valid_bn128_g1_pubkey());
    }

    #[test]
    fn test_operator_snapshot_stake_weight_operations() {
        let mut snapshot = OperatorSnapshot::default();

        // Test initial state
        assert_eq!(snapshot.stake_weight().stake_weight(), 0);
        assert!(!snapshot.has_minimum_stake());

        // Test setting stake weight
        let stake_weight = StakeWeights::new(100);
        snapshot.set_stake_weight(&stake_weight);
        assert_eq!(snapshot.stake_weight().stake_weight(), 100);

        // Test incrementing stake weight
        let increment = StakeWeights::new(50);
        let _ = snapshot.stake_weight.increment(&increment);
        assert_eq!(snapshot.stake_weight().stake_weight(), 150);

        // Test setting minimum stake weight flag
        snapshot.set_has_minimum_stake_this_epoch(true);
        assert!(snapshot.has_minimum_stake());
    }

    #[test]
    fn test_vault_operator_stake_weight_new() {
        let vault = Pubkey::new_unique();
        let stake_weight = StakeWeights::new(500);

        let vault_stake_weight = VaultOperatorStakeWeight::new(&vault, 10, &stake_weight);

        assert_eq!(vault_stake_weight.vault(), &vault);
        assert_eq!(vault_stake_weight.vault_index(), 10);
        assert_eq!(vault_stake_weight.stake_weights().stake_weight(), 500);
        assert!(!vault_stake_weight.is_empty());
    }

    #[test]
    fn test_vault_operator_stake_weight_default() {
        let default_weight = VaultOperatorStakeWeight::default();

        assert_eq!(default_weight.vault(), &Pubkey::default());
        assert_eq!(default_weight.vault_index(), u64::MAX);
        assert_eq!(default_weight.stake_weights().stake_weight(), 0);
        assert!(default_weight.is_empty());
    }

    #[test]
    fn test_snapshot_register_operator_g1_pubkey_invalid_pubkey() {
        let mut snapshot = Snapshot::new(
            &Pubkey::new_unique(),
            1,                    // bump
            100,                  // current_slot
            StakeWeights::new(1), // minimum_stake
        );

        // Test with invalid G1 pubkey (all zeros except one byte)
        let mut invalid_pubkey = [0u8; G1_COMPRESSED_POINT_SIZE];
        invalid_pubkey[0] = 1; // Make it invalid G1 point

        let result = snapshot.register_operator_g1_pubkey(&invalid_pubkey);
        assert!(result.is_err()); // Should fail with invalid G1 point
    }

    #[test]
    fn test_snapshot_edge_cases() {
        // Test with maximum values
        let snapshot = Snapshot::new(
            &Pubkey::new_unique(),
            255,                          // bump
            u64::MAX,                     // current_slot
            StakeWeights::new(u128::MAX), // minimum_stake
        );

        assert_eq!(snapshot.minimum_stake().stake_weight(), u128::MAX);
    }

    #[test]
    fn test_operator_snapshot_edge_cases() {
        let operator = Pubkey::new_unique();
        let g1_pubkey = G1CompressedPoint::from_random().0;

        let snapshot = OperatorSnapshot::new(
            &operator,
            u64::MAX,                 // current_slot
            true,                     // is_active
            MAX_OPERATORS as u64 - 1, // ncn_operator_index
            u64::MAX,                 // operator_index
            g1_pubkey,                // g1_pubkey
        )
        .unwrap();

        assert_eq!(u64::from(snapshot.slot_created), u64::MAX);
        assert_eq!(snapshot.ncn_operator_index(), MAX_OPERATORS as u64 - 1);
        assert_eq!(u64::from(snapshot.operator_index), u64::MAX);
    }

    #[test]
    fn test_snapshot_arithmetic_overflow_protection() {
        let mut snapshot = Snapshot::new(
            &Pubkey::new_unique(),
            1,                    // bump
            100,                  // current_slot
            StakeWeights::new(1), // minimum_stake
        );

        // Set to maximum values to test overflow
        snapshot.operators_registered = PodU64::from(u64::MAX);

        let result = snapshot.increment_operator_registration(200);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), NCNProgramError::ArithmeticOverflow);
    }

    #[test]
    fn test_snapshot_vault_operator_delegation_single_use() {
        // Create an active operator snapshot
        let operator = Pubkey::new_unique();
        let g1_pubkey = G1CompressedPoint::from_random().0;
        let mut operator_snapshot = OperatorSnapshot::new(
            &operator, 100,       // current_slot
            true,      // is_active
            0,         // ncn_operator_index
            0,         // operator_index
            g1_pubkey, // g1_pubkey
        )
        .unwrap();

        // Verify initial state
        assert_eq!(operator_snapshot.stake_weight().stake_weight(), 0);
        assert_eq!(
            operator_snapshot.next_epoch_stake_weight().stake_weight(),
            0
        );
        assert!(!operator_snapshot.has_minimum_stake());
        assert!(!operator_snapshot.has_minimum_stake_next_epoch());
        assert_eq!(operator_snapshot.last_snapshot_slot(), 0);

        // Create stake weights for testing
        let current_stake_weights = StakeWeights::new(1000);
        let next_epoch_stake_weights = StakeWeights::new(1200);
        let minimum_stake = StakeWeights::new(500);

        // Call snapshot_vault_operator_delegation once
        let current_slot = 150;
        let result = operator_snapshot.snapshot_vault_operator_delegation(
            current_slot,
            &current_stake_weights,
            &next_epoch_stake_weights,
            &minimum_stake,
        );

        // Verify the call succeeded
        assert!(result.is_ok());

        // Verify the state was updated correctly
        assert_eq!(operator_snapshot.stake_weight().stake_weight(), 1000);
        assert_eq!(
            operator_snapshot.next_epoch_stake_weight().stake_weight(),
            1200
        );
        assert!(operator_snapshot.has_minimum_stake()); // 1000 >= 500
        assert!(operator_snapshot.has_minimum_stake_next_epoch()); // 1200 >= 500
        assert_eq!(operator_snapshot.last_snapshot_slot(), current_slot);
    }

    #[test]
    fn test_snapshot_vault_operator_delegation_multiple_updates() {
        // Create an active operator snapshot
        let operator = Pubkey::new_unique();
        let g1_pubkey = G1CompressedPoint::from_random().0;
        let mut operator_snapshot = OperatorSnapshot::new(
            &operator, 100,       // current_slot
            true,      // is_active
            0,         // ncn_operator_index
            0,         // operator_index
            g1_pubkey, // g1_pubkey
        )
        .unwrap();

        let minimum_stake = StakeWeights::new(500);

        // First snapshot - initial stake weights
        let current_stake_weights_1 = StakeWeights::new(1000);
        let next_epoch_stake_weights_1 = StakeWeights::new(1200);
        let current_slot_1 = 150;

        let result = operator_snapshot.snapshot_vault_operator_delegation(
            current_slot_1,
            &current_stake_weights_1,
            &next_epoch_stake_weights_1,
            &minimum_stake,
        );
        assert!(result.is_ok());

        // Verify first snapshot state
        assert_eq!(operator_snapshot.stake_weight().stake_weight(), 1000);
        assert_eq!(
            operator_snapshot.next_epoch_stake_weight().stake_weight(),
            1200
        );
        assert!(operator_snapshot.has_minimum_stake());
        assert!(operator_snapshot.has_minimum_stake_next_epoch());
        assert_eq!(operator_snapshot.last_snapshot_slot(), current_slot_1);

        // Second snapshot - updated stake weights (increased)
        let current_stake_weights_2 = StakeWeights::new(1500);
        let next_epoch_stake_weights_2 = StakeWeights::new(1800);
        let current_slot_2 = 200;

        let result = operator_snapshot.snapshot_vault_operator_delegation(
            current_slot_2,
            &current_stake_weights_2,
            &next_epoch_stake_weights_2,
            &minimum_stake,
        );
        assert!(result.is_ok());

        // Verify second snapshot state (should be updated)
        assert_eq!(operator_snapshot.stake_weight().stake_weight(), 1500);
        assert_eq!(
            operator_snapshot.next_epoch_stake_weight().stake_weight(),
            1800
        );
        assert!(operator_snapshot.has_minimum_stake()); // 1500 >= 500
        assert!(operator_snapshot.has_minimum_stake_next_epoch()); // 1800 >= 500
        assert_eq!(operator_snapshot.last_snapshot_slot(), current_slot_2);

        // Third snapshot - decreased stake weights
        let current_stake_weights_3 = StakeWeights::new(800);
        let next_epoch_stake_weights_3 = StakeWeights::new(900);
        let current_slot_3 = 250;

        let result = operator_snapshot.snapshot_vault_operator_delegation(
            current_slot_3,
            &current_stake_weights_3,
            &next_epoch_stake_weights_3,
            &minimum_stake,
        );
        assert!(result.is_ok());

        // Verify third snapshot state (should be updated again)
        assert_eq!(operator_snapshot.stake_weight().stake_weight(), 800);
        assert_eq!(
            operator_snapshot.next_epoch_stake_weight().stake_weight(),
            900
        );
        assert!(operator_snapshot.has_minimum_stake()); // 800 >= 500
        assert!(operator_snapshot.has_minimum_stake_next_epoch()); // 900 >= 500
        assert_eq!(operator_snapshot.last_snapshot_slot(), current_slot_3);

        // Fourth snapshot - stake weights below minimum
        let current_stake_weights_4 = StakeWeights::new(300);
        let next_epoch_stake_weights_4 = StakeWeights::new(400);
        let current_slot_4 = 300;

        let result = operator_snapshot.snapshot_vault_operator_delegation(
            current_slot_4,
            &current_stake_weights_4,
            &next_epoch_stake_weights_4,
            &minimum_stake,
        );
        assert!(result.is_ok());

        // Verify fourth snapshot state
        assert_eq!(operator_snapshot.stake_weight().stake_weight(), 300);
        assert_eq!(
            operator_snapshot.next_epoch_stake_weight().stake_weight(),
            400
        );
        assert!(!operator_snapshot.has_minimum_stake()); // 300 < 500
        assert!(!operator_snapshot.has_minimum_stake_next_epoch()); // 400 < 500
        assert_eq!(operator_snapshot.last_snapshot_slot(), current_slot_4);
    }

    #[test]
    fn test_snapshot_vault_operator_delegation_inactive_operator() {
        // Create an inactive operator snapshot
        let operator = Pubkey::new_unique();
        let g1_pubkey = G1CompressedPoint::from_random().0;
        let mut operator_snapshot = OperatorSnapshot::new(
            &operator, 100,       // current_slot
            false,     // is_active = false
            0,         // ncn_operator_index
            0,         // operator_index
            g1_pubkey, // g1_pubkey
        )
        .unwrap();

        // Verify operator is inactive
        assert!(!operator_snapshot.is_active());

        // Try to call snapshot_vault_operator_delegation on inactive operator
        let current_stake_weights = StakeWeights::new(1000);
        let next_epoch_stake_weights = StakeWeights::new(1200);
        let minimum_stake = StakeWeights::new(500);

        let result = operator_snapshot.snapshot_vault_operator_delegation(
            150, // current_slot
            &current_stake_weights,
            &next_epoch_stake_weights,
            &minimum_stake,
        );

        // Should fail with OperatorSnapshotIsNotActive error
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err(),
            NCNProgramError::OperatorSnapshotIsNotActive
        );
    }

    #[test]
    fn test_snapshot_vault_operator_delegation_edge_cases() {
        // Create an active operator snapshot
        let operator = Pubkey::new_unique();
        let g1_pubkey = G1CompressedPoint::from_random().0;
        let mut operator_snapshot = OperatorSnapshot::new(
            &operator, 100,       // current_slot
            true,      // is_active
            0,         // ncn_operator_index
            0,         // operator_index
            g1_pubkey, // g1_pubkey
        )
        .unwrap();

        let minimum_stake = StakeWeights::new(1000);

        // Test with zero stake weights
        let zero_stake_weights = StakeWeights::new(0);
        let result = operator_snapshot.snapshot_vault_operator_delegation(
            150,
            &zero_stake_weights,
            &zero_stake_weights,
            &minimum_stake,
        );
        assert!(result.is_ok());
        assert_eq!(operator_snapshot.stake_weight().stake_weight(), 0);
        assert_eq!(
            operator_snapshot.next_epoch_stake_weight().stake_weight(),
            0
        );
        assert!(!operator_snapshot.has_minimum_stake()); // 0 < 1000
        assert!(!operator_snapshot.has_minimum_stake_next_epoch()); // 0 < 1000

        // Test with exactly minimum stake weight
        let exact_minimum = StakeWeights::new(1000);
        let result = operator_snapshot.snapshot_vault_operator_delegation(
            200,
            &exact_minimum,
            &exact_minimum,
            &minimum_stake,
        );
        assert!(result.is_ok());
        assert_eq!(operator_snapshot.stake_weight().stake_weight(), 1000);
        assert_eq!(
            operator_snapshot.next_epoch_stake_weight().stake_weight(),
            1000
        );
        assert!(operator_snapshot.has_minimum_stake()); // 1000 >= 1000
        assert!(operator_snapshot.has_minimum_stake_next_epoch()); // 1000 >= 1000

        // Test with maximum stake weights
        let max_stake_weights = StakeWeights::new(u128::MAX);
        let result = operator_snapshot.snapshot_vault_operator_delegation(
            250,
            &max_stake_weights,
            &max_stake_weights,
            &minimum_stake,
        );
        assert!(result.is_ok());
        assert_eq!(operator_snapshot.stake_weight().stake_weight(), u128::MAX);
        assert_eq!(
            operator_snapshot.next_epoch_stake_weight().stake_weight(),
            u128::MAX
        );
        assert!(operator_snapshot.has_minimum_stake()); // u128::MAX >= 1000
        assert!(operator_snapshot.has_minimum_stake_next_epoch()); // u128::MAX >= 1000
    }

    #[test]
    fn test_snapshot_vault_operator_delegation_slot_tracking() {
        // Create an active operator snapshot
        let operator = Pubkey::new_unique();
        let g1_pubkey = G1CompressedPoint::from_random().0;
        let mut operator_snapshot = OperatorSnapshot::new(
            &operator, 100,       // current_slot
            true,      // is_active
            0,         // ncn_operator_index
            0,         // operator_index
            g1_pubkey, // g1_pubkey
        )
        .unwrap();

        let stake_weights = StakeWeights::new(1000);
        let minimum_stake = StakeWeights::new(500);

        // Initial state
        assert_eq!(operator_snapshot.last_snapshot_slot(), 0);

        // First snapshot
        let result = operator_snapshot.snapshot_vault_operator_delegation(
            150,
            &stake_weights,
            &stake_weights,
            &minimum_stake,
        );
        assert!(result.is_ok());
        assert_eq!(operator_snapshot.last_snapshot_slot(), 150);

        // Second snapshot with later slot
        let result = operator_snapshot.snapshot_vault_operator_delegation(
            200,
            &stake_weights,
            &stake_weights,
            &minimum_stake,
        );
        assert!(result.is_ok());
        assert_eq!(operator_snapshot.last_snapshot_slot(), 200);

        // Third snapshot with earlier slot (should still update)
        let result = operator_snapshot.snapshot_vault_operator_delegation(
            175,
            &stake_weights,
            &stake_weights,
            &minimum_stake,
        );
        assert!(result.is_ok());
        assert_eq!(operator_snapshot.last_snapshot_slot(), 175);
    }
}
