use solana_program::program_error::ProgramError;
use solana_program::pubkey::Pubkey;

use crate::constants::{FR_MODULUS, POP_MESSAGE_DOMAIN};
use crate::schemes::MessageDigest;
use crate::{constants::MAX_REALLOC_BYTES, error::NCNProgramError, snapshot::OperatorSnapshot};
use dashu::integer::UBig;

/// Calculate new size for reallocation, capped at target size
/// Returns the minimum of (current_size + MAX_REALLOC_BYTES) and target_size
pub fn get_new_size(current_size: usize, target_size: usize) -> Result<usize, ProgramError> {
    Ok(current_size
        .checked_add(MAX_REALLOC_BYTES as usize)
        .ok_or(ProgramError::ArithmeticOverflow)?
        .min(target_size))
}

#[inline(always)]
#[track_caller]
pub fn assert_ncn_program_error<T>(
    test_error: Result<T, NCNProgramError>,
    ncn_program_error: NCNProgramError,
) {
    assert!(test_error.is_err());
    assert_eq!(test_error.err().unwrap(), ncn_program_error);
}

pub fn get_epoch(slot: u64, ncn_epoch_length: u64) -> Result<u64, NCNProgramError> {
    slot.checked_div(ncn_epoch_length)
        .ok_or(NCNProgramError::DenominatorIsZero)
}

/// Determines if an operator is eligible to vote in the current epoch
///
/// An operator can vote if:
/// 1. They haven't already voted in this epoch
/// 2. They have a non-zero stake weight
///
/// # Arguments
/// * `ballot_box` - The current epoch's ballot box tracking votes
/// * `operator_snapshot` - Snapshot of operator's state for this epoch
/// * `operator` - Public key of the operator to check
///
/// # Returns
/// * `bool` - True if operator can vote, false otherwise
pub fn can_operator_vote(operator_snapshot: OperatorSnapshot) -> bool {
    // Check if operator has already voted in this epoch

    operator_snapshot.is_active() && operator_snapshot.has_minimum_stake()
}

/// Reduces a 32-byte big-endian hash modulo the BN254 group order Fr,
/// returning a 32-byte big-endian scalar.
fn reduce_mod_fr(hash: &[u8; 32]) -> [u8; 32] {
    let reduced = UBig::from_be_bytes(hash) % FR_MODULUS.clone();
    let mut out = [0u8; 32];
    let be = reduced.to_be_bytes();
    out[32 - be.len()..].copy_from_slice(&be);
    out
}

/// EigenLayer-exact certificate challenge (BLSSignatureChecker.sol#L214):
/// gamma = keccak256(msgDigest ‖ apk.X ‖ apk.Y ‖ apkG2.X[0] ‖ apkG2.X[1]
/// ‖ apkG2.Y[0] ‖ apkG2.Y[1] ‖ sigma.X ‖ sigma.Y) mod FR_MODULUS.
/// All coordinates are 32-byte big-endian words in abi.encodePacked order;
/// the G2 byte layout in `apk_g2` already matches BN254.sol (c1 before c0).
pub fn compute_certificate_gamma(
    msg_digest: &[u8; 32],
    apk1: &[u8; 64],
    apk_g2: &[u8; 128],
    sigma: &[u8; 64],
) -> [u8; 32] {
    let hash = solana_program::keccak::hashv(&[msg_digest, apk1, apk_g2, sigma]).0;
    reduce_mod_fr(&hash)
}

/// Challenge scalar for the registration proof-of-possession, mirroring
/// BLSApkRegistry.registerBLSPublicKey's packing order
/// (signature, pubkeyG1, pubkeyG2, messagePoint), keccak256 mod Fr.
/// The preimage length (320 bytes) differs from the certificate gamma's
/// (288 bytes), so the two challenge domains cannot collide.
pub fn compute_pop_gamma(
    signature: &[u8; 64],
    pubkey_g1: &[u8; 64],
    pubkey_g2: &[u8; 128],
    msg_point: &[u8; 64],
) -> [u8; 32] {
    let hash = solana_program::keccak::hashv(&[signature, pubkey_g1, pubkey_g2, msg_point]).0;
    reduce_mod_fr(&hash)
}

/// The proof-of-possession message digest: binds the NCN, the operator, and
/// the G1 key being registered, so a PoP observed on-chain cannot be replayed
/// by a different operator or against a different NCN (EigenLayer binds the
/// operator address into pubkeyRegistrationMessageHash for the same reason).
pub fn pop_message_digest(
    ncn: &Pubkey,
    operator: &Pubkey,
    g1_compressed: &[u8; 32],
) -> MessageDigest {
    let hash = solana_nostd_sha256::hashv(&[
        POP_MESSAGE_DOMAIN,
        ncn.as_ref(),
        operator.as_ref(),
        g1_compressed,
    ]);
    MessageDigest(hash)
}

/// Creates a bitmap representing which operators have signed, given their indices and the total number of operators.
/// Each bit in the bitmap corresponds to an operator: bit set to 1 means the operator at that index has signed.
///
/// # Arguments
/// * `signer_indices` - A slice of indices (usize) indicating which operators have signed.
/// * `total_operators` - The total number of operators (determines the bitmap length).
///
/// # Returns
/// A vector of bytes (`Vec<u8>`) where each bit represents the signing status of an operator.
pub fn create_signer_bitmap(non_signer_indices: &[usize], total_operators: usize) -> Vec<u8> {
    // Calculate the number of bytes needed to represent all operators (1 bit per operator).
    // Add 7 before dividing by 8 to ensure rounding up for any remainder bits.
    let bitmap_size = (total_operators + 7) / 8;
    // Initialize the bitmap with all bits set to 1 (all operators have signed).
    let mut bitmap = vec![255u8; bitmap_size];

    // Iterate over each index in non_signer_indices, setting the corresponding bit in the bitmap.
    for &index in non_signer_indices {
        // Determine which byte in the bitmap this operator's bit falls into.
        let byte_index = index / 8;
        // Determine the bit position within the byte (0 = least significant bit).
        let bit_index = index % 8;
        // Only set the bit if the byte_index is within the bitmap bounds.
        if byte_index < bitmap.len() {
            // Set the bit at bit_index in the byte at byte_index to 0.
            bitmap[byte_index] &= !(1 << bit_index);
        }
    }

    // Return the constructed bitmap.
    bitmap
}

#[cfg(all(test, not(target_os = "solana")))]
mod tests {
    use super::*;

    #[test]
    fn certificate_gamma_is_reduced_mod_fr() {
        // ~81% of random 256-bit hashes exceed Fr, so a small sweep must hit
        // the reduction path; pre-fix (no reduction / mod-Fq) this diverges.
        let mut exercised_reduction = false;
        for seed in 0u8..32 {
            let digest = [seed; 32];
            let apk1 = [seed.wrapping_add(1); 64];
            let apk2 = [seed.wrapping_add(2); 128];
            let sigma = [seed.wrapping_add(3); 64];
            let raw = solana_program::keccak::hashv(&[&digest, &apk1, &apk2, &sigma]).0;
            let gamma = compute_certificate_gamma(&digest, &apk1, &apk2, &sigma);
            assert!(
                UBig::from_be_bytes(&gamma) < FR_MODULUS.clone(),
                "gamma must be a valid Fr scalar"
            );
            if UBig::from_be_bytes(&raw) >= FR_MODULUS.clone() {
                exercised_reduction = true;
                assert_ne!(raw, gamma, "oversized hash must actually be reduced");
            } else {
                assert_eq!(raw, gamma, "in-range hash must pass through unchanged");
            }
        }
        assert!(exercised_reduction, "sweep never exercised the reduction path");
    }

    #[test]
    fn certificate_gamma_binds_every_input() {
        let digest = [1u8; 32];
        let apk1 = [2u8; 64];
        let apk2 = [3u8; 128];
        let sigma = [4u8; 64];
        let base = compute_certificate_gamma(&digest, &apk1, &apk2, &sigma);
        assert_ne!(base, compute_certificate_gamma(&[9u8; 32], &apk1, &apk2, &sigma));
        assert_ne!(base, compute_certificate_gamma(&digest, &[9u8; 64], &apk2, &sigma));
        assert_ne!(base, compute_certificate_gamma(&digest, &apk1, &[9u8; 128], &sigma));
        assert_ne!(base, compute_certificate_gamma(&digest, &apk1, &apk2, &[9u8; 64]));
    }

    #[test]
    fn pop_gamma_binds_every_input() {
        let sig = [1u8; 64];
        let g1 = [2u8; 64];
        let g2 = [3u8; 128];
        let msg = [4u8; 64];
        let base = compute_pop_gamma(&sig, &g1, &g2, &msg);
        assert!(UBig::from_be_bytes(&base) < FR_MODULUS.clone());
        assert_ne!(base, compute_pop_gamma(&[9u8; 64], &g1, &g2, &msg));
        assert_ne!(base, compute_pop_gamma(&sig, &[9u8; 64], &g2, &msg));
        assert_ne!(base, compute_pop_gamma(&sig, &g1, &[9u8; 128], &msg));
        assert_ne!(base, compute_pop_gamma(&sig, &g1, &g2, &[9u8; 64]));
    }

    #[test]
    fn pop_digest_binds_ncn_operator_and_key() {
        let ncn = Pubkey::new_unique();
        let operator = Pubkey::new_unique();
        let g1 = [5u8; 32];
        let base = pop_message_digest(&ncn, &operator, &g1);
        assert_eq!(base, pop_message_digest(&ncn, &operator, &g1));
        assert_ne!(base, pop_message_digest(&Pubkey::new_unique(), &operator, &g1));
        assert_ne!(base, pop_message_digest(&ncn, &Pubkey::new_unique(), &g1));
        assert_ne!(base, pop_message_digest(&ncn, &operator, &[6u8; 32]));
    }
}
