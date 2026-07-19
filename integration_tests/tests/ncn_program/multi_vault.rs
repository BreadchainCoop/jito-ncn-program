#[cfg(test)]
mod tests {
    use jito_restaking_core::ncn_vault_ticket::NcnVaultTicket;
    use ncn_program_core::{constants::MAX_VAULTS, error::NCNProgramError};
    use solana_sdk::pubkey::Pubkey;

    use crate::fixtures::{
        ncn_program_client::assert_ncn_program_error,
        test_builder::{TestBuilder, TestNcn, TEST_DIGEST},
        TestResult,
    };

    /// Registers each vault's mint with the given weight (bps) and the vault
    /// itself, mirroring add_vault_registry_to_test_ncn but with per-vault
    /// weights.
    async fn register_vaults_with_weights(
        fixture: &mut TestBuilder,
        test_ncn: &TestNcn,
        weights_bps: &[u16],
    ) -> TestResult<()> {
        let mut ncn_program_client = fixture.ncn_program_client();
        let mut vault_client = fixture.vault_program_client();

        assert_eq!(test_ncn.vaults.len(), weights_bps.len());

        // Vaults must be updated for the current epoch before registration
        fixture.warp_epoch_incremental(2).await?;

        let ncn = test_ncn.ncn_root.ncn_pubkey;
        let operators = test_ncn
            .operators
            .iter()
            .map(|operator| operator.operator_pubkey)
            .collect::<Vec<Pubkey>>();

        for (vault_root, weight_bps) in test_ncn.vaults.iter().zip(weights_bps.iter()) {
            let vault = vault_root.vault_pubkey;

            vault_client
                .do_full_vault_update(&vault, &operators)
                .await?;

            let st_mint = vault_client.get_vault(&vault).await?.supported_mint;

            let ncn_vault_ticket =
                NcnVaultTicket::find_program_address(&jito_restaking_program::id(), &ncn, &vault).0;

            ncn_program_client
                .do_admin_register_st_mint_with_weight(ncn, st_mint, *weight_bps)
                .await?;

            ncn_program_client
                .do_register_vault(ncn, vault, ncn_vault_ticket)
                .await?;
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_multi_vault_weighted_stake_accumulation() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;
        let mut ncn_program_client = fixture.ncn_program_client();

        // Manual setup: 1 operator, 2 vaults (distinct mints), so we control
        // the per-mint weights: vault0 at full weight, vault1 at half weight.
        fixture.initialize_restaking_and_vault_programs().await?;
        let mut test_ncn = fixture.create_test_ncn().await?;
        ncn_program_client
            .do_initialize_config(
                test_ncn.ncn_root.ncn_pubkey,
                &test_ncn.ncn_root.ncn_admin,
                Some(10),
            )
            .await?;
        ncn_program_client
            .do_full_initialize_vault_registry(test_ncn.ncn_root.ncn_pubkey)
            .await?;

        fixture
            .add_operators_to_test_ncn(&mut test_ncn, 1, None)
            .await?;
        fixture
            .add_vaults_to_test_ncn(&mut test_ncn, 2, None)
            .await?;
        fixture.add_delegation_in_test_ncn(&test_ncn, 100).await?;

        register_vaults_with_weights(&mut fixture, &test_ncn, &[10_000, 5_000]).await?;

        let ncn = test_ncn.ncn_root.ncn_pubkey;

        ncn_program_client.do_full_initialize_snapshot(ncn).await?;
        fixture.register_operators_to_test_ncn(&test_ncn).await?;
        fixture
            .add_vault_operator_delegation_snapshots_to_test_ncn(&test_ncn)
            .await?;

        let operator = test_ncn.operators[0].operator_pubkey;
        let snapshot = ncn_program_client.get_snapshot(ncn).await?;
        let operator_snapshot = snapshot.find_operator_snapshot(&operator).unwrap();

        // 100 * 10_000/10_000 + 100 * 5_000/10_000 = 150
        assert_eq!(operator_snapshot.stake_weight().stake_weight(), 150);
        assert_eq!(
            operator_snapshot
                .vault_contributions()
                .iter()
                .filter(|c| !c.is_empty())
                .count(),
            2
        );

        // Re-cranking a vault replaces its contribution (no double count)
        ncn_program_client
            .do_snapshot_vault_operator_delegation(test_ncn.vaults[1].vault_pubkey, operator, ncn)
            .await?;

        let snapshot = ncn_program_client.get_snapshot(ncn).await?;
        let operator_snapshot = snapshot.find_operator_snapshot(&operator).unwrap();
        assert_eq!(operator_snapshot.stake_weight().stake_weight(), 150);
        assert_eq!(
            operator_snapshot
                .vault_contributions()
                .iter()
                .filter(|c| !c.is_empty())
                .count(),
            2
        );

        // A full-quorum certificate verifies over the weighted snapshot
        fixture
            .verify_certificate_for_test_ncn(&test_ncn, TEST_DIGEST, vec![])
            .await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_multi_vault_weighted_threshold_gating() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;
        let mut ncn_program_client = fixture.ncn_program_client();
        let mut vault_client = fixture.vault_program_client();

        // 2 operators, 2 vaults. Operator 0 carries its stake on the
        // full-weight vault, operator 1 on the half-weight vault:
        //   op0 = 100*1.0 + 1*0.5 (floors to 0) = 100
        //   op1 =   1*1.0 + 100*0.5           =  51
        // total = 151
        fixture.initialize_restaking_and_vault_programs().await?;
        let mut test_ncn = fixture.create_test_ncn().await?;
        ncn_program_client
            .do_initialize_config(
                test_ncn.ncn_root.ncn_pubkey,
                &test_ncn.ncn_root.ncn_admin,
                Some(10),
            )
            .await?;
        ncn_program_client
            .do_full_initialize_vault_registry(test_ncn.ncn_root.ncn_pubkey)
            .await?;

        fixture
            .add_operators_to_test_ncn(&mut test_ncn, 2, None)
            .await?;
        fixture
            .add_vaults_to_test_ncn(&mut test_ncn, 2, None)
            .await?;

        let op0 = test_ncn.operators[0].operator_pubkey;
        let op1 = test_ncn.operators[1].operator_pubkey;

        vault_client
            .do_add_delegation(&test_ncn.vaults[0], &op0, 100)
            .await?;
        vault_client
            .do_add_delegation(&test_ncn.vaults[0], &op1, 1)
            .await?;
        vault_client
            .do_add_delegation(&test_ncn.vaults[1], &op0, 1)
            .await?;
        vault_client
            .do_add_delegation(&test_ncn.vaults[1], &op1, 100)
            .await?;

        register_vaults_with_weights(&mut fixture, &test_ncn, &[10_000, 5_000]).await?;

        let ncn = test_ncn.ncn_root.ncn_pubkey;

        ncn_program_client.do_full_initialize_snapshot(ncn).await?;
        fixture.register_operators_to_test_ncn(&test_ncn).await?;
        fixture
            .add_vault_operator_delegation_snapshots_to_test_ncn(&test_ncn)
            .await?;

        // Confirm the weighted stakes
        let snapshot = ncn_program_client.get_snapshot(ncn).await?;
        assert_eq!(
            snapshot
                .find_operator_snapshot(&op0)
                .unwrap()
                .stake_weight()
                .stake_weight(),
            100
        );
        assert_eq!(
            snapshot
                .find_operator_snapshot(&op1)
                .unwrap()
                .stake_weight()
                .stake_weight(),
            51
        );

        // Threshold 6000 bps of 151 total = 90.6 -> op0 alone (100) passes,
        // op1 alone (51) fails
        ncn_program_client
            .do_set_parameters(
                None,
                None,
                None,
                None,
                None,
                Some(6_000),
                &test_ncn.ncn_root,
            )
            .await?;

        fixture
            .verify_certificate_for_test_ncn(&test_ncn, TEST_DIGEST, vec![1])
            .await?;

        let result = fixture
            .verify_certificate_for_test_ncn(&test_ncn, TEST_DIGEST, vec![0])
            .await;
        assert_ncn_program_error(result, NCNProgramError::InsufficientStakeBps, Some(1));

        Ok(())
    }

    #[tokio::test]
    async fn test_multi_vault_max_vaults_capacity() -> TestResult<()> {
        // MAX_VAULTS/MAX_ST_MINTS is 16 per docs/INTERFACES.md par.3
        assert_eq!(MAX_VAULTS, 16);

        let mut fixture = TestBuilder::new().await;
        let mut ncn_program_client = fixture.ncn_program_client();

        // Registry-level capacity is covered by core unit tests; here we
        // exercise a real flow well beyond the old single-vault limit.
        fixture.initialize_restaking_and_vault_programs().await?;
        let mut test_ncn = fixture.create_test_ncn().await?;
        ncn_program_client
            .do_initialize_config(
                test_ncn.ncn_root.ncn_pubkey,
                &test_ncn.ncn_root.ncn_admin,
                Some(10),
            )
            .await?;
        ncn_program_client
            .do_full_initialize_vault_registry(test_ncn.ncn_root.ncn_pubkey)
            .await?;

        fixture
            .add_operators_to_test_ncn(&mut test_ncn, 1, None)
            .await?;
        fixture
            .add_vaults_to_test_ncn(&mut test_ncn, 4, None)
            .await?;
        fixture.add_delegation_in_test_ncn(&test_ncn, 100).await?;

        register_vaults_with_weights(&mut fixture, &test_ncn, &[10_000, 10_000, 10_000, 10_000])
            .await?;

        let ncn = test_ncn.ncn_root.ncn_pubkey;

        ncn_program_client.do_full_initialize_snapshot(ncn).await?;
        fixture.register_operators_to_test_ncn(&test_ncn).await?;
        fixture
            .add_vault_operator_delegation_snapshots_to_test_ncn(&test_ncn)
            .await?;

        let operator = test_ncn.operators[0].operator_pubkey;
        let snapshot = ncn_program_client.get_snapshot(ncn).await?;
        let operator_snapshot = snapshot.find_operator_snapshot(&operator).unwrap();

        // 4 vaults x 100 at full weight
        assert_eq!(operator_snapshot.stake_weight().stake_weight(), 400);
        assert_eq!(
            operator_snapshot
                .vault_contributions()
                .iter()
                .filter(|c| !c.is_empty())
                .count(),
            4
        );

        fixture
            .verify_certificate_for_test_ncn(&test_ncn, TEST_DIGEST, vec![])
            .await?;

        Ok(())
    }
}
