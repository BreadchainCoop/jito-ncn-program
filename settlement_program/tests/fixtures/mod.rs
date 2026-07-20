//! Test fixtures for the settlement program integration suite.
//!
//! The restaking/vault/NCN client helpers are the REAL fixtures from
//! `integration_tests/tests/fixtures/` (included by `#[path]`, not copied),
//! so this suite drives the same source-of-truth flows the NCN suite does.
//! Only the builder is local: it registers the settlement program alongside
//! the NCN + jito programs, and adds settlement-specific helpers.

use std::cell::RefCell;

use solana_program::{instruction::InstructionError, program_error::ProgramError};
use solana_program_test::{BanksClient, BanksClientError, ProgramTestBanksClientExt};
use solana_sdk::{hash::Hash, transaction::TransactionError};
use thiserror::Error;

#[allow(dead_code, clippy::arithmetic_side_effects, clippy::integer_division)]
#[path = "../../../integration_tests/tests/fixtures/ncn_program_client.rs"]
pub mod ncn_program_client;

#[allow(dead_code, clippy::arithmetic_side_effects, clippy::integer_division)]
#[path = "../../../integration_tests/tests/fixtures/restaking_client.rs"]
pub mod restaking_client;

#[allow(dead_code, clippy::arithmetic_side_effects, clippy::integer_division)]
#[path = "../../../integration_tests/tests/fixtures/vault_client.rs"]
pub mod vault_client;

pub mod settlement_client;
pub mod settlement_test_builder;

pub type TestResult<T> = Result<T, TestError>;

thread_local! {
    /// Mirrors `integration_tests/tests/fixtures/mod.rs::LAST_BLOCKHASH` (the
    /// included client fixtures call `crate::fixtures::fresh_blockhash`): the
    /// last blockhash handed to a caller of [`fresh_blockhash`] within this
    /// test, a per-test-global cursor (current-thread runtime + one process
    /// per nextest test).
    static LAST_BLOCKHASH: RefCell<Option<Hash>> = const { RefCell::new(None) };
}

/// Returns a recent blockhash guaranteed to differ from the last one handed to
/// ANY client in this test, so every submitted transaction has a distinct
/// signature. Mirrors `integration_tests/tests/fixtures/mod.rs` verbatim —
/// see that file for the duplicate-signature/status-cache rationale.
pub async fn fresh_blockhash(banks_client: &mut BanksClient) -> Result<Hash, BanksClientError> {
    let last = LAST_BLOCKHASH.with(|cell| *cell.borrow());
    let mut blockhash = banks_client.get_latest_blockhash().await?;
    if Some(blockhash) == last {
        // Unchanged since our last submission — wait for the PohService to
        // register a new one so this transaction's signature is distinct.
        blockhash = banks_client.get_new_latest_blockhash(&blockhash).await?;
    }
    LAST_BLOCKHASH.with(|cell| *cell.borrow_mut() = Some(blockhash));
    Ok(blockhash)
}

// Mirrors integration_tests/tests/fixtures/mod.rs::TestError verbatim; the
// `*Error` variant names come from the wrapped types.
#[allow(clippy::enum_variant_names)]
#[derive(Error, Debug)]
pub enum TestError {
    #[error(transparent)]
    BanksClientError(#[from] BanksClientError),
    #[error(transparent)]
    ProgramError(#[from] ProgramError),
    #[error(transparent)]
    IoError(#[from] std::io::Error),
    #[error(transparent)]
    AnchorError(#[from] anchor_lang::error::Error),
}

impl TestError {
    pub fn to_transaction_error(&self) -> Option<TransactionError> {
        match self {
            Self::BanksClientError(e) => match e {
                BanksClientError::TransactionError(e) => Some(e.clone()),
                BanksClientError::SimulationError { err, .. } => Some(err.clone()),
                _ => None,
            },
            _ => None,
        }
    }
}

/// Asserts the result failed with `InstructionError(ix_index, ix_error)`.
#[inline(always)]
#[track_caller]
pub fn assert_ix_error_at<T: std::fmt::Debug>(
    test_error: Result<T, TestError>,
    ix_index: u8,
    ix_error: InstructionError,
) {
    assert!(test_error.is_err(), "expected error, got Ok");
    assert_eq!(
        test_error.err().unwrap().to_transaction_error().unwrap(),
        TransactionError::InstructionError(ix_index, ix_error)
    );
}

/// Asserts the result failed with the given settlement custom error code.
#[inline(always)]
#[track_caller]
pub fn assert_settlement_error<T: std::fmt::Debug>(
    test_error: Result<T, TestError>,
    ix_index: u8,
    error: settlement_core::error::SettlementError,
) {
    assert_ix_error_at(test_error, ix_index, InstructionError::Custom(error.into()));
}
