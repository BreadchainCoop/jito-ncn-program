#[cfg(test)]
mod tests {
    use jito_restaking_core::{config::Config, ncn_vault_ticket::NcnVaultTicket};
    use ncn_program_core::{
        error::NCNProgramError,
        g1_point::{G1CompressedPoint, G1Point},
        g2_point::{G2CompressedPoint, G2Point},
        schemes::{MessageDigest, Sha256Normalized},
        utils::{create_signer_bitmap, pop_message_digest},
    };

    use solana_sdk::{msg, signature::Keypair, signer::Signer};

    use crate::fixtures::{
        ncn_program_client::assert_ncn_program_error, test_builder::TestBuilder, TestResult,
    };

    // This test runs a complete end-to-end NCN (Network of Consensus Nodes) consensus workflow
    #[tokio::test]
    async fn simulation_test() -> TestResult<()> {
        // 1. Setup test environment
        // 1.a. Building the test environment
        let mut fixture = TestBuilder::new().await;
        // 1.b. Initialize the configuration for the restaking and vault programs
        // Note: On mainnet, these programs would already be configured
        fixture.initialize_restaking_and_vault_programs().await?;

        let mut ncn_program_client = fixture.ncn_program_client();
        let mut vault_program_client = fixture.vault_client();
        let mut restaking_client = fixture.restaking_program_client();

        // 2. Define test parameters
        const OPERATOR_COUNT: usize = 13; // Number of operators to create for testing
        let mint_keypair = Keypair::new();

        let delegations = [
            1, // minimum delegation amount
            1_000,
        ];

        // 3. Initialize system accounts and establish relationships
        // 3.a. Initialize the NCN account using the Jito Restaking program
        let mut test_ncn = fixture.create_test_ncn().await?;
        let ncn_pubkey = test_ncn.ncn_root.ncn_pubkey;

        // 3.b. Initialize operators and establish NCN <> operator relationships
        {
            for _ in 0..OPERATOR_COUNT {
                // Set operator fee to 100 basis points (1%)
                let operator_fees_bps: Option<u16> = Some(100);

                // Initialize a new operator account with the specified fee
                let operator_root = restaking_client
                    .do_initialize_operator(operator_fees_bps)
                    .await?;

                // Establish bidirectional handshake between NCN and operator:
                // 1. Initialize the NCN's state tracking for this operator
                restaking_client
                    .do_initialize_ncn_operator_state(
                        &test_ncn.ncn_root,
                        &operator_root.operator_pubkey,
                    )
                    .await?;

                // 2. Advance slot to satisfy timing requirements
                fixture.warp_slot_incremental(1).await.unwrap();

                // 3. NCN warms up to operator - creates NCN's half of the handshake
                restaking_client
                    .do_ncn_warmup_operator(&test_ncn.ncn_root, &operator_root.operator_pubkey)
                    .await?;

                // 4. Operator warms up to NCN - completes operator's half of the handshake
                restaking_client
                    .do_operator_warmup_ncn(&operator_root, &test_ncn.ncn_root.ncn_pubkey)
                    .await?;

                // Add the initialized operator to our test NCN's operator list
                test_ncn.operators.push(operator_root);
            }
        }

        // 3.c. Initialize vaults and establish NCN <> vaults and vault <> operator relationships
        {
            // Create the vault
            fixture
                .add_vaults_to_test_ncn(&mut test_ncn, 1, Some(mint_keypair.insecure_clone()))
                .await?;
        }

        // 3.d. Vaults delegate stakes to operators
        // Each vault delegates different amounts to different operators based on the delegation amounts array
        {
            for operator_root in test_ncn.operators.iter().take(OPERATOR_COUNT - 1).skip(1) {
                for vault_root in test_ncn.vaults.iter() {
                    // Cycle through delegation amounts based on operator index
                    let delegation_amount = delegations[1];

                    if delegation_amount > 0 {
                        vault_program_client
                            .do_add_delegation(
                                vault_root,
                                &operator_root.operator_pubkey,
                                delegation_amount,
                            )
                            .await
                            .unwrap();
                    }
                }
            }
            let first_operator_root = &test_ncn.operators[0];
            let last_operator_root = &test_ncn.operators[OPERATOR_COUNT - 1];

            let vault_root = &test_ncn.vaults[0];
            let delegation_amount = delegations[0];

            if delegation_amount > 0 {
                vault_program_client
                    .do_add_delegation(
                        vault_root,
                        &first_operator_root.operator_pubkey,
                        delegation_amount,
                    )
                    .await
                    .unwrap();

                vault_program_client
                    .do_add_delegation(
                        vault_root,
                        &last_operator_root.operator_pubkey,
                        delegation_amount,
                    )
                    .await
                    .unwrap();
            }
        }

        // 3.e. Fast-forward time to simulate a full epoch passing
        // This is needed for all the relationships to finish warming up
        {
            let restaking_config_address =
                Config::find_program_address(&jito_restaking_program::id()).0;
            let restaking_config = restaking_client
                .get_config(&restaking_config_address)
                .await?;
            let epoch_length = restaking_config.epoch_length();
            fixture
                .warp_slot_incremental(epoch_length * 2)
                .await
                .unwrap();
        }

        // 4. Setting up the NCN-program
        // The following instructions would be executed by the NCN admin in a production environment
        {
            // 4.a. Initialize the config for the NCN program
            ncn_program_client
                .do_initialize_config(
                    test_ncn.ncn_root.ncn_pubkey,
                    &test_ncn.ncn_root.ncn_admin,
                    Some(1000),
                )
                .await?;

            ncn_program_client
                .do_initialize_vote_counter(test_ncn.ncn_root.ncn_pubkey)
                .await?;

            // 4.b Initialize the vault_registry - creates accounts to track vaults
            ncn_program_client
                .do_full_initialize_vault_registry(test_ncn.ncn_root.ncn_pubkey)
                .await?;

            // 5.d. Take the snapshot - records the current state for this epoch
            fixture.add_snapshot_to_test_ncn(&test_ncn).await?;

            // 4.d. Register all the Supported Token (ST) mints in the NCN program
            // This assigns weights to each mint for voting power calculations
            let mint = mint_keypair;
            ncn_program_client
                .do_admin_register_st_mint(ncn_pubkey, mint.pubkey())
                .await?;

            // 4.c Register all the vaults in the NCN program
            // This is permissionless because the admin already approved it by initiating
            // the handshake before
            for vault in test_ncn.vaults.iter() {
                let vault = vault.vault_pubkey;
                let (ncn_vault_ticket, _, _) = NcnVaultTicket::find_program_address(
                    &jito_restaking_program::id(),
                    &ncn_pubkey,
                    &vault,
                );

                ncn_program_client
                    .do_register_vault(ncn_pubkey, vault, ncn_vault_ticket)
                    .await?;
            }
        }

        // 5. Register all the operators in the NCN program
        {
            for operator_root in test_ncn.operators.iter() {
                let g1_pubkey = G1Point::try_from(operator_root.bn128_privkey).unwrap();
                let g1_compressed = G1CompressedPoint::try_from(g1_pubkey).unwrap();
                let g2_compressed =
                    G2CompressedPoint::try_from(&operator_root.bn128_privkey).unwrap();

                let signature = operator_root
                    .bn128_privkey
                    .sign::<Sha256Normalized>(&pop_message_digest(
                        &ncn_pubkey,
                        &operator_root.operator_pubkey,
                        &g1_compressed.0,
                    ))
                    .unwrap();

                ncn_program_client
                    .do_register_operator(
                        ncn_pubkey,
                        operator_root.operator_pubkey,
                        &operator_root.operator_admin,
                        g1_compressed.0,
                        g2_compressed.0,
                        signature.0,
                    )
                    .await?;
            }
        }

        // Snapshot operators and vaults
        // In a real system, this step would run each epoch to keep the snapshot updated
        {
            fixture
                .add_vault_operator_delegation_snapshots_to_test_ncn(&test_ncn)
                .await?;
        }

        // 6. Cast votes from operators
        {
            let snapshot = ncn_program_client.get_snapshot(ncn_pubkey).await?;

            msg!("Snapshot: {}", snapshot);

            {
                // Get the current vote counter to use as the message
                let vote_counter = ncn_program_client
                    .get_vote_counter(ncn_pubkey)
                    .await
                    .unwrap();
                let current_count = vote_counter.count();

                // Create message from the current counter value (padded to 32 bytes)
                let count_bytes = current_count.to_le_bytes();
                let mut sunny_vote_message = [0u8; 32];
                sunny_vote_message[..8].copy_from_slice(&count_bytes);

                let mut sunny_signatures: Vec<G1Point> = vec![];
                let mut sunny_apk2_pubkeys: Vec<G2Point> = vec![];
                let mut non_signers_indices: Vec<usize> = vec![];

                for (i, operator) in test_ncn.operators.iter().enumerate() {
                    let operator_snapshot = snapshot
                        .find_operator_snapshot(&operator.operator_pubkey)
                        .unwrap();
                    if operator_snapshot.has_minimum_stake() {
                        sunny_apk2_pubkeys.push(operator.bn128_g2_pubkey);
                        let signature = operator
                            .bn128_privkey
                            .sign::<Sha256Normalized>(&MessageDigest(sunny_vote_message))
                            .unwrap();
                        sunny_signatures.push(signature);
                    } else {
                        non_signers_indices.push(i); // First operator didn't sign
                    }
                }

                // Aggregate signatures and public keys for Sunny vote
                let sunny_apk2 = sunny_apk2_pubkeys
                    .into_iter()
                    .reduce(|acc, x| acc + x)
                    .unwrap();
                let sunny_apk2_compressed = G2CompressedPoint::try_from(&sunny_apk2).unwrap().0;

                let sunny_agg_sig = sunny_signatures
                    .into_iter()
                    .reduce(|acc, x| acc + x)
                    .unwrap();
                let sunny_agg_sig_compressed =
                    G1CompressedPoint::try_from(sunny_agg_sig).unwrap().0;

                // Create signers bitmap
                let sunny_signers_bitmap =
                    create_signer_bitmap(&non_signers_indices, test_ncn.operators.len());

                // Cast the aggregated vote for Sunny weather
                ncn_program_client
                    .do_cast_vote(
                        ncn_pubkey,
                        sunny_agg_sig_compressed,
                        sunny_apk2_compressed,
                        sunny_signers_bitmap,
                    )
                    .await?;
            }
            {
                // Quorum not met case - get the current vote counter for this second vote attempt
                let vote_counter = ncn_program_client
                    .get_vote_counter(ncn_pubkey)
                    .await
                    .unwrap();
                let current_count = vote_counter.count();

                // Create message from the current counter value (padded to 32 bytes)
                let count_bytes = current_count.to_le_bytes();
                let mut cloudy_vote_message = [0u8; 32];
                cloudy_vote_message[..8].copy_from_slice(&count_bytes);

                let mut signatures: Vec<G1Point> = vec![];
                let mut apk2_pubkeys: Vec<G2Point> = vec![];
                let mut non_signers_indices: Vec<usize> = (4..OPERATOR_COUNT).collect();

                for (i, operator) in test_ncn.operators.iter().take(4).enumerate() {
                    let operator_snapshot = snapshot
                        .find_operator_snapshot(&operator.operator_pubkey)
                        .unwrap();
                    if operator_snapshot.has_minimum_stake() {
                        apk2_pubkeys.push(operator.bn128_g2_pubkey);
                        let signature = operator
                            .bn128_privkey
                            .sign::<Sha256Normalized>(&MessageDigest(cloudy_vote_message))
                            .unwrap();
                        signatures.push(signature);
                    } else {
                        non_signers_indices.push(i); // First operator didn't sign
                    }
                }

                // Aggregate signatures and public keys for  vote
                let apk2 = apk2_pubkeys.into_iter().reduce(|acc, x| acc + x).unwrap();
                let apk2_compressed = G2CompressedPoint::try_from(&apk2).unwrap().0;

                let agg_sig = signatures.into_iter().reduce(|acc, x| acc + x).unwrap();
                let agg_sig_compressed = G1CompressedPoint::try_from(agg_sig).unwrap().0;

                // Create signers bitmap
                let signers_bitmap =
                    create_signer_bitmap(&non_signers_indices, test_ncn.operators.len());

                // Cast the aggregated vote for  weather
                let result = ncn_program_client
                    .do_cast_vote(
                        ncn_pubkey,
                        agg_sig_compressed,
                        apk2_compressed,
                        signers_bitmap,
                    )
                    .await;
                assert_ncn_program_error(result, NCNProgramError::QuorumNotMet, Some(1));
            }

            println!("✅ BLS aggregate signature verification successful!");
            println!("✅ Cast vote operations completed successfully!");
        }

        println!("✅ BLS aggregate signature verification implementation complete!");
        println!("✅ Successfully demonstrated BLS signature aggregation and verification!");
        println!("✅ Simulation test completed successfully!");

        Ok(())
    }
}
