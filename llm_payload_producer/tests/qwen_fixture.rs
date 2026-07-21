//! Pins the checked-in REAL Qwen fixture (§8): generated from an actual
//! `Qwen3Engine.chat` execution of the real Qwen3-0.6B overlay for the prompt
//! "What is the capital of France?" (max_new 8) — the monolithic baseline of
//! the sharded-inference driver, 8 tokens in 740.7s at 740,884,020,041 gas, at
//! gas-killer/solidity-sdk (monterrey-v3) commit
//! d44f65d6267cf68edc86c64cff446d1d4fe48d06. The engine answered
//! "The capital of France is **Paris**". The commitment root is
//! `GasKillerChat.computeChatRoot(0, promptIds, answer)` (cross-checked against
//! Solidity keccak/abi.encode via foundry `cast`). No fabricated token ids.

use llm_payload_producer::{QwenFixture, StateUpdate, verify_qwen_fixture};

const FIXTURE: &str = include_str!("../fixtures/qwen06_capital_of_france.json");

/// The real overlay manifest = keccak(keccak(weights)||keccak(tok)), asserted
/// against the handoff's 0x23216cb9…c4a7ae9 before settling.
const MANIFEST_HEX: &str = "23216cb9ed9ef2b4bc20c84d27b68fa62ab194fc0845dfa707836f48ec4a7ae9";
/// The single-slot commitment root from the real chat run.
const COMMITMENT_ROOT_HEX: &str =
    "cffe78a8f0c333b2abe759584ae397bbbc168eb57aea0aacfaca447a57be46bc";
/// The greedy-decoded answer ids from the real engine run.
const ANSWER_IDS: &[u32] = &[785, 6722, 315, 9625, 374, 3070, 59604, 334];
/// The tokenized prompt (chat template applied off-chain).
const PROMPT_IDS: &[u32] = &[
    151644, 872, 198, 3838, 374, 279, 6722, 315, 9625, 30, 151645, 198, 151644, 77091, 198, 151667,
    271, 151668, 271,
];

#[test]
fn real_qwen_fixture_recomputes() {
    let fixture: QwenFixture = serde_json::from_str(FIXTURE).expect("fixture parses");
    // digest recomputation, canonical borsh, single Store == root, qwen_answer
    // event ids/manifest consistency.
    let verified = verify_qwen_fixture(&fixture).expect("fixture verifies");

    assert_eq!(fixture.prompt, "What is the capital of France?");
    assert_eq!(fixture.prompt_ids, PROMPT_IDS);
    assert_eq!(fixture.answer_ids, ANSWER_IDS);
    assert_eq!(fixture.answer_text, "The capital of France is **Paris**");
    assert_eq!(fixture.manifest, MANIFEST_HEX);
    assert_eq!(hex::encode(verified.new_root), COMMITMENT_ROOT_HEX);
    assert_eq!(fixture.commitment_root, COMMITMENT_ROOT_HEX);

    assert_eq!(
        verified.payload.transition_index, 0,
        "first transition of a fresh consumer"
    );
    assert_eq!(verified.answer.model, 0, "qwen3-0.6b");
    assert_eq!(verified.answer.answer_ids, ANSWER_IDS);
    assert_eq!(verified.answer.prompt_ids, PROMPT_IDS);
    assert_eq!(hex::encode(verified.answer.manifest), MANIFEST_HEX);

    assert_eq!(
        fixture.source.sdk_commit,
        "d44f65d6267cf68edc86c64cff446d1d4fe48d06"
    );

    // updates order is [Store, Event{qwen_answer}] per §8 (no story_meta, no buffer)
    assert!(matches!(
        verified.payload.updates[0],
        StateUpdate::Store { .. }
    ));
    assert!(matches!(
        verified.payload.updates[1],
        StateUpdate::Event { .. }
    ));
    assert_eq!(verified.payload.updates.len(), 2);
}

#[test]
fn real_qwen_fixture_digest_golden() {
    let fixture: QwenFixture = serde_json::from_str(FIXTURE).expect("fixture parses");
    // Golden pin of the checked-in (placeholder state_pda) MessageDigest. The
    // e2e regenerates it against the real, deployment-derived state PDA; the
    // pinned invariants are the answer ids, root, manifest and provenance.
    assert_eq!(
        fixture.digest_hex,
        "788656159600a9a09ba10de407bf87ddef0ee33b009fadac0d10a838ad5d58da"
    );
}
