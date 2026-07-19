#[cfg(test)]
mod tests {

    use crate::fixtures::{test_builder::TestBuilder, TestResult};

    #[tokio::test]
    async fn test_all_test_ncn_functions() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;
        fixture.initialize_restaking_and_vault_programs().await?;

        const OPERATOR_COUNT: usize = 1;

        let mut test_ncn = fixture.create_test_ncn().await?;

        let mut ncn_program_client = fixture.ncn_program_client();
        ncn_program_client
            .setup_ncn_program(&test_ncn.ncn_root)
            .await?;

        fixture
            .add_operators_to_test_ncn(&mut test_ncn, OPERATOR_COUNT, None)
            .await?;
        fixture
            .add_vaults_to_test_ncn(&mut test_ncn, 1, None)
            .await?;
        fixture.add_delegation_in_test_ncn(&test_ncn, 100).await?;
        fixture.add_vault_registry_to_test_ncn(&test_ncn).await?;
        fixture.add_snapshot_to_test_ncn(&test_ncn).await?;
        fixture.register_operators_to_test_ncn(&test_ncn).await?;
        fixture
            .add_vault_operator_delegation_snapshots_to_test_ncn(&test_ncn)
            .await?;
        fixture
            .verify_certificate_all_operators_who_can_sign(&test_ncn)
            .await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_intermission_test_ncn_functions() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;

        const OPERATOR_COUNT: usize = 1;

        let test_ncn = fixture
            .create_initial_test_ncn(OPERATOR_COUNT, None)
            .await?;
        fixture
            .update_snapshot_test_ncn_new_epoch(&test_ncn)
            .await?;

        fixture
            .verify_certificate_all_operators_who_can_sign(&test_ncn)
            .await?;

        Ok(())
    }

    #[tokio::test]
    async fn test_multiple_operators() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;
        const OPERATOR_COUNT: usize = 10;

        let test_ncn = fixture
            .create_initial_test_ncn(OPERATOR_COUNT, None)
            .await?;
        fixture
            .update_snapshot_test_ncn_new_epoch(&test_ncn)
            .await?;

        fixture
            .verify_certificate_all_operators_who_can_sign(&test_ncn)
            .await?;

        Ok(())
    }
}
