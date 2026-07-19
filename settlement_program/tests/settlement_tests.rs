//! Integration tests for the gaskiller-settlement program: full NCN
//! environment (real jito restaking/vault programs + real NCN program), real
//! BLS keys and signatures, real snapshot state — no mocks.

#![allow(clippy::arithmetic_side_effects)]

mod fixtures;

use borsh::BorshSerialize;
use settlement_core::{
    buffer::BUFFER_TRAILER_LEN,
    instruction::{SettleArgs, SETTLE_DISCRIMINATOR},
    payload::{event_cpi_data, SettlementPayload, StateUpdate, StoryMeta, STORY_META_DISCRIMINANT},
};
use solana_program::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signer};

use crate::fixtures::{
    assert_settlement_error,
    settlement_test_builder::{SettlementTestBuilder, TestNcn},
    TestResult,
};
use settlement_core::error::SettlementError;

const OPERATOR_COUNT: usize = 4;
/// Generation proxy pre-Phase-1: snapshot.operators_registered.
const CURRENT_GENERATION: u64 = OPERATOR_COUNT as u64;

fn story_bytes(len: usize) -> Vec<u8> {
    // Deterministic pseudo-random content.
    (0..len).map(|i| ((i * 31 + 7) % 251) as u8).collect()
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    solana_nostd_sha256::hashv(&[bytes])
}

fn store_payload(state: &Pubkey, transition_index: u64, root: [u8; 32]) -> SettlementPayload {
    SettlementPayload {
        transition_index,
        state_pda: state.to_bytes(),
        ix_discriminator: SETTLE_DISCRIMINATOR,
        updates: vec![StateUpdate::Store { data: root }],
    }
}

fn story_payload(
    state: &Pubkey,
    transition_index: u64,
    root: [u8; 32],
    story: &[u8],
    buffer: &Pubkey,
) -> (SettlementPayload, StoryMeta) {
    let meta = StoryMeta {
        story_sha256: sha256(story),
        buffer: *buffer,
        len: story.len() as u32,
    };
    let payload = SettlementPayload {
        transition_index,
        state_pda: state.to_bytes(),
        ix_discriminator: SETTLE_DISCRIMINATOR,
        updates: vec![
            StateUpdate::Store { data: root },
            StateUpdate::Event {
                discriminant: STORY_META_DISCRIMINANT,
                payload: meta.try_to_vec().unwrap(),
            },
        ],
    };
    (payload, meta)
}

fn settle_args(
    fixture: &SettlementTestBuilder,
    test_ncn: &TestNcn,
    payload: SettlementPayload,
    non_signers: &[usize],
    expected_generation: u64,
) -> SettleArgs {
    let digest = payload.digest().unwrap();
    let cert = fixture.sign_digest(test_ncn, &digest, non_signers);
    SettleArgs {
        payload,
        aggregated_g2: cert.aggregated_g2,
        aggregated_signature: cert.aggregated_signature,
        operators_signature_bitmap: cert.bitmap,
        expected_generation,
    }
}

/// Full flow: state init -> multi-chunk buffer staging -> corrupted-buffer
/// settle failure -> repair -> settle with a REAL 4-operator certificate ->
/// state/tx-meta assertions -> replay failure -> wrong-generation failure ->
/// second store-only transition -> buffer close with rent refund.
#[tokio::test]
async fn test_settle_full_flow() -> TestResult<()> {
    let mut fixture = SettlementTestBuilder::new().await;
    let test_ncn = fixture
        .create_initial_test_ncn(OPERATOR_COUNT, None)
        .await?;
    let ncn = test_ncn.ncn_root.ncn_pubkey;
    let mut settlement = fixture.settlement_client();

    // ---- InitializeState ----
    let app_id = [7u8; 32];
    let sim_profile_id = [8u8; 32];
    let env_commitment = [9u8; 32];
    let state = settlement
        .do_initialize_state(&ncn, app_id, sim_profile_id, env_commitment)
        .await?;

    let state_account = settlement.get_state(&state).await?;
    assert_eq!(state_account.ncn(), &ncn);
    assert_eq!(state_account.app_id(), &app_id);
    assert_eq!(state_account.commitment_root(), &[0u8; 32]);
    assert_eq!(state_account.transition_count(), 0);
    assert_eq!(state_account.sim_profile_id(), &sim_profile_id);
    assert_eq!(state_account.env_commitment(), &env_commitment);

    // ---- Stage the story across multiple chunked transactions ----
    let story = story_bytes(3000);
    let buffer = settlement
        .do_write_buffer_chunked(&state, 0, &story, 800)
        .await?;

    let root = [0xAB; 32];
    let (payload, meta) = story_payload(&state, 0, root, &story, &buffer);
    let args = settle_args(&fixture, &test_ncn, payload, &[], CURRENT_GENERATION);

    // ---- Corrupt one staged chunk: settle must fail the buffer hash ----
    settlement
        .do_write_buffer(&state, 0, 800, vec![0xFF; 100], story.len() as u32)
        .await?;
    let result = settlement
        .do_settle(&ncn, &state, Some(&buffer), &args)
        .await;
    assert_settlement_error(result, 1, SettlementError::BufferHashMismatch);

    // ---- Repair the chunk (permissionless writer can rewrite) ----
    settlement
        .do_write_buffer(&state, 0, 800, story[800..900].to_vec(), story.len() as u32)
        .await?;

    // ---- Settle with the real aggregated certificate ----
    let (logs, inner_ix_data) = settlement
        .do_settle_with_meta(&ncn, &state, Some(&buffer), &args)
        .await?;

    // Self-CPI event evidence in the tx meta: the program invokes itself at
    // depth 2 (this jito-solana version's metadata.log_messages carries the
    // program-frame lines; the inner `Program log:` lines print to stdout but
    // are not surfaced here, so the payload bytes are asserted via the
    // inner-instruction record below).
    let program_id_str = settlement_program::id().to_string();
    assert!(
        logs.iter()
            .any(|l| l.contains(&format!("Program {} invoke [2]", program_id_str))),
        "no self-CPI in logs: {logs:#?}"
    );
    // The inner instruction carries `discriminant ‖ borsh(StoryMeta)`.
    let expected_event_data = event_cpi_data(&STORY_META_DISCRIMINANT, &meta.try_to_vec().unwrap());
    assert!(
        inner_ix_data.iter().any(|d| d == &expected_event_data),
        "story_meta event bytes not found in inner instructions: {inner_ix_data:?}"
    );

    let state_account = settlement.get_state(&state).await?;
    assert_eq!(state_account.commitment_root(), &root);
    assert_eq!(state_account.transition_count(), 1);

    // ---- Replay: the same certificate again must hit the nonce ----
    let result = settlement
        .do_settle(&ncn, &state, Some(&buffer), &args)
        .await;
    assert_settlement_error(result, 1, SettlementError::InvalidTransitionIndex);

    // ---- Wrong-generation certificate ----
    let payload = store_payload(&state, 1, [0xCD; 32]);
    let bad_gen_args = settle_args(&fixture, &test_ncn, payload, &[], CURRENT_GENERATION - 1);
    let result = settlement
        .do_settle(&ncn, &state, None, &bad_gen_args)
        .await;
    assert_settlement_error(result, 1, SettlementError::GenerationMismatch);

    // ---- Second transition: store-only payload, no buffer ----
    let payload = store_payload(&state, 1, [0xCD; 32]);
    let args = settle_args(&fixture, &test_ncn, payload, &[], CURRENT_GENERATION);
    settlement.do_settle(&ncn, &state, None, &args).await?;
    let state_account = settlement.get_state(&state).await?;
    assert_eq!(state_account.commitment_root(), &[0xCD; 32]);
    assert_eq!(state_account.transition_count(), 2);

    // ---- CloseBuffer: rent refunded to the recorded payer ----
    let payer = settlement.payer_keypair();
    let balance_before = settlement.get_balance(&payer.pubkey()).await?;
    let buffer_rent = settlement.get_balance(&buffer).await?;
    assert!(buffer_rent > 0);
    settlement.do_close_buffer(&state, 0, &payer).await?;
    let balance_after = settlement.get_balance(&payer.pubkey()).await?;
    assert!(!settlement.account_exists(&buffer).await?);
    // The refund minus the close-tx fee lands with the payer.
    assert!(balance_after > balance_before + buffer_rent - 50_000);

    Ok(())
}

/// Certificate and payload gates: threshold, bitmap shape, tampered payload,
/// store cardinality, digest/state binding, staleness.
#[tokio::test]
async fn test_settle_certificate_and_payload_gates() -> TestResult<()> {
    let mut fixture = SettlementTestBuilder::new().await;
    let test_ncn = fixture
        .create_initial_test_ncn(OPERATOR_COUNT, None)
        .await?;
    let ncn = test_ncn.ncn_root.ncn_pubkey;
    let mut settlement = fixture.settlement_client();

    let state = settlement
        .do_initialize_state(&ncn, [1u8; 32], [0u8; 32], [0u8; 32])
        .await?;

    // ---- 2 of 4 signers = 5000 bps < 6667: InsufficientStakeBps ----
    let payload = store_payload(&state, 0, [0x11; 32]);
    let args = settle_args(&fixture, &test_ncn, payload, &[2, 3], CURRENT_GENERATION);
    let result = settlement.do_settle(&ncn, &state, None, &args).await;
    assert_settlement_error(result, 1, SettlementError::InsufficientStakeBps);

    // ---- 3 of 4 signers = 7500 bps >= 6667: settles ----
    let payload = store_payload(&state, 0, [0x11; 32]);
    let args = settle_args(&fixture, &test_ncn, payload, &[3], CURRENT_GENERATION);
    settlement.do_settle(&ncn, &state, None, &args).await?;
    assert_eq!(settlement.get_state(&state).await?.transition_count(), 1);

    // ---- Bitmap length mismatch ----
    let payload = store_payload(&state, 1, [0x22; 32]);
    let mut args = settle_args(&fixture, &test_ncn, payload, &[], CURRENT_GENERATION);
    args.operators_signature_bitmap = vec![0xFF, 0xFF];
    let result = settlement.do_settle(&ncn, &state, None, &args).await;
    assert_settlement_error(result, 1, SettlementError::InvalidBitmapLength);

    // ---- Tampered payload: cert over a different digest ----
    let signed_payload = store_payload(&state, 1, [0x22; 32]);
    let mut args = settle_args(&fixture, &test_ncn, signed_payload, &[], CURRENT_GENERATION);
    args.payload = store_payload(&state, 1, [0x99; 32]); // not what was signed
    let result = settlement.do_settle(&ncn, &state, None, &args).await;
    assert_settlement_error(result, 1, SettlementError::SignatureVerificationFailed);

    // ---- No Store update ----
    let payload = SettlementPayload {
        transition_index: 1,
        state_pda: state.to_bytes(),
        ix_discriminator: SETTLE_DISCRIMINATOR,
        updates: vec![StateUpdate::Event {
            discriminant: [0xEE; 8],
            payload: vec![1, 2, 3],
        }],
    };
    let args = settle_args(&fixture, &test_ncn, payload, &[], CURRENT_GENERATION);
    let result = settlement.do_settle(&ncn, &state, None, &args).await;
    assert_settlement_error(result, 1, SettlementError::MissingStore);

    // ---- Two Store updates ----
    let payload = SettlementPayload {
        transition_index: 1,
        state_pda: state.to_bytes(),
        ix_discriminator: SETTLE_DISCRIMINATOR,
        updates: vec![
            StateUpdate::Store { data: [0x33; 32] },
            StateUpdate::Store { data: [0x44; 32] },
        ],
    };
    let args = settle_args(&fixture, &test_ncn, payload, &[], CURRENT_GENERATION);
    let result = settlement.do_settle(&ncn, &state, None, &args).await;
    assert_settlement_error(result, 1, SettlementError::MultipleStore);

    // ---- Payload not bound to the settle discriminator ----
    let mut payload = store_payload(&state, 1, [0x55; 32]);
    payload.ix_discriminator = [0u8; 8];
    let args = settle_args(&fixture, &test_ncn, payload, &[], CURRENT_GENERATION);
    let result = settlement.do_settle(&ncn, &state, None, &args).await;
    assert_settlement_error(result, 1, SettlementError::DigestMismatch);

    // ---- Payload bound to a different state PDA ----
    let mut payload = store_payload(&state, 1, [0x66; 32]);
    payload.state_pda = Pubkey::new_unique().to_bytes();
    let args = settle_args(&fixture, &test_ncn, payload, &[], CURRENT_GENERATION);
    let result = settlement.do_settle(&ncn, &state, None, &args).await;
    assert_settlement_error(result, 1, SettlementError::InvalidStatePda);

    // ---- story_meta present but no buffer account passed ----
    let (payload, _) = story_payload(&state, 1, [0x77; 32], b"story", &Pubkey::new_unique());
    let args = settle_args(&fixture, &test_ncn, payload, &[], CURRENT_GENERATION);
    let result = settlement.do_settle(&ncn, &state, None, &args).await;
    assert_settlement_error(result, 1, SettlementError::MissingBufferAccount);

    // ---- Stale snapshot (past valid_slots_after_consensus = 10_000) ----
    fixture.warp_slot_incremental(10_500).await.unwrap();
    let payload = store_payload(&state, 1, [0x88; 32]);
    let args = settle_args(&fixture, &test_ncn, payload, &[], CURRENT_GENERATION);
    let result = settlement.do_settle(&ncn, &state, None, &args).await;
    assert_settlement_error(result, 1, SettlementError::StaleSnapshot);

    Ok(())
}

/// WriteBuffer / CloseBuffer lifecycle gates.
#[tokio::test]
async fn test_buffer_lifecycle_gates() -> TestResult<()> {
    let mut fixture = SettlementTestBuilder::new().await;
    let test_ncn = fixture
        .create_initial_test_ncn(OPERATOR_COUNT, None)
        .await?;
    let ncn = test_ncn.ncn_root.ncn_pubkey;
    let mut settlement = fixture.settlement_client();

    let state = settlement
        .do_initialize_state(&ncn, [2u8; 32], [0u8; 32], [0u8; 32])
        .await?;

    let story = story_bytes(500);
    let buffer = settlement
        .do_write_buffer_chunked(&state, 0, &story, 200)
        .await?;

    // ---- Write past max_size is rejected ----
    let result = settlement
        .do_write_buffer(&state, 0, 480, vec![0u8; 100], story.len() as u32)
        .await;
    assert_settlement_error(result, 0, SettlementError::InvalidBufferBounds);

    // ---- Close before settle is rejected (retain-until-indexed) ----
    let payer = settlement.payer_keypair();
    let result = settlement.do_close_buffer(&state, 0, &payer).await;
    assert_settlement_error(result, 0, SettlementError::BufferNotSettled);

    // ---- Settle transition 0 with the story buffer ----
    let (payload, _) = story_payload(&state, 0, [0x10; 32], &story, &buffer);
    let args = settle_args(&fixture, &test_ncn, payload, &[], CURRENT_GENERATION);
    settlement
        .do_settle(&ncn, &state, Some(&buffer), &args)
        .await?;

    // ---- Writes to a settled transition are rejected ----
    let result = settlement
        .do_write_buffer(&state, 0, 0, vec![1, 2, 3], story.len() as u32)
        .await;
    assert_settlement_error(result, 0, SettlementError::InvalidTransitionIndex);

    // ---- Only the recorded payer may close ----
    let stranger = Keypair::new();
    settlement.airdrop(&stranger.pubkey(), 1.0).await?;
    let result = settlement.do_close_buffer(&state, 0, &stranger).await;
    assert_settlement_error(result, 0, SettlementError::InvalidBufferPayer);

    // ---- Recorded payer closes; buffer account is reclaimed ----
    settlement.do_close_buffer(&state, 0, &payer).await?;
    assert!(!settlement.account_exists(&buffer).await?);

    // Trailer arithmetic sanity: content + trailer fits the created size.
    assert!(story.len() + BUFFER_TRAILER_LEN <= 10_240);

    Ok(())
}
