//! Shared, stateless certificate verification core (docs/INTERFACES.md par.2).
//!
//! This is a pure function over account *views* (a deserialized [`Snapshot`]
//! plus the relevant [`Config`](crate::config::Config) fields), so the NCN
//! program's `VerifyCertificate` instruction and the gaskiller-settlement
//! program's inline verify path (D-VERIFY option B) share one implementation.

use num::CheckedAdd;
use solana_program::msg;

use crate::{
    constants::G1_COMPRESSED_POINT_SIZE,
    error::NCNProgramError,
    g1_point::{G1CompressedPoint, G1Point},
    g2_point::{G2CompressedPoint, G2Point},
    schemes::{MessageDigest, Sha256Normalized},
    snapshot::Snapshot,
    utils::get_epoch,
};

const MAX_BPS: u128 = 10_000;

/// Verifies a BLS certificate against a snapshot + config view, mutating
/// nothing. Checks, in order (docs/INTERFACES.md par.2):
///
/// 1. bitmap length vs `operators_registered`
/// 2. snapshot `generation == expected_generation`
/// 3. snapshot slot-freshness (`last_snapshot_slot` within
///    `valid_slots_after_consensus` of `current_slot`)
/// 4. per-signer minimum stake (with the has_minimum_stake_now epoch decay)
/// 5. signed stake (bps of total snapshot stake) `>= consensus_threshold_bps`
/// 6. single challenge-combined pairing via
///    `verify_aggregated_signature::<Sha256Normalized>`
#[allow(clippy::too_many_arguments)]
pub fn verify_certificate(
    snapshot: &Snapshot,
    consensus_threshold_bps: u16,
    valid_slots_after_consensus: u64,
    ncn_epoch_length: u64,
    current_slot: u64,
    digest: &[u8; 32],
    aggregated_g2: &[u8; 64],
    aggregated_signature: &[u8; 32],
    operators_signature_bitmap: &[u8],
    expected_generation: u64,
) -> Result<(), NCNProgramError> {
    let operators_registered = snapshot.operators_registered();

    // 1. Bitmap length must exactly cover the registered operator set
    let required_bitmap_bytes = operators_registered.div_ceil(8);
    if operators_signature_bitmap.len() as u64 != required_bitmap_bytes {
        msg!(
            "Invalid bitmap size: got {} bytes, need {}",
            operators_signature_bitmap.len(),
            required_bitmap_bytes
        );
        return Err(NCNProgramError::InvalidInputLength);
    }

    // 2. The certificate is only valid against the operator-set generation it
    // was assembled for (operator removal / key rotation bumps it)
    if snapshot.generation() != expected_generation {
        msg!(
            "Snapshot generation mismatch: snapshot {}, certificate {}",
            snapshot.generation(),
            expected_generation
        );
        return Err(NCNProgramError::SnapshotGenerationMismatch);
    }

    // 3. Snapshot slot-freshness: the stake view must have been snapshotted
    // within the configured window of the current slot
    let slots_since_snapshot = current_slot
        .checked_sub(snapshot.last_snapshot_slot())
        .ok_or(NCNProgramError::ArithmeticUnderflowError)?;
    if slots_since_snapshot > valid_slots_after_consensus {
        msg!(
            "Snapshot is stale: {} slots since last snapshot, window {}",
            slots_since_snapshot,
            valid_slots_after_consensus
        );
        return Err(NCNProgramError::VotingNotValid);
    }

    let current_epoch = get_epoch(current_slot, ncn_epoch_length)?;

    // 4. + 5. Walk the operator set once: enforce per-signer minimum stake and
    // accumulate signed vs total stake, and aggregate the non-signer G1 keys
    // for subtraction from the running APK.
    let mut aggregated_nonsigners_pubkey: Option<G1Point> = None;
    let mut non_signers_count: u64 = 0;
    let mut signed_stake: u128 = 0;
    let mut total_stake: u128 = 0;

    for (i, operator_snapshot) in snapshot
        .operator_snapshots()
        .iter()
        .enumerate()
        .take(operators_registered as usize)
    {
        let byte_index = i >> 3;
        let bit_index = i & 7;
        let signed = (operators_signature_bitmap[byte_index] >> bit_index) & 1 == 1;

        // Slots that never held an operator carry no key and no stake; a
        // signer bit for one can never be valid.
        if operator_snapshot.ncn_operator_index() == u64::MAX {
            if signed {
                msg!("Signer bit set for an empty operator slot {}", i);
                return Err(NCNProgramError::OperatorIsNotInSnapshot);
            }
            continue;
        }

        let snapshot_epoch = get_epoch(operator_snapshot.last_snapshot_slot(), ncn_epoch_length)?;
        let epoch_diff = current_epoch
            .checked_sub(snapshot_epoch)
            .ok_or(NCNProgramError::ArithmeticUnderflowError)?;

        // The stake view decays across epochs: current epoch uses the
        // snapshotted weight, one epoch later the next-epoch weight, anything
        // older contributes nothing (has_minimum_stake_now semantics).
        let effective_stake = match epoch_diff {
            0 => operator_snapshot.stake_weight().stake_weight(),
            1 => operator_snapshot.next_epoch_stake_weight().stake_weight(),
            _ => 0,
        };

        if signed {
            let has_minimum_stake =
                operator_snapshot.has_minimum_stake_now(current_epoch, snapshot_epoch)?;
            if !has_minimum_stake {
                msg!(
                    "The operator {} does not have enough stake to sign",
                    operator_snapshot.operator()
                );
                return Err(NCNProgramError::OperatorHasNoMinimumStake);
            }

            signed_stake = signed_stake
                .checked_add(effective_stake)
                .ok_or(NCNProgramError::ArithmeticOverflow)?;
        } else {
            // Subtract this non-signer's key from the running APK
            let g1_compressed = G1CompressedPoint::from(operator_snapshot.g1_pubkey());
            let g1_point = G1Point::try_from(&g1_compressed)
                .map_err(|_| NCNProgramError::G1PointDecompressionError)?;

            aggregated_nonsigners_pubkey = Some(match aggregated_nonsigners_pubkey {
                None => g1_point,
                Some(current) => current
                    .checked_add(&g1_point)
                    .ok_or(NCNProgramError::AltBN128AddError)?,
            });

            non_signers_count = non_signers_count
                .checked_add(1)
                .ok_or(NCNProgramError::ArithmeticOverflow)?;
        }

        total_stake = total_stake
            .checked_add(effective_stake)
            .ok_or(NCNProgramError::ArithmeticOverflow)?;
    }

    // 5. Stake-weighted quorum: signed stake must reach
    // consensus_threshold_bps of the total snapshot stake. Compared
    // multiplicatively (signed * 10_000 >= total * threshold) to avoid
    // integer division.
    if total_stake == 0 {
        msg!("Total snapshot stake is zero; no quorum possible");
        return Err(NCNProgramError::InsufficientStakeBps);
    }
    let scaled_signed = signed_stake
        .checked_mul(MAX_BPS)
        .ok_or(NCNProgramError::ArithmeticOverflow)?;
    let scaled_required = total_stake
        .checked_mul(consensus_threshold_bps as u128)
        .ok_or(NCNProgramError::ArithmeticOverflow)?;
    if scaled_signed < scaled_required {
        msg!(
            "Insufficient signed stake: {} of {} total (threshold {} bps)",
            signed_stake,
            total_stake,
            consensus_threshold_bps
        );
        return Err(NCNProgramError::InsufficientStakeBps);
    }

    // 6. Single challenge-combined pairing
    let aggregated_g2_point = G2Point::try_from(G2CompressedPoint::from(*aggregated_g2))
        .map_err(|_| NCNProgramError::G2PointDecompressionError)?;

    let total_agg_g1_compressed = G1CompressedPoint::from(snapshot.total_aggregated_g1_pubkey());
    if total_agg_g1_compressed.0 == [0u8; G1_COMPRESSED_POINT_SIZE] {
        msg!("Snapshot has no aggregated G1 key");
        return Err(NCNProgramError::NoOperatorsRegistered);
    }
    let total_aggregated_g1_pubkey = G1Point::try_from(&total_agg_g1_compressed)
        .map_err(|_| NCNProgramError::G1PointDecompressionError)?;

    let signature = G1Point::try_from(&G1CompressedPoint(*aggregated_signature))
        .map_err(|_| NCNProgramError::G1PointDecompressionError)?;

    let apk1 = match aggregated_nonsigners_pubkey {
        // All operators signed: verify against the full running APK (the
        // zeroed identity placeholder is not a valid curve point to subtract)
        None => total_aggregated_g1_pubkey,
        Some(nonsigners) => total_aggregated_g1_pubkey
            .checked_add(&nonsigners.negate())
            .ok_or(NCNProgramError::AltBN128AddError)?,
    };

    msg!("Verifying certificate ({} non-signers)", non_signers_count);
    aggregated_g2_point
        .verify_aggregated_signature::<Sha256Normalized>(signature, &MessageDigest(*digest), apk1)
        .map_err(|_| NCNProgramError::SignatureVerificationFailed)?;

    Ok(())
}
