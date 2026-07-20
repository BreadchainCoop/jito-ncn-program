#[cfg(test)]
mod tests {
    use ncn_program_core::{
        constants::MAX_OPERATORS,
        error::NCNProgramError,
        g1_point::{G1CompressedPoint, G1Point},
        g2_point::{G2CompressedPoint, G2Point},
        schemes::{MessageDigest, Sha256Normalized},
        snapshot::Snapshot,
        utils::create_signer_bitmap,
    };
    use rand::Rng;
    use solana_sdk::pubkey::Pubkey;
    use std::collections::HashSet;

    use crate::fixtures::{
        ncn_program_client::assert_ncn_program_error,
        test_builder::{TestBuilder, TestNcn, TEST_DIGEST},
        TestResult,
    };

    pub fn get_random_none_signers_indecies(
        total_operators: usize,
        none_signers_count: usize,
    ) -> Vec<usize> {
        assert!(
            none_signers_count <= total_operators,
            "Cannot have more non-signers than total operators"
        );

        let mut rng = rand::rng();
        let mut none_signers_indices = HashSet::new();

        // Generate unique random indices
        while none_signers_indices.len() < none_signers_count {
            let index = rng.random_range(0..total_operators);
            none_signers_indices.insert(index);
        }

        // Convert to vector and sort for consistent output
        let mut result: Vec<usize> = none_signers_indices.into_iter().collect();
        result.sort();
        result
    }

    /// Signs `digest` with every operator not in `none_signers_indecies` and
    /// returns (agg_sig, apk2, bitmap).
    fn build_certificate(
        test_ncn: &TestNcn,
        digest: [u8; 32],
        none_signers_indecies: &[usize],
    ) -> ([u8; 32], [u8; 64], Vec<u8>) {
        let mut signitures: Vec<G1Point> = vec![];
        let mut apk2_pubkeys: Vec<G2Point> = vec![];
        for (i, operator) in test_ncn.operators.iter().enumerate() {
            if !none_signers_indecies.contains(&i) {
                apk2_pubkeys.push(operator.bn128_g2_pubkey);
                let signature = operator
                    .bn128_privkey
                    .sign::<Sha256Normalized>(&MessageDigest(digest))
                    .unwrap();
                signitures.push(signature);
            }
        }

        let apk2 = apk2_pubkeys.into_iter().reduce(|acc, x| acc + x).unwrap();
        let apk2 = G2CompressedPoint::try_from(&apk2).unwrap().0;

        let agg_sig = signitures.into_iter().reduce(|acc, x| acc + x).unwrap();
        let agg_sig = G1CompressedPoint::try_from(agg_sig).unwrap().0;

        let signers_bitmap = create_signer_bitmap(none_signers_indecies, test_ncn.operators.len());

        (agg_sig, apk2, signers_bitmap)
    }

    #[tokio::test]
    async fn test_verify_certificate_multiple_signers() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;
        let mut ncn_program_client = fixture.ncn_program_client();

        let test_ncn = fixture.create_initial_test_ncn(10, None).await?;

        ///// NCNProgram Setup /////
        fixture.warp_slot_incremental(1000).await?;
        fixture
            .update_snapshot_test_ncn_new_epoch(&test_ncn)
            .await?;
        //////

        let ncn = test_ncn.ncn_root.ncn_pubkey;

        let none_signers_indecies = get_random_none_signers_indecies(test_ncn.operators.len(), 2);
        let (agg_sig, apk2, signers_bitmap) =
            build_certificate(&test_ncn, TEST_DIGEST, &none_signers_indecies);

        let expected_generation = ncn_program_client.get_snapshot(ncn).await?.generation();

        ncn_program_client
            .do_verify_certificate(
                ncn,
                TEST_DIGEST,
                agg_sig,
                apk2,
                signers_bitmap,
                expected_generation,
            )
            .await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_verify_certificate_is_stateless_and_repeatable() -> TestResult<()> {
        // VerifyCertificate mutates nothing: the same certificate must verify
        // twice, and no involved account may change on success.
        let mut fixture = TestBuilder::new().await;
        let mut ncn_program_client = fixture.ncn_program_client();

        let test_ncn = fixture.create_initial_test_ncn(5, None).await?;

        ///// NCNProgram Setup /////
        fixture.warp_slot_incremental(1000).await?;
        fixture
            .update_snapshot_test_ncn_new_epoch(&test_ncn)
            .await?;
        //////

        let ncn = test_ncn.ncn_root.ncn_pubkey;

        let none_signers_indecies: Vec<usize> = vec![];
        let (agg_sig, apk2, signers_bitmap) =
            build_certificate(&test_ncn, TEST_DIGEST, &none_signers_indecies);

        let expected_generation = ncn_program_client.get_snapshot(ncn).await?.generation();

        let snapshot_address = Snapshot::find_program_address(&ncn_program::id(), &ncn).0;
        let snapshot_before = fixture.get_account(&snapshot_address).await?.unwrap();

        // First verification succeeds
        ncn_program_client
            .do_verify_certificate(
                ncn,
                TEST_DIGEST,
                agg_sig,
                apk2,
                signers_bitmap.clone(),
                expected_generation,
            )
            .await?;

        // No state change on success
        let snapshot_after = fixture.get_account(&snapshot_address).await?.unwrap();
        assert_eq!(
            snapshot_before.data, snapshot_after.data,
            "VerifyCertificate must not mutate the snapshot"
        );
        assert_eq!(snapshot_before.lamports, snapshot_after.lamports);

        // The exact same certificate verifies again (no replay protection at
        // the NCN layer; consumers bind their own nonces into the digest)
        fixture.warp_slot_incremental(1).await?;
        ncn_program_client
            .do_verify_certificate(
                ncn,
                TEST_DIGEST,
                agg_sig,
                apk2,
                signers_bitmap,
                expected_generation,
            )
            .await?;

        let snapshot_final = fixture.get_account(&snapshot_address).await?.unwrap();
        assert_eq!(snapshot_before.data, snapshot_final.data);

        Ok(())
    }

    #[tokio::test]
    async fn test_verify_certificate_wrong_generation_fails() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;
        let mut ncn_program_client = fixture.ncn_program_client();

        let test_ncn = fixture.create_initial_test_ncn(5, None).await?;

        ///// NCNProgram Setup /////
        fixture.warp_slot_incremental(1000).await?;
        fixture
            .update_snapshot_test_ncn_new_epoch(&test_ncn)
            .await?;
        //////

        let ncn = test_ncn.ncn_root.ncn_pubkey;

        let none_signers_indecies: Vec<usize> = vec![];
        let (agg_sig, apk2, signers_bitmap) =
            build_certificate(&test_ncn, TEST_DIGEST, &none_signers_indecies);

        let current_generation = ncn_program_client.get_snapshot(ncn).await?.generation();

        let result = ncn_program_client
            .do_verify_certificate(
                ncn,
                TEST_DIGEST,
                agg_sig,
                apk2,
                signers_bitmap,
                current_generation + 1,
            )
            .await;

        assert_ncn_program_error(result, NCNProgramError::SnapshotGenerationMismatch, Some(1));

        Ok(())
    }

    #[tokio::test]
    async fn test_verify_certificate_signer_below_minimum_stake_fails() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;
        let mut vault_client = fixture.vault_client();

        let test_ncn = fixture.create_initial_test_ncn(10, None).await?;

        ///// NCNProgram Setup /////
        fixture.warp_slot_incremental(1000).await?;
        fixture
            .update_snapshot_test_ncn_new_epoch(&test_ncn)
            .await?;
        //////

        {
            // Remove stake from one operator to get it to below minimum stake
            let operator_index = 5;
            let operator_root = &test_ncn.operators[operator_index];

            vault_client
                .do_cooldown_delegation(&test_ncn.vaults[0], &operator_root.operator_pubkey, 99)
                .await?;

            fixture.warp_epoch_incremental(2).await?;

            fixture
                .update_snapshot_test_ncn_new_epoch(&test_ncn)
                .await?;
        }
        // Operator 5 signs even though it is below the minimum stake
        let none_signers_indecies: Vec<usize> = vec![1, 9];
        let result = fixture
            .verify_certificate_for_test_ncn(&test_ncn, TEST_DIGEST, none_signers_indecies)
            .await;

        assert_ncn_program_error(result, NCNProgramError::OperatorHasNoMinimumStake, Some(1));

        Ok(())
    }

    #[tokio::test]
    async fn test_verify_certificate_stake_decays_next_epoch_for_signer() -> TestResult<()> {
        // Even without a fresh crank, an operator that fell below the minimum
        // stake cannot sign in the next epoch (has_minimum_stake_next_epoch
        // decay). The freshness window is widened so the epoch-decay path is
        // what gets exercised.
        let mut fixture = TestBuilder::new().await;
        let mut vault_client = fixture.vault_client();
        let mut ncn_program_client = fixture.ncn_program_client();

        let test_ncn = fixture.create_initial_test_ncn(10, None).await?;

        ///// NCNProgram Setup /////
        fixture.warp_slot_incremental(1000).await?;
        fixture
            .update_snapshot_test_ncn_new_epoch(&test_ncn)
            .await?;
        //////

        // Widen the slot-freshness window beyond one epoch so the per-signer
        // epoch decay (not snapshot staleness) is exercised
        ncn_program_client
            .do_set_parameters(
                None,
                None,
                None,
                Some(500_000), // valid_slots_after_consensus > 1 epoch
                None,
                None,
                &test_ncn.ncn_root,
            )
            .await?;

        {
            // Remove stake from one operator to get it below the minimum
            let operator_index = 5;
            let operator_root = &test_ncn.operators[operator_index];

            vault_client
                .do_cooldown_delegation(&test_ncn.vaults[0], &operator_root.operator_pubkey, 99)
                .await?;

            fixture.warp_epoch_incremental(1).await?;

            fixture
                .update_snapshot_test_ncn_new_epoch(&test_ncn)
                .await?;

            fixture.warp_epoch_incremental(1).await?;
        }
        let none_signers_indecies: Vec<usize> = vec![1, 9];

        let result = fixture
            .verify_certificate_for_test_ncn(&test_ncn, TEST_DIGEST, none_signers_indecies)
            .await;

        assert_ncn_program_error(result, NCNProgramError::OperatorHasNoMinimumStake, Some(1));

        Ok(())
    }

    #[tokio::test]
    async fn test_verify_certificate_stale_snapshot_fails() -> TestResult<()> {
        // No crank for two epochs: the snapshot-level slot-freshness check
        // rejects the certificate outright.
        let mut fixture = TestBuilder::new().await;
        let test_ncn = fixture.create_initial_test_ncn(10, None).await?;

        ///// NCNProgram Setup /////
        fixture.warp_slot_incremental(1000).await?;
        fixture
            .update_snapshot_test_ncn_new_epoch(&test_ncn)
            .await?;
        //////

        fixture.warp_epoch_incremental(2).await?;

        let none_signers_indecies: Vec<usize> = vec![1, 9];
        let result = fixture
            .verify_certificate_for_test_ncn(&test_ncn, TEST_DIGEST, none_signers_indecies)
            .await;

        assert_ncn_program_error(result, NCNProgramError::VotingNotValid, Some(1));

        Ok(())
    }

    #[tokio::test]
    async fn test_verify_certificate_outdated_signer_fails() -> TestResult<()> {
        // The snapshot as a whole is fresh (recranked), but one SIGNER's own
        // stake view is >1 epoch old: per-signer staleness must reject.
        let mut fixture = TestBuilder::new().await;
        let mut ncn_program_client = fixture.ncn_program_client();
        let mut vault_program_client = fixture.vault_program_client();
        let test_ncn = fixture.create_initial_test_ncn(10, None).await?;

        ///// NCNProgram Setup /////
        fixture.warp_slot_incremental(1000).await?;
        fixture
            .update_snapshot_test_ncn_new_epoch(&test_ncn)
            .await?;
        //////

        fixture.warp_epoch_incremental(2).await?;

        let clock = fixture.clock().await;
        let slot = clock.slot;
        let ncn = test_ncn.ncn_root.ncn_pubkey;

        // Recrank everyone EXCEPT operator 1, which will still sign below
        let operators_to_skip_indexes = [1usize];

        let operators_for_update = test_ncn
            .operators
            .iter()
            .map(|operator_root| operator_root.operator_pubkey)
            .collect::<Vec<Pubkey>>();
        let vault = test_ncn.vaults[0].vault_pubkey;

        let vault_is_update_needed = vault_program_client
            .get_vault_is_update_needed(&vault, slot)
            .await?;

        if vault_is_update_needed {
            vault_program_client
                .do_full_vault_update(&vault, &operators_for_update)
                .await?;
        }

        for (i, operator_root) in test_ncn.operators.iter().enumerate() {
            if operators_to_skip_indexes.contains(&i) {
                continue;
            }
            let operator = operator_root.operator_pubkey;

            let operator_snapshot = ncn_program_client
                .get_operator_snapshot(operator, ncn)
                .await?;

            if !operator_snapshot.is_active() {
                continue;
            }

            ncn_program_client
                .do_snapshot_vault_operator_delegation(vault, operator, ncn)
                .await?;
        }

        // Operator 1 signs with a 2-epoch-old stake view
        let result = fixture
            .verify_certificate_for_test_ncn(&test_ncn, TEST_DIGEST, vec![])
            .await;

        assert_ncn_program_error(result, NCNProgramError::OperatorSnapshotOutdated, Some(1));

        Ok(())
    }

    #[tokio::test]
    async fn test_verify_certificate_outdated_pass_if_not_signer() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;
        let mut ncn_program_client = fixture.ncn_program_client();
        let mut vault_program_client = fixture.vault_program_client();
        let test_ncn = fixture.create_initial_test_ncn(10, None).await?;

        ///// NCNProgram Setup /////
        fixture.warp_slot_incremental(1000).await?;
        fixture
            .update_snapshot_test_ncn_new_epoch(&test_ncn)
            .await?;
        //////

        fixture.warp_epoch_incremental(2).await?;

        let clock = fixture.clock().await;
        let slot = clock.slot;
        let ncn = test_ncn.ncn_root.ncn_pubkey;

        let operators_to_skip_indexes = vec![1, 9];

        let operators_for_update = test_ncn
            .operators
            .iter()
            .map(|operator_root| operator_root.operator_pubkey)
            .collect::<Vec<Pubkey>>();
        let vault = test_ncn.vaults[0].vault_pubkey;

        let vault_is_update_needed = vault_program_client
            .get_vault_is_update_needed(&vault, slot)
            .await?;

        if vault_is_update_needed {
            vault_program_client
                .do_full_vault_update(&vault, &operators_for_update)
                .await?;
        }

        for (i, operator_root) in test_ncn.operators.iter().enumerate() {
            if operators_to_skip_indexes.contains(&i) {
                // Skip the operator that is not signing
                continue;
            }
            let operator = operator_root.operator_pubkey;

            let operator_snapshot = ncn_program_client
                .get_operator_snapshot(operator, ncn)
                .await?;

            // If operator snapshot is finalized, we should not take more snapshots, it is
            if !operator_snapshot.is_active() {
                continue;
            }

            ncn_program_client
                .do_snapshot_vault_operator_delegation(vault, operator, ncn)
                .await?;
        }

        fixture
            .verify_certificate_for_test_ncn(&test_ncn, TEST_DIGEST, operators_to_skip_indexes)
            .await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_verify_certificate_operator_below_threshold_pass_if_not_signer() -> TestResult<()>
    {
        let mut fixture = TestBuilder::new().await;
        let mut vault_client = fixture.vault_client();

        let test_ncn = fixture.create_initial_test_ncn(10, None).await?;

        ///// NCNProgram Setup /////
        fixture.warp_slot_incremental(1000).await?;
        fixture
            .update_snapshot_test_ncn_new_epoch(&test_ncn)
            .await?;
        //////

        // Remove stake from one operator to get it to below minimum stake
        let operator_index = 5;
        let operator_root = &test_ncn.operators[operator_index];

        vault_client
            .do_cooldown_delegation(&test_ncn.vaults[0], &operator_root.operator_pubkey, 99)
            .await?;

        fixture.warp_epoch_incremental(2).await?;

        fixture
            .update_snapshot_test_ncn_new_epoch(&test_ncn)
            .await?;
        let none_signers_indecies: Vec<usize> = vec![operator_index];
        fixture
            .verify_certificate_for_test_ncn(&test_ncn, TEST_DIGEST, none_signers_indecies)
            .await?;

        Ok(())
    }

    #[ignore = "takes too long"]
    #[tokio::test]
    async fn test_verify_certificate_multiple_signers_max_limits() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;
        let mut ncn_program_client = fixture.ncn_program_client();

        let test_ncn = fixture.create_initial_test_ncn(MAX_OPERATORS, None).await?;

        ///// NCNProgram Setup /////
        fixture.warp_slot_incremental(1000).await?;
        fixture
            .update_snapshot_test_ncn_new_epoch(&test_ncn)
            .await?;
        //////

        let ncn = test_ncn.ncn_root.ncn_pubkey;

        let none_signers_indecies = get_random_none_signers_indecies(test_ncn.operators.len(), 85);
        let (agg_sig, apk2, signers_bitmap) =
            build_certificate(&test_ncn, TEST_DIGEST, &none_signers_indecies);

        let expected_generation = ncn_program_client.get_snapshot(ncn).await?.generation();

        ncn_program_client
            .do_verify_certificate(
                ncn,
                TEST_DIGEST,
                agg_sig,
                apk2,
                signers_bitmap,
                expected_generation,
            )
            .await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_verify_certificate_passing_wrong_bitmap() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;
        let mut ncn_program_client = fixture.ncn_program_client();

        let test_ncn = fixture.create_initial_test_ncn(10, None).await?;

        ///// NCNProgram Setup /////
        fixture.warp_slot_incremental(1000).await?;
        fixture
            .update_snapshot_test_ncn_new_epoch(&test_ncn)
            .await?;
        //////

        let ncn = test_ncn.ncn_root.ncn_pubkey;

        let none_signers_indecies = get_random_none_signers_indecies(test_ncn.operators.len(), 2);
        let (agg_sig, apk2, _correct_bitmap) =
            build_certificate(&test_ncn, TEST_DIGEST, &none_signers_indecies);

        // create a wrong bitmap
        let wrong_none_signers_indecies =
            get_random_none_signers_indecies(test_ncn.operators.len(), 3);
        let wrong_signers_bitmap =
            create_signer_bitmap(&wrong_none_signers_indecies, test_ncn.operators.len());

        let expected_generation = ncn_program_client.get_snapshot(ncn).await?.generation();

        let result = ncn_program_client
            .do_verify_certificate(
                ncn,
                TEST_DIGEST,
                agg_sig,
                apk2,
                wrong_signers_bitmap,
                expected_generation,
            )
            .await;

        assert_ncn_program_error(
            result,
            NCNProgramError::SignatureVerificationFailed,
            Some(1),
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_verify_certificate_invalid_signature_fails() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;
        let mut ncn_program_client = fixture.ncn_program_client();

        let test_ncn = fixture.create_initial_test_ncn(10, None).await?;

        ///// NCNProgram Setup /////
        fixture.warp_slot_incremental(1000).await?;
        fixture
            .update_snapshot_test_ncn_new_epoch(&test_ncn)
            .await?;
        //////

        let ncn = test_ncn.ncn_root.ncn_pubkey;

        // Use correct operator key but create invalid signature
        let operator_key = test_ncn.operators[0].bn128_privkey;
        let apk2 = G2CompressedPoint::try_from(&operator_key).unwrap().0;

        // Create an invalid signature (just random bytes)
        let agg_sig = [1u8; 32]; // Invalid signature

        let none_signers_indecies = get_random_none_signers_indecies(test_ncn.operators.len(), 0);
        // all have signed in the bitmap
        let signers_bitmap = create_signer_bitmap(&none_signers_indecies, test_ncn.operators.len());

        let expected_generation = ncn_program_client.get_snapshot(ncn).await?.generation();

        let result = ncn_program_client
            .do_verify_certificate(
                ncn,
                TEST_DIGEST,
                agg_sig,
                apk2,
                signers_bitmap,
                expected_generation,
            )
            .await;

        assert_ncn_program_error(
            result,
            NCNProgramError::SignatureVerificationFailed,
            Some(1),
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_verify_certificate_invalid_bitmap_size_fails() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;
        let mut ncn_program_client = fixture.ncn_program_client();

        let test_ncn = fixture.create_initial_test_ncn(1, None).await?;

        ///// NCNProgram Setup /////
        fixture.warp_slot_incremental(1000).await?;
        fixture
            .update_snapshot_test_ncn_new_epoch(&test_ncn)
            .await?;
        //////

        let ncn = test_ncn.ncn_root.ncn_pubkey;

        let operator_key = test_ncn.operators[0].bn128_privkey;
        let signature = operator_key
            .sign::<Sha256Normalized>(&MessageDigest(TEST_DIGEST))
            .unwrap();
        let agg_sig = G1CompressedPoint::try_from(signature).unwrap().0;
        let apk2 = G2CompressedPoint::try_from(&operator_key).unwrap().0;

        // Wrong bitmap size - should be 1 byte for 1 operator, but provide 2 bytes
        let signers_bitmap = vec![0u8; 2];

        let expected_generation = ncn_program_client.get_snapshot(ncn).await?.generation();

        let result = ncn_program_client
            .do_verify_certificate(
                ncn,
                TEST_DIGEST,
                agg_sig,
                apk2,
                signers_bitmap,
                expected_generation,
            )
            .await;

        assert_ncn_program_error(result, NCNProgramError::InvalidInputLength, Some(1));

        Ok(())
    }

    #[tokio::test]
    async fn test_verify_certificate_wrong_digest_fails() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;
        let mut ncn_program_client = fixture.ncn_program_client();

        let test_ncn = fixture.create_initial_test_ncn(10, None).await?;

        ///// NCNProgram Setup /////
        fixture.warp_slot_incremental(1000).await?;
        fixture
            .update_snapshot_test_ncn_new_epoch(&test_ncn)
            .await?;
        //////

        let ncn = test_ncn.ncn_root.ncn_pubkey;

        // Sign one digest but submit the certificate for a different one
        let signed_digest = solana_nostd_sha256::hashv(&[b"the digest that was signed"]);
        let submitted_digest = solana_nostd_sha256::hashv(&[b"a different digest"]);

        let none_signers_indecies = get_random_none_signers_indecies(test_ncn.operators.len(), 3);
        let (agg_sig, apk2, signers_bitmap) =
            build_certificate(&test_ncn, signed_digest, &none_signers_indecies);

        let expected_generation = ncn_program_client.get_snapshot(ncn).await?.generation();

        let result = ncn_program_client
            .do_verify_certificate(
                ncn,
                submitted_digest,
                agg_sig,
                apk2,
                signers_bitmap,
                expected_generation,
            )
            .await;

        assert_ncn_program_error(
            result,
            NCNProgramError::SignatureVerificationFailed,
            Some(1),
        );

        Ok(())
    }
}
