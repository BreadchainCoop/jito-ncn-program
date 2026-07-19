#[cfg(test)]
mod tests {
    use ncn_program_core::{
        constants::MAX_OPERATORS,
        error::NCNProgramError,
        g1_point::{G1CompressedPoint, G1Point},
        g2_point::{G2CompressedPoint, G2Point},
        schemes::{MessageDigest, Sha256Normalized},
        utils::create_signer_bitmap,
    };
    use rand::Rng;
    use solana_sdk::pubkey::Pubkey;
    use std::collections::HashSet;

    use crate::fixtures::{
        ncn_program_client::assert_ncn_program_error, test_builder::TestBuilder, TestResult,
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

    #[tokio::test]
    async fn test_cast_vote_multiple_signers() -> TestResult<()> {
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

        // Get the current vote counter to use as the message
        let vote_counter = ncn_program_client.get_vote_counter(ncn).await.unwrap();
        let current_count = vote_counter.count();

        // Create message from the current counter value (padded to 32 bytes)
        let count_bytes = current_count.to_le_bytes();
        let mut message = [0u8; 32];
        message[..8].copy_from_slice(&count_bytes);

        let none_signers_indecies = get_random_none_signers_indecies(test_ncn.operators.len(), 2); // Let's say these operators didn't sign

        let mut signitures: Vec<G1Point> = vec![];
        let mut apk2_pubkeys: Vec<G2Point> = vec![];
        for (i, operator) in test_ncn.operators.iter().enumerate() {
            if !none_signers_indecies.contains(&i) {
                apk2_pubkeys.push(operator.bn128_g2_pubkey);
                let signature = operator
                    .bn128_privkey
                    .sign::<Sha256Normalized>(&MessageDigest(message))
                    .unwrap();
                signitures.push(signature);
            }
        }

        let apk2 = apk2_pubkeys.into_iter().reduce(|acc, x| acc + x).unwrap();
        let apk2 = G2CompressedPoint::try_from(&apk2).unwrap().0;

        let agg_sig = signitures.into_iter().reduce(|acc, x| acc + x).unwrap();
        let agg_sig = G1CompressedPoint::try_from(agg_sig).unwrap().0;

        // Create signers bitmap - all operators signed (bit 0 = 0 means they signed)
        let signers_bitmap = create_signer_bitmap(&none_signers_indecies, test_ncn.operators.len());

        // print the signers_bitmap as a binary string
        let mut binary_string = String::new();
        for byte in signers_bitmap.clone() {
            binary_string.push_str(&format!("{:08b}", byte));
        }
        println!("signers_bitmap: {}", binary_string);
        println!("apk2: {:?}", apk2);

        ncn_program_client
            .do_cast_vote(ncn, agg_sig, apk2, signers_bitmap)
            .await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_cast_vote_not_enough_stake() -> TestResult<()> {
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
        let none_signers_indecies: Vec<usize> = vec![1, 9];
        let result = fixture
            .cast_vote_for_test_ncn(&test_ncn, none_signers_indecies)
            .await;

        assert_ncn_program_error(result, NCNProgramError::OperatorHasNoMinimumStake, Some(1));

        Ok(())
    }

    #[tokio::test]
    async fn test_cast_vote_not_enough_stake_cant_vote_next_epoch() -> TestResult<()> {
        // Even if you don't take the snapshot, if the operator has less than the minimum stake, it
        // should not be able to vote
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

            fixture.warp_epoch_incremental(1).await?;

            fixture
                .update_snapshot_test_ncn_new_epoch(&test_ncn)
                .await?;

            fixture.warp_epoch_incremental(1).await?;
        }
        let none_signers_indecies: Vec<usize> = vec![1, 9];

        let result = fixture
            .cast_vote_for_test_ncn(&test_ncn, none_signers_indecies)
            .await;

        assert_ncn_program_error(result, NCNProgramError::OperatorHasNoMinimumStake, Some(1));

        Ok(())
    }

    #[tokio::test]
    async fn test_cast_vote_operator_snapshot_outdated_error() -> TestResult<()> {
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
            .cast_vote_for_test_ncn(&test_ncn, none_signers_indecies)
            .await;

        assert_ncn_program_error(result, NCNProgramError::OperatorSnapshotOutdated, Some(1));

        Ok(())
    }

    #[tokio::test]
    async fn test_cast_vote_operator_snapshot_outdated_pass_if_not_signer() -> TestResult<()> {
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
        let epoch = clock.epoch;
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
            .cast_vote_for_test_ncn(&test_ncn, operators_to_skip_indexes)
            .await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_cast_vote_operator_below_threshold_pass_if_not_signer() -> TestResult<()> {
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
            .cast_vote_for_test_ncn(&test_ncn, none_signers_indecies)
            .await?;

        Ok(())
    }

    #[ignore = "takes too long"]
    #[tokio::test]
    async fn test_cast_vote_multiple_signers_max_limits() -> TestResult<()> {
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

        // Get the current vote counter to use as the message
        let vote_counter = ncn_program_client.get_vote_counter(ncn).await.unwrap();
        let current_count = vote_counter.count();

        // Create message from the current counter value (padded to 32 bytes)
        let count_bytes = current_count.to_le_bytes();
        let mut message = [0u8; 32];
        message[..8].copy_from_slice(&count_bytes);

        let none_signers_indecies = get_random_none_signers_indecies(test_ncn.operators.len(), 85); // Let's say these operators didn't sign

        let mut signitures: Vec<G1Point> = vec![];
        let mut apk2_pubkeys: Vec<G2Point> = vec![];
        for (i, operator) in test_ncn.operators.iter().enumerate() {
            if !none_signers_indecies.contains(&i) {
                apk2_pubkeys.push(operator.bn128_g2_pubkey);
                let signature = operator
                    .bn128_privkey
                    .sign::<Sha256Normalized>(&MessageDigest(message))
                    .unwrap();
                signitures.push(signature);
            }
        }

        let apk2 = apk2_pubkeys.into_iter().reduce(|acc, x| acc + x).unwrap();
        let apk2 = G2CompressedPoint::try_from(&apk2).unwrap().0;

        let agg_sig = signitures.into_iter().reduce(|acc, x| acc + x).unwrap();
        let agg_sig = G1CompressedPoint::try_from(agg_sig).unwrap().0;

        // Create signers bitmap - all operators signed (bit 0 = 0 means they signed)
        let signers_bitmap = create_signer_bitmap(&none_signers_indecies, test_ncn.operators.len());

        // print the signers_bitmap as a binary string
        let mut binary_string = String::new();
        for byte in signers_bitmap.clone() {
            binary_string.push_str(&format!("{:08b}", byte));
        }
        println!("signers_bitmap: {}", binary_string);
        println!("apk2: {:?}", apk2);

        ncn_program_client
            .do_cast_vote(ncn, agg_sig, apk2, signers_bitmap)
            .await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_cast_vote_multiple_signers_passing_wrong_bitmap() -> TestResult<()> {
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

        // Get the current vote counter to use as the message
        let vote_counter = ncn_program_client.get_vote_counter(ncn).await.unwrap();
        let current_count = vote_counter.count();

        // Create message from the current counter value (padded to 32 bytes)
        let count_bytes = current_count.to_le_bytes();
        let mut message = [0u8; 32];
        message[..8].copy_from_slice(&count_bytes);

        let none_signers_indecies = get_random_none_signers_indecies(test_ncn.operators.len(), 2); // Let's say these operators didn't sign

        let mut signitures: Vec<G1Point> = vec![];
        let mut apk2_pubkeys: Vec<G2Point> = vec![];
        for (i, operator) in test_ncn.operators.iter().enumerate() {
            if !none_signers_indecies.contains(&i) {
                apk2_pubkeys.push(operator.bn128_g2_pubkey);
                let signature = operator
                    .bn128_privkey
                    .sign::<Sha256Normalized>(&MessageDigest(message))
                    .unwrap();
                signitures.push(signature);
            }
        }

        let apk2 = apk2_pubkeys.into_iter().reduce(|acc, x| acc + x).unwrap();
        let apk2 = G2CompressedPoint::try_from(&apk2).unwrap().0;

        let agg_sig = signitures.into_iter().reduce(|acc, x| acc + x).unwrap();
        let agg_sig = G1CompressedPoint::try_from(agg_sig).unwrap().0;

        // create a wrong bitmap
        let wrong_none_signers_indecies =
            get_random_none_signers_indecies(test_ncn.operators.len(), 3); // Let's say these operators didn't sign
        let wrong_signers_bitmap =
            create_signer_bitmap(&wrong_none_signers_indecies, test_ncn.operators.len());

        // print the signers_bitmap as a binary string
        let mut binary_string = String::new();
        for byte in wrong_signers_bitmap.clone() {
            binary_string.push_str(&format!("{:08b}", byte));
        }

        let result = ncn_program_client
            .do_cast_vote(ncn, agg_sig, apk2, wrong_signers_bitmap)
            .await;

        assert_ncn_program_error(
            result,
            NCNProgramError::SignatureVerificationFailed,
            Some(1),
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_cast_vote_invalid_signature_fails() -> TestResult<()> {
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

        // Get the current vote counter to use as the message
        let vote_counter = ncn_program_client.get_vote_counter(ncn).await.unwrap();
        let current_count = vote_counter.count();

        // Create message from the current counter value (padded to 32 bytes)
        let count_bytes = current_count.to_le_bytes();
        let mut message = [0u8; 32];
        message[..8].copy_from_slice(&count_bytes);

        // Use correct operator key but create invalid signature
        let operator_key = test_ncn.operators[0].bn128_privkey;
        let apk2 = G2CompressedPoint::try_from(&operator_key).unwrap().0;

        // Create an invalid signature (just random bytes)
        let agg_sig = [1u8; 32]; // Invalid signature

        let none_signers_indecies = get_random_none_signers_indecies(test_ncn.operators.len(), 0);
        // all have signed in the bitmap
        let signers_bitmap = create_signer_bitmap(&none_signers_indecies, test_ncn.operators.len());

        let result = ncn_program_client
            .do_cast_vote(ncn, agg_sig, apk2, signers_bitmap)
            .await;

        assert_ncn_program_error(
            result,
            NCNProgramError::SignatureVerificationFailed,
            Some(1),
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_cast_vote_invalid_bitmap_size_fails() -> TestResult<()> {
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

        // Get the current vote counter to use as the message
        let vote_counter = ncn_program_client.get_vote_counter(ncn).await.unwrap();
        let current_count = vote_counter.count();

        // Create message from the current counter value (padded to 32 bytes)
        let count_bytes = current_count.to_le_bytes();
        let mut message = [0u8; 32];
        message[..8].copy_from_slice(&count_bytes);

        let operator_key = test_ncn.operators[0].bn128_privkey;
        let signature = operator_key
            .sign::<Sha256Normalized>(&MessageDigest(message))
            .unwrap();
        let agg_sig = G1CompressedPoint::try_from(signature).unwrap().0;
        let apk2 = G2CompressedPoint::try_from(&operator_key).unwrap().0;

        // Wrong bitmap size - should be 1 byte for 1 operator, but provide 2 bytes
        let signers_bitmap = vec![0u8; 2];

        let result = ncn_program_client
            .do_cast_vote(ncn, agg_sig, apk2, signers_bitmap)
            .await;

        assert_ncn_program_error(result, NCNProgramError::InvalidInputLength, Some(1));

        Ok(())
    }

    #[tokio::test]
    async fn test_cast_vote_invalid_message_fails() -> TestResult<()> {
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

        // the right message is the current vote counter
        let wrong_message = solana_nostd_sha256::hashv(&[b"wrong message"]);

        let none_signers_indecies = get_random_none_signers_indecies(test_ncn.operators.len(), 3); // Let's say these operators didn't sign

        let mut signitures: Vec<G1Point> = vec![];
        let mut apk2_pubkeys: Vec<G2Point> = vec![];
        for (i, operator) in test_ncn.operators.iter().enumerate() {
            if !none_signers_indecies.contains(&i) {
                apk2_pubkeys.push(operator.bn128_g2_pubkey);
                let signature = operator
                    .bn128_privkey
                    .sign::<Sha256Normalized>(&MessageDigest(wrong_message))
                    .unwrap();
                signitures.push(signature);
            }
        }

        let apk2 = apk2_pubkeys.into_iter().reduce(|acc, x| acc + x).unwrap();
        let apk2 = G2CompressedPoint::try_from(&apk2).unwrap().0;

        let agg_sig = signitures.into_iter().reduce(|acc, x| acc + x).unwrap();
        let agg_sig = G1CompressedPoint::try_from(agg_sig).unwrap().0;

        let signers_bitmap = create_signer_bitmap(&none_signers_indecies, test_ncn.operators.len());

        let result = ncn_program_client
            .do_cast_vote(ncn, agg_sig, apk2, signers_bitmap)
            .await;

        assert_ncn_program_error(
            result,
            NCNProgramError::SignatureVerificationFailed,
            Some(1),
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_cast_vote_counter_advancement() -> TestResult<()> {
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

        // Check initial counter value
        let initial_vote_counter = ncn_program_client.get_vote_counter(ncn).await.unwrap();
        let initial_count = initial_vote_counter.count();
        println!("Initial vote counter: {}", initial_count);

        // Cast first vote
        {
            // Get the current vote counter to use as the message
            let vote_counter = ncn_program_client.get_vote_counter(ncn).await.unwrap();
            let current_count = vote_counter.count();

            // Create message from the current counter value (padded to 32 bytes)
            let count_bytes = current_count.to_le_bytes();
            let mut message = [0u8; 32];
            message[..8].copy_from_slice(&count_bytes);

            let none_signers_indices = vec![]; // All operators sign

            let mut signatures: Vec<G1Point> = vec![];
            let mut apk2_pubkeys: Vec<G2Point> = vec![];
            for operator in test_ncn.operators.iter() {
                apk2_pubkeys.push(operator.bn128_g2_pubkey);
                let signature = operator
                    .bn128_privkey
                    .sign::<Sha256Normalized>(&MessageDigest(message))
                    .unwrap();
                signatures.push(signature);
            }

            let apk2 = apk2_pubkeys.into_iter().reduce(|acc, x| acc + x).unwrap();
            let apk2 = G2CompressedPoint::try_from(&apk2).unwrap().0;

            let agg_sig = signatures.into_iter().reduce(|acc, x| acc + x).unwrap();
            let agg_sig = G1CompressedPoint::try_from(agg_sig).unwrap().0;

            let signers_bitmap =
                create_signer_bitmap(&none_signers_indices, test_ncn.operators.len());

            ncn_program_client
                .do_cast_vote(ncn, agg_sig, apk2, signers_bitmap)
                .await?;
        }

        // Check counter after first vote
        let first_vote_counter = ncn_program_client.get_vote_counter(ncn).await.unwrap();
        let first_count = first_vote_counter.count();
        println!("Counter after first vote: {}", first_count);
        assert_eq!(
            first_count,
            initial_count + 1,
            "Counter should increment by 1 after first vote"
        );

        // Cast second vote
        {
            // Get the updated vote counter to use as the message
            let vote_counter = ncn_program_client.get_vote_counter(ncn).await.unwrap();
            let current_count = vote_counter.count();

            // Create message from the current counter value (padded to 32 bytes)
            let count_bytes = current_count.to_le_bytes();
            let mut message = [0u8; 32];
            message[..8].copy_from_slice(&count_bytes);

            let none_signers_indices = vec![1]; // One operator doesn't sign this time

            let mut signatures: Vec<G1Point> = vec![];
            let mut apk2_pubkeys: Vec<G2Point> = vec![];
            for (i, operator) in test_ncn.operators.iter().enumerate() {
                if !none_signers_indices.contains(&i) {
                    apk2_pubkeys.push(operator.bn128_g2_pubkey);
                    let signature = operator
                        .bn128_privkey
                        .sign::<Sha256Normalized>(&MessageDigest(message))
                        .unwrap();
                    signatures.push(signature);
                }
            }

            let apk2 = apk2_pubkeys.into_iter().reduce(|acc, x| acc + x).unwrap();
            let apk2 = G2CompressedPoint::try_from(&apk2).unwrap().0;

            let agg_sig = signatures.into_iter().reduce(|acc, x| acc + x).unwrap();
            let agg_sig = G1CompressedPoint::try_from(agg_sig).unwrap().0;

            let signers_bitmap =
                create_signer_bitmap(&none_signers_indices, test_ncn.operators.len());

            ncn_program_client
                .do_cast_vote(ncn, agg_sig, apk2, signers_bitmap)
                .await?;
        }

        // Check counter after second vote
        let second_vote_counter = ncn_program_client.get_vote_counter(ncn).await.unwrap();
        let second_count = second_vote_counter.count();
        println!("Counter after second vote: {}", second_count);
        assert_eq!(
            second_count,
            first_count + 1,
            "Counter should increment by 1 after second vote"
        );
        assert_eq!(
            second_count,
            initial_count + 2,
            "Total counter should be initial + 2"
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_cast_vote_duplicate_signature_fails() -> TestResult<()> {
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

        // Cast first vote successfully
        {
            // Get the current vote counter to use as the message
            let vote_counter = ncn_program_client.get_vote_counter(ncn).await.unwrap();
            let current_count = vote_counter.count();

            // Create message from the current counter value (padded to 32 bytes)
            let count_bytes = current_count.to_le_bytes();
            let mut message = [0u8; 32];
            message[..8].copy_from_slice(&count_bytes);

            let none_signers_indices = vec![]; // All operators sign

            let mut signatures: Vec<G1Point> = vec![];
            let mut apk2_pubkeys: Vec<G2Point> = vec![];
            for operator in test_ncn.operators.iter() {
                apk2_pubkeys.push(operator.bn128_g2_pubkey);
                let signature = operator
                    .bn128_privkey
                    .sign::<Sha256Normalized>(&MessageDigest(message))
                    .unwrap();
                signatures.push(signature);
            }

            let apk2 = apk2_pubkeys.into_iter().reduce(|acc, x| acc + x).unwrap();
            let apk2 = G2CompressedPoint::try_from(&apk2).unwrap().0;

            let agg_sig = signatures.into_iter().reduce(|acc, x| acc + x).unwrap();
            let agg_sig = G1CompressedPoint::try_from(agg_sig).unwrap().0;

            let signers_bitmap =
                create_signer_bitmap(&none_signers_indices, test_ncn.operators.len());

            ncn_program_client
                .do_cast_vote(ncn, agg_sig, apk2, signers_bitmap)
                .await?;

            println!("First vote cast successfully");
        }

        // Attempt to submit the same signature again (should fail because counter has advanced)
        {
            // Use the OLD counter value (before the first vote)
            let old_count = 0u64; // This was the counter before the first vote
            let count_bytes = old_count.to_le_bytes();
            let mut old_message = [0u8; 32];
            old_message[..8].copy_from_slice(&count_bytes);

            let none_signers_indices = vec![]; // All operators sign

            let mut signatures: Vec<G1Point> = vec![];
            let mut apk2_pubkeys: Vec<G2Point> = vec![];
            for operator in test_ncn.operators.iter() {
                apk2_pubkeys.push(operator.bn128_g2_pubkey);
                // Sign the OLD message (same as first vote)
                let signature = operator
                    .bn128_privkey
                    .sign::<Sha256Normalized>(&MessageDigest(old_message))
                    .unwrap();
                signatures.push(signature);
            }

            let apk2 = apk2_pubkeys.into_iter().reduce(|acc, x| acc + x).unwrap();
            let apk2 = G2CompressedPoint::try_from(&apk2).unwrap().0;

            let agg_sig = signatures.into_iter().reduce(|acc, x| acc + x).unwrap();
            let agg_sig = G1CompressedPoint::try_from(agg_sig).unwrap().0;

            let signers_bitmap =
                create_signer_bitmap(&none_signers_indices, test_ncn.operators.len());

            // This should fail because the signatures are for the old counter value,
            // but the system will use the current counter value for verification
            let result = ncn_program_client
                .do_cast_vote(ncn, agg_sig, apk2, signers_bitmap)
                .await;

            assert_ncn_program_error(
                result,
                NCNProgramError::SignatureVerificationFailed,
                Some(1),
            );

            println!("Duplicate signature correctly rejected");
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_cast_vote_sequential_voting_with_counter_tracking() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;
        let mut ncn_program_client = fixture.ncn_program_client();

        let test_ncn = fixture.create_initial_test_ncn(3, None).await?;

        ///// NCNProgram Setup /////
        fixture.warp_slot_incremental(1000).await?;
        fixture
            .update_snapshot_test_ncn_new_epoch(&test_ncn)
            .await?;
        //////

        let ncn = test_ncn.ncn_root.ncn_pubkey;

        // Get initial counter
        let initial_counter = ncn_program_client.get_vote_counter(ncn).await.unwrap();
        let mut expected_count = initial_counter.count();
        println!("Initial counter: {}", expected_count);

        // Cast multiple votes and verify counter advancement
        for vote_round in 1..=5 {
            println!("\n--- Vote Round {} ---", vote_round);

            // Get current counter and prepare message
            let current_counter = ncn_program_client.get_vote_counter(ncn).await.unwrap();
            let current_count = current_counter.count();
            assert_eq!(
                current_count, expected_count,
                "Counter should match expected value before vote {}",
                vote_round
            );

            let count_bytes = current_count.to_le_bytes();
            let mut message = [0u8; 32];
            message[..8].copy_from_slice(&count_bytes);

            // Vary the signers for each round
            let none_signers_indices = if vote_round % 2 == 0 { vec![0] } else { vec![] };

            let mut signatures: Vec<G1Point> = vec![];
            let mut apk2_pubkeys: Vec<G2Point> = vec![];
            for (i, operator) in test_ncn.operators.iter().enumerate() {
                if !none_signers_indices.contains(&i) {
                    apk2_pubkeys.push(operator.bn128_g2_pubkey);
                    let signature = operator
                        .bn128_privkey
                        .sign::<Sha256Normalized>(&MessageDigest(message))
                        .unwrap();
                    signatures.push(signature);
                }
            }

            let apk2 = apk2_pubkeys.into_iter().reduce(|acc, x| acc + x).unwrap();
            let apk2 = G2CompressedPoint::try_from(&apk2).unwrap().0;

            let agg_sig = signatures.into_iter().reduce(|acc, x| acc + x).unwrap();
            let agg_sig = G1CompressedPoint::try_from(agg_sig).unwrap().0;

            let signers_bitmap =
                create_signer_bitmap(&none_signers_indices, test_ncn.operators.len());

            // Cast the vote
            ncn_program_client
                .do_cast_vote(ncn, agg_sig, apk2, signers_bitmap)
                .await?;

            // Verify counter was incremented
            expected_count += 1;
            let updated_counter = ncn_program_client.get_vote_counter(ncn).await.unwrap();
            let updated_count = updated_counter.count();
            assert_eq!(
                updated_count, expected_count,
                "Counter should be incremented after vote {}",
                vote_round
            );

            println!(
                "Vote {} successful. Counter: {} -> {}",
                vote_round, current_count, updated_count
            );
        }

        println!("\nTotal votes cast: 5");
        println!("Final counter value: {}", expected_count);
        println!("Counter advanced correctly through all votes!");

        Ok(())
    }

    #[tokio::test]
    async fn test_cast_vote_wrong_counter_message_fails() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;
        let mut ncn_program_client = fixture.ncn_program_client();

        let test_ncn = fixture.create_initial_test_ncn(3, None).await?;

        ///// NCNProgram Setup /////
        fixture.warp_slot_incremental(1000).await?;
        fixture
            .update_snapshot_test_ncn_new_epoch(&test_ncn)
            .await?;
        //////

        let ncn = test_ncn.ncn_root.ncn_pubkey;

        // Get current counter
        let current_counter = ncn_program_client.get_vote_counter(ncn).await.unwrap();
        let current_count = current_counter.count();
        println!("Current counter: {}", current_count);

        // Create signatures for a WRONG counter value (future value)
        let wrong_count = current_count + 10; // Use a future counter value
        let wrong_count_bytes = wrong_count.to_le_bytes();
        let mut wrong_message = [0u8; 32];
        wrong_message[..8].copy_from_slice(&wrong_count_bytes);

        let none_signers_indices = vec![];

        let mut signatures: Vec<G1Point> = vec![];
        let mut apk2_pubkeys: Vec<G2Point> = vec![];
        for operator in test_ncn.operators.iter() {
            apk2_pubkeys.push(operator.bn128_g2_pubkey);
            // Sign with the WRONG message
            let signature = operator
                .bn128_privkey
                .sign::<Sha256Normalized>(&MessageDigest(wrong_message))
                .unwrap();
            signatures.push(signature);
        }

        let apk2 = apk2_pubkeys.into_iter().reduce(|acc, x| acc + x).unwrap();
        let apk2 = G2CompressedPoint::try_from(&apk2).unwrap().0;

        let agg_sig = signatures.into_iter().reduce(|acc, x| acc + x).unwrap();
        let agg_sig = G1CompressedPoint::try_from(agg_sig).unwrap().0;

        let signers_bitmap = create_signer_bitmap(&none_signers_indices, test_ncn.operators.len());

        // This should fail because signatures are for wrong counter value
        let result = ncn_program_client
            .do_cast_vote(ncn, agg_sig, apk2, signers_bitmap)
            .await;

        assert_ncn_program_error(
            result,
            NCNProgramError::SignatureVerificationFailed,
            Some(1),
        );

        println!("Vote with wrong counter message correctly failed");

        Ok(())
    }
}
