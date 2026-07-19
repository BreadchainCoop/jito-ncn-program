#[cfg(test)]
mod tests {
    use crate::fixtures::{test_builder::TestBuilder, TestResult};
    use ncn_program_core::{
        g1_point::G1CompressedPoint,
        g2_point::G2CompressedPoint,
        privkey::PrivKey,
        schemes::Sha256Normalized,
        utils::pop_message_digest,
    };

    use crate::fixtures::ncn_program_client::assert_ncn_program_error;
    use ncn_program_core::error::NCNProgramError;

    #[tokio::test]
    async fn test_register_operator_success() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;

        let mut restaking_program_client = fixture.restaking_program_client();
        let mut ncn_program_client = fixture.ncn_program_client();

        // Setup NCN
        let ncn_root = fixture.setup_ncn().await?;

        ncn_program_client
            .do_initialize_config(ncn_root.ncn_pubkey, &ncn_root.ncn_admin, None)
            .await?;

        // Setup operator
        let operator_root = restaking_program_client
            .do_initialize_operator(Some(200))
            .await?;

        // Setup operator and handshake
        restaking_program_client
            .do_initialize_ncn_operator_state(&ncn_root, &operator_root.operator_pubkey)
            .await?;
        fixture.warp_slot_incremental(1).await.unwrap();
        restaking_program_client
            .do_ncn_warmup_operator(&ncn_root, &operator_root.operator_pubkey)
            .await?;
        restaking_program_client
            .do_operator_warmup_ncn(&operator_root, &ncn_root.ncn_pubkey)
            .await?;
        ncn_program_client
            .do_full_initialize_snapshot(ncn_root.ncn_pubkey)
            .await?;

        // Generate BLS keypair
        let g1_compressed = G1CompressedPoint::try_from(operator_root.bn128_privkey).unwrap();
        let g2_compressed = G2CompressedPoint::try_from(&operator_root.bn128_privkey).unwrap();

        let signature = operator_root
            .bn128_privkey
            .sign::<Sha256Normalized>(&pop_message_digest(
                &ncn_root.ncn_pubkey,
                &operator_root.operator_pubkey,
                &g1_compressed.0,
            ))
            .unwrap();

        // Test operator registration
        ncn_program_client
            .do_register_operator(
                ncn_root.ncn_pubkey,
                operator_root.operator_pubkey,
                &operator_root.operator_admin,
                g1_compressed.0,
                g2_compressed.0,
                signature.0,
            )
            .await?;

        let ncn_operator_account = ncn_program_client
            .get_ncn_operator_account(ncn_root.ncn_pubkey, operator_root.operator_pubkey)
            .await?;

        assert_eq!(
            ncn_operator_account.operator_pubkey(),
            &operator_root.operator_pubkey
        );
        assert_eq!(ncn_operator_account.g1_pubkey(), &g1_compressed.0);
        assert_eq!(ncn_operator_account.g2_pubkey(), &g2_compressed.0);

        Ok(())
    }

    #[tokio::test]
    async fn test_register_operator_mismatched_bls_keys() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;

        let mut restaking_program_client = fixture.restaking_program_client();
        let mut ncn_program_client = fixture.ncn_program_client();

        // Setup NCN
        let ncn_root = fixture.setup_ncn().await?;

        ncn_program_client
            .do_initialize_config(ncn_root.ncn_pubkey, &ncn_root.ncn_admin, None)
            .await?;

        // Setup operator
        let operator_root = restaking_program_client
            .do_initialize_operator(Some(200))
            .await?;
        ncn_program_client
            .do_full_initialize_snapshot(ncn_root.ncn_pubkey)
            .await?;

        // Generate mismatched BLS keypair
        let fake_privkey = PrivKey::from_random();
        let g1_compressed = G1CompressedPoint::try_from(operator_root.bn128_privkey).unwrap();
        let g2_compressed = G2CompressedPoint::try_from(&fake_privkey).unwrap(); // Different key!

        let signature = operator_root
            .bn128_privkey
            .sign::<Sha256Normalized>(&pop_message_digest(
                &ncn_root.ncn_pubkey,
                &operator_root.operator_pubkey,
                &g1_compressed.0,
            ))
            .unwrap();

        // Test operator registration with mismatched keys should fail
        let result = ncn_program_client
            .do_register_operator(
                ncn_root.ncn_pubkey,
                operator_root.operator_pubkey,
                &operator_root.operator_admin,
                g1_compressed.0,
                g2_compressed.0,
                signature.0,
            )
            .await;

        // This should fail due to BLS verification error
        assert!(result.is_err());

        Ok(())
    }

    #[tokio::test]
    async fn test_register_operator_without_registry_fails() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;

        let mut restaking_program_client = fixture.restaking_program_client();
        let mut ncn_program_client = fixture.ncn_program_client();

        // Setup NCN but DON'T initialize operator registry
        let ncn_root = fixture.setup_ncn().await?;

        ncn_program_client
            .do_initialize_config(ncn_root.ncn_pubkey, &ncn_root.ncn_admin, None)
            .await?;

        // Setup operator
        let operator_root = restaking_program_client
            .do_initialize_operator(Some(200))
            .await?;

        ncn_program_client
            .do_full_initialize_snapshot(ncn_root.ncn_pubkey)
            .await?;

        // Generate BLS keypair
        let g1_compressed = G1CompressedPoint::try_from(operator_root.bn128_privkey).unwrap();
        let g2_compressed = G2CompressedPoint::try_from(&operator_root.bn128_privkey).unwrap();

        let signature = operator_root
            .bn128_privkey
            .sign::<Sha256Normalized>(&pop_message_digest(
                &ncn_root.ncn_pubkey,
                &operator_root.operator_pubkey,
                &g1_compressed.0,
            ))
            .unwrap();

        // Try to register operator without operator registry should fail
        let result = ncn_program_client
            .do_register_operator(
                ncn_root.ncn_pubkey,
                operator_root.operator_pubkey,
                &operator_root.operator_admin,
                g1_compressed.0,
                g2_compressed.0,
                signature.0,
            )
            .await;

        assert!(result.is_err());

        Ok(())
    }

    #[tokio::test]
    async fn test_register_operator_fails_if_ncn_not_opted_in() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;
        let mut restaking_program_client = fixture.restaking_program_client();
        let mut ncn_program_client = fixture.ncn_program_client();

        // Setup NCN and registry
        let ncn_root = fixture.setup_ncn().await?;
        ncn_program_client
            .do_initialize_config(ncn_root.ncn_pubkey, &ncn_root.ncn_admin, None)
            .await?;

        // Setup operator and handshake
        let operator_root = restaking_program_client
            .do_initialize_operator(Some(200))
            .await?;
        restaking_program_client
            .do_initialize_ncn_operator_state(&ncn_root, &operator_root.operator_pubkey)
            .await?;
        fixture.warp_slot_incremental(1).await.unwrap();
        restaking_program_client
            .do_ncn_warmup_operator(&ncn_root, &operator_root.operator_pubkey)
            .await?;
        restaking_program_client
            .do_operator_warmup_ncn(&operator_root, &ncn_root.ncn_pubkey)
            .await?;

        fixture.warp_epoch_incremental(2).await?;
        ncn_program_client
            .do_full_initialize_snapshot(ncn_root.ncn_pubkey)
            .await?;

        // Now, NCN disables the operator
        restaking_program_client
            .do_ncn_cooldown_operator(&ncn_root, &operator_root.operator_pubkey)
            .await?;

        // Generate BLS keypair
        let g1_compressed = G1CompressedPoint::try_from(operator_root.bn128_privkey).unwrap();
        let g2_compressed = G2CompressedPoint::try_from(&operator_root.bn128_privkey).unwrap();
        let signature = operator_root
            .bn128_privkey
            .sign::<Sha256Normalized>(&pop_message_digest(
                &ncn_root.ncn_pubkey,
                &operator_root.operator_pubkey,
                &g1_compressed.0,
            ))
            .unwrap();

        // Try to register operator (should fail)
        let result = ncn_program_client
            .do_register_operator(
                ncn_root.ncn_pubkey,
                operator_root.operator_pubkey,
                &operator_root.operator_admin,
                g1_compressed.0,
                g2_compressed.0,
                signature.0,
            )
            .await;
        assert_ncn_program_error(
            result,
            NCNProgramError::OperatorNcnConnectionNotActive,
            None,
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_register_operator_fails_if_operator_not_opted_in() -> TestResult<()> {
        let mut fixture = TestBuilder::new().await;
        let mut restaking_program_client = fixture.restaking_program_client();
        let mut ncn_program_client = fixture.ncn_program_client();

        // Setup NCN and registry
        let ncn_root = fixture.setup_ncn().await?;
        ncn_program_client
            .do_initialize_config(ncn_root.ncn_pubkey, &ncn_root.ncn_admin, None)
            .await?;

        // Setup operator and handshake
        let operator_root = restaking_program_client
            .do_initialize_operator(Some(200))
            .await?;
        restaking_program_client
            .do_initialize_ncn_operator_state(&ncn_root, &operator_root.operator_pubkey)
            .await?;
        fixture.warp_slot_incremental(1).await.unwrap();
        restaking_program_client
            .do_ncn_warmup_operator(&ncn_root, &operator_root.operator_pubkey)
            .await?;
        restaking_program_client
            .do_operator_warmup_ncn(&operator_root, &ncn_root.ncn_pubkey)
            .await?;
        ncn_program_client
            .do_full_initialize_snapshot(ncn_root.ncn_pubkey)
            .await?;

        fixture.warp_epoch_incremental(2).await?;

        // Operator disables itself
        restaking_program_client
            .do_operator_cooldown_ncn(&operator_root, &ncn_root.ncn_pubkey)
            .await?;

        // Generate BLS keypair
        let g1_compressed = G1CompressedPoint::try_from(operator_root.bn128_privkey).unwrap();
        let g2_compressed = G2CompressedPoint::try_from(&operator_root.bn128_privkey).unwrap();
        let signature = operator_root
            .bn128_privkey
            .sign::<Sha256Normalized>(&pop_message_digest(
                &ncn_root.ncn_pubkey,
                &operator_root.operator_pubkey,
                &g1_compressed.0,
            ))
            .unwrap();

        // Try to register operator (should fail)
        let result = ncn_program_client
            .do_register_operator(
                ncn_root.ncn_pubkey,
                operator_root.operator_pubkey,
                &operator_root.operator_admin,
                g1_compressed.0,
                g2_compressed.0,
                signature.0,
            )
            .await;
        assert_ncn_program_error(
            result,
            NCNProgramError::OperatorNcnConnectionNotActive,
            None,
        );
        Ok(())
    }
}
