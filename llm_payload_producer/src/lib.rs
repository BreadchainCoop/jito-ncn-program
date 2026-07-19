//! Track D — LLM task producer (docs/INTERFACES.md §6).
//!
//! Turns the output of a REAL Gas Killer LLM EVM simulation
//! (`GasKillerLLM.tellStory(prompt, maxNewTokens)` under the UnboundedV1 profile in
//! gas-killer/solidity-sdk) into the §4 `SettlementPayload` consumed by the
//! gaskiller-settlement program and the router (Tracks B/C):
//!
//! - `Store { data }` carries the consumer's new commitment root — the single-slot
//!   consumer pattern: the story root IS the one mutable slot value.
//! - `Event { discriminant: sha256("gk:story_meta")[..8], payload: borsh(StoryMeta) }`
//!   references the story bytes, which ride a buffer account (not the transaction).
//! - `digest = sha256(borsh(SettlementPayload))` is the `MessageDigest` operators sign.
//!
//! TODO(dedup): the borsh types below (`StateUpdate`, `SettlementPayload`) are local
//! copies of docs/INTERFACES.md §4 because Track C's `settlement_core` crate (branch
//! `settlement-program`) is not pushed yet. Once it lands on `main`, depend on it and
//! delete these definitions; the golden-bytes tests in this crate pin the wire format
//! so the swap is a pure refactor.

use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Seed string for the story-meta event discriminant (§4).
pub const STORY_META_EVENT_SEED: &[u8] = b"gk:story_meta";

/// One state update inside a settlement payload (§4). Borsh enum tag = 1 byte.
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
pub enum StateUpdate {
    /// Variant 0: the new commitment root (exactly one per payload).
    Store { data: [u8; 32] },
    /// Variant 1: an event emitted via self-CPI (`discriminant ‖ payload`).
    Event {
        discriminant: [u8; 8],
        payload: Vec<u8>,
    },
}

/// The §4 settlement payload. `digest = sha256(borsh(SettlementPayload))` is the
/// `MessageDigest` that the operator quorum BLS-signs.
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct SettlementPayload {
    /// Must equal the state PDA's `transition_count` at apply time.
    pub transition_index: u64,
    /// The consumer app's state PDA (as raw 32 bytes).
    pub state_pda: [u8; 32],
    /// The settle instruction's 8-byte discriminator.
    pub ix_discriminator: [u8; 8],
    /// The updates to apply: exactly one `Store` plus any `Event`s.
    pub updates: Vec<StateUpdate>,
}

/// Payload of the `gk:story_meta` event (§4): the story bytes live in a buffer
/// account, the event only carries their hash, location and length.
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct StoryMeta {
    /// sha256 of the story bytes stored in the buffer account.
    pub story_sha256: [u8; 32],
    /// The buffer account (PDA `[b"gk_buffer", state_pda, transition_index_le]`).
    pub buffer: [u8; 32],
    /// Story byte length inside the buffer account.
    pub len: u32,
}

/// sha256 convenience wrapper.
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Sha256::digest(bytes));
    out
}

/// The story-meta event discriminant: `sha256("gk:story_meta")[..8]`.
pub fn story_meta_discriminant() -> [u8; 8] {
    let h = sha256(STORY_META_EVENT_SEED);
    let mut d = [0u8; 8];
    d.copy_from_slice(&h[..8]);
    d
}

/// Everything the producer needs from the EVM simulation + the Solana side.
pub struct ProducerInputs<'a> {
    /// UTF-8 prompt fed to `tellStory`.
    pub prompt: &'a str,
    /// The generated story bytes (buffer account content).
    pub story: &'a [u8],
    /// The consumer's new commitment root (the single-slot value after the transition).
    pub new_root: [u8; 32],
    /// The state PDA's transition_count BEFORE this transition.
    pub transition_index: u64,
    /// The consumer app's state PDA.
    pub state_pda: [u8; 32],
    /// The settle instruction's 8-byte discriminator.
    pub ix_discriminator: [u8; 8],
    /// The story buffer account for this transition.
    pub buffer: [u8; 32],
}

/// Build the §4 payload: `[Store { new_root }, Event { story_meta }]`.
pub fn build_payload(inputs: &ProducerInputs) -> anyhow::Result<SettlementPayload> {
    let meta = StoryMeta {
        story_sha256: sha256(inputs.story),
        buffer: inputs.buffer,
        len: u32::try_from(inputs.story.len())?,
    };
    Ok(SettlementPayload {
        transition_index: inputs.transition_index,
        state_pda: inputs.state_pda,
        ix_discriminator: inputs.ix_discriminator,
        updates: vec![
            StateUpdate::Store {
                data: inputs.new_root,
            },
            StateUpdate::Event {
                discriminant: story_meta_discriminant(),
                payload: meta.try_to_vec()?,
            },
        ],
    })
}

/// `digest = sha256(borsh(payload))` — the MessageDigest the quorum signs.
pub fn payload_digest(payload: &SettlementPayload) -> anyhow::Result<[u8; 32]> {
    Ok(sha256(&payload.try_to_vec()?))
}

/// Where the fixture came from: the exact reproduction command + pinned commit.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct FixtureSource {
    /// The exact EVM simulation command(s) that produced the story + root.
    pub sim_command: String,
    /// The gas-killer/solidity-sdk commit the simulation ran at.
    pub solidity_sdk_commit: String,
}

/// The §6 JSON fixture consumed by Track B/C tests. Field names are frozen.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Fixture {
    /// UTF-8 prompt fed to `tellStory`.
    pub prompt: String,
    /// The generated story as UTF-8 text (the buffer account content).
    pub story_utf8: String,
    /// sha256 of the story bytes, hex.
    pub story_sha256_hex: String,
    /// base64 of `borsh(SettlementPayload)`.
    pub payload_borsh_base64: String,
    /// hex of `sha256(borsh(SettlementPayload))` (the MessageDigest).
    pub digest_hex: String,
    /// Provenance of the run.
    pub source: FixtureSource,
}

/// Build the full fixture from producer inputs (story must be valid UTF-8).
pub fn make_fixture(inputs: &ProducerInputs, source: FixtureSource) -> anyhow::Result<Fixture> {
    use base64::Engine as _;
    let story_utf8 = std::str::from_utf8(inputs.story)?.to_string();
    let payload = build_payload(inputs)?;
    let payload_bytes = payload.try_to_vec()?;
    Ok(Fixture {
        prompt: inputs.prompt.to_string(),
        story_utf8,
        story_sha256_hex: hex::encode(sha256(inputs.story)),
        payload_borsh_base64: base64::engine::general_purpose::STANDARD.encode(&payload_bytes),
        digest_hex: hex::encode(sha256(&payload_bytes)),
        source,
    })
}

/// A fixture whose internal consistency has been re-verified.
pub struct VerifiedFixture {
    /// The decoded settlement payload.
    pub payload: SettlementPayload,
    /// The recomputed message digest.
    pub digest: [u8; 32],
    /// The single Store's data (the new commitment root).
    pub new_root: [u8; 32],
    /// The decoded story-meta event payload.
    pub story_meta: StoryMeta,
}

/// Re-verify a fixture end to end: base64/borsh round-trip, digest recomputation,
/// story hash, exactly one `Store`, and a consistent `story_meta` event.
pub fn verify_fixture(fixture: &Fixture) -> anyhow::Result<VerifiedFixture> {
    use base64::Engine as _;
    let payload_bytes =
        base64::engine::general_purpose::STANDARD.decode(&fixture.payload_borsh_base64)?;
    let payload = SettlementPayload::try_from_slice(&payload_bytes)?;

    // borsh round-trip must be canonical
    anyhow::ensure!(
        payload.try_to_vec()? == payload_bytes,
        "borsh round-trip mismatch"
    );

    let digest = sha256(&payload_bytes);
    anyhow::ensure!(hex::encode(digest) == fixture.digest_hex, "digest mismatch");

    let story_hash = sha256(fixture.story_utf8.as_bytes());
    anyhow::ensure!(
        hex::encode(story_hash) == fixture.story_sha256_hex,
        "story sha256 mismatch"
    );

    let stores: Vec<&StateUpdate> = payload
        .updates
        .iter()
        .filter(|u| matches!(u, StateUpdate::Store { .. }))
        .collect();
    anyhow::ensure!(
        stores.len() == 1,
        "expected exactly one Store, got {}",
        stores.len()
    );
    let StateUpdate::Store { data: new_root } = stores[0] else {
        unreachable!()
    };

    let mut story_meta = None;
    for update in &payload.updates {
        if let StateUpdate::Event {
            discriminant,
            payload: event_payload,
        } = update
        {
            anyhow::ensure!(
                *discriminant == story_meta_discriminant(),
                "unknown event discriminant"
            );
            let meta = StoryMeta::try_from_slice(event_payload)?;
            anyhow::ensure!(meta.story_sha256 == story_hash, "story_meta hash mismatch");
            anyhow::ensure!(
                meta.len as usize == fixture.story_utf8.len(),
                "story_meta len mismatch"
            );
            story_meta = Some(meta);
        }
    }
    let story_meta = story_meta.ok_or_else(|| anyhow::anyhow!("missing story_meta event"))?;

    Ok(VerifiedFixture {
        new_root: *new_root,
        payload,
        digest,
        story_meta,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn golden_payload() -> SettlementPayload {
        SettlementPayload {
            transition_index: 7,
            state_pda: [0x11; 32],
            ix_discriminator: [1, 2, 3, 4, 5, 6, 7, 8],
            updates: vec![
                StateUpdate::Store { data: [0x22; 32] },
                StateUpdate::Event {
                    discriminant: [9, 10, 11, 12, 13, 14, 15, 16],
                    payload: vec![0xAA, 0xBB],
                },
            ],
        }
    }

    /// Pins the §4 wire format byte for byte (enum tag = 1 byte, u64/u32 LE,
    /// fixed arrays raw, Vec = u32 LE length prefix). Must match settlement_core
    /// (Track C) exactly.
    #[test]
    fn golden_borsh_bytes() {
        let expected = hex::decode(
            "0700000000000000\
             1111111111111111111111111111111111111111111111111111111111111111\
             0102030405060708\
             02000000\
             002222222222222222222222222222222222222222222222222222222222222222\
             01090a0b0c0d0e0f1002000000aabb",
        )
        .unwrap();
        let got = golden_payload().try_to_vec().unwrap();
        assert_eq!(got, expected, "borsh wire format drifted from §4");
        assert_eq!(
            SettlementPayload::try_from_slice(&expected).unwrap(),
            golden_payload()
        );
    }

    #[test]
    fn golden_digest() {
        let digest = payload_digest(&golden_payload()).unwrap();
        assert_eq!(
            hex::encode(digest),
            "c3b33f9d5b6ab218050c04fa371566bd522d4325c9f247fc68a266ec60db8041"
        );
    }

    #[test]
    fn story_meta_discriminant_matches_spec() {
        // sha256("gk:story_meta")[..8]
        assert_eq!(hex::encode(story_meta_discriminant()), "cca755e2a2a25bed");
    }

    #[test]
    fn story_meta_wire_format() {
        let meta = StoryMeta {
            story_sha256: [0x33; 32],
            buffer: [0x44; 32],
            len: 457,
        };
        let mut expected = vec![0x33u8; 32];
        expected.extend([0x44; 32]);
        expected.extend(457u32.to_le_bytes());
        assert_eq!(meta.try_to_vec().unwrap(), expected);
    }

    #[test]
    fn build_payload_shape() {
        let story = b"hello story";
        let inputs = ProducerInputs {
            prompt: "hi",
            story,
            new_root: [0xEE; 32],
            transition_index: 0,
            state_pda: [0x55; 32],
            ix_discriminator: [0x66; 8],
            buffer: [0x77; 32],
        };
        let payload = build_payload(&inputs).unwrap();
        assert_eq!(payload.transition_index, 0);
        assert_eq!(payload.updates.len(), 2);
        assert_eq!(payload.updates[0], StateUpdate::Store { data: [0xEE; 32] });
        let StateUpdate::Event {
            discriminant,
            payload: event_payload,
        } = &payload.updates[1]
        else {
            panic!("expected Event");
        };
        assert_eq!(*discriminant, story_meta_discriminant());
        let meta = StoryMeta::try_from_slice(event_payload).unwrap();
        assert_eq!(meta.story_sha256, sha256(story));
        assert_eq!(meta.buffer, [0x77; 32]);
        assert_eq!(meta.len, story.len() as u32);
    }

    #[test]
    fn fixture_round_trip() {
        let inputs = ProducerInputs {
            prompt: "hi",
            story: b"a tiny story",
            new_root: [0xEE; 32],
            transition_index: 3,
            state_pda: [0x55; 32],
            ix_discriminator: [0x66; 8],
            buffer: [0x77; 32],
        };
        let source = FixtureSource {
            sim_command: "test".to_string(),
            solidity_sdk_commit: "deadbeef".to_string(),
        };
        let fixture = make_fixture(&inputs, source).unwrap();
        let verified = verify_fixture(&fixture).unwrap();
        assert_eq!(verified.new_root, [0xEE; 32]);
        assert_eq!(verified.payload.transition_index, 3);
        assert_eq!(verified.story_meta.len, 12);
        assert_eq!(hex::encode(verified.digest), fixture.digest_hex);
    }
}
