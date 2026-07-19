use jito_bytemuck::AccountDeserialize;
use jito_restaking_core::{ncn::Ncn, operator::Operator};
use ncn_program_core::{
    utils::pop_message_digest,
    config::Config,
    constants::{G1_COMPRESSED_POINT_SIZE, G2_COMPRESSED_POINT_SIZE},
    error::NCNProgramError,
    g1_point::{G1CompressedPoint, G1Point},
    g2_point::{G2CompressedPoint, G2Point},
    ncn_operator_account::NCNOperatorAccount,
    schemes::sha256_normalized::Sha256Normalized,
    snapshot::Snapshot,
};
use solana_program::{
    account_info::AccountInfo,
    entrypoint::ProgramResult,
    msg,
    program_error::ProgramError,
    pubkey::Pubkey,
    sysvar::{clock::Clock, Sysvar},
};

/// Updates an operator's BLS keys in their individual ncn operator account account with signature verification.
///
/// ### Parameters:
/// - `g1_pubkey`: New G1 public key in compressed format (32 bytes)
/// - `g2_pubkey`: New G2 public key in compressed format (64 bytes)  
/// - `signature`: BLS signature of the new G1 pubkey signed by the new G2 private key (64 bytes uncompressed G1 point)
///
/// ### Accounts:
/// 1. `[]` config: NCN configuration account
/// 2. `[writable]` ncn_operator_account: The ncn operator account PDA account to update
/// 3. `[]` ncn: The NCN account
/// 4. `[]` operator: The operator to update
/// 5. `[signer]` operator_admin: The operator admin that must sign
/// 6. `[writable]` snapshot: The snapshot account
pub fn process_update_operator_bn128_keys(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    g1_pubkey: [u8; G1_COMPRESSED_POINT_SIZE],
    g2_pubkey: [u8; G2_COMPRESSED_POINT_SIZE],
    signature: [u8; 64],
) -> ProgramResult {
    let [config, ncn_operator_account, ncn, operator, operator_admin, snapshot] = accounts else {
        msg!("Error: Not enough account keys provided");
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    Config::load(program_id, config, ncn.key, false)?;
    NCNOperatorAccount::load(
        program_id,
        ncn_operator_account,
        ncn.key,
        operator.key,
        true,
    )?;
    Ncn::load(&jito_restaking_program::id(), ncn, false)?;
    Operator::load(&jito_restaking_program::id(), operator, false)?;
    Snapshot::load(program_id, snapshot, ncn.key, true)?;

    // Verify that the operator_admin is authorized to update this operator
    {
        let operator_data = operator.data.borrow();
        let operator_account = Operator::try_from_slice_unchecked(&operator_data)?;

        if operator_account.admin.ne(operator_admin.key) {
            msg!("Error: Operator admin is not authorized to update this operator");
            return Err(ProgramError::InvalidAccountData);
        }

        if !operator_admin.is_signer {
            msg!("Error: Operator admin must sign the transaction");
            return Err(ProgramError::MissingRequiredSignature);
        }
    }

    // Verify that the ncn operator account exists and belongs to the right operator
    {
        let ncn_operator_account_data = ncn_operator_account.try_borrow_data()?;
        let ncn_operator_account_account =
            NCNOperatorAccount::try_from_slice_unchecked(&ncn_operator_account_data)?;

        if ncn_operator_account_account.operator_pubkey() != operator.key {
            msg!("Error: NCN Operator Account does not belong to the specified operator");
            return Err(ProgramError::InvalidAccountData);
        }

        if ncn_operator_account_account.ncn() != ncn.key {
            msg!("Error: NCN Operator Account does not belong to the specified NCN");
            return Err(ProgramError::InvalidAccountData);
        }
    }

    // Verify BLS signature: signature should be new G1 pubkey signed by new G2 private key
    {
        // Convert the provided keys to points
        let g1_compressed = G1CompressedPoint::from(g1_pubkey);
        let g2_compressed = G2CompressedPoint::from(g2_pubkey);
        let signature = G1Point::from(signature);

        // Convert to uncompressed points for verification
        let g1_point = G1Point::try_from(&g1_compressed)
            .map_err(|_| NCNProgramError::G1PointDecompressionError)?;
        let g2_point = G2Point::try_from(g2_compressed)
            .map_err(|_| NCNProgramError::G2PointDecompressionError)?;

        // First verify that G1 and G2 are from the same private key
        let keypair_valid = g1_point
            .verify_g2(&g2_point)
            .map_err(|_| NCNProgramError::BLSVerificationError)?;

        if !keypair_valid {
            msg!("Error: New G1 and G2 public keys are not from the same private key");
            return Err(ProgramError::from(NCNProgramError::BLSVerificationError));
        }

        // Verify the BLS proof-of-possession for the NEW key: the signature is
        // over the domain-tagged digest binding (ncn, operator, new g1 pubkey),
        // signed by the new G2 private key
        let pop_digest = pop_message_digest(ncn.key, operator.key, &g1_pubkey);
        g2_point
            .verify_signature::<Sha256Normalized, _>(signature, &pop_digest)
            .map_err(|_| NCNProgramError::BLSVerificationError)?;

        msg!("BLS signature verification successful");
    }

    let clock = Clock::get()?;
    let slot = clock.slot;

    let mut ncn_operator_account_data = ncn_operator_account.try_borrow_mut_data()?;
    let ncn_operator_account_account =
        NCNOperatorAccount::try_from_slice_unchecked_mut(&mut ncn_operator_account_data)?;

    // Update the operator's keys
    ncn_operator_account_account.update_keys(&g1_pubkey, &g2_pubkey, slot)?;

    // update the key in the snapshot as well
    let mut snapshot_data = snapshot.try_borrow_mut_data()?;
    let snapshot_account = Snapshot::try_from_slice_unchecked_mut(&mut snapshot_data)?;

    snapshot_account.update_g1_operator_pubkey(&g1_pubkey, operator.key)?;

    msg!(
        "Operator BLS keys updated successfully for operator {}",
        operator.key
    );

    Ok(())
}
