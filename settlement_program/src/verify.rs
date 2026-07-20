//! Inline, stateless certificate verification (D-VERIFY option B).
//!
//! Thin adapter over the SHARED Phase 1 verify path:
//! `ncn_program::verify_certificate::verify_certificate_readonly`, which
//! validates the four NCN-side accounts and delegates to the shared core in
//! `ncn_program_core::certificate::verify_certificate` — the same
//! implementation the NCN program's `VerifyCertificate` instruction runs
//! (docs/INTERFACES.md §2). Nothing verification-related is implemented here:
//! the snapshot `generation` and the admin-settable
//! `Config.consensus_threshold_bps` are read by the shared path, and
//! verification failures surface as `NCNProgramError` codes
//! (`SnapshotGenerationMismatch`, `InsufficientStakeBps`, `VotingNotValid`
//! for staleness, `InvalidInputLength` for the bitmap,
//! `SignatureVerificationFailed` for the pairing).

use ncn_program::verify_certificate::verify_certificate_readonly;
use ncn_program_core::schemes::MessageDigest;
use solana_program::{account_info::AccountInfo, entrypoint::ProgramResult};

/// Verifies an aggregated BLS certificate over `digest` against the NCN's
/// snapshot. Read-only: mutates nothing.
///
/// Checks (§2, performed by the shared core): bitmap length; snapshot
/// generation; snapshot slot-freshness; per-signer minimum stake;
/// signed-stake bps vs `Config.consensus_threshold_bps`; single
/// challenge-combined pairing.
#[allow(clippy::too_many_arguments)]
pub fn verify_certificate_inline<'info>(
    ncn_config: &AccountInfo<'info>,
    ncn: &AccountInfo<'info>,
    snapshot: &AccountInfo<'info>,
    restaking_config: &AccountInfo<'info>,
    digest: &MessageDigest,
    aggregated_g2: &[u8; 64],
    aggregated_signature: &[u8; 32],
    operators_signature_bitmap: &[u8],
    expected_generation: u64,
) -> ProgramResult {
    verify_certificate_readonly(
        &ncn_program::id(),
        &[
            ncn_config.clone(),
            ncn.clone(),
            snapshot.clone(),
            restaking_config.clone(),
        ],
        &digest.0,
        aggregated_g2,
        aggregated_signature,
        operators_signature_bitmap,
        expected_generation,
    )
}
