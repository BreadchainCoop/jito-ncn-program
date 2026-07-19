#[cfg(not(target_os = "solana"))]
use rand::RngCore;

use solana_bn254::prelude::alt_bn128_multiplication;

use crate::{
    error::NCNProgramError,
    g1_point::G1Point,
    schemes::{HashToCurve, MessageDigest},
};

#[derive(Debug, Clone, Copy)]
pub struct PrivKey(pub [u8; 32]);

impl PrivKey {
    #[cfg(not(target_os = "solana"))]
    pub fn from_random() -> PrivKey {
        // Secret keys are scalars: sample below the group order Fr, not the
        // base field Fq — keys in [Fr, Fq) fail ark Fr deserialization during
        // G2 derivation and bias the keyspace.
        use crate::constants::FR_MODULUS;

        loop {
            let mut bytes = [0u8; 32];

            rand::thread_rng().fill_bytes(&mut bytes);

            let num = dashu::integer::UBig::from_be_bytes(&bytes);

            if num < FR_MODULUS {
                return Self(bytes);
            }
        }
    }

    pub fn sign<H: HashToCurve>(&self, digest: &MessageDigest) -> Result<G1Point, NCNProgramError> {
        let point = H::try_hash_to_curve(digest)?;

        let input = [&point.0[..], &self.0[..]].concat();

        let mut g1_sol_uncompressed = [0x00u8; 64];
        g1_sol_uncompressed.clone_from_slice(
            &alt_bn128_multiplication(&input).map_err(|_| NCNProgramError::BLSSigningError)?,
        );

        Ok(G1Point(g1_sol_uncompressed))
    }
}

#[cfg(all(test, not(target_os = "solana")))]
mod test {
    use crate::{
        g1_point::{G1CompressedPoint, G1Point},
        g2_point::G2Point,
        schemes::{sha256_normalized::Sha256Normalized, MessageDigest},
    };

    use super::PrivKey;

    fn sample_digest() -> MessageDigest {
        MessageDigest(solana_nostd_sha256::hashv(&[b"sample"]))
    }

    #[test]
    fn sign_is_deterministic() {
        let privkey = PrivKey([
            0x21, 0x6f, 0x05, 0xb4, 0x64, 0xd2, 0xca, 0xb2, 0x72, 0x95, 0x4c, 0x66, 0x0d, 0xd4,
            0x5c, 0xf8, 0xab, 0x0b, 0x26, 0x13, 0x65, 0x4d, 0xcc, 0xc7, 0x4c, 0x11, 0x55, 0xfe,
            0xba, 0xaf, 0xb5, 0xc9,
        ]);
        let a = privkey
            .sign::<Sha256Normalized>(&sample_digest())
            .expect("Failed to sign");
        let b = privkey
            .sign::<Sha256Normalized>(&sample_digest())
            .expect("Failed to sign");
        assert_eq!(a.0, b.0);
        // Compression round-trips
        let compressed = G1CompressedPoint::try_from(a).unwrap();
        let decompressed = G1Point::try_from(&compressed).unwrap();
        assert_eq!(a.0, decompressed.0);
    }

    #[test]
    fn sign_random_roundtrip() {
        let digest = sample_digest();
        let privkey = PrivKey::from_random();
        let signature = privkey
            .sign::<Sha256Normalized>(&digest)
            .expect("Failed to sign");
        let pubkey = G2Point::try_from(&privkey).expect("Invalid private key");
        assert!(pubkey
            .verify_signature::<Sha256Normalized, G1Point>(signature, &digest)
            .is_ok());
        // A different digest must not verify against the same signature
        let other = MessageDigest([0xAB; 32]);
        assert!(pubkey
            .verify_signature::<Sha256Normalized, G1Point>(signature, &other)
            .is_err());
    }

    #[test]
    fn random_keys_always_derive_g2() {
        // Pre-fix, keys sampled in [Fr, Fq) made G2 derivation fail; the
        // Fr-bounded sampler must never produce such a key.
        for _ in 0..64 {
            let privkey = PrivKey::from_random();
            G2Point::try_from(&privkey).expect("Fr-sampled key must derive G2");
        }
    }
}
