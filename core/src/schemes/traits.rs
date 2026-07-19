use crate::{error::NCNProgramError, g1_point::G1Point};

/// A 32-byte, collision-resistant message digest — the only signable input.
///
/// Hash-to-curve must never see a raw, attacker-shaped message
/// (eigenlayer-middleware issue #172 class): wrapping the input in this type
/// forces every signing and verification path to commit to a fixed-length
/// digest produced by a domain-tagged hash upstream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessageDigest(pub [u8; 32]);

impl From<[u8; 32]> for MessageDigest {
    fn from(bytes: [u8; 32]) -> Self {
        MessageDigest(bytes)
    }
}

impl AsRef<[u8]> for MessageDigest {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

pub trait HashToCurve {
    /// # Try Hash To Curve
    ///
    /// Maps a 32-byte digest to a valid point in G1 for our AltBN128 BLS
    /// scheme. Implementations must be deterministic, domain-separated, and
    /// free of modulo bias (reject-and-retry, never plain reduction).
    fn try_hash_to_curve(digest: &MessageDigest) -> Result<G1Point, NCNProgramError>;
}

// Trait to represent any type that can be used as a BLS signature
pub trait BLSSignature {
    fn to_bytes(&self) -> Result<[u8; 64], NCNProgramError>;
}
