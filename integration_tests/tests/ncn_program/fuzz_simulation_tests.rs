#[cfg(test)]
mod fuzz_tests {
    use crate::fixtures::{
        test_builder::{TestBuilder, TEST_DIGEST},
        TestResult,
    };
    use jito_restaking_core::{config::Config, ncn_vault_ticket::NcnVaultTicket};
    use ncn_program_core::{
        constants::MAX_OPERATORS,
        g1_point::{G1CompressedPoint, G1Point},
        g2_point::{G2CompressedPoint, G2Point},
        schemes::{MessageDigest, Sha256Normalized},
        utils::create_signer_bitmap,
    };
    use solana_sdk::{native_token::sol_to_lamports, signature::Keypair, signer::Signer};

    // Struct to configure mint token parameters for simulation
    struct MintConfig {
        keypair: Keypair,
        vault_count: usize, // Number of vaults to create for this mint
    }

    // Overall simulation configuration
    struct SimConfig {
        operator_count: usize,  // Number of operators to create
        mints: Vec<MintConfig>, // Token mint configurations
        delegations: Vec<u64>,  // Array of delegation amounts for vaults
        operator_fee_bps: u16,  // Operator fee in basis points (100 = 1%)
    }

    /// Main simulation function that runs a full consensus cycle with the given configuration
    /// This is a modular version of the simulation_test that can be run with different parameters
    /// It follows the same workflow: setup → initialization → voting  → verification
    async fn run_simulation(config: SimConfig) -> TestResult<()> {
        // 1. Create and initialize test environment
        let mut fixture = TestBuilder::new().await;
        fixture.initialize_restaking_and_vault_programs().await?;

        let mut ncn_program_client = fixture.ncn_program_client();
        let mut vault_program_client = fixture.vault_client();
        let mut restaking_client = fixture.restaking_program_client();

        // Validate configuration - ensure we have delegation amounts for each vault
        let total_vaults = config.mints.iter().map(|m| m.vault_count).sum::<usize>();
        assert_eq!(config.delegations.len(), total_vaults);

        // 2. Initialize system accounts and establish relationships
        // 2.a. Initialize the NCN account using the Jito Restaking program
        let mut test_ncn = fixture.create_test_ncn().await?;
        let ncn_pubkey = test_ncn.ncn_root.ncn_pubkey;

        // 2.b. Initialize operators and establish NCN <> operator relationships
        {
            for _i in 0..config.operator_count {
                // Set operator fee to the configured value
                let operator_fees_bps: Option<u16> = Some(config.operator_fee_bps);

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

        // 2.c. Initialize vaults and establish NCN <> vaults and vault <> operator relationships
        {
            // Create vaults for each mint according to the configuration
            for mint_config in config.mints.iter() {
                fixture
                    .add_vaults_to_test_ncn(
                        &mut test_ncn,
                        mint_config.vault_count,
                        Some(mint_config.keypair.insecure_clone()),
                    )
                    .await?;
            }
        }

        // 2.d. Vaults delegate stakes to operators
        // Each vault delegates to each operator with configured delegation amounts
        {
            for operator_root in test_ncn.operators.iter() {
                for (vault_index, vault_root) in test_ncn.vaults.iter().enumerate() {
                    // Use the delegation amount for this specific vault
                    let delegation_amount = config.delegations[vault_index];

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

            let total_delegations =
                config.operator_count * config.delegations.iter().filter(|&&d| d > 0).count();
            println!(
                "  ✅ Created {} delegation relationships",
                total_delegations
            );
        }

        // 2.e. Fast-forward time to simulate epochs passing
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

        // 3. Setting up the NCN-program
        // The following instructions would be executed by the NCN admin in a production environment
        {
            // 3.a. Initialize the config for the NCN program and the vote counter
            ncn_program_client
                .do_initialize_config(
                    test_ncn.ncn_root.ncn_pubkey,
                    &test_ncn.ncn_root.ncn_admin,
                    None,
                )
                .await?;
            // 3.b Initialize the vault_registry - creates accounts to track vaults
            ncn_program_client
                .do_full_initialize_vault_registry(test_ncn.ncn_root.ncn_pubkey)
                .await?;

            // 3.d. Register all the Supported Token (ST) mints in the NCN program
            // This assigns weights to each mint for voting power calculations
            for mint_config in config.mints.iter() {
                ncn_program_client
                    .do_admin_register_st_mint(ncn_pubkey, mint_config.keypair.pubkey())
                    .await?;
            }

            // 3.e Register all the vaults in the NCN program
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

            println!("  ✅ NCN program setup completed");
        }

        // initialize snapshots and register operators
        fixture.add_snapshot_to_test_ncn(&test_ncn).await?;
        fixture.register_operators_to_test_ncn(&test_ncn).await?;

        // 4. Snapshot operators and vaults
        // In a real system, this step would run each epoch to keep the snapshot updated
        {
            fixture
                .add_vault_operator_delegation_snapshots_to_test_ncn(&test_ncn)
                .await?;
        }

        // 5. Verify a certificate signed by the full operator set
        {
            let digest = TEST_DIGEST;
            let expected_generation = ncn_program_client.get_snapshot(ncn_pubkey).await?.generation();

            // All operators sign the same digest (no non-signers in this simulation)
            let mut signatures: Vec<G1Point> = vec![];
            let mut apk2_pubkeys: Vec<G2Point> = vec![];

            for operator_root in test_ncn.operators.iter() {
                apk2_pubkeys.push(operator_root.bn128_g2_pubkey);
                let signature = operator_root
                    .bn128_privkey
                    .sign::<Sha256Normalized>(&MessageDigest(digest))
                    .unwrap();
                signatures.push(signature);
            }

            // Aggregate signatures and public keys
            let apk2 = apk2_pubkeys.into_iter().reduce(|acc, x| acc + x).unwrap();
            let apk2_compressed = G2CompressedPoint::try_from(&apk2).unwrap().0;

            let agg_sig = signatures.into_iter().reduce(|acc, x| acc + x).unwrap();
            let agg_sig_compressed = G1CompressedPoint::try_from(agg_sig).unwrap().0;

            // Create signers bitmap - all operators signed (no non-signers)
            let non_signers_indices: Vec<usize> = vec![];
            let signers_bitmap =
                create_signer_bitmap(&non_signers_indices, test_ncn.operators.len());

            // Verify the aggregated certificate
            ncn_program_client
                .do_verify_certificate(
                    ncn_pubkey,
                    digest,
                    agg_sig_compressed,
                    apk2_compressed,
                    signers_bitmap,
                    expected_generation,
                )
                .await?;

            println!("  ✅ Certificate verification completed successfully");
        }

        Ok(())
    }

    // Test with basic configuration
    // This test runs the core simulation with a standard set of parameters
    #[ignore = "long test"]
    #[tokio::test]
    async fn test_basic_simulation() -> TestResult<()> {
        // Basic configuration with multiple mints and delegation amounts
        let config = SimConfig {
            operator_count: MAX_OPERATORS,
            mints: vec![MintConfig {
                keypair: Keypair::new(),
                vault_count: 1,
            }],
            delegations: vec![
                sol_to_lamports(1.0), // 1 SOL
            ],
            operator_fee_bps: 100, // 1% operator fee
        };

        run_simulation(config).await
    }

    // Test with high operator count to verify system scalability
    // This test ensures the system can handle a large number of operators
    #[ignore = "long test"]
    #[tokio::test]
    async fn test_high_operator_count_simulation() -> TestResult<()> {
        // Test with a large number of operators to verify scalability
        let config = SimConfig {
            operator_count: 50, // High number of operators
            mints: vec![MintConfig {
                keypair: Keypair::new(),
                vault_count: 1,
            }],
            delegations: vec![sol_to_lamports(1000.0)],
            operator_fee_bps: 100,
        };

        run_simulation(config).await
    }

    // Comprehensive fuzz testing with multiple configuration variations
    // This test runs several different configurations sequentially to stress test the system
    #[ignore = "long test"]
    #[tokio::test]
    async fn test_fuzz_simulation() -> TestResult<()> {
        // Create multiple test configurations with different parameters
        let test_configs = vec![
            // Test 1: Mid-size operator set with varied delegation amounts
            SimConfig {
                operator_count: 15,
                mints: vec![MintConfig {
                    keypair: Keypair::new(),
                    vault_count: 1,
                }],
                delegations: vec![sol_to_lamports(50.0)],
                operator_fee_bps: 90, // 0.9% fee
            },
            // Test 2: Extreme delegation amounts
            SimConfig {
                operator_count: 20,
                mints: vec![MintConfig {
                    keypair: Keypair::new(),
                    vault_count: 1,
                }],
                delegations: vec![
                    sol_to_lamports(1.0), // Very small delegation
                ],
                operator_fee_bps: 150, // 1.5% fee
            },
            // Test 3: Mixed token weights and varied delegation amounts
            SimConfig {
                operator_count: 30,
                mints: vec![MintConfig {
                    keypair: Keypair::new(),
                    vault_count: 1,
                }],
                delegations: vec![
                    sol_to_lamports(100.0), // Small delegation
                ],
                operator_fee_bps: 80, // 0.8% fee
            },
        ];

        // Run all configurations sequentially
        for (i, config) in test_configs.into_iter().enumerate() {
            println!("Running fuzz test configuration {}", i + 1);
            run_simulation(config).await?;
        }

        Ok(())
    }
}
