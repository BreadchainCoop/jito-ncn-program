use solana_program::{decode_error::DecodeError, program_error::ProgramError};
use thiserror::Error;

/// Errors for the gaskiller-settlement program. The base (0x9100) is disjoint
/// from `NCNProgramError`'s ranges (0x2100/0x2200) so custom codes stay
/// unambiguous when both programs appear in one transaction.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SettlementError {
    /// `payload.transition_index` != `state.transition_count` — the consumer
    /// -local replay nonce (StateTracker analog). A replayed certificate
    /// always lands here.
    #[error("Invalid transition index (replay or gap)")]
    InvalidTransitionIndex = 0x9100,
    /// The payload does not bind this settle instruction / this digest domain
    /// (wrong `ix_discriminator`).
    #[error("Digest mismatch: payload does not bind the settle instruction")]
    DigestMismatch,
    /// `payload.state_pda` does not match the state account passed in.
    #[error("Payload state PDA does not match the state account")]
    InvalidStatePda,
    /// The payload carries no `Store` update.
    #[error("Payload has no Store update")]
    MissingStore,
    /// The payload carries more than one `Store` update.
    #[error("Payload has more than one Store update")]
    MultipleStore,
    /// sha256(buffer.data[..len]) != story_sha256 from the story_meta event.
    #[error("Buffer content hash does not match story_sha256")]
    BufferHashMismatch,
    /// Signed stake is below the consensus threshold (bps of total stake).
    #[error("Insufficient signed stake (bps below consensus threshold)")]
    InsufficientStakeBps,
    /// The snapshot is older than the freshness window.
    #[error("Snapshot is stale")]
    StaleSnapshot,
    /// `expected_generation` does not match the snapshot's generation.
    #[error("Snapshot generation mismatch")]
    GenerationMismatch,
    /// The signer bitmap length does not match the registered operator count.
    #[error("Invalid signer bitmap length")]
    InvalidBitmapLength,
    /// A signer in the bitmap does not meet the minimum stake requirement.
    #[error("Signer does not have minimum stake")]
    SignerHasNoMinimumStake,
    /// Aggregated BLS certificate failed pairing verification.
    #[error("Certificate signature verification failed")]
    SignatureVerificationFailed,
    /// Self-CPI event data would exceed MAX_CPI_INSTRUCTION_DATA_LEN (10 KiB).
    #[error("Event data exceeds the 10 KiB self-CPI limit")]
    EventTooLarge,
    /// A story_meta event is present but no buffer account was passed.
    #[error("Missing buffer account for story_meta event")]
    MissingBufferAccount,
    /// The buffer account passed does not match the event/PDA-derived buffer.
    #[error("Buffer account key mismatch")]
    BufferKeyMismatch,
    /// Write or hash range is out of the buffer's content bounds.
    #[error("Buffer offset/length out of bounds")]
    InvalidBufferBounds,
    /// The buffer for this transition has not been settled yet.
    #[error("Buffer transition has not settled yet")]
    BufferNotSettled,
    /// Only the recorded rent payer may close the buffer.
    #[error("Close authority is not the recorded buffer payer")]
    InvalidBufferPayer,
    /// The story_meta event payload failed to deserialize.
    #[error("Malformed story_meta event payload")]
    MalformedStoryMeta,
    /// Borsh (de)serialization failure.
    #[error("Serialization error")]
    SerializationError,
    /// Checked arithmetic overflowed.
    #[error("Arithmetic overflow")]
    ArithmeticOverflow,
    /// No operators registered in the snapshot / zero total stake.
    #[error("No stake registered in the snapshot")]
    NoStakeRegistered,
    /// The event self-CPI branch was invoked without the event authority.
    #[error("Event authority did not sign")]
    EventAuthorityNotSigner,
}

impl<T> DecodeError<T> for SettlementError {
    fn type_of() -> &'static str {
        "gaskiller::settlement"
    }
}

impl From<SettlementError> for ProgramError {
    fn from(e: SettlementError) -> Self {
        Self::Custom(e as u32)
    }
}

impl From<SettlementError> for u32 {
    fn from(e: SettlementError) -> Self {
        e as Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_start_at_base() {
        assert_eq!(SettlementError::InvalidTransitionIndex as u32, 0x9100);
        assert_eq!(
            SettlementError::DigestMismatch as u32,
            SettlementError::InvalidTransitionIndex as u32 + 1
        );
    }

    #[test]
    fn error_converts_to_program_error() {
        let e: ProgramError = SettlementError::BufferHashMismatch.into();
        assert_eq!(
            e,
            ProgramError::Custom(SettlementError::BufferHashMismatch as u32)
        );
    }
}
