use std::cell::RefCell;

use solana_program::{instruction::InstructionError, program_error::ProgramError};
use solana_program_test::{BanksClient, BanksClientError, ProgramTestBanksClientExt};
use solana_sdk::{hash::Hash, transaction::TransactionError};
use thiserror::Error;

pub mod ncn_program_client;
pub mod restaking_client;
pub mod test_builder;
pub mod vault_client;

pub type TestResult<T> = Result<T, TestError>;

thread_local! {
    /// The last blockhash handed to a caller of [`fresh_blockhash`] within this
    /// test. `#[tokio::test]` runs on a single (current-thread) runtime, and
    /// nextest isolates each test in its own process, so a thread-local is an
    /// effectively per-test-global cursor shared by every client the test uses.
    static LAST_BLOCKHASH: RefCell<Option<Hash>> = const { RefCell::new(None) };
}

/// Returns a recent blockhash guaranteed to differ from the last one handed to
/// ANY client in this test, so every submitted transaction has a distinct
/// signature.
///
/// Why this exists: solana-program-test runs a wall-clock PohService that
/// registers new blockhashes on a timer. Two transactions built close together
/// (a fast/unloaded moment) can therefore share a blockhash; if they are
/// otherwise IDENTICAL (same instruction data, accounts, and signers) they have
/// the same signature, and BanksClient treats the second as a duplicate —
/// returning the FIRST transaction's cached result instead of executing the
/// second. That silently breaks any test that (a) expects a second identical
/// call to fail on-chain (e.g. remove-operator-twice) or (b) re-cranks the same
/// accounts and reads the updated state (e.g. the vault-operator-delegation
/// snapshot taken across successive delegation changes). Tracking the cursor
/// globally (not per client instance) closes the gap where two identical
/// transactions are built through DIFFERENT short-lived client objects. The
/// bank only ever mints brand-new unique hashes, so "different from the
/// immediately preceding one" is sufficient for global uniqueness.
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

#[derive(Error, Debug)]
#[allow(clippy::enum_variant_names)]
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
            Self::ProgramError(_) => None,
            _ => None,
        }
    }
}

#[inline(always)]
#[track_caller]
pub fn assert_ix_error<T>(test_error: Result<T, TestError>, ix_error: InstructionError) {
    assert!(test_error.is_err());
    assert_eq!(
        test_error.err().unwrap().to_transaction_error().unwrap(),
        TransactionError::InstructionError(0, ix_error)
    );
}
