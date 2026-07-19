#[derive(Clone, Debug, Copy)]
pub struct G2Point(pub [u8; 128]);
#[derive(Clone, Debug, Copy)]
pub struct G2CompressedPoint(pub [u8; 64]);

#[cfg(not(target_os = "solana"))]
use ark_bn254::Fr;
#[cfg(not(target_os = "solana"))]
use ark_ec::AffineRepr;
#[cfg(not(target_os = "solana"))]
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};
use num::CheckedAdd;

use solana_bn254::{
    compression::prelude::{alt_bn128_g2_compress, alt_bn128_g2_decompress},
    prelude::alt_bn128_pairing,
};
use solana_program::msg;

use crate::{
    constants::{BN128_ADDITION_SUCESS_RESULT, G1_GENERATOR, G2_MINUS_ONE},
    error::NCNProgramError,
    g1_point::{G1CompressedPoint, G1Point},
    privkey::PrivKey,
    schemes::{BLSSignature, HashToCurve, MessageDigest, Sha256Normalized},
    utils::{compute_certificate_gamma, compute_pop_gamma},
};

impl G2Point {
    pub fn verify_signature<H: HashToCurve, S: BLSSignature>(
        self,
        signature: S,
        digest: &MessageDigest,
    ) -> Result<(), NCNProgramError> {
        let mut input = [0u8; 384];

        // 1) Hash message to curve
        input[..64].clone_from_slice(&H::try_hash_to_curve(digest)?.0);
        // 2) Decompress our public key
        input[64..192].clone_from_slice(&self.0);
        // 3) Decompress our signature
        input[192..256].clone_from_slice(&signature.to_bytes()?);
        // 4) Pair with -G2::one()
        input[256..].clone_from_slice(&G2_MINUS_ONE);

        // Calculate result
        if let Ok(r) = alt_bn128_pairing(&input) {
            msg!("Pairing result: {:?}", r);
            if r.eq(&BN128_ADDITION_SUCESS_RESULT) {
                Ok(())
            } else {
                Err(NCNProgramError::BLSVerificationError)
            }
        } else {
            Err(NCNProgramError::AltBN128PairingError)
        }
    }

    /// Verifies the registration proof-of-possession: `signature` must be the
    /// operator's BLS signature over `pop_digest` (which the caller builds via
    /// `utils::pop_message_digest`, binding ncn + operator + g1 key), and the
    /// challenge-combined pairing simultaneously proves the G1/G2 keys match.
    pub fn verify_operator_registeration(
        self,
        signature: G1Point,
        g1_pubkey: [u8; 32],
        pop_digest: &MessageDigest,
    ) -> Result<(), NCNProgramError> {
        let g1_compressed = G1CompressedPoint::from(g1_pubkey);
        let g1_pubkey_point = G1Point::try_from(&g1_compressed)
            .map_err(|_| NCNProgramError::G1PointDecompressionError)?;

        let message_hash = Sha256Normalized::try_hash_to_curve(pop_digest)?.0;
        let alpha = compute_pop_gamma(&signature.0, &g1_pubkey_point.0, &self.0, &message_hash);

        let scaled_g1_generator = G1Point::from(G1_GENERATOR).mul(alpha)?;
        let scaled_g1_pubkey = g1_pubkey_point.mul(alpha)?;

        let msg_hash_plus_scaled_g1_generator = G1Point::from(message_hash)
            .checked_add(&scaled_g1_generator)
            .ok_or(NCNProgramError::AltBN128AddError)?;
        let signature_plus_scaled_g1 = signature
            .checked_add(&scaled_g1_pubkey)
            .ok_or(NCNProgramError::AltBN128AddError)?;

        let mut input = [0u8; 384];

        // Pairing equation is:
        // e(H(m) + G1_Generator * alpha, g2_pubkey) = e(signature + g1_pubkey * alpha, G2_MINUS_ONE)

        // 1) Hash message to curve
        input[..64].clone_from_slice(&msg_hash_plus_scaled_g1_generator.0);
        // 2) Decompress our public key
        input[64..192].clone_from_slice(&self.0);
        // 3) Decompress our signature
        input[192..256].clone_from_slice(&signature_plus_scaled_g1.0);
        // 4) Pair with -G2::one()
        input[256..].clone_from_slice(&G2_MINUS_ONE);

        // Calculate result
        if let Ok(r) = alt_bn128_pairing(&input) {
            msg!("Pairing result: {:?}", r);
            if r.eq(&BN128_ADDITION_SUCESS_RESULT) {
                Ok(())
            } else {
                Err(NCNProgramError::BLSVerificationError)
            }
        } else {
            Err(NCNProgramError::AltBN128PairingError)
        }
    }

    /// Verifies an aggregated certificate: one challenge-combined pairing
    /// proving both the aggregate signature over `digest` and the consistency
    /// of the supplied aggregate G2 key with `apk1`. The challenge is
    /// EigenLayer's exact gamma (keccak over digest‖apk‖apkG2‖sigma, mod Fr).
    pub fn verify_aggregated_signature<H: HashToCurve>(
        self,
        aggregated_signature: G1Point,
        digest: &MessageDigest,
        apk1: G1Point,
    ) -> Result<(), NCNProgramError> {
        let message_hash = H::try_hash_to_curve(digest)?.0;
        let alpha = compute_certificate_gamma(&digest.0, &apk1.0, &self.0, &aggregated_signature.0);

        let scaled_g1 = G1Point::from(G1_GENERATOR).mul(alpha)?;
        let scaled_aggregated_g1 = apk1.mul(alpha)?;

        let msg_hash_plus_g1 = G1Point::from(message_hash)
            .checked_add(&scaled_g1)
            .ok_or(NCNProgramError::AltBN128AddError)?;
        let aggregated_signature_plus_aggregated_g1 = aggregated_signature
            .checked_add(&scaled_aggregated_g1)
            .ok_or(NCNProgramError::AltBN128AddError)?;

        let mut input = [0u8; 384];

        // Pairing equation is:
        // e(H(m) + G1_Generator * alpha, aggregated_g2) = e(aggregated_signature + aggregated_g1 * alpha, G2_MINUS_ONE)

        // 1) Hash message to curve
        input[..64].clone_from_slice(&msg_hash_plus_g1.0);
        // 2) Decompress our public key
        input[64..192].clone_from_slice(&self.0);
        // 3) Decompress our signature
        input[192..256].clone_from_slice(&aggregated_signature_plus_aggregated_g1.0);
        // 4) Pair with -G2::one()
        input[256..].clone_from_slice(&G2_MINUS_ONE);

        // Calculate result
        if let Ok(r) = alt_bn128_pairing(&input) {
            msg!("Pairing result: {:?}", r);
            if r.eq(&BN128_ADDITION_SUCESS_RESULT) {
                Ok(())
            } else {
                Err(NCNProgramError::BLSVerificationError)
            }
        } else {
            Err(NCNProgramError::AltBN128PairingError)
        }
    }
}

#[cfg(not(target_os = "solana"))]
impl core::ops::Add for G2Point {
    type Output = G2Point;

    fn add(self, rhs: Self) -> G2Point {
        self.checked_add(&rhs).expect("G2Point addition failed")
    }
}

#[cfg(not(target_os = "solana"))]
impl CheckedAdd for G2Point {
    // ark-bn254 group addition is total: no panics, no overflow (host-only)
    #[allow(clippy::arithmetic_side_effects)]
    fn checked_add(&self, rhs: &Self) -> Option<Self> {
        let result = (|| -> Result<Self, NCNProgramError> {
            let mut s0 = G2CompressedPoint::try_from(self)?.0;
            let mut s1 = G2CompressedPoint::try_from(rhs)?.0;

            s0.reverse();
            s1.reverse();

            let g2_agg = ark_bn254::G2Affine::deserialize_compressed(&s0[..])
                .map_err(|_| NCNProgramError::G2PointCompressionError)?
                + ark_bn254::G2Affine::deserialize_compressed(&s1[..])
                    .map_err(|_| NCNProgramError::G2PointCompressionError)?;

            let mut g2_aggregated_bytes = [0u8; 64];
            g2_agg
                .serialize_compressed(&mut &mut g2_aggregated_bytes[..])
                .map_err(|_| NCNProgramError::SerializationError)?;

            g2_aggregated_bytes.reverse();

            G2Point::try_from(G2CompressedPoint(g2_aggregated_bytes))
                .map_err(|_| NCNProgramError::G2PointDecompressionError)
        })();

        result.ok()
    }
}

impl G2CompressedPoint {
    pub fn verify_signature<H: HashToCurve, S: BLSSignature>(
        self,
        signature: S,
        digest: &MessageDigest,
    ) -> Result<(), NCNProgramError> {
        let mut input = [0u8; 384];

        // 1) Hash message to curve
        input[..64].clone_from_slice(&H::try_hash_to_curve(digest)?.0);
        // 2) Decompress our public key
        input[64..192].clone_from_slice(&G2Point::try_from(self)?.0);
        // 3) Decompress our signature
        input[192..256].clone_from_slice(&signature.to_bytes()?);
        // 4) Pair with -G2::one()
        input[256..].clone_from_slice(&G2_MINUS_ONE);

        // Calculate result
        if let Ok(r) = alt_bn128_pairing(&input) {
            if r.eq(&BN128_ADDITION_SUCESS_RESULT) {
                Ok(())
            } else {
                Err(NCNProgramError::BLSVerificationError)
            }
        } else {
            Err(NCNProgramError::AltBN128PairingError)
        }
    }
}

#[cfg(not(target_os = "solana"))]
impl TryFrom<&PrivKey> for G2CompressedPoint {
    type Error = NCNProgramError;

    // ark-bn254 scalar multiplication is total: no panics, no overflow (host-only)
    #[allow(clippy::arithmetic_side_effects)]
    fn try_from(value: &PrivKey) -> Result<G2CompressedPoint, Self::Error> {
        let mut pk = value.0;

        pk.reverse();

        let secret_key =
            Fr::deserialize_compressed(&pk[..]).map_err(|_| NCNProgramError::SecretKeyError)?;

        let g2_public_key = ark_bn254::G2Affine::generator() * secret_key;

        let mut g2_public_key_bytes = [0u8; 64];

        g2_public_key
            .serialize_compressed(&mut &mut g2_public_key_bytes[..])
            .map_err(|_| NCNProgramError::G2PointCompressionError)?;

        g2_public_key_bytes.reverse();

        Ok(Self(g2_public_key_bytes))
    }
}

#[cfg(not(target_os = "solana"))]
impl TryFrom<&PrivKey> for G2Point {
    type Error = NCNProgramError;

    fn try_from(value: &PrivKey) -> Result<G2Point, Self::Error> {
        Ok(G2Point(
            alt_bn128_g2_decompress(&G2CompressedPoint::try_from(value)?.0)
                .map_err(|_| NCNProgramError::G2PointDecompressionError)?,
        ))
    }
}

impl TryFrom<&G2Point> for G2CompressedPoint {
    type Error = NCNProgramError;

    fn try_from(value: &G2Point) -> Result<Self, Self::Error> {
        Ok(G2CompressedPoint(
            alt_bn128_g2_compress(&value.0)
                .map_err(|_| NCNProgramError::G2PointCompressionError)?,
        ))
    }
}

impl TryFrom<G2CompressedPoint> for G2Point {
    type Error = NCNProgramError;

    fn try_from(value: G2CompressedPoint) -> Result<Self, Self::Error> {
        Ok(G2Point(
            alt_bn128_g2_decompress(&value.0)
                .map_err(|_| NCNProgramError::G2PointDecompressionError)?,
        ))
    }
}

// Constructors from byte arrays
impl From<[u8; 128]> for G2Point {
    fn from(bytes: [u8; 128]) -> Self {
        G2Point(bytes)
    }
}

impl From<[u8; 64]> for G2CompressedPoint {
    fn from(bytes: [u8; 64]) -> Self {
        G2CompressedPoint(bytes)
    }
}

impl TryFrom<&[u8]> for G2Point {
    type Error = NCNProgramError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        if bytes.len() != 128 {
            return Err(NCNProgramError::InvalidInputLength);
        }
        let mut array = [0u8; 128];
        array.copy_from_slice(bytes);
        Ok(G2Point(array))
    }
}

impl TryFrom<&[u8]> for G2CompressedPoint {
    type Error = NCNProgramError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        if bytes.len() != 64 {
            return Err(NCNProgramError::InvalidInputLength);
        }
        let mut array = [0u8; 64];
        array.copy_from_slice(bytes);
        Ok(G2CompressedPoint(array))
    }
}

impl TryFrom<Vec<u8>> for G2Point {
    type Error = NCNProgramError;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        if bytes.len() != 128 {
            return Err(NCNProgramError::InvalidInputLength);
        }
        let mut array = [0u8; 128];
        array.copy_from_slice(&bytes);
        Ok(G2Point(array))
    }
}

impl TryFrom<Vec<u8>> for G2CompressedPoint {
    type Error = NCNProgramError;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        if bytes.len() != 64 {
            return Err(NCNProgramError::InvalidInputLength);
        }
        let mut array = [0u8; 64];
        array.copy_from_slice(&bytes);
        Ok(G2CompressedPoint(array))
    }
}
