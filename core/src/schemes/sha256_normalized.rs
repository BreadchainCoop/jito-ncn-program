use dashu::integer::UBig;
use solana_bn254::compression::prelude::alt_bn128_g1_decompress;

use crate::{constants::MODULUS, error::NCNProgramError, g1_point::G1Point};

use super::{HashToCurve, MessageDigest};

/// Domain tag mixed into every hash-to-curve preimage (exactly 32 bytes).
/// Changing this changes the signature domain of every certificate and PoP —
/// the router scheme and all operators must agree on it.
pub const HASH_TO_CURVE_DOMAIN: &[u8; 32] = b"JITO-NCN-BN254-G1-SHA256NORM-V01";

/// The last multiple of the modulus before 2^256 used to normalize
/// hash values for our signing scheme.
///
/// 0xf1f5883e65f820d099915c908786b9d3f58714d70a38f4c22ca2bc723a70f263
pub static NORMALIZE_MODULUS: UBig = unsafe {
    UBig::from_static_words(&[
        0x2ca2bc723a70f263,
        0xf58714d70a38f4c2,
        0x99915c908786b9d3,
        0xf1f5883e65f820d0,
    ])
};

pub struct Sha256Normalized;

impl HashToCurve for Sha256Normalized {
    fn try_hash_to_curve(digest: &MessageDigest) -> Result<G1Point, NCNProgramError> {
        (0..=254u8)
            .find_map(|n: u8| {
                // Domain ‖ digest ‖ counter — fixed-length, domain-separated preimage
                let hash = solana_nostd_sha256::hashv(&[HASH_TO_CURVE_DOMAIN, &digest.0, &[n]]);

                // Convert hash to a Ubig for Bigint operations
                let hash_ubig = UBig::from_be_bytes(&hash);

                // Reject-and-retry above the largest multiple of the field
                // modulus below 2^256, so the reduction below is unbiased
                if hash_ubig >= NORMALIZE_MODULUS {
                    return None;
                }

                // UBig rem by the nonzero field-modulus constant cannot fail
                #[allow(clippy::arithmetic_side_effects)]
                let modulus_ubig = hash_ubig % &MODULUS;

                // Fixed 32-byte big-endian x-candidate (UBig::to_be_bytes is
                // minimal-length; a leading-zero x must not shrink the buffer)
                let mut x_bytes = [0u8; 32];
                let be = modulus_ubig.to_be_bytes();
                let offset = 32usize.saturating_sub(be.len());
                x_bytes[offset..].copy_from_slice(&be);

                // Decompress the point
                match alt_bn128_g1_decompress(&x_bytes) {
                    Ok(p) => Some(G1Point(p)),
                    Err(_) => None,
                }
            })
            .ok_or(NCNProgramError::HashToCurveError)
    }
}

#[cfg(all(test, not(target_os = "solana")))]
mod tests {
    use super::*;

    #[test]
    fn hash_to_curve_is_deterministic() {
        let d = MessageDigest([7u8; 32]);
        let a = Sha256Normalized::try_hash_to_curve(&d).unwrap();
        let b = Sha256Normalized::try_hash_to_curve(&d).unwrap();
        assert_eq!(a.0, b.0);
    }

    #[test]
    fn hash_to_curve_separates_digests() {
        let a = Sha256Normalized::try_hash_to_curve(&MessageDigest([1u8; 32])).unwrap();
        let b = Sha256Normalized::try_hash_to_curve(&MessageDigest([2u8; 32])).unwrap();
        assert_ne!(a.0, b.0);
    }
}
