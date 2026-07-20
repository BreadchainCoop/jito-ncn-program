#[cfg(test)]
mod tests {

    use crate::fixtures::{test_builder::TestBuilder, TestResult};

    #[tokio::test]
    async fn test_initialize_snapshot_ok() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;

        fixture.create_initial_test_ncn(1, None).await?;

        Ok(())
    }
}
