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

// ===========================================================================
// §8 — Qwen answer settlement (real-model demo; supersedes the story fixture).
//
// The committee settles a CHAT ANSWER as token ids the browser BPE-decodes,
// mirroring the EVM `GasKillerChat.ChatAnswered(promptIds, answerIds)`.
//   Store { data } = the single-slot commitment root from the Qwen engine run
//                    (`GasKillerChat.computeChatRoot`), same single-slot rule.
//   Event { discriminant = sha256("gk:qwen_answer")[..8], payload = QwenAnswer }
// Small answers (<=24 tok) ride the event inline — no buffer account.
// ===========================================================================

/// Seed for the Qwen answer event discriminant (§8).
pub const QWEN_ANSWER_EVENT_SEED: &[u8] = b"gk:qwen_answer";

/// Model tag: real Qwen3-0.6B overlay.
pub const MODEL_QWEN3_0_6B: u8 = 0;
/// Model tag: Qwen3.5-35B-A3B overlay.
pub const MODEL_QWEN3_5_35B: u8 = 1;

/// The §8 `QwenAnswer` event payload (borsh, frozen). Rides a
/// `StateUpdate::Event` whose discriminant is `sha256("gk:qwen_answer")[..8]`.
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct QwenAnswer {
    /// 0 = qwen3-0.6b, 1 = qwen3.5-35b.
    pub model: u8,
    /// The tokenized prompt (browser-verifiable; chat template applied off-chain).
    pub prompt_ids: Vec<u32>,
    /// The greedy-decoded answer token ids from the real engine run.
    pub answer_ids: Vec<u32>,
    /// Overlay manifest = keccak(keccak(weights)||keccak(tok)).
    pub manifest: [u8; 32],
}

/// The Qwen answer event discriminant: `sha256("gk:qwen_answer")[..8]`.
pub fn qwen_answer_discriminant() -> [u8; 8] {
    let h = sha256(QWEN_ANSWER_EVENT_SEED);
    let mut d = [0u8; 8];
    d.copy_from_slice(&h[..8]);
    d
}

/// Everything the producer needs from the Qwen engine run + the Solana side.
pub struct QwenInputs<'a> {
    /// UTF-8 prompt (before the chat template is applied).
    pub prompt: &'a str,
    /// Model tag (see `MODEL_QWEN3_*`).
    pub model: u8,
    /// The tokenized prompt ids fed to the engine.
    pub prompt_ids: Vec<u32>,
    /// The greedy-decoded answer ids the engine returned.
    pub answer_ids: Vec<u32>,
    /// The overlay manifest.
    pub manifest: [u8; 32],
    /// The single-slot commitment root after this exchange
    /// (`GasKillerChat.computeChatRoot`).
    pub new_root: [u8; 32],
    /// The state PDA's transition_count BEFORE this transition.
    pub transition_index: u64,
    /// The consumer app's state PDA.
    pub state_pda: [u8; 32],
    /// The settle instruction's 8-byte discriminator.
    pub ix_discriminator: [u8; 8],
}

/// Build the §8 payload: `[Store { new_root }, Event { qwen_answer }]`.
pub fn build_qwen_payload(inputs: &QwenInputs) -> anyhow::Result<SettlementPayload> {
    let answer = QwenAnswer {
        model: inputs.model,
        prompt_ids: inputs.prompt_ids.clone(),
        answer_ids: inputs.answer_ids.clone(),
        manifest: inputs.manifest,
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
                discriminant: qwen_answer_discriminant(),
                payload: answer.try_to_vec()?,
            },
        ],
    })
}

/// Provenance of a Qwen run. Field names frozen per §8/Track Q1 (`cmd`,
/// `sdk_commit`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct QwenFixtureSource {
    /// The exact inference command that produced the answer ids.
    pub cmd: String,
    /// The gas-killer/solidity-sdk commit the inference ran at.
    pub sdk_commit: String,
}

/// The §8 Qwen JSON fixture. Field names are frozen (Track Q3 decodes it).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct QwenFixture {
    /// UTF-8 prompt (before the chat template).
    pub prompt: String,
    /// The tokenized prompt ids.
    pub prompt_ids: Vec<u32>,
    /// The real engine answer ids.
    pub answer_ids: Vec<u32>,
    /// The engine's detokenized answer text (for humans; the browser re-derives
    /// it from `answer_ids`).
    pub answer_text: String,
    /// hex of the single-slot commitment root (the `Store` value).
    pub commitment_root: String,
    /// hex of the overlay manifest.
    pub manifest: String,
    /// base64 of `borsh(SettlementPayload)`.
    pub payload_borsh_base64: String,
    /// hex of `sha256(borsh(SettlementPayload))` (the MessageDigest).
    pub digest_hex: String,
    /// Provenance of the run.
    pub source: QwenFixtureSource,
}

/// Build the full Qwen fixture from real engine values.
pub fn make_qwen_fixture(
    inputs: &QwenInputs,
    answer_text: &str,
    source: QwenFixtureSource,
) -> anyhow::Result<QwenFixture> {
    use base64::Engine as _;
    let payload = build_qwen_payload(inputs)?;
    let payload_bytes = payload.try_to_vec()?;
    Ok(QwenFixture {
        prompt: inputs.prompt.to_string(),
        prompt_ids: inputs.prompt_ids.clone(),
        answer_ids: inputs.answer_ids.clone(),
        answer_text: answer_text.to_string(),
        commitment_root: hex::encode(inputs.new_root),
        manifest: hex::encode(inputs.manifest),
        payload_borsh_base64: base64::engine::general_purpose::STANDARD.encode(&payload_bytes),
        digest_hex: hex::encode(sha256(&payload_bytes)),
        source,
    })
}

/// A Qwen fixture re-verified end to end.
pub struct VerifiedQwenFixture {
    /// The decoded settlement payload.
    pub payload: SettlementPayload,
    /// The recomputed message digest.
    pub digest: [u8; 32],
    /// The single Store's data (the commitment root).
    pub new_root: [u8; 32],
    /// The decoded qwen_answer event payload.
    pub answer: QwenAnswer,
}

/// Re-verify a Qwen fixture: base64/borsh round-trip (canonical), digest
/// recomputation, exactly one `Store` equal to `commitment_root`, and a
/// `qwen_answer` event whose ids/manifest match the top-level fields.
pub fn verify_qwen_fixture(fixture: &QwenFixture) -> anyhow::Result<VerifiedQwenFixture> {
    use base64::Engine as _;
    let payload_bytes =
        base64::engine::general_purpose::STANDARD.decode(&fixture.payload_borsh_base64)?;
    let payload = SettlementPayload::try_from_slice(&payload_bytes)?;

    // borsh round-trip must be canonical (the digest covers the raw bytes).
    anyhow::ensure!(
        payload.try_to_vec()? == payload_bytes,
        "borsh round-trip mismatch"
    );

    let digest = sha256(&payload_bytes);
    anyhow::ensure!(hex::encode(digest) == fixture.digest_hex, "digest mismatch");

    // Exactly one Store, equal to the fixture's commitment_root.
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
    anyhow::ensure!(
        hex::encode(new_root) == fixture.commitment_root,
        "Store data != fixture commitment_root"
    );

    // The qwen_answer event: present, ids + manifest match the top-level fields.
    let mut answer = None;
    for update in &payload.updates {
        if let StateUpdate::Event {
            discriminant,
            payload: event_payload,
        } = update
        {
            anyhow::ensure!(
                *discriminant == qwen_answer_discriminant(),
                "unknown event discriminant"
            );
            let decoded = QwenAnswer::try_from_slice(event_payload)?;
            anyhow::ensure!(
                decoded.prompt_ids == fixture.prompt_ids,
                "qwen_answer prompt_ids mismatch"
            );
            anyhow::ensure!(
                decoded.answer_ids == fixture.answer_ids,
                "qwen_answer answer_ids mismatch"
            );
            anyhow::ensure!(
                hex::encode(decoded.manifest) == fixture.manifest,
                "qwen_answer manifest mismatch"
            );
            answer = Some(decoded);
        }
    }
    let answer = answer.ok_or_else(|| anyhow::anyhow!("missing qwen_answer event"))?;

    Ok(VerifiedQwenFixture {
        new_root: *new_root,
        payload,
        digest,
        answer,
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

#[cfg(test)]
mod qwen_tests {
    use super::*;

    fn sample_inputs() -> QwenInputs<'static> {
        QwenInputs {
            prompt: "What is the capital of France?",
            model: MODEL_QWEN3_0_6B,
            prompt_ids: vec![151644, 872, 198],
            answer_ids: vec![785, 6722],
            manifest: [0xAB; 32],
            new_root: [0xCD; 32],
            transition_index: 0,
            state_pda: [0x55; 32],
            ix_discriminator: [1, 2, 3, 4, 5, 6, 7, 8],
        }
    }

    /// `sha256("gk:qwen_answer")[..8]` — the §8 discriminant.
    #[test]
    fn qwen_answer_discriminant_matches_spec() {
        // Independent recomputation of sha256("gk:qwen_answer")[..8].
        let full = sha256(b"gk:qwen_answer");
        assert_eq!(&qwen_answer_discriminant(), &full[..8]);
    }

    /// Pins the QwenAnswer borsh wire format byte for byte (u8 tag, u32-LE Vec
    /// length prefix + u32-LE items, [u8;32] raw). The browser and any
    /// re-implementation MUST match these bytes.
    #[test]
    fn qwen_answer_golden_borsh() {
        let answer = QwenAnswer {
            model: 0,
            prompt_ids: vec![1, 258],
            answer_ids: vec![785],
            manifest: [0x11; 32],
        };
        let expected = hex::decode(
            "00\
             020000000100000002010000\
             0100000011030000\
             1111111111111111111111111111111111111111111111111111111111111111",
        )
        .unwrap();
        assert_eq!(
            answer.try_to_vec().unwrap(),
            expected,
            "QwenAnswer wire drifted"
        );
        assert_eq!(QwenAnswer::try_from_slice(&expected).unwrap(), answer);
    }

    /// The payload is `[Store, Event{qwen_answer}]` — one Store, no story_meta.
    #[test]
    fn build_qwen_payload_shape() {
        let inputs = sample_inputs();
        let payload = build_qwen_payload(&inputs).unwrap();
        assert_eq!(payload.updates.len(), 2);
        assert_eq!(payload.updates[0], StateUpdate::Store { data: [0xCD; 32] });
        let StateUpdate::Event {
            discriminant,
            payload: event_payload,
        } = &payload.updates[1]
        else {
            panic!("expected Event");
        };
        assert_eq!(*discriminant, qwen_answer_discriminant());
        let decoded = QwenAnswer::try_from_slice(event_payload).unwrap();
        assert_eq!(decoded.prompt_ids, inputs.prompt_ids);
        assert_eq!(decoded.answer_ids, inputs.answer_ids);
        assert_eq!(decoded.manifest, inputs.manifest);
        assert_eq!(decoded.model, MODEL_QWEN3_0_6B);
    }

    /// make -> verify round-trip: digest, canonical borsh, one Store == root,
    /// qwen_answer ids/manifest consistency.
    #[test]
    fn qwen_fixture_round_trip() {
        let inputs = sample_inputs();
        let source = QwenFixtureSource {
            cmd: "sharded_infer.py --real ...".to_string(),
            sdk_commit: "deadbeef".to_string(),
        };
        let fixture = make_qwen_fixture(&inputs, "The capital", source).unwrap();
        let verified = verify_qwen_fixture(&fixture).unwrap();
        assert_eq!(verified.new_root, [0xCD; 32]);
        assert_eq!(verified.answer.answer_ids, inputs.answer_ids);
        assert_eq!(hex::encode(verified.digest), fixture.digest_hex);
        assert_eq!(verified.payload.transition_index, 0);
        assert_eq!(fixture.answer_text, "The capital");
    }

    /// A tampered answer id must break re-verification (digest is recomputed).
    #[test]
    fn qwen_fixture_detects_tamper() {
        let inputs = sample_inputs();
        let source = QwenFixtureSource {
            cmd: "x".to_string(),
            sdk_commit: "y".to_string(),
        };
        let mut fixture = make_qwen_fixture(&inputs, "hi", source).unwrap();
        fixture.answer_ids[0] ^= 1; // top-level ids now disagree with the event
        assert!(verify_qwen_fixture(&fixture).is_err());
    }
}
