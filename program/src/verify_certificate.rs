use jito_bytemuck::AccountDeserialize;
use jito_restaking_core::{config::Config, ncn::Ncn};
use ncn_program_core::{
    certificate,
    config::Config as NcnConfig,
    constants::{G1_COMPRESSED_POINT_SIZE, G2_COMPRESSED_POINT_SIZE},
    snapshot::Snapshot,
};
use solana_program::{
    account_info::AccountInfo, clock::Clock, entrypoint::ProgramResult, msg,
    program_error::ProgramError, pubkey::Pubkey, sysvar::Sysvar,
};

/// Stateless certificate verification (docs/INTERFACES.md par.2).
///
/// ### Parameters:
/// - `digest`: The signed 32-byte message digest
/// - `aggregated_g2`: Aggregate G2 public key of the signers (compressed, 64 bytes)
/// - `aggregated_signature`: Aggregate G1 signature (compressed, 32 bytes)
/// - `operators_signature_bitmap`: LSB-first signer bitmap per operator index
/// - `expected_generation`: Snapshot operator-set generation the certificate targets
///
/// ### Accounts (ALL read-only; nothing is mutated):
/// 1. `[]` ncn_config: NCN configuration account
/// 2. `[]` ncn: The NCN account
/// 3. `[]` snapshot: Snapshot containing stakes and operator snapshots
/// 4. `[]` restaking_config: Restaking configuration account
pub fn process_verify_certificate(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    digest: [u8; 32],
    aggregated_g2: [u8; G2_COMPRESSED_POINT_SIZE],
    aggregated_signature: [u8; G1_COMPRESSED_POINT_SIZE],
    operators_signature_bitmap: Vec<u8>,
    expected_generation: u64,
) -> ProgramResult {
    verify_certificate_readonly(
        program_id,
        accounts,
        &digest,
        &aggregated_g2,
        &aggregated_signature,
        &operators_signature_bitmap,
        expected_generation,
    )
}

/// Read-only verification entry point, callable inline from other programs
/// (D-VERIFY option B): the settlement program passes its own view of the
/// `[ncn_config, ncn, snapshot, restaking_config]` accounts together with the
/// NCN program id that owns them.
#[allow(clippy::too_many_arguments)]
pub fn verify_certificate_readonly(
    ncn_program_id: &Pubkey,
    accounts: &[AccountInfo],
    digest: &[u8; 32],
    aggregated_g2: &[u8; G2_COMPRESSED_POINT_SIZE],
    aggregated_signature: &[u8; G1_COMPRESSED_POINT_SIZE],
    operators_signature_bitmap: &[u8],
    expected_generation: u64,
) -> ProgramResult {
    let [ncn_config, ncn, snapshot, restaking_config] = accounts else {
        msg!("Error: Not enough account keys provided");
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    NcnConfig::load(ncn_program_id, ncn_config, ncn.key, false)?;
    Ncn::load(&jito_restaking_program::id(), ncn, false)?;
    Snapshot::load(ncn_program_id, snapshot, ncn.key, false)?;
    Config::load(&jito_restaking_program::id(), restaking_config, false)?;

    let (consensus_threshold_bps, valid_slots_after_consensus) = {
        let ncn_config_data = ncn_config.data.borrow();
        let ncn_config_account = NcnConfig::try_from_slice_unchecked(&ncn_config_data)?;
        (
            ncn_config_account.consensus_threshold_bps(),
            ncn_config_account.valid_slots_after_consensus(),
        )
    };

    let ncn_epoch_length = {
        let restaking_config_data = restaking_config.data.borrow();
        let restaking_config_account = Config::try_from_slice_unchecked(&restaking_config_data)?;
        restaking_config_account.epoch_length()
    };

    let current_slot = Clock::get()?.slot;

    let snapshot_data = snapshot.data.borrow();
    let snapshot_account = Snapshot::try_from_slice_unchecked(&snapshot_data)?;

    certificate::verify_certificate(
        snapshot_account,
        consensus_threshold_bps,
        valid_slots_after_consensus,
        ncn_epoch_length,
        current_slot,
        digest,
        aggregated_g2,
        aggregated_signature,
        operators_signature_bitmap,
        expected_generation,
    )?;

    msg!("Certificate verified");
    Ok(())
}
