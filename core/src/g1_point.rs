// =============================================================================
// IMPORTS
// =============================================================================

use core::ops::Add;
use core::ops::Mul;
use dashu::integer::UBig;
use num::CheckedAdd;

use solana_bn254::{
    compression::prelude::{alt_bn128_g1_compress, alt_bn128_g1_decompress},
    prelude::{alt_bn128_addition, alt_bn128_multiplication, alt_bn128_pairing},
};

use solana_program::msg;

use crate::constants::BN128_ADDITION_SUCESS_RESULT;
use crate::{
    constants::{G1_GENERATOR, G2_MINUS_ONE, MODULUS},
    error::NCNProgramError,
};
use crate::{g2_point::G2Point, privkey::PrivKey, schemes::BLSSignature};

// =============================================================================
// STRUCT DEFINITIONS
// =============================================================================

#[derive(Clone, Debug, Copy)]
pub struct G1Point(pub [u8; 64]);

#[derive(Clone, Default, Debug, Copy)]
pub struct G1CompressedPoint(pub [u8; 32]);

// =============================================================================
// BASIC CONSTRUCTORS AND CONVERSIONS
// =============================================================================

// From byte arrays
impl From<[u8; 64]> for G1Point {
    fn from(bytes: [u8; 64]) -> Self {
        G1Point(bytes)
    }
}

impl From<[u8; 32]> for G1CompressedPoint {
    fn from(bytes: [u8; 32]) -> Self {
        G1CompressedPoint(bytes)
    }
}

// From byte slices
impl TryFrom<&[u8]> for G1Point {
    type Error = NCNProgramError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        if bytes.len() != 64 {
            return Err(NCNProgramError::InvalidInputLength);
        }
        let mut array = [0u8; 64];
        array.copy_from_slice(bytes);
        Ok(G1Point(array))
    }
}

impl TryFrom<&[u8]> for G1CompressedPoint {
    type Error = NCNProgramError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        if bytes.len() != 32 {
            return Err(NCNProgramError::InvalidInputLength);
        }
        let mut array = [0u8; 32];
        array.copy_from_slice(bytes);
        Ok(G1CompressedPoint(array))
    }
}

// From Vec<u8>
impl TryFrom<Vec<u8>> for G1Point {
    type Error = NCNProgramError;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        if bytes.len() != 64 {
            return Err(NCNProgramError::InvalidInputLength);
        }
        let mut array = [0u8; 64];
        array.copy_from_slice(&bytes);
        Ok(G1Point(array))
    }
}

impl Default for G1Point {
    fn default() -> Self {
        G1Point([0u8; 64])
    }
}

// impl Default for G1CompressedPoint {
//     fn default() -> Self {
//         G1CompressedPoint([0u8; 32])
//     }
// }

impl TryFrom<Vec<u8>> for G1CompressedPoint {
    type Error = NCNProgramError;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        if bytes.len() != 32 {
            return Err(NCNProgramError::InvalidInputLength);
        }
        let mut array = [0u8; 32];
        array.copy_from_slice(&bytes);
        Ok(G1CompressedPoint(array))
    }
}

// From PrivKey
impl TryFrom<PrivKey> for G1Point {
    type Error = NCNProgramError;

    fn try_from(value: PrivKey) -> Result<Self, Self::Error> {
        let g1_generator = G1Point::from(G1_GENERATOR);
        Ok(g1_generator.mul(value.0))
    }
}

impl TryFrom<PrivKey> for G1CompressedPoint {
    type Error = NCNProgramError;

    fn try_from(value: PrivKey) -> Result<Self, Self::Error> {
        let g1_generator = G1Point::from(G1_GENERATOR);
        let pubkey = g1_generator.mul(value.0);
        G1CompressedPoint::try_from(pubkey)
    }
}

// Between compressed and uncompressed points
impl TryFrom<G1Point> for G1CompressedPoint {
    type Error = NCNProgramError;

    fn try_from(value: G1Point) -> Result<Self, Self::Error> {
        Ok(G1CompressedPoint(
            alt_bn128_g1_compress(&value.0)
                .map_err(|_| NCNProgramError::G1PointCompressionError)?,
        ))
    }
}

impl TryFrom<&G1CompressedPoint> for G1Point {
    type Error = NCNProgramError;

    fn try_from(value: &G1CompressedPoint) -> Result<Self, Self::Error> {
        Ok(G1Point(
            alt_bn128_g1_decompress(&value.0)
                .map_err(|_| NCNProgramError::G1PointDecompressionError)?,
        ))
    }
}

// =============================================================================
// TRAIT IMPLEMENTATIONS
// =============================================================================

// BLSSignature trait
impl BLSSignature for G1Point {
    fn to_bytes(&self) -> Result<[u8; 64], NCNProgramError> {
        Ok(self.0)
    }
}

impl BLSSignature for G1CompressedPoint {
    fn to_bytes(&self) -> Result<[u8; 64], NCNProgramError> {
        Ok(G1Point::try_from(self)?.0)
    }
}

// Addition operations
impl Add for G1Point {
    type Output = G1Point;

    fn add(self, rhs: Self) -> G1Point {
        self.checked_add(&rhs).expect("G1Point addition failed")
    }
}

impl CheckedAdd for G1Point {
    fn checked_add(&self, rhs: &Self) -> Option<Self> {
        let mut combined_input = [0u8; 128]; // Create a buffer large enough for both 64-byte arrays.

        unsafe {
            *(combined_input.as_mut_ptr() as *mut [u8; 64]) = self.0;
            *(combined_input.as_mut_ptr().add(64) as *mut [u8; 64]) = rhs.0;
        }

        let result = (|| -> Result<Self, NCNProgramError> {
            let result = alt_bn128_addition(&combined_input)
                .map_err(|_| NCNProgramError::AltBN128AddError)?;
            Ok(G1Point(
                result
                    .try_into()
                    .map_err(|_| NCNProgramError::AltBN128AddError)?,
            ))
        })();

        result.ok()
    }
}

// Multiplication operations
impl Mul<[u8; 32]> for G1Point {
    type Output = G1Point;
    fn mul(self, rhs: [u8; 32]) -> G1Point {
        match G1Point::mul(&self, rhs) {
            Ok(point) => point,
            Err(_) => panic!("G1Point multiplication failed"),
        }
    }
}

// =============================================================================
// CORE FUNCTIONALITY
// =============================================================================

impl G1Point {
    /// Multiply this G1 point by a scalar (big-endian 32 bytes)
    pub fn mul(&self, scalar: [u8; 32]) -> Result<G1Point, NCNProgramError> {
        let mut input = [0u8; 96];
        input[..64].copy_from_slice(&self.0);
        input[64..].copy_from_slice(&scalar);
        let result =
            alt_bn128_multiplication(&input).map_err(|_| NCNProgramError::AltBN128MulError)?;
        Ok(G1Point(
            result
                .try_into()
                .map_err(|_| NCNProgramError::AltBN128MulError)?,
        ))
    }

    /// Returns the negation of the point: (x, -y mod p)
    // UBig arithmetic cannot overflow, and y < p for any valid on-curve point,
    // so p - y cannot underflow; the pad is bounded by the 32-byte coordinate.
    #[allow(clippy::arithmetic_side_effects)]
    pub fn negate(&self) -> Self {
        // x: first 32 bytes, y: last 32 bytes
        let x_bytes = &self.0[0..32];
        let y_bytes = &self.0[32..64];
        let y = UBig::from_be_bytes(y_bytes);
        let neg_y = if y == UBig::ZERO {
            UBig::ZERO
        } else {
            (MODULUS.clone() - y) % MODULUS.clone()
        };
        let mut neg_point = [0u8; 64];
        neg_point[0..32].copy_from_slice(x_bytes);
        let neg_y_bytes = neg_y.to_be_bytes();
        // pad to 32 bytes if needed
        let pad = 32 - neg_y_bytes.len();
        if pad > 0 {
            for i in 0..pad {
                neg_point[32 + i] = 0;
            }
            neg_point[32 + pad..64].copy_from_slice(&neg_y_bytes);
        } else {
            neg_point[32..64].copy_from_slice(&neg_y_bytes);
        }
        G1Point(neg_point)
    }

    /// Verify G2 point using pairing check: e(self, G2) = e(G1_MINUS, g2)
    /// This is equivalent to checking: e(self, G2) * e(-G1_MINUS, g2) = 1
    /// Since G1_MINUS = (1, -2), then -G1_MINUS = (1, 2) = G1_generator
    pub fn verify_g2(&self, g2: &G2Point) -> Result<bool, NCNProgramError> {
        // Check for zero points first
        let self_is_zero = self.0.iter().all(|&x| x == 0);
        let g2_is_zero = g2.0.iter().all(|&x| x == 0);

        if self_is_zero || g2_is_zero {
            return Ok(false);
        }

        // Build input for pairing check (384 bytes total: 2 pairings × 192 bytes each)
        // First pairing: e(G1_Generator, g2 point)
        // Second pairing: e(self, G2_MINUS_ONE)
        let mut input = [0u8; 384];

        // First pairing:
        input[..64].copy_from_slice(&G1_GENERATOR); // G1 generator (G1 point)
        input[64..192].copy_from_slice(&g2.0); // g2 (G2 point)

        // Second pairing:
        input[192..256].copy_from_slice(&self.0); // self (G1 point)
        input[256..].copy_from_slice(&G2_MINUS_ONE); // G2 generator (G2 point)

        // Perform the pairing operation
        match alt_bn128_pairing(&input) {
            Ok(result) => {
                // The pairing returns 32 bytes: all zeros with 1 in the last byte if successful
                msg!("Pairing result: {:?}", result);
                if result.len() == 32 && result == BN128_ADDITION_SUCESS_RESULT {
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            Err(e) => {
                msg!("Pairing check failed with error: {:?}", e);
                Err(NCNProgramError::AltBN128PairingError)
            }
        }
    }
}

impl G1CompressedPoint {
    /// Negate the G1 compressed point by decompressing, negating, and compressing back
    pub fn negate(&self) -> Result<Self, NCNProgramError> {
        let g1_point = G1Point::try_from(self)?;
        let negated_g1 = g1_point.negate();
        G1CompressedPoint::try_from(negated_g1)
    }

    /// Verify G2 point using pairing check: e(self, G2) = e(G1_MINUS, g2)
    /// This is equivalent to checking: e(self, G2) * e(-G1_MINUS, g2) = 1
    /// Since G1_MINUS = (1, -2), then -G1_MINUS = (1, 2) = G1_generator
    pub fn verify_g2(&self, g2: &G2Point) -> Result<bool, NCNProgramError> {
        // First decompress this point to G1Point, then call its verify_g2 method
        let g1_point = G1Point::try_from(self)?;
        g1_point.verify_g2(g2)
    }
}

// =============================================================================
// UTILITY METHODS
// =============================================================================

#[cfg(not(target_os = "solana"))]
impl G1Point {
    pub fn from_random() -> G1Point {
        let private_key = PrivKey::from_random();
        G1Point::try_from(private_key).expect("Invalid private key for G1")
    }
}

#[cfg(not(target_os = "solana"))]
impl G1CompressedPoint {
    pub fn from_random() -> G1CompressedPoint {
        let private_key = PrivKey::from_random();
        G1CompressedPoint::try_from(private_key).expect("Invalid private key for G1")
    }
}
