//! Settlement payload wire types — MUST match docs/INTERFACES.md §4 exactly.
//!
//! `digest = sha256(borsh(SettlementPayload))` is the only thing operators
//! sign for a settle; the borsh byte-form here is therefore consensus-critical
//! and pinned by golden-byte tests below. Do not touch field order or types
//! without updating INTERFACES.md on `main` first.

use borsh::{BorshDeserialize, BorshSerialize};
use ncn_program_core::schemes::MessageDigest;
use solana_program::pubkey::Pubkey;

use crate::error::SettlementError;

/// A single state update inside a settlement payload (borsh enum, 1-byte tag).
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum StateUpdate {
    /// Tag 0: the new commitment root. Exactly one `Store` is required per
    /// payload; its `data` becomes `GkState.commitment_root`.
    Store { data: [u8; 32] },
    /// Tag 1: an event, emitted via self-CPI as `discriminant ‖ payload`.
    Event {
        discriminant: [u8; 8],
        payload: Vec<u8>,
    },
}

/// The signable settlement payload (borsh struct).
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct SettlementPayload {
    /// Consumer-local replay nonce: must equal `GkState.transition_count`.
    pub transition_index: u64,
    /// The state PDA this payload settles into (raw 32 bytes of the pubkey).
    pub state_pda: [u8; 32],
    /// The settle instruction's 8-byte discriminator (binds the payload to
    /// the settle verb, the bytes4-selector analog).
    pub ix_discriminator: [u8; 8],
    /// The state updates: exactly one `Store` plus any number of `Event`s.
    pub updates: Vec<StateUpdate>,
}

impl SettlementPayload {
    /// `sha256(borsh(SettlementPayload))` — the certified message digest.
    pub fn digest(&self) -> Result<MessageDigest, SettlementError> {
        let bytes = self
            .try_to_vec()
            .map_err(|_| SettlementError::SerializationError)?;
        Ok(MessageDigest(solana_nostd_sha256::hashv(&[&bytes])))
    }
}

/// Event discriminant for the large-payload story metadata event:
/// `sha256("gk:story_meta")[..8]`.
pub const STORY_META_DISCRIMINANT: [u8; 8] = [0xcc, 0xa7, 0x55, 0xe2, 0xa2, 0xa2, 0x5b, 0xed];

/// Borsh payload of a `STORY_META_DISCRIMINANT` event (INTERFACES.md §4):
/// the story itself rides the digest-verified buffer account, not the tx.
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct StoryMeta {
    /// sha256 of the story bytes staged in the buffer account.
    pub story_sha256: [u8; 32],
    /// The buffer PDA holding the story bytes.
    pub buffer: Pubkey,
    /// Story byte length: settle verifies `sha256(buffer.data[..len])`.
    pub len: u32,
}

/// Agave's `MAX_CPI_INSTRUCTION_DATA_LEN`: self-CPI event data (discriminant
/// ‖ payload) must stay at or under this.
pub const MAX_EVENT_CPI_DATA_LEN: usize = 10 * 1024;

/// Builds the self-CPI instruction data for an event: `discriminant ‖ payload`.
pub fn event_cpi_data(discriminant: &[u8; 8], payload: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(discriminant.len().saturating_add(payload.len()));
    data.extend_from_slice(discriminant);
    data.extend_from_slice(payload);
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instruction::SETTLE_DISCRIMINATOR;

    fn sample_payload() -> SettlementPayload {
        let meta = StoryMeta {
            story_sha256: [0x33; 32],
            buffer: Pubkey::new_from_array([0x44; 32]),
            len: 1234,
        };
        SettlementPayload {
            transition_index: 7,
            state_pda: [0x11; 32],
            ix_discriminator: SETTLE_DISCRIMINATOR,
            updates: vec![
                StateUpdate::Store { data: [0x22; 32] },
                StateUpdate::Event {
                    discriminant: STORY_META_DISCRIMINANT,
                    payload: meta.try_to_vec().unwrap(),
                },
            ],
        }
    }

    #[test]
    fn story_meta_discriminant_is_sha256_prefix() {
        let hash = solana_nostd_sha256::hashv(&[b"gk:story_meta"]);
        assert_eq!(&hash[..8], &STORY_META_DISCRIMINANT);
    }

    /// Golden borsh bytes: any change to the wire form (field order, enum tag
    /// order, integer widths) breaks this test — and the signature domain.
    #[test]
    fn payload_borsh_golden_bytes() {
        let bytes = sample_payload().try_to_vec().unwrap();
        let expected = hex::decode(concat!(
            "0700000000000000",
            "1111111111111111111111111111111111111111111111111111111111111111",
            "35f8e21f3ee14eae",
            "02000000",
            "00",
            "2222222222222222222222222222222222222222222222222222222222222222",
            "01",
            "cca755e2a2a25bed",
            "44000000",
            "3333333333333333333333333333333333333333333333333333333333333333",
            "4444444444444444444444444444444444444444444444444444444444444444",
            "d2040000",
        ))
        .unwrap();
        assert_eq!(bytes, expected);
    }

    #[test]
    fn payload_digest_golden_vector() {
        let digest = sample_payload().digest().unwrap();
        assert_eq!(
            hex::encode(digest.0),
            "44598d490f2ab7f03f90caaa67175cf783b3db3247c042c452211154ef70a0a3"
        );
    }

    #[test]
    fn store_only_payload_digest_golden_vector() {
        let payload = SettlementPayload {
            transition_index: 0,
            state_pda: [0xaa; 32],
            ix_discriminator: SETTLE_DISCRIMINATOR,
            updates: vec![StateUpdate::Store { data: [0xbb; 32] }],
        };
        let bytes = payload.try_to_vec().unwrap();
        let expected = hex::decode(concat!(
            "0000000000000000",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "35f8e21f3ee14eae",
            "01000000",
            "00",
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        ))
        .unwrap();
        assert_eq!(bytes, expected);
        assert_eq!(
            hex::encode(payload.digest().unwrap().0),
            "df1f09f6b77d2d45a2906e1824cbfd0a978b9ae9e030b58c2ced8c9d5d87bb1b"
        );
    }

    #[test]
    fn payload_borsh_roundtrip() {
        let payload = sample_payload();
        let bytes = payload.try_to_vec().unwrap();
        let decoded = SettlementPayload::try_from_slice(&bytes).unwrap();
        assert_eq!(payload, decoded);
    }

    #[test]
    fn story_meta_borsh_roundtrip() {
        let meta = StoryMeta {
            story_sha256: [7; 32],
            buffer: Pubkey::new_unique(),
            len: 42,
        };
        let bytes = meta.try_to_vec().unwrap();
        assert_eq!(bytes.len(), 32 + 32 + 4);
        assert_eq!(StoryMeta::try_from_slice(&bytes).unwrap(), meta);
    }

    #[test]
    fn digest_changes_with_any_field() {
        let base = sample_payload().digest().unwrap();
        let mut p = sample_payload();
        p.transition_index = 8;
        assert_ne!(base, p.digest().unwrap());
        let mut p = sample_payload();
        p.state_pda = [0x12; 32];
        assert_ne!(base, p.digest().unwrap());
        let mut p = sample_payload();
        p.ix_discriminator = [0; 8];
        assert_ne!(base, p.digest().unwrap());
        let mut p = sample_payload();
        p.updates.pop();
        assert_ne!(base, p.digest().unwrap());
    }

    #[test]
    fn event_cpi_data_layout() {
        let data = event_cpi_data(&STORY_META_DISCRIMINANT, &[1, 2, 3]);
        assert_eq!(&data[..8], &STORY_META_DISCRIMINANT);
        assert_eq!(&data[8..], &[1, 2, 3]);
    }
}
