//! Pins the checked-in REAL fixture: generated from an actual on-chain
//! `GasKillerLLM.tellStory("Once upon a time", 200)` execution (10,465,037,826 gas
//! in anvil/revm under the unbounded gas environment) at gas-killer/solidity-sdk
//! commit d44f65d6267cf68edc86c64cff446d1d4fe48d06. The story bytes are bit-exact
//! against llama2.c run.c on the stories260K checkpoint at temperature 0.

use llm_payload_producer::{Fixture, StateUpdate, verify_fixture};

const FIXTURE: &str = include_str!("../fixtures/tell_story_once_upon_a_time.json");

/// Values observed on-chain during the real run (root_before = 0x00..00,
/// stateTransitionCount before = 0, StoryTold newRoot after).
const NEW_ROOT_HEX: &str = "ee0dd4fb2b5bc913cb6b01f0964cf0a336dc04aa79c246067ffb7841d478b7d6";
const STORY_SHA256_HEX: &str = "98f77c3b2d2025d5ad628eff93bced73f90a2efc4e22fbe4b6778c69fdf37f14";

#[test]
fn real_fixture_recomputes() {
    let fixture: Fixture = serde_json::from_str(FIXTURE).expect("fixture parses");
    // digest recomputation, borsh round-trip, story hash, single-Store shape,
    // story_meta consistency
    let verified = verify_fixture(&fixture).expect("fixture verifies");

    assert_eq!(fixture.prompt, "Once upon a time");
    assert_eq!(fixture.story_sha256_hex, STORY_SHA256_HEX);
    assert_eq!(hex::encode(verified.new_root), NEW_ROOT_HEX);
    assert_eq!(
        verified.payload.transition_index, 0,
        "first transition of a fresh consumer"
    );
    assert_eq!(
        verified.story_meta.len, 457,
        "story byte length from the real run"
    );
    assert!(
        fixture
            .story_utf8
            .starts_with(", there was a little girl named Lily."),
        "story must be the real engine output"
    );
    assert_eq!(
        fixture.source.solidity_sdk_commit,
        "d44f65d6267cf68edc86c64cff446d1d4fe48d06"
    );

    // updates order is [Store, Event] per §6
    assert!(matches!(
        verified.payload.updates[0],
        StateUpdate::Store { .. }
    ));
    assert!(matches!(
        verified.payload.updates[1],
        StateUpdate::Event { .. }
    ));
}

#[test]
fn real_fixture_digest_golden() {
    let fixture: Fixture = serde_json::from_str(FIXTURE).expect("fixture parses");
    // Golden pin of the exact MessageDigest the quorum would sign for this fixture.
    assert_eq!(
        fixture.digest_hex,
        "374b70d839fd1f850e2722c47944feec40f775466245a1d4f31a8ae21022da7c"
    );
}
