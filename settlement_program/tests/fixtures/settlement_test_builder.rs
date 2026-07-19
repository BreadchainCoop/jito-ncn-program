//! Builder for the settlement integration environment: the real jito
//! restaking + vault programs, the real NCN program, and the settlement
//! program under test. Forked from
//! `integration_tests/tests/fixtures/test_builder.rs` (which cannot register
//! the settlement program); the NCN/vault/operator flows below mirror it
//! verbatim so the snapshot state is built exactly like the NCN suite does.

use std::fmt::{Debug, Formatter};

use jito_restaking_core::{config::Config, ncn_vault_ticket::NcnVaultTicket};
use ncn_program_core::{
    g1_point::{G1CompressedPoint, G1Point},
    g2_point::{G2CompressedPoint, G2Point},
    schemes::{MessageDigest, Sha256Normalized},
    utils::{create_signer_bitmap, pop_message_digest},
};
use solana_program::{clock::Clock, native_token::sol_to_lamports, pubkey::Pubkey};
use solana_program_test::{processor, BanksClientError, ProgramTest, ProgramTestContext};
use solana_sdk::signature::{Keypair, Signer};

use super::{
    ncn_program_client::NCNProgramClient,
    restaking_client::{NcnRoot, OperatorRoot, RestakingProgramClient},
    settlement_client::SettlementClient,
    vault_client::{VaultProgramClient, VaultRoot},
    TestResult,
};

/// A complete NCN setup: the NCN, its operators, and vaults.
pub struct TestNcn {
    pub ncn_root: NcnRoot,
    pub operators: Vec<OperatorRoot>,
    pub vaults: Vec<VaultRoot>,
}

/// An aggregated BLS certificate over a message digest.
pub struct Certificate {
    pub aggregated_signature: [u8; 32],
    pub aggregated_g2: [u8; 64],
    pub bitmap: Vec<u8>,
}

pub struct SettlementTestBuilder {
    context: ProgramTestContext,
}

impl Debug for SettlementTestBuilder {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "SettlementTestBuilder")
    }
}

impl SettlementTestBuilder {
    pub async fn new() -> Self {
        let run_as_bpf = std::env::vars().any(|(key, _)| key.eq("SBF_OUT_DIR"));

        let program_test = if run_as_bpf {
            let mut program_test = ProgramTest::new("ncn_program", ncn_program::id(), None);
            program_test.add_program("settlement_program", settlement_program::id(), None);
            program_test.add_program("jito_vault_program", jito_vault_program::id(), None);
            program_test.add_program("jito_restaking_program", jito_restaking_program::id(), None);
            program_test
        } else {
            let mut program_test = ProgramTest::new(
                "ncn_program",
                ncn_program::id(),
                processor!(ncn_program::process_instruction),
            );
            program_test.add_program(
                "settlement_program",
                settlement_program::id(),
                processor!(settlement_program::process_instruction),
            );
            program_test.add_program(
                "jito_vault_program",
                jito_vault_program::id(),
                processor!(jito_vault_program::process_instruction),
            );
            program_test.add_program(
                "jito_restaking_program",
                jito_restaking_program::id(),
                processor!(jito_restaking_program::process_instruction),
            );
            program_test
        };

        Self {
            context: program_test.start_with_context().await,
        }
    }

    pub async fn warp_slot_incremental(
        &mut self,
        incremental_slots: u64,
    ) -> Result<(), BanksClientError> {
        let clock: Clock = self.context.banks_client.get_sysvar().await?;
        self.context
            .warp_to_slot(clock.slot.checked_add(incremental_slots).unwrap())
            .map_err(|_| BanksClientError::ClientError("failed to warp slot"))?;
        Ok(())
    }

    pub async fn clock(&mut self) -> Clock {
        self.context.banks_client.get_sysvar().await.unwrap()
    }

    pub fn ncn_program_client(&self) -> NCNProgramClient {
        NCNProgramClient::new(
            self.context.banks_client.clone(),
            self.context.payer.insecure_clone(),
        )
    }

    pub fn restaking_program_client(&self) -> RestakingProgramClient {
        RestakingProgramClient::new(
            self.context.banks_client.clone(),
            self.context.payer.insecure_clone(),
        )
    }

    pub fn vault_program_client(&self) -> VaultProgramClient {
        VaultProgramClient::new(
            self.context.banks_client.clone(),
            self.context.payer.insecure_clone(),
        )
    }

    pub fn settlement_client(&self) -> SettlementClient {
        SettlementClient::new(
            self.context.banks_client.clone(),
            self.context.payer.insecure_clone(),
        )
    }

    // ---- NCN environment assembly (mirrors test_builder.rs) ----

    pub async fn setup_ncn(&mut self) -> TestResult<NcnRoot> {
        let mut restaking_program_client = self.restaking_program_client();
        let mut vault_program_client = self.vault_program_client();

        vault_program_client.do_initialize_config().await?;
        restaking_program_client.do_initialize_config().await?;
        let ncn_root = restaking_program_client
            .do_initialize_ncn(Some(self.context.payer.insecure_clone()))
            .await?;

        Ok(ncn_root)
    }

    pub async fn add_operators_to_test_ncn(
        &mut self,
        test_ncn: &mut TestNcn,
        operator_count: usize,
        operator_fees_bps: Option<u16>,
    ) -> TestResult<()> {
        let mut restaking_program_client = self.restaking_program_client();

        for _ in 0..operator_count {
            let operator_root = restaking_program_client
                .do_initialize_operator(operator_fees_bps)
                .await?;

            // ncn <> operator
            restaking_program_client
                .do_initialize_ncn_operator_state(
                    &test_ncn.ncn_root,
                    &operator_root.operator_pubkey,
                )
                .await?;
            self.warp_slot_incremental(1).await.unwrap();
            restaking_program_client
                .do_ncn_warmup_operator(&test_ncn.ncn_root, &operator_root.operator_pubkey)
                .await?;
            restaking_program_client
                .do_operator_warmup_ncn(&operator_root, &test_ncn.ncn_root.ncn_pubkey)
                .await?;

            test_ncn.operators.push(operator_root);
        }

        Ok(())
    }

    pub async fn add_vaults_to_test_ncn(
        &mut self,
        test_ncn: &mut TestNcn,
        vault_count: usize,
    ) -> TestResult<()> {
        let mut vault_program_client = self.vault_program_client();
        let mut restaking_program_client = self.restaking_program_client();

        const DEPOSIT_FEE_BPS: u16 = 0;
        const WITHDRAWAL_FEE_BPS: u16 = 0;
        const REWARD_FEE_BPS: u16 = 0;
        let mint_amount: u64 = sol_to_lamports(100_000_000.0);

        for _ in 0..vault_count {
            let vault_root = vault_program_client
                .do_initialize_vault(
                    DEPOSIT_FEE_BPS,
                    WITHDRAWAL_FEE_BPS,
                    REWARD_FEE_BPS,
                    9,
                    &self.context.payer.pubkey(),
                    Some(Keypair::new()),
                )
                .await?;

            // vault <> ncn
            restaking_program_client
                .do_initialize_ncn_vault_ticket(&test_ncn.ncn_root, &vault_root.vault_pubkey)
                .await?;
            self.warp_slot_incremental(1).await.unwrap();
            restaking_program_client
                .do_warmup_ncn_vault_ticket(&test_ncn.ncn_root, &vault_root.vault_pubkey)
                .await?;
            vault_program_client
                .do_initialize_vault_ncn_ticket(&vault_root, &test_ncn.ncn_root.ncn_pubkey)
                .await?;
            self.warp_slot_incremental(1).await.unwrap();
            vault_program_client
                .do_warmup_vault_ncn_ticket(&vault_root, &test_ncn.ncn_root.ncn_pubkey)
                .await?;

            for operator_root in test_ncn.operators.iter() {
                // vault <> operator
                restaking_program_client
                    .do_initialize_operator_vault_ticket(operator_root, &vault_root.vault_pubkey)
                    .await?;
                self.warp_slot_incremental(1).await.unwrap();
                restaking_program_client
                    .do_warmup_operator_vault_ticket(operator_root, &vault_root.vault_pubkey)
                    .await?;
                vault_program_client
                    .do_initialize_vault_operator_delegation(
                        &vault_root,
                        &operator_root.operator_pubkey,
                    )
                    .await?;
            }

            let depositor_keypair = self.context.payer.insecure_clone();
            let depositor = depositor_keypair.pubkey();
            vault_program_client
                .configure_depositor(&vault_root, &depositor, mint_amount)
                .await?;
            vault_program_client
                .do_mint_to(&vault_root, &depositor_keypair, mint_amount, mint_amount)
                .await
                .unwrap();

            test_ncn.vaults.push(vault_root);
        }

        Ok(())
    }

    pub async fn add_delegation_in_test_ncn(
        &mut self,
        test_ncn: &TestNcn,
        delegation_amount: u64,
    ) -> TestResult<()> {
        let mut vault_program_client = self.vault_program_client();

        for vault_root in test_ncn.vaults.iter() {
            for operator_root in test_ncn.operators.iter() {
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

        Ok(())
    }

    pub async fn add_vault_registry_to_test_ncn(&mut self, test_ncn: &TestNcn) -> TestResult<()> {
        let mut ncn_program_client = self.ncn_program_client();
        let mut restaking_client = self.restaking_program_client();
        let mut vault_client = self.vault_program_client();

        let restaking_config_address =
            Config::find_program_address(&jito_restaking_program::id()).0;
        let restaking_config = restaking_client
            .get_config(&restaking_config_address)
            .await?;

        let epoch_length = restaking_config.epoch_length();

        self.warp_slot_incremental(epoch_length * 2).await.unwrap();

        for vault in test_ncn.vaults.iter() {
            let ncn = test_ncn.ncn_root.ncn_pubkey;
            let vault = vault.vault_pubkey;

            let operators = test_ncn
                .operators
                .iter()
                .map(|operator| operator.operator_pubkey)
                .collect::<Vec<Pubkey>>();

            vault_client
                .do_full_vault_update(&vault, &operators)
                .await?;

            let st_mint = vault_client.get_vault(&vault).await?.supported_mint;

            let ncn_vault_ticket =
                NcnVaultTicket::find_program_address(&jito_restaking_program::id(), &ncn, &vault).0;

            ncn_program_client
                .do_admin_register_st_mint(ncn, st_mint)
                .await?;

            ncn_program_client
                .do_register_vault(ncn, vault, ncn_vault_ticket)
                .await?;
        }

        Ok(())
    }

    pub async fn register_operators_to_test_ncn(&mut self, test_ncn: &TestNcn) -> TestResult<()> {
        let mut ncn_program_client = self.ncn_program_client();
        for operator_root in test_ncn.operators.iter() {
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
        }

        Ok(())
    }

    pub async fn add_vault_operator_delegation_snapshots_to_test_ncn(
        &mut self,
        test_ncn: &TestNcn,
    ) -> TestResult<()> {
        let mut ncn_program_client = self.ncn_program_client();
        let mut vault_program_client = self.vault_program_client();

        let clock = self.clock().await;
        let slot = clock.slot;
        let ncn = test_ncn.ncn_root.ncn_pubkey;

        let operators_for_update = test_ncn
            .operators
            .iter()
            .map(|operator_root| operator_root.operator_pubkey)
            .collect::<Vec<Pubkey>>();

        for operator_root in test_ncn.operators.iter() {
            let operator = operator_root.operator_pubkey;

            let operator_snapshot = ncn_program_client
                .get_operator_snapshot(operator, ncn)
                .await?;

            if !operator_snapshot.is_active() {
                continue;
            }

            for vault_root in test_ncn.vaults.iter() {
                let vault = vault_root.vault_pubkey;

                let vault_is_update_needed = vault_program_client
                    .get_vault_is_update_needed(&vault, slot)
                    .await?;

                if vault_is_update_needed {
                    vault_program_client
                        .do_full_vault_update(&vault, &operators_for_update)
                        .await?;
                }

                ncn_program_client
                    .do_snapshot_vault_operator_delegation(vault, operator, ncn)
                    .await?;
            }
        }

        Ok(())
    }

    /// Full environment: programs, NCN, `operator_count` operators, one
    /// vault, delegations, vault registry, snapshot + operator registration,
    /// then a slot warp + delegation snapshots so every operator carries
    /// stake weight (the flow the NCN cast_vote suite uses).
    pub async fn create_initial_test_ncn(
        &mut self,
        operator_count: usize,
        operator_fees_bps: Option<u16>,
    ) -> TestResult<TestNcn> {
        let ncn_root = self.setup_ncn().await?;

        let mut test_ncn = TestNcn {
            ncn_root,
            operators: vec![],
            vaults: vec![],
        };

        let mut ncn_program_client = self.ncn_program_client();
        ncn_program_client
            .setup_ncn_program(&test_ncn.ncn_root)
            .await?;

        self.add_operators_to_test_ncn(&mut test_ncn, operator_count, operator_fees_bps)
            .await?;
        self.add_vaults_to_test_ncn(&mut test_ncn, 1).await?;
        self.add_delegation_in_test_ncn(&test_ncn, 100).await?;
        self.add_vault_registry_to_test_ncn(&test_ncn).await?;

        ncn_program_client
            .do_full_initialize_snapshot(test_ncn.ncn_root.ncn_pubkey)
            .await?;

        self.register_operators_to_test_ncn(&test_ncn).await?;

        self.warp_slot_incremental(1000).await.unwrap();
        self.add_vault_operator_delegation_snapshots_to_test_ncn(&test_ncn)
            .await?;

        Ok(test_ncn)
    }

    /// Collects REAL BLS signatures over `digest` from every operator not in
    /// `non_signer_indices`, aggregates them, and builds the LSB-first
    /// bitmap (INTERFACES.md §1 wire form).
    pub fn sign_digest(
        &self,
        test_ncn: &TestNcn,
        digest: &MessageDigest,
        non_signer_indices: &[usize],
    ) -> Certificate {
        let mut signatures: Vec<G1Point> = vec![];
        let mut apk2_pubkeys: Vec<G2Point> = vec![];

        for (i, operator) in test_ncn.operators.iter().enumerate() {
            if !non_signer_indices.contains(&i) {
                apk2_pubkeys.push(operator.bn128_g2_pubkey);
                let signature = operator
                    .bn128_privkey
                    .sign::<Sha256Normalized>(digest)
                    .unwrap();
                signatures.push(signature);
            }
        }

        let apk2 = apk2_pubkeys.into_iter().reduce(|acc, x| acc + x).unwrap();
        let aggregated_g2 = G2CompressedPoint::try_from(&apk2).unwrap().0;

        let agg_sig = signatures.into_iter().reduce(|acc, x| acc + x).unwrap();
        let aggregated_signature = G1CompressedPoint::try_from(agg_sig).unwrap().0;

        let bitmap = create_signer_bitmap(non_signer_indices, test_ncn.operators.len());

        Certificate {
            aggregated_signature,
            aggregated_g2,
            bitmap,
        }
    }
}
