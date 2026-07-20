#[cfg(test)]
mod tests {

    use ncn_program_core::g1_point::{G1CompressedPoint, G1Point};
    use solana_sdk::msg;

    use crate::fixtures::{test_builder::TestBuilder, TestResult};

    #[tokio::test]
    async fn test_snapshot_vault_operator_delegation() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;

        let mut ncn_program_client = fixture.ncn_program_client();

        let test_ncn = fixture.create_initial_test_ncn(1, None).await?;

        fixture.warp_slot_incremental(1000).await?;

        let ncn = test_ncn.ncn_root.ncn_pubkey;

        let vault_root = test_ncn.vaults[0].clone();
        let vault_address = vault_root.vault_pubkey;

        let operator = test_ncn.operators[0].operator_pubkey;

        ncn_program_client
            .do_snapshot_vault_operator_delegation(vault_address, operator, ncn)
            .await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_snapshot_aggregates_the_right_g1_pubkey() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;

        let mut ncn_program_client = fixture.ncn_program_client();

        const OPERATORS: usize = 10;

        let test_ncn = fixture.create_initial_test_ncn(OPERATORS, None).await?;

        fixture.warp_slot_incremental(1000).await?;

        let ncn = test_ncn.ncn_root.ncn_pubkey;

        let vault_root = test_ncn.vaults[0].clone();
        let vault_address = vault_root.vault_pubkey;

        for operator_root in test_ncn.operators.iter() {
            ncn_program_client
                .do_snapshot_vault_operator_delegation(
                    vault_address,
                    operator_root.operator_pubkey,
                    ncn,
                )
                .await?;
        }

        // Verify that the snapshot aggregates the G1 pubkeys correctly
        let mut g1_pubkeys = Vec::new();
        for operator_root in test_ncn.operators.iter() {
            let g1_pubkey = G1Point::try_from(operator_root.bn128_privkey).unwrap();
            g1_pubkeys.push(g1_pubkey);
        }
        let agg_g1_pubkey = g1_pubkeys.into_iter().reduce(|acc, x| acc + x).unwrap();
        let agg_g1_pubkey_compressed = G1CompressedPoint::try_from(agg_g1_pubkey).unwrap();

        let snapshot = ncn_program_client.get_snapshot(ncn).await?;

        assert_eq!(
            snapshot.total_aggregated_g1_pubkey(),
            agg_g1_pubkey_compressed.0
        );

        Ok(())
    }

    #[tokio::test]
    async fn test_operator_snapshot_gets_updated_when_snapshotted() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;

        let mut ncn_program_client = fixture.ncn_program_client();

        let test_ncn = fixture.create_initial_test_ncn(1, None).await?;

        let ncn = test_ncn.ncn_root.ncn_pubkey;
        let operator = test_ncn.operators[0].operator_pubkey;
        let vault_root = test_ncn.vaults[0].clone();
        let vault_address = vault_root.vault_pubkey;

        {
            // Warp to new epoch
            fixture.warp_slot_incremental(1000).await?;
            let current_epoch = fixture.clock().await.epoch;
            println!("=== Testing Epoch: {} ===", current_epoch);

            // Get operator snapshot before delegation snapshot
            let operator_snapshot_before = ncn_program_client
                .get_operator_snapshot(operator, ncn)
                .await?;

            println!(
                "Before snapshot - Last snapshot slot: {}",
                operator_snapshot_before.last_snapshot_slot()
            );
            println!(
                "Before snapshot - Slot created: {}",
                operator_snapshot_before.slot_created()
            );
            println!(
                "Before snapshot - Stake weight: {}",
                operator_snapshot_before.stake_weight().stake_weight()
            );
            println!(
                "Before snapshot - Next epoch stake weight: {}",
                operator_snapshot_before
                    .next_epoch_stake_weight()
                    .stake_weight()
            );
            println!(
                "Before snapshot - Has minimum stake weight: {}",
                operator_snapshot_before.has_minimum_stake()
            );

            // Record the current slot before snapshot
            let current_slot = fixture.clock().await.slot;

            // Take the operator snapshot
            ncn_program_client
                .do_snapshot_vault_operator_delegation(vault_address, operator, ncn)
                .await?;

            // Get operator snapshot after delegation snapshot
            let snapshot_after = ncn_program_client.get_snapshot(ncn).await?;
            let operator_snapshot_after = ncn_program_client
                .get_operator_snapshot(operator, ncn)
                .await?;

            println!(
                "After snapshot - Last snapshot slot: {}",
                operator_snapshot_after.last_snapshot_slot()
            );
            println!(
                "After snapshot - Stake weight: {}",
                operator_snapshot_after.stake_weight().stake_weight()
            );
            println!(
                "After snapshot - Next epoch stake weight: {}",
                operator_snapshot_after
                    .next_epoch_stake_weight()
                    .stake_weight()
            );
            println!(
                "After snapshot - Has minimum stake weight: {}",
                operator_snapshot_after.has_minimum_stake()
            );

            // Verify that last_snapshot_slot was updated
            assert!(
                operator_snapshot_after.last_snapshot_slot()
                    > operator_snapshot_before.last_snapshot_slot(),
                "Last snapshot slot should be updated in epoch {}",
                current_epoch
            );

            // Verify that last_snapshot_slot is around the current slot
            assert!(
                operator_snapshot_after.last_snapshot_slot() >= current_slot,
                "Last snapshot slot should be at least the current slot in epoch {}",
                current_epoch
            );

            // Verify that stake weights were calculated and set
            assert!(
                operator_snapshot_after.stake_weight().stake_weight() > 0,
                "Stake weight should be greater than 0 after snapshot"
            );
            assert!(
                operator_snapshot_after
                    .next_epoch_stake_weight()
                    .stake_weight()
                    > 0,
                "Next epoch stake weight should be greater than 0 after snapshot"
            );

            // Verify operator registration count was updated properly
            assert_eq!(snapshot_after.operators_registered(), 1);

            // Verify that the operator snapshot can vote (has minimum stake weight and is active)
            assert!(
            operator_snapshot_after.has_minimum_stake()
                && operator_snapshot_after.is_active(),
            "Operator snapshot should be eligible to vote (has minimum stake weight and is active)"
            );

            println!("✅ Operator snapshot update verification completed!");
            println!(
                "   • Last snapshot slot: {} → {}",
                operator_snapshot_before.last_snapshot_slot(),
                operator_snapshot_after.last_snapshot_slot()
            );
            println!(
                "   • Stake weight: {} → {}",
                operator_snapshot_before.stake_weight().stake_weight(),
                operator_snapshot_after.stake_weight().stake_weight()
            );
            println!(
                "   • Has minimum stake: {} → {}",
                operator_snapshot_before.has_minimum_stake(),
                operator_snapshot_after.has_minimum_stake()
            );
        }

        {
            // Warp to new epoch
            fixture.warp_epoch_incremental(1).await?;
            fixture
                .update_snapshot_test_ncn_new_epoch(&test_ncn)
                .await?;

            let current_epoch = fixture.clock().await.epoch;
            println!("=== Testing Epoch: {} ===", current_epoch);

            // operator snapshot before adding more delegation
            let operator_snapshot_before = ncn_program_client
                .get_operator_snapshot(operator, ncn)
                .await?;

            fixture.warp_slot_incremental(100).await?;

            // Record the current slot before snapshot
            let current_slot = fixture.clock().await.slot;

            fixture.add_delegation_in_test_ncn(&test_ncn, 1000).await?;
            fixture
                .add_vault_operator_delegation_snapshots_to_test_ncn(&test_ncn)
                .await?;

            // Get operator snapshot after adding more delegation
            let snapshot_after = ncn_program_client.get_snapshot(ncn).await?;
            let operator_snapshot_after = ncn_program_client
                .get_operator_snapshot(operator, ncn)
                .await?;

            println!(
                "After snapshot - Last snapshot slot: {}",
                operator_snapshot_after.last_snapshot_slot()
            );
            println!(
                "After snapshot - Stake weight: {}",
                operator_snapshot_after.stake_weight().stake_weight()
            );
            println!(
                "After snapshot - Next epoch stake weight: {}",
                operator_snapshot_after
                    .next_epoch_stake_weight()
                    .stake_weight()
            );
            println!(
                "After snapshot - Has minimum stake weight: {}",
                operator_snapshot_after.has_minimum_stake()
            );

            // Verify that last_snapshot_slot was updated
            assert!(
                operator_snapshot_after.last_snapshot_slot()
                    > operator_snapshot_before.last_snapshot_slot(),
                "Last snapshot slot should be updated in epoch {}",
                current_epoch
            );

            // Verify that last_snapshot_slot is around the current slot
            assert!(
                operator_snapshot_after.last_snapshot_slot() >= current_slot,
                "Last snapshot slot should be at least the current slot in epoch {}",
                current_epoch
            );

            msg!("Last snapshot slot: {}", operator_snapshot_after);

            msg!("Last snapshot slot before: {}", operator_snapshot_before);

            // Verify that stake weights were calculated and set
            assert!(
                operator_snapshot_after.stake_weight().stake_weight()
                    > operator_snapshot_before.stake_weight().stake_weight(),
                "Stake weight should be greater than 0 after snapshot"
            );
            assert!(
                operator_snapshot_after
                    .next_epoch_stake_weight()
                    .stake_weight()
                    > 0,
                "Next epoch stake weight should be greater than 0 after snapshot"
            );

            // Verify operator registration count was updated properly
            assert_eq!(snapshot_after.operators_registered(), 1);

            // Verify that the operator snapshot can vote (has minimum stake weight and is active)
            assert!(
            operator_snapshot_after.has_minimum_stake()
                && operator_snapshot_after.is_active(),
            "Operator snapshot should be eligible to vote (has minimum stake weight and is active)"
            );

            println!("✅ Operator snapshot update verification completed!");
            println!(
                "   • Last snapshot slot: {} → {}",
                operator_snapshot_before.last_snapshot_slot(),
                operator_snapshot_after.last_snapshot_slot()
            );
            println!(
                "   • Stake weight: {} → {}",
                operator_snapshot_before.stake_weight().stake_weight(),
                operator_snapshot_after.stake_weight().stake_weight()
            );
            println!(
                "   • Has minimum stake: {} → {}",
                operator_snapshot_before.has_minimum_stake(),
                operator_snapshot_after.has_minimum_stake()
            );
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_operator_snapshot_delegation_next_epoch_calculations() -> TestResult<()> {
        const INITIAL_DELEGATION: u128 = 1000;
        const MINIMUM_STAKE_WEIGHT: u128 = 10000;
        const DELEGATION_TO_ADD: u128 = MINIMUM_STAKE_WEIGHT;
        const DELEGATION_TO_REMOVE: u128 = MINIMUM_STAKE_WEIGHT / 2;
        const DELEGATION_AFTER_ADDING: u128 = INITIAL_DELEGATION + DELEGATION_TO_ADD;
        const DELEGATION_AFTER_COOLDOWN: u128 = DELEGATION_AFTER_ADDING - DELEGATION_TO_REMOVE;

        let mut fixture = TestBuilder::new().await;
        let mut vault_client = fixture.vault_client();

        fixture.initialize_restaking_and_vault_programs().await?;

        // NCN prep
        let mut test_ncn = fixture.create_test_ncn().await?;
        let mut ncn_program_client = fixture.ncn_program_client();
        ncn_program_client
            .do_initialize_config(
                test_ncn.ncn_root.ncn_pubkey,
                &test_ncn.ncn_root.ncn_admin,
                Some(MINIMUM_STAKE_WEIGHT),
            )
            .await?;

        ncn_program_client
            .do_full_initialize_vault_registry(test_ncn.ncn_root.ncn_pubkey)
            .await?;

        fixture
            .add_operators_to_test_ncn(&mut test_ncn, 1, None)
            .await?;
        fixture
            .add_vaults_to_test_ncn(&mut test_ncn, 1, None)
            .await?;
        fixture
            .add_delegation_in_test_ncn(&test_ncn, INITIAL_DELEGATION as u64)
            .await?;
        fixture.add_vault_registry_to_test_ncn(&test_ncn).await?;
        fixture.add_snapshot_to_test_ncn(&test_ncn).await?;
        fixture.register_operators_to_test_ncn(&test_ncn).await?;

        fixture
            .add_vault_operator_delegation_snapshots_to_test_ncn(&test_ncn)
            .await?;

        {
            // the operator does not have enough stake
            let snapshot = ncn_program_client
                .get_snapshot(test_ncn.ncn_root.ncn_pubkey)
                .await?;
            let operator_snapshot = snapshot.get_operator_snapshot(0).unwrap();

            assert_eq!(
                operator_snapshot.stake_weight().stake_weight(),
                INITIAL_DELEGATION
            );
            assert_eq!(
                operator_snapshot.next_epoch_stake_weight().stake_weight(),
                INITIAL_DELEGATION
            );

            assert!(!operator_snapshot.has_minimum_stake());
            assert!(!operator_snapshot.has_minimum_stake_next_epoch());
            msg!("Snapshot before more delegation: {}", snapshot);
        }

        {
            // adding more delegation should takes the operator to over the minimum stake weight
            fixture
                .add_delegation_in_test_ncn(&test_ncn, MINIMUM_STAKE_WEIGHT as u64)
                .await?;
            fixture
                .add_vault_operator_delegation_snapshots_to_test_ncn(&test_ncn)
                .await?;
            let snapshot = ncn_program_client
                .get_snapshot(test_ncn.ncn_root.ncn_pubkey)
                .await?;
            let operator_snapshot = snapshot.get_operator_snapshot(0).unwrap();

            assert_eq!(
                operator_snapshot.stake_weight().stake_weight(),
                DELEGATION_AFTER_ADDING
            );
            assert_eq!(
                operator_snapshot.next_epoch_stake_weight().stake_weight(),
                DELEGATION_AFTER_ADDING
            );
            //
            assert!(operator_snapshot.has_minimum_stake());
            assert!(operator_snapshot.has_minimum_stake_next_epoch());
            msg!("Snapshot after delegation: {}", snapshot);
        }

        {
            // cooling down some of the delegation should only take effect in the next epoch
            vault_client
                .do_cooldown_delegation(
                    &test_ncn.vaults[0],
                    &test_ncn.operators[0].operator_pubkey,
                    DELEGATION_TO_REMOVE as u64,
                )
                .await?;

            fixture
                .add_vault_operator_delegation_snapshots_to_test_ncn(&test_ncn)
                .await?;
            let snapshot = ncn_program_client
                .get_snapshot(test_ncn.ncn_root.ncn_pubkey)
                .await?;
            let operator_snapshot = snapshot.get_operator_snapshot(0).unwrap();

            assert_eq!(
                operator_snapshot.stake_weight().stake_weight(),
                DELEGATION_AFTER_ADDING
            );
            assert_eq!(
                operator_snapshot.next_epoch_stake_weight().stake_weight(),
                DELEGATION_AFTER_ADDING
            );
            assert!(operator_snapshot.has_minimum_stake());
            assert!(operator_snapshot.has_minimum_stake_next_epoch());

            msg!("Snapshot after cooldown delegation: {}", snapshot);
        }

        fixture.warp_epoch_incremental(1).await?;
        fixture
            .update_snapshot_test_ncn_new_epoch(&test_ncn)
            .await?;

        {
            // the operator should still have the same stake weight, but next epoch stake weight should be updated
            let snapshot = ncn_program_client
                .get_snapshot(test_ncn.ncn_root.ncn_pubkey)
                .await?;
            let operator_snapshot = snapshot.get_operator_snapshot(0).unwrap();
            assert_eq!(
                operator_snapshot.stake_weight().stake_weight(),
                DELEGATION_AFTER_ADDING
            );
            assert_eq!(
                operator_snapshot.next_epoch_stake_weight().stake_weight(),
                DELEGATION_AFTER_COOLDOWN
            );
            assert!(operator_snapshot.has_minimum_stake());
            assert!(!operator_snapshot.has_minimum_stake_next_epoch());
            msg!("Snapshot after warpping one epoch: {}", snapshot);
        }

        fixture.warp_epoch_incremental(1).await?;
        fixture
            .update_snapshot_test_ncn_new_epoch(&test_ncn)
            .await?;

        {
            // after two epochs, the funds should be fully withdrawn from the operator
            let snapshot = ncn_program_client
                .get_snapshot(test_ncn.ncn_root.ncn_pubkey)
                .await?;
            let operator_snapshot = snapshot.get_operator_snapshot(0).unwrap();
            assert_eq!(
                operator_snapshot.stake_weight().stake_weight(),
                DELEGATION_AFTER_COOLDOWN
            );
            assert_eq!(
                operator_snapshot.next_epoch_stake_weight().stake_weight(),
                DELEGATION_AFTER_COOLDOWN
            );
            assert!(!operator_snapshot.has_minimum_stake());
            assert!(!operator_snapshot.has_minimum_stake_next_epoch());
            msg!("Snapshot after warpping two epochs: {}", snapshot);
        }

        Ok(())
    }
}
