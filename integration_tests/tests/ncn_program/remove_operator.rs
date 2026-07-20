#[cfg(test)]
mod tests {
    use ncn_program_core::{
        error::NCNProgramError,
        g1_point::{G1CompressedPoint, G1Point},
        g2_point::{G2CompressedPoint, G2Point},
        schemes::{MessageDigest, Sha256Normalized},
        utils::create_signer_bitmap,
    };
    use solana_sdk::signature::Keypair;

    use crate::fixtures::{
        ncn_program_client::assert_ncn_program_error,
        test_builder::{TestBuilder, TestNcn, TEST_DIGEST},
        TestResult,
    };

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
    async fn test_register_operator_bumps_generation() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;
        let mut ncn_program_client = fixture.ncn_program_client();

        const OPERATOR_COUNT: usize = 3;
        let test_ncn = fixture
            .create_initial_test_ncn(OPERATOR_COUNT, None)
            .await?;

        // Every registration bumps the generation once
        let snapshot = ncn_program_client
            .get_snapshot(test_ncn.ncn_root.ncn_pubkey)
            .await?;
        assert_eq!(snapshot.generation(), OPERATOR_COUNT as u64);

        Ok(())
    }

    #[tokio::test]
    async fn test_remove_operator_by_ncn_admin() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;
        let mut ncn_program_client = fixture.ncn_program_client();

        let test_ncn = fixture.create_initial_test_ncn(3, None).await?;
        let ncn = test_ncn.ncn_root.ncn_pubkey;

        let removed = &test_ncn.operators[1];

        let before = ncn_program_client.get_snapshot(ncn).await?;
        let generation_before = before.generation();
        let operators_registered_before = before.operators_registered();
        assert!(before
            .find_operator_snapshot(&removed.operator_pubkey)
            .is_some());

        ncn_program_client
            .do_remove_operator(ncn, removed.operator_pubkey, &test_ncn.ncn_root.ncn_admin)
            .await?;

        let after = ncn_program_client.get_snapshot(ncn).await?;

        // Generation bumped: in-flight certificates die with the old set
        assert_eq!(after.generation(), generation_before + 1);

        // Slot is tombstoned, not compacted: registered count is unchanged and
        // the operator is no longer findable
        assert_eq!(
            after.operators_registered(),
            operators_registered_before,
            "tombstoning must not reuse/compact indices"
        );
        assert!(after
            .find_operator_snapshot(&removed.operator_pubkey)
            .is_none());
        assert_eq!(after.operator_snapshots()[1].ncn_operator_index(), u64::MAX);

        // The removed key was subtracted from the running APK: total is now
        // exactly the sum of the two remaining keys
        let expected_apk = G1CompressedPoint::try_from(
            test_ncn.operators[0].bn128_g1_pubkey + test_ncn.operators[2].bn128_g1_pubkey,
        )
        .unwrap();
        assert_eq!(after.total_aggregated_g1_pubkey(), expected_apk.0);

        Ok(())
    }

    #[tokio::test]
    async fn test_remove_operator_by_operator_admin() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;
        let mut ncn_program_client = fixture.ncn_program_client();

        let test_ncn = fixture.create_initial_test_ncn(2, None).await?;
        let ncn = test_ncn.ncn_root.ncn_pubkey;

        let removed = &test_ncn.operators[0];
        let generation_before = ncn_program_client.get_snapshot(ncn).await?.generation();

        // The operator's own admin can remove it (voluntary exit)
        ncn_program_client
            .do_remove_operator(ncn, removed.operator_pubkey, &removed.operator_admin)
            .await?;

        let after = ncn_program_client.get_snapshot(ncn).await?;
        assert_eq!(after.generation(), generation_before + 1);
        assert!(after
            .find_operator_snapshot(&removed.operator_pubkey)
            .is_none());

        Ok(())
    }

    #[tokio::test]
    async fn test_remove_operator_unauthorized_fails() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;
        let mut ncn_program_client = fixture.ncn_program_client();

        let test_ncn = fixture.create_initial_test_ncn(2, None).await?;
        let ncn = test_ncn.ncn_root.ncn_pubkey;

        let intruder = Keypair::new();
        let result = ncn_program_client
            .do_remove_operator(ncn, test_ncn.operators[0].operator_pubkey, &intruder)
            .await;

        assert_ncn_program_error(result, NCNProgramError::CannotRemoveOperator, Some(0));

        // Nothing changed
        let snapshot = ncn_program_client.get_snapshot(ncn).await?;
        assert!(snapshot
            .find_operator_snapshot(&test_ncn.operators[0].operator_pubkey)
            .is_some());

        Ok(())
    }

    #[tokio::test]
    async fn test_remove_operator_twice_fails() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;
        let mut ncn_program_client = fixture.ncn_program_client();

        let test_ncn = fixture.create_initial_test_ncn(2, None).await?;
        let ncn = test_ncn.ncn_root.ncn_pubkey;
        let removed = &test_ncn.operators[1];

        ncn_program_client
            .do_remove_operator(ncn, removed.operator_pubkey, &test_ncn.ncn_root.ncn_admin)
            .await?;

        let result = ncn_program_client
            .do_remove_operator(ncn, removed.operator_pubkey, &test_ncn.ncn_root.ncn_admin)
            .await;

        assert_ncn_program_error(result, NCNProgramError::OperatorIsNotInSnapshot, Some(0));

        Ok(())
    }

    #[tokio::test]
    async fn test_certificate_race_removal_invalidates_in_flight_certificate() -> TestResult<()> {
        // The assembly-vs-submission race: a certificate assembled against
        // generation G must be rejected if an operator is removed (generation
        // bump) before it lands.
        let mut fixture = TestBuilder::new().await;
        let mut ncn_program_client = fixture.ncn_program_client();

        let test_ncn = fixture.create_initial_test_ncn(4, None).await?;

        fixture.warp_slot_incremental(1000).await?;
        fixture
            .update_snapshot_test_ncn_new_epoch(&test_ncn)
            .await?;

        let ncn = test_ncn.ncn_root.ncn_pubkey;

        // Assemble a full-quorum certificate against the current generation
        let generation_at_assembly = ncn_program_client.get_snapshot(ncn).await?.generation();
        let (agg_sig, apk2, signers_bitmap) = build_certificate(&test_ncn, TEST_DIGEST, &[]);

        // ... but before it lands, operator 2 is removed
        ncn_program_client
            .do_remove_operator(
                ncn,
                test_ncn.operators[2].operator_pubkey,
                &test_ncn.ncn_root.ncn_admin,
            )
            .await?;

        // The in-flight certificate must die with the old generation
        let result = ncn_program_client
            .do_verify_certificate(
                ncn,
                TEST_DIGEST,
                agg_sig,
                apk2,
                signers_bitmap,
                generation_at_assembly,
            )
            .await;
        assert_ncn_program_error(result, NCNProgramError::SnapshotGenerationMismatch, Some(1));

        // A certificate re-assembled against the new set verifies: the
        // tombstoned slot keeps its index (bit stays 0), the remaining three
        // operators carry 100% of the remaining stake
        let new_generation = ncn_program_client.get_snapshot(ncn).await?.generation();
        assert_eq!(new_generation, generation_at_assembly + 1);

        let (agg_sig, apk2, signers_bitmap) = build_certificate(&test_ncn, TEST_DIGEST, &[2]);
        ncn_program_client
            .do_verify_certificate(
                ncn,
                TEST_DIGEST,
                agg_sig,
                apk2,
                signers_bitmap,
                new_generation,
            )
            .await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_certificate_race_key_rotation_invalidates_in_flight_certificate() -> TestResult<()>
    {
        use ncn_program_core::{privkey::PrivKey, utils::pop_message_digest};

        let mut fixture = TestBuilder::new().await;
        let mut ncn_program_client = fixture.ncn_program_client();

        let test_ncn = fixture.create_initial_test_ncn(2, None).await?;

        fixture.warp_slot_incremental(1000).await?;
        fixture
            .update_snapshot_test_ncn_new_epoch(&test_ncn)
            .await?;

        let ncn = test_ncn.ncn_root.ncn_pubkey;
        let rotating = &test_ncn.operators[0];

        // Assemble a full-quorum certificate against the current generation
        let generation_at_assembly = ncn_program_client.get_snapshot(ncn).await?.generation();
        let (agg_sig, apk2, signers_bitmap) = build_certificate(&test_ncn, TEST_DIGEST, &[]);

        // Operator 0 rotates its BLS keys before the certificate lands
        let new_private_key = PrivKey::from_random();
        let new_g1_compressed = G1CompressedPoint::try_from(new_private_key).unwrap();
        let new_g2_compressed = G2CompressedPoint::try_from(&new_private_key).unwrap();
        let new_signature = new_private_key
            .sign::<Sha256Normalized>(&pop_message_digest(
                &ncn,
                &rotating.operator_pubkey,
                &new_g1_compressed.0,
            ))
            .unwrap();

        ncn_program_client
            .do_update_operator_bn128_keys(
                ncn,
                rotating.operator_pubkey,
                &rotating.operator_admin,
                new_g1_compressed.0,
                new_g2_compressed.0,
                new_signature.0,
            )
            .await?;

        // Key rotation bumps the generation
        let new_generation = ncn_program_client.get_snapshot(ncn).await?.generation();
        assert_eq!(new_generation, generation_at_assembly + 1);

        // The in-flight certificate signed with the OLD key must be rejected
        // at the generation gate (before any pairing runs)
        let result = ncn_program_client
            .do_verify_certificate(
                ncn,
                TEST_DIGEST,
                agg_sig,
                apk2,
                signers_bitmap,
                generation_at_assembly,
            )
            .await;
        assert_ncn_program_error(result, NCNProgramError::SnapshotGenerationMismatch, Some(1));

        Ok(())
    }
}
