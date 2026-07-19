#[cfg(test)]
mod tests {

    use ncn_program_core::{
        g1_point::{G1CompressedPoint, G1Point},
        g2_point::G2CompressedPoint,
        schemes::Sha256Normalized,
        snapshot::Snapshot,
        utils::pop_message_digest,
    };

    use crate::fixtures::{test_builder::TestBuilder, TestResult};

    #[tokio::test]
    async fn test_initialize_operator_snapshot() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;
        let mut ncn_program_client = fixture.ncn_program_client();

        let test_ncn = fixture.create_initial_test_ncn(1, None).await?;

        fixture.warp_slot_incremental(1000).await?;

        let ncn = test_ncn.ncn_root.ncn_pubkey;
        let operator = test_ncn.operators[0].operator_pubkey;

        // Check initial size is MAX_REALLOC_BYTES
        // OperatorSnapshot is now embedded in Snapshot, so we verify the snapshot exists
        let snapshot_address = Snapshot::find_program_address(&ncn_program::id(), &ncn).0;
        let raw_account = fixture.get_account(&snapshot_address).await?.unwrap();
        assert_eq!(raw_account.owner, ncn_program::id());

        // Get operator snapshot from the snapshot and verify it was initialized correctly
        let operator_snapshot = ncn_program_client
            .get_operator_snapshot(operator, ncn)
            .await?;

        // Verify initial state - operator snapshot should exist in the snapshot
        assert_eq!(*operator_snapshot.operator(), operator);

        Ok(())
    }

    #[tokio::test]
    async fn test_add_operator_after_snapshot() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;
        let mut ncn_program_client = fixture.ncn_program_client();

        let mut test_ncn = fixture.create_initial_test_ncn(1, None).await?;

        fixture.warp_slot_incremental(1000).await?;

        // Add New Operator
        fixture
            .add_operators_to_test_ncn(&mut test_ncn, 1, None)
            .await?;
        fixture.warp_epoch_incremental(2).await?;

        let operator_root = test_ncn.operators.last().unwrap();

        let g1_pubkey = G1Point::try_from(operator_root.bn128_privkey).unwrap();
        let g1_compressed = G1CompressedPoint::try_from(g1_pubkey).unwrap();
        let g2_compressed = G2CompressedPoint::try_from(&operator_root.bn128_privkey).unwrap();

        let signature = operator_root
            .bn128_privkey
            .sign::<Sha256Normalized>(&pop_message_digest(
                &test_ncn.ncn_root.ncn_pubkey,
                &operator_root.operator_pubkey,
                &g1_compressed.0,
            ))
            .unwrap();

        ncn_program_client
            .do_register_operator(
                test_ncn.ncn_root.ncn_pubkey,
                operator_root.operator_pubkey,
                &operator_root.operator_admin,
                g1_compressed.0,
                g2_compressed.0,
                signature.0,
            )
            .await?;

        Ok(())
    }
}
