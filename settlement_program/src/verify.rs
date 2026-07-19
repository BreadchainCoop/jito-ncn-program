//! Inline, stateless certificate verification (D-VERIFY option B).
//!
//! This mirrors docs/INTERFACES.md §2 `VerifyCertificate` semantics against
//! the CURRENT (pre-Phase-1) snapshot/config layouts, reusing
//! `ncn-program-core`'s crypto (`verify_aggregated_signature`, point types)
//! so nothing cryptographic is reimplemented.
//!
//! PHASE-1 REBASE PLAN (small diff by construction): once branch `phase1-dmsg`
//! lands `pub fn verify_certificate_readonly(...)` in the NCN processor /
//! core, the body of [`verify_certificate_inline`] collapses to a single call
//! into that shared function, and the two stand-ins below disappear:
//! 1. GENERATION: the current `Snapshot` has no `generation` field; operator
//!    registration is the only generation-bumping mutation that exists today,
//!    so `snapshot.operators_registered()` is used as the generation proxy.
//!    Post-Phase-1: compare against `snapshot.generation()`.
//! 2. THRESHOLD: the current `Config` has no `consensus_threshold_bps`;
//!    `DEFAULT_CONSENSUS_THRESHOLD_BPS` (6667) from `settlement_core` is used.
//!    Post-Phase-1: read the admin-settable config field.

use jito_bytemuck::AccountDeserialize;
use jito_restaking_core::{config::Config as RestakingConfig, ncn::Ncn};
use ncn_program_core::{
    config::Config as NcnConfig,
    error::NCNProgramError,
    g1_point::{G1CompressedPoint, G1Point},
    g2_point::{G2CompressedPoint, G2Point},
    schemes::{MessageDigest, Sha256Normalized},
    snapshot::Snapshot,
    utils::get_epoch,
};
use num::CheckedAdd;
use settlement_core::{
    error::SettlementError,
    instruction::{BPS_DENOMINATOR, DEFAULT_CONSENSUS_THRESHOLD_BPS},
};
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult, msg,
    program_error::ProgramError, sysvar::Sysvar,
};

/// Verifies an aggregated BLS certificate over `digest` against the NCN's
/// snapshot. Read-only: mutates nothing.
///
/// Checks, in order (§2): bitmap length; generation; snapshot slot-freshness;
/// per-signer minimum stake; signed-stake bps threshold; single
/// challenge-combined pairing.
#[allow(clippy::too_many_arguments)]
pub fn verify_certificate_inline(
    ncn_config: &AccountInfo,
    ncn: &AccountInfo,
    snapshot: &AccountInfo,
    restaking_config: &AccountInfo,
    digest: &MessageDigest,
    aggregated_g2: &[u8; 64],
    aggregated_signature: &[u8; 32],
    operators_signature_bitmap: &[u8],
    expected_generation: u64,
) -> ProgramResult {
    let ncn_program_id = ncn_program::id();
    NcnConfig::load(&ncn_program_id, ncn_config, ncn.key, false)?;
    RestakingConfig::load(&jito_restaking_program::id(), restaking_config, false)?;
    Ncn::load(&jito_restaking_program::id(), ncn, false)?;
    Snapshot::load(&ncn_program_id, snapshot, ncn.key, false)?;

    let ncn_epoch_length = {
        let config_data = restaking_config.data.borrow();
        let config = RestakingConfig::try_from_slice_unchecked(&config_data)?;
        config.epoch_length()
    };

    let valid_slots_after_consensus = {
        let config_data = ncn_config.data.borrow();
        let config = NcnConfig::try_from_slice_unchecked(&config_data)?;
        config.valid_slots_after_consensus()
    };

    let current_slot = Clock::get()?.slot;

    let snapshot_data = snapshot.data.borrow();
    let snapshot = Snapshot::try_from_slice_unchecked(&snapshot_data)?;

    let operators_registered = snapshot.operators_registered();

    // 1. Bitmap sized to the registered operator set.
    let required_bitmap_bytes = operators_registered.div_ceil(8);
    if operators_signature_bitmap.len() as u64 != required_bitmap_bytes {
        msg!(
            "Invalid bitmap size: got {} bytes, need {}",
            operators_signature_bitmap.len(),
            required_bitmap_bytes
        );
        return Err(SettlementError::InvalidBitmapLength.into());
    }

    // 2. Generation (pre-Phase-1 proxy: operators_registered, see module doc).
    if expected_generation != operators_registered {
        msg!(
            "Generation mismatch: expected {}, snapshot at {}",
            expected_generation,
            operators_registered
        );
        return Err(SettlementError::GenerationMismatch.into());
    }

    // 3. Snapshot slot-freshness.
    let snapshot_age = current_slot.saturating_sub(snapshot.last_snapshot_slot());
    if snapshot_age > valid_slots_after_consensus {
        msg!(
            "Snapshot is stale: {} slots old, window is {}",
            snapshot_age,
            valid_slots_after_consensus
        );
        return Err(SettlementError::StaleSnapshot.into());
    }

    let current_epoch = get_epoch(current_slot, ncn_epoch_length)?;

    // 4./5. Per-signer minimum stake + stake accounting for the threshold.
    let mut aggregated_nonsigners_pubkey: Option<G1Point> = None;
    let mut signed_stake: u128 = 0;
    let mut total_stake: u128 = 0;
    let mut non_signers_count: u64 = 0;

    for (i, operator_snapshot) in snapshot
        .operator_snapshots()
        .iter()
        .take(operators_registered as usize)
        .enumerate()
    {
        total_stake = total_stake
            .checked_add(operator_snapshot.stake_weight().stake_weight())
            .ok_or(SettlementError::ArithmeticOverflow)?;

        let byte_index = i >> 3;
        let bit_index = i & 7;
        let signed = operators_signature_bitmap
            .get(byte_index)
            .map(|byte| (byte >> bit_index) & 1 == 1)
            .ok_or(SettlementError::InvalidBitmapLength)?;

        if signed {
            let snapshot_epoch =
                get_epoch(operator_snapshot.last_snapshot_slot(), ncn_epoch_length)?;
            let has_minimum_stake =
                operator_snapshot.has_minimum_stake_now(current_epoch, snapshot_epoch)?;
            if !has_minimum_stake {
                msg!(
                    "Signer {} does not meet the minimum stake",
                    operator_snapshot.operator()
                );
                return Err(SettlementError::SignerHasNoMinimumStake.into());
            }
            signed_stake = signed_stake
                .checked_add(operator_snapshot.stake_weight().stake_weight())
                .ok_or(SettlementError::ArithmeticOverflow)?;
        } else {
            let g1_compressed = G1CompressedPoint::from(operator_snapshot.g1_pubkey());
            let g1_point = G1Point::try_from(&g1_compressed)
                .map_err(|_| NCNProgramError::G1PointDecompressionError)?;

            aggregated_nonsigners_pubkey = match aggregated_nonsigners_pubkey {
                None => Some(g1_point),
                Some(current) => Some(
                    current
                        .checked_add(&g1_point)
                        .ok_or(NCNProgramError::AltBN128AddError)?,
                ),
            };

            non_signers_count = non_signers_count
                .checked_add(1)
                .ok_or(ProgramError::ArithmeticOverflow)?;
        }
    }

    if total_stake == 0 {
        msg!("Snapshot has zero total stake");
        return Err(SettlementError::NoStakeRegistered.into());
    }

    // 5. Signed stake must clear the consensus threshold (bps of total).
    // Cross-multiplied to avoid integer division:
    //   signed / total >= threshold / 10_000
    let signed_scaled = signed_stake
        .checked_mul(u128::from(BPS_DENOMINATOR))
        .ok_or(SettlementError::ArithmeticOverflow)?;
    let threshold_scaled = total_stake
        .checked_mul(u128::from(DEFAULT_CONSENSUS_THRESHOLD_BPS))
        .ok_or(SettlementError::ArithmeticOverflow)?;
    if signed_scaled < threshold_scaled {
        msg!(
            "Insufficient signed stake: {} of {} (threshold {} bps)",
            signed_stake,
            total_stake,
            DEFAULT_CONSENSUS_THRESHOLD_BPS
        );
        return Err(SettlementError::InsufficientStakeBps.into());
    }

    // 6. Single challenge-combined pairing over the digest.
    let aggregated_g2_point = G2Point::try_from(G2CompressedPoint::from(*aggregated_g2))
        .map_err(|_| NCNProgramError::G2PointDecompressionError)?;

    let total_agg_g1_compressed = G1CompressedPoint::from(snapshot.total_aggregated_g1_pubkey());
    let total_aggregated_g1 = G1Point::try_from(&total_agg_g1_compressed)
        .map_err(|_| NCNProgramError::G1PointDecompressionError)?;

    let signature = G1Point::try_from(&G1CompressedPoint(*aggregated_signature))
        .map_err(|_| NCNProgramError::G1PointDecompressionError)?;

    let apk1 = if non_signers_count == 0 {
        total_aggregated_g1
    } else {
        let nonsigners =
            aggregated_nonsigners_pubkey.ok_or(NCNProgramError::NoNonSignersAggregatedPubkey)?;
        total_aggregated_g1
            .checked_add(&nonsigners.negate())
            .ok_or(NCNProgramError::AltBN128AddError)?
    };

    aggregated_g2_point
        .verify_aggregated_signature::<Sha256Normalized>(signature, digest, apk1)
        .map_err(|_| SettlementError::SignatureVerificationFailed)?;

    msg!(
        "Certificate verified: {} of {} stake signed, {} non-signers",
        signed_stake,
        total_stake,
        non_signers_count
    );

    Ok(())
}
