use jito_bytemuck::AccountDeserialize;
use jito_restaking_core::{
    config::Config, ncn_operator_state::NcnOperatorState, ncn_vault_ticket::NcnVaultTicket,
};
use jito_restaking_program;
use jito_vault_core::{
    vault_ncn_ticket::VaultNcnTicket, vault_operator_delegation::VaultOperatorDelegation,
};
use ncn_program_client::{
    instructions::{
        AdminRegisterStMintBuilder, AdminSetNewAdminBuilder, AdminSetParametersBuilder,
        InitializeConfigBuilder, InitializeSnapshotBuilder, InitializeVaultRegistryBuilder,
        ReallocSnapshotBuilder, RegisterOperatorBuilder, RegisterVaultBuilder,
        RemoveOperatorBuilder, SnapshotVaultOperatorDelegationBuilder,
        UpdateOperatorBN128KeysBuilder, UpdateOperatorIpPortBuilder, VerifyCertificateBuilder,
    },
    types::ConfigAdminRole,
};
use ncn_program_core::{
    account_payer::AccountPayer,
    config::Config as NcnConfig,
    constants::{G1_COMPRESSED_POINT_SIZE, G2_COMPRESSED_POINT_SIZE, MAX_REALLOC_BYTES},
    error::NCNProgramError,
    fees::FeeConfig,
    ncn_operator_account::NCNOperatorAccount,
    snapshot::{OperatorSnapshot, Snapshot},
    vault_registry::VaultRegistry,
};
use solana_program::{
    instruction::InstructionError, native_token::sol_to_lamports, pubkey::Pubkey,
    system_instruction::transfer,
};
use solana_program_test::{BanksClient, ProgramTestBanksClientExt};
use solana_sdk::{
    commitment_config::CommitmentLevel,
    compute_budget::ComputeBudgetInstruction,
    signature::{Keypair, Signer},
    system_program,
    transaction::{Transaction, TransactionError},
};

use super::restaking_client::NcnRoot;
use crate::fixtures::{TestError, TestResult};

/// A client for interacting with the NCN program in integration tests.
/// Provides helper methods for initializing accounts, fetching state, and sending transactions.
pub struct NCNProgramClient {
    banks_client: BanksClient,
    payer: Keypair,
}

impl NCNProgramClient {
    /// Creates a new NCN program client.
    pub const fn new(banks_client: BanksClient, payer: Keypair) -> Self {
        Self {
            banks_client,
            payer,
        }
    }

    /// Processes a transaction using the BanksClient with processed commitment level.
    pub async fn process_transaction(&mut self, tx: &Transaction) -> TestResult<()> {
        self.banks_client
            .process_transaction_with_preflight_and_commitment(
                tx.clone(),
                CommitmentLevel::Processed,
            )
            .await?;
        Ok(())
    }

    /// Airdrops SOL to a specified public key.
    pub async fn airdrop(&mut self, to: &Pubkey, sol: f64) -> TestResult<()> {
        let blockhash = self.banks_client.get_latest_blockhash().await?;
        let new_blockhash = self
            .banks_client
            .get_new_latest_blockhash(&blockhash)
            .await
            .unwrap();
        self.banks_client
            .process_transaction_with_preflight_and_commitment(
                Transaction::new_signed_with_payer(
                    &[transfer(&self.payer.pubkey(), to, sol_to_lamports(sol))],
                    Some(&self.payer.pubkey()),
                    &[&self.payer],
                    new_blockhash,
                ),
                CommitmentLevel::Processed,
            )
            .await?;
        Ok(())
    }

    /// Sets up the NCN program by initializing the config and vault registry.
    pub async fn setup_ncn_program(&mut self, ncn_root: &NcnRoot) -> TestResult<()> {
        self.do_initialize_config(ncn_root.ncn_pubkey, &ncn_root.ncn_admin, None)
            .await?;

        self.do_full_initialize_vault_registry(ncn_root.ncn_pubkey)
            .await?;

        Ok(())
    }

    /// Fetches the NCN Config account for a given NCN pubkey.
    pub async fn get_ncn_config(&mut self, ncn_pubkey: Pubkey) -> TestResult<NcnConfig> {
        let config_pda = NcnConfig::find_program_address(&ncn_program::id(), &ncn_pubkey).0;
        let config = self.banks_client.get_account(config_pda).await?.unwrap();
        Ok(*NcnConfig::try_from_slice_unchecked(config.data.as_slice()).unwrap())
    }

    /// Fetches the VaultRegistry account for a given NCN pubkey.
    pub async fn get_vault_registry(&mut self, ncn_pubkey: Pubkey) -> TestResult<VaultRegistry> {
        let vault_registry_pda =
            VaultRegistry::find_program_address(&ncn_program::id(), &ncn_pubkey).0;
        let vault_registry = self
            .banks_client
            .get_account(vault_registry_pda)
            .await?
            .unwrap();
        Ok(*VaultRegistry::try_from_slice_unchecked(vault_registry.data.as_slice()).unwrap())
    }

    /// Fetches the Snapshot account for a given NCN and epoch.
    pub async fn get_snapshot(&mut self, ncn: Pubkey) -> TestResult<Box<Snapshot>> {
        let address = Snapshot::find_program_address(&ncn_program::id(), &ncn).0;

        let raw_account = Box::new(self.banks_client.get_account(address).await?.unwrap());

        let account =
            Box::new(*Snapshot::try_from_slice_unchecked(raw_account.data.as_slice()).unwrap());

        Ok(account)
    }

    /// Fetches the NCNOperatorAccount account for a given NCN and operator.
    pub async fn get_ncn_operator_account(
        &mut self,
        ncn: Pubkey,
        operator: Pubkey,
    ) -> TestResult<NCNOperatorAccount> {
        let ncn_operator_account =
            NCNOperatorAccount::find_program_address(&ncn_program::id(), &ncn, &operator).0;
        let raw_account = self
            .banks_client
            .get_account(ncn_operator_account)
            .await?
            .unwrap();
        Ok(*NCNOperatorAccount::try_from_slice_unchecked(raw_account.data.as_slice()).unwrap())
    }

    /// Fetches the OperatorSnapshot from the Snapshot for a given operator, NCN, and epoch.
    #[allow(dead_code)]
    pub async fn get_operator_snapshot(
        &mut self,
        operator: Pubkey,
        ncn: Pubkey,
    ) -> TestResult<OperatorSnapshot> {
        // Get the snapshot which contains the operator snapshots
        let snapshot = self.get_snapshot(ncn).await?;

        // Find the operator snapshot by operator pubkey
        let operator_snapshot = snapshot.find_operator_snapshot(&operator);

        if operator_snapshot.is_none() {
            return Err(TestError::ProgramError(
                NCNProgramError::OperatorIsNotInSnapshot.into(),
            ));
        }
        Ok(*operator_snapshot.unwrap())
    }

    /// Initializes the NCN config account and airdrops funds to the account payer.
    pub async fn do_initialize_config(
        &mut self,
        ncn: Pubkey,
        ncn_admin: &Keypair,
        minimum_stake: Option<u128>,
    ) -> TestResult<()> {
        // Setup Payer
        self.airdrop(&self.payer.pubkey(), 1.0).await?;

        // Setup account payer
        let (account_payer, _, _) = AccountPayer::find_program_address(&ncn_program::id(), &ncn);
        self.airdrop(&account_payer, 100.0).await?;

        let ncn_admin_pubkey = ncn_admin.pubkey();

        let ncn_fee_wallet = Keypair::new();
        self.airdrop(&ncn_fee_wallet.pubkey(), 0.1).await?;

        // Airdroping some SOL to Protocol fee wallet to get it started.
        let jito_fee_wallet = FeeConfig::PROTOCOL_FEE_WALLET;
        self.airdrop(&jito_fee_wallet, 0.1).await?;

        self.initialize_config(
            ncn,
            ncn_admin,
            &ncn_admin_pubkey,
            3,
            10,
            10000,
            &ncn_fee_wallet.pubkey(),
            400,
            minimum_stake.unwrap_or(100),
        )
        .await
    }

    /// Sends a transaction to initialize the NCN config account.
    #[allow(clippy::too_many_arguments)]
    pub async fn initialize_config(
        &mut self,
        ncn: Pubkey,
        ncn_admin: &Keypair,
        tie_breaker_admin: &Pubkey,
        epochs_before_stall: u64,
        epochs_after_consensus_before_close: u64,
        valid_slots_after_consensus: u64,
        ncn_fee_wallet: &Pubkey,
        ncn_fee_bps: u16,
        minimum_stake: u128,
    ) -> TestResult<()> {
        let config = NcnConfig::find_program_address(&ncn_program::id(), &ncn).0;

        let (account_payer, _, _) = AccountPayer::find_program_address(&ncn_program::id(), &ncn);

        let ix = InitializeConfigBuilder::new()
            .config(config)
            .ncn(ncn)
            .ncn_fee_wallet(*ncn_fee_wallet)
            .ncn_admin(ncn_admin.pubkey())
            .account_payer(account_payer)
            .tie_breaker_admin(*tie_breaker_admin)
            .epochs_before_stall(epochs_before_stall)
            .epochs_after_consensus_before_close(epochs_after_consensus_before_close)
            .valid_slots_after_consensus(valid_slots_after_consensus)
            .minimum_stake(minimum_stake)
            .ncn_fee_bps(ncn_fee_bps)
            .instruction();

        let blockhash = self.banks_client.get_latest_blockhash().await?;
        self.process_transaction(&Transaction::new_signed_with_payer(
            &[ix],
            Some(&ncn_admin.pubkey()),
            &[&ncn_admin],
            blockhash,
        ))
        .await
    }

    /// Sets a new admin for a specific role in the NCN config.
    pub async fn do_set_new_admin(
        &mut self,
        role: ConfigAdminRole,
        new_admin: Pubkey,
        ncn_root: &NcnRoot,
    ) -> TestResult<()> {
        let config_pda =
            NcnConfig::find_program_address(&ncn_program::id(), &ncn_root.ncn_pubkey).0;
        self.airdrop(&ncn_root.ncn_admin.pubkey(), 1.0).await?;
        self.set_new_admin(config_pda, role, new_admin, ncn_root)
            .await
    }

    /// Sends a transaction to set a new admin in the NCN config.
    pub async fn set_new_admin(
        &mut self,
        config_pda: Pubkey,
        role: ConfigAdminRole,
        new_admin: Pubkey,
        ncn_root: &NcnRoot,
    ) -> TestResult<()> {
        let ix = AdminSetNewAdminBuilder::new()
            .config(config_pda)
            .ncn(ncn_root.ncn_pubkey)
            .ncn_admin(ncn_root.ncn_admin.pubkey())
            .new_admin(new_admin)
            .role(role)
            .instruction();

        let blockhash = self.banks_client.get_latest_blockhash().await?;
        self.process_transaction(&Transaction::new_signed_with_payer(
            &[ix],
            Some(&ncn_root.ncn_admin.pubkey()),
            &[&ncn_root.ncn_admin],
            blockhash,
        ))
        .await
    }

    /// Initializes and fully reallocates the vault registry account for a given NCN.
    pub async fn do_full_initialize_vault_registry(&mut self, ncn: Pubkey) -> TestResult<()> {
        self.do_initialize_vault_registry(ncn).await?;
        Ok(())
    }

    /// Initializes the vault registry account for a given NCN.
    pub async fn do_initialize_vault_registry(&mut self, ncn: Pubkey) -> TestResult<()> {
        let ncn_config = NcnConfig::find_program_address(&ncn_program::id(), &ncn).0;
        let vault_registry = VaultRegistry::find_program_address(&ncn_program::id(), &ncn).0;

        self.initialize_vault_registry(&ncn_config, &vault_registry, &ncn)
            .await
    }

    /// Sends a transaction to initialize the vault registry account.
    pub async fn initialize_vault_registry(
        &mut self,
        ncn_config: &Pubkey,
        vault_registry: &Pubkey,
        ncn: &Pubkey,
    ) -> TestResult<()> {
        let (account_payer, _, _) = AccountPayer::find_program_address(&ncn_program::id(), ncn);

        let ix = InitializeVaultRegistryBuilder::new()
            .config(*ncn_config)
            .vault_registry(*vault_registry)
            .ncn(*ncn)
            .account_payer(account_payer)
            .system_program(system_program::id())
            .instruction();

        let blockhash = self.banks_client.get_latest_blockhash().await?;
        self.process_transaction(&Transaction::new_signed_with_payer(
            &[ix],
            Some(&self.payer.pubkey()),
            &[&self.payer],
            blockhash,
        ))
        .await
    }

    /// Registers a vault with the NCN program.
    pub async fn do_register_vault(
        &mut self,
        ncn: Pubkey,
        vault: Pubkey,
        ncn_vault_ticket: Pubkey,
    ) -> TestResult<()> {
        let ncn_config = NcnConfig::find_program_address(&ncn_program::id(), &ncn).0;

        let vault_registry = VaultRegistry::find_program_address(&ncn_program::id(), &ncn).0;

        self.register_vault(ncn_config, vault_registry, ncn, vault, ncn_vault_ticket)
            .await
    }

    /// Sends a transaction to register a vault.
    pub async fn register_vault(
        &mut self,
        config: Pubkey,
        vault_registry: Pubkey,
        ncn: Pubkey,
        vault: Pubkey,
        ncn_vault_ticket: Pubkey,
    ) -> TestResult<()> {
        let ix = RegisterVaultBuilder::new()
            .config(config)
            .vault_registry(vault_registry)
            .ncn(ncn)
            .vault(vault)
            .ncn_vault_ticket(ncn_vault_ticket)
            .instruction();

        let blockhash = self.banks_client.get_latest_blockhash().await?;
        self.process_transaction(&Transaction::new_signed_with_payer(
            &[ix],
            Some(&self.payer.pubkey()),
            &[&self.payer],
            blockhash,
        ))
        .await
    }

    /// Registers an st_mint in the vault registry (admin operation).
    pub async fn do_admin_register_st_mint(
        &mut self,
        ncn: Pubkey,
        st_mint: Pubkey,
    ) -> TestResult<()> {
        let vault_registry = VaultRegistry::find_program_address(&ncn_program::id(), &ncn).0;

        let (ncn_config, _, _) = NcnConfig::find_program_address(&ncn_program::id(), &ncn);

        let admin = self.payer.pubkey();

        self.admin_register_st_mint(ncn, ncn_config, vault_registry, admin, st_mint)
            .await
    }

    /// Sends a transaction to register an st_mint in the vault registry (admin operation).
    #[allow(clippy::too_many_arguments)]
    pub async fn admin_register_st_mint(
        &mut self,
        ncn: Pubkey,
        ncn_config: Pubkey,
        vault_registry: Pubkey,
        admin: Pubkey,
        st_mint: Pubkey,
    ) -> TestResult<()> {
        let ix = {
            let mut builder = AdminRegisterStMintBuilder::new();
            builder
                .config(ncn_config)
                .ncn(ncn)
                .vault_registry(vault_registry)
                .admin(admin)
                .st_mint(st_mint);

            builder.instruction()
        };

        let blockhash = self.banks_client.get_latest_blockhash().await?;
        self.process_transaction(&Transaction::new_signed_with_payer(
            &[ix],
            Some(&self.payer.pubkey()),
            &[&self.payer],
            blockhash,
        ))
        .await
    }

    /// Initializes the snapshot account for a given NCN and epoch.
    pub async fn do_initialize_snapshot(&mut self, ncn: Pubkey) -> TestResult<()> {
        self.initialize_snapshot(ncn).await
    }

    /// Sends a transaction to initialize the snapshot account.
    pub async fn initialize_snapshot(&mut self, ncn: Pubkey) -> TestResult<()> {
        let snapshot = Snapshot::find_program_address(&ncn_program::id(), &ncn).0;

        let (account_payer, _, _) = AccountPayer::find_program_address(&ncn_program::id(), &ncn);

        let ix = InitializeSnapshotBuilder::new()
            .ncn(ncn)
            .snapshot(snapshot)
            .account_payer(account_payer)
            .system_program(system_program::id())
            .instruction();

        let blockhash = self.banks_client.get_latest_blockhash().await?;
        self.process_transaction(&Transaction::new_signed_with_payer(
            &[ix],
            Some(&self.payer.pubkey()),
            &[&self.payer],
            blockhash,
        ))
        .await
    }

    /// Initializes and fully reallocates the snapshot account for a given NCN and epoch.
    pub async fn do_full_initialize_snapshot(&mut self, ncn: Pubkey) -> TestResult<()> {
        self.do_initialize_snapshot(ncn).await?;
        let num_reallocs = (Snapshot::SIZE as f64 / MAX_REALLOC_BYTES as f64).ceil() as u64 - 1;
        self.do_realloc_snapshot(ncn, num_reallocs).await?;
        Ok(())
    }

    /// Reallocates the snapshot account multiple times.
    pub async fn do_realloc_snapshot(
        &mut self,
        ncn: Pubkey,
        num_reallocations: u64,
    ) -> TestResult<()> {
        let snapshot = Snapshot::find_program_address(&ncn_program::id(), &ncn).0;
        let config = NcnConfig::find_program_address(&ncn_program::id(), &ncn).0;

        self.realloc_snapshot(&ncn, &snapshot, &config, num_reallocations)
            .await
    }

    /// Sends transactions to reallocate the snapshot account.
    #[allow(clippy::too_many_arguments)]
    pub async fn realloc_snapshot(
        &mut self,
        ncn: &Pubkey,
        snapshot: &Pubkey,
        config: &Pubkey,
        num_reallocations: u64,
    ) -> TestResult<()> {
        let (account_payer, _, _) = AccountPayer::find_program_address(&ncn_program::id(), ncn);

        let ix = ReallocSnapshotBuilder::new()
            .ncn(*ncn)
            .snapshot(*snapshot)
            .account_payer(account_payer)
            .system_program(system_program::id())
            .config(*config)
            .instruction();

        let ixs = vec![ix; num_reallocations as usize];

        let blockhash = self.banks_client.get_latest_blockhash().await?;
        self.process_transaction(&Transaction::new_signed_with_payer(
            &ixs,
            Some(&self.payer.pubkey()),
            &[&self.payer],
            blockhash,
        ))
        .await
    }

    /// Snapshots the delegation information from a vault to an operator for a given NCN and epoch.
    pub async fn do_snapshot_vault_operator_delegation(
        &mut self,
        vault: Pubkey,
        operator: Pubkey,
        ncn: Pubkey,
    ) -> TestResult<()> {
        self.snapshot_vault_operator_delegation(vault, operator, ncn)
            .await
    }

    /// Sends a transaction to snapshot the vault operator delegation.
    pub async fn snapshot_vault_operator_delegation(
        &mut self,
        vault: Pubkey,
        operator: Pubkey,
        ncn: Pubkey,
    ) -> TestResult<()> {
        let restaking_config = Config::find_program_address(&jito_restaking_program::id()).0;

        let config_pda = NcnConfig::find_program_address(&ncn_program::id(), &ncn).0;

        let snapshot = Snapshot::find_program_address(&ncn_program::id(), &ncn).0;

        let vault_ncn_ticket =
            VaultNcnTicket::find_program_address(&jito_vault_program::id(), &vault, &ncn).0;

        let ncn_vault_ticket =
            NcnVaultTicket::find_program_address(&jito_restaking_program::id(), &ncn, &vault).0;

        let vault_operator_delegation = VaultOperatorDelegation::find_program_address(
            &jito_vault_program::id(),
            &vault,
            &operator,
        )
        .0;

        let ncn_operator_state =
            NcnOperatorState::find_program_address(&jito_restaking_program::id(), &ncn, &operator)
                .0;

        let ix = SnapshotVaultOperatorDelegationBuilder::new()
            .config(config_pda)
            .restaking_config(restaking_config)
            .ncn(ncn)
            .operator(operator)
            .vault(vault)
            .vault_ncn_ticket(vault_ncn_ticket)
            .ncn_vault_ticket(ncn_vault_ticket)
            .vault_operator_delegation(vault_operator_delegation)
            .ncn_operator_state(ncn_operator_state)
            .snapshot(snapshot)
            .instruction();

        let blockhash = self.banks_client.get_latest_blockhash().await?;
        self.process_transaction(&Transaction::new_signed_with_payer(
            &[ix],
            Some(&self.payer.pubkey()),
            &[&self.payer],
            blockhash,
        ))
        .await
    }

    /// Verifies a BLS certificate over `digest` (stateless; mutates nothing).
    pub async fn do_verify_certificate(
        &mut self,
        ncn: Pubkey,
        digest: [u8; 32],
        agg_sig: [u8; 32],
        apk2: [u8; 64],
        signers_bitmap: Vec<u8>,
        expected_generation: u64,
    ) -> Result<(), TestError> {
        let ncn_config = NcnConfig::find_program_address(&ncn_program::id(), &ncn).0;
        let snapshot = Snapshot::find_program_address(&ncn_program::id(), &ncn).0;
        let restaking_config = Config::find_program_address(&jito_restaking_program::id()).0;

        self.verify_certificate(
            ncn_config,
            ncn,
            snapshot,
            restaking_config,
            digest,
            agg_sig,
            apk2,
            signers_bitmap,
            expected_generation,
        )
        .await
    }

    /// Sends a VerifyCertificate transaction (stateless BLS certificate check).
    #[allow(clippy::too_many_arguments)]
    pub async fn verify_certificate(
        &mut self,
        ncn_config: Pubkey,
        ncn: Pubkey,
        snapshot: Pubkey,
        restaking_config: Pubkey,
        digest: [u8; 32],
        agg_sig: [u8; 32],
        apk2: [u8; 64],
        signers_bitmap: Vec<u8>,
        expected_generation: u64,
    ) -> Result<(), TestError> {
        let compute_budget_ix = ComputeBudgetInstruction::set_compute_unit_limit(2_000_000);

        let ix = VerifyCertificateBuilder::new()
            .ncn_config(ncn_config)
            .ncn(ncn)
            .snapshot(snapshot)
            .restaking_config(restaking_config)
            .digest(digest)
            .aggregated_g2(apk2)
            .aggregated_signature(agg_sig)
            .operators_signature_bitmap(signers_bitmap)
            .expected_generation(expected_generation)
            .instruction();

        let blockhash = self.banks_client.get_latest_blockhash().await?;
        self.process_transaction(&Transaction::new_signed_with_payer(
            &[compute_budget_ix, ix],
            Some(&self.payer.pubkey()),
            &[&self.payer],
            blockhash,
        ))
        .await
    }

    /// Sets various parameters in the NCN config (admin operation).
    #[allow(clippy::too_many_arguments)]
    pub async fn do_set_parameters(
        &mut self,
        starting_valid_epoch: Option<u64>,
        epochs_before_stall: Option<u64>,
        epochs_after_consensus_before_close: Option<u64>,
        valid_slots_after_consensus: Option<u64>,
        minimum_stake: Option<u128>,
        consensus_threshold_bps: Option<u16>,
        ncn_root: &NcnRoot,
    ) -> TestResult<()> {
        let config_pda =
            NcnConfig::find_program_address(&ncn_program::id(), &ncn_root.ncn_pubkey).0;

        let mut ix = AdminSetParametersBuilder::new();
        ix.config(config_pda)
            .ncn(ncn_root.ncn_pubkey)
            .ncn_admin(ncn_root.ncn_admin.pubkey());

        if let Some(epoch) = starting_valid_epoch {
            ix.starting_valid_epoch(epoch);
        }

        if let Some(epochs) = epochs_before_stall {
            ix.epochs_before_stall(epochs);
        }

        if let Some(epochs) = epochs_after_consensus_before_close {
            ix.epochs_after_consensus_before_close(epochs);
        }

        if let Some(slots) = valid_slots_after_consensus {
            ix.valid_slots_after_consensus(slots);
        }

        if let Some(minimum_stake) = minimum_stake {
            ix.minimum_stake(minimum_stake);
        }

        if let Some(threshold_bps) = consensus_threshold_bps {
            ix.consensus_threshold_bps(threshold_bps);
        }

        let blockhash = self.banks_client.get_latest_blockhash().await?;
        self.process_transaction(&Transaction::new_signed_with_payer(
            &[ix.instruction()],
            Some(&ncn_root.ncn_admin.pubkey()),
            &[&ncn_root.ncn_admin],
            blockhash,
        ))
        .await
    }

    pub async fn do_register_operator(
        &mut self,
        ncn: Pubkey,
        operator_pubkey: Pubkey,
        operator_admin: &Keypair,
        g1_pubkey: [u8; G1_COMPRESSED_POINT_SIZE],
        g2_pubkey: [u8; G2_COMPRESSED_POINT_SIZE],
        signature: [u8; 64],
    ) -> TestResult<()> {
        let config = NcnConfig::find_program_address(&ncn_program::id(), &ncn).0;
        let ncn_operator_account =
            NCNOperatorAccount::find_program_address(&ncn_program::id(), &ncn, &operator_pubkey).0;
        let ncn_operator_state = NcnOperatorState::find_program_address(
            &jito_restaking_program::id(),
            &ncn,
            &operator_pubkey,
        )
        .0;
        let restaking_config = Config::find_program_address(&jito_restaking_program::id()).0;
        let (account_payer, _, _) = AccountPayer::find_program_address(&ncn_program::id(), &ncn);
        let snapshot = Snapshot::find_program_address(&ncn_program::id(), &ncn).0;

        self.register_operator(
            config,
            ncn_operator_account,
            ncn_operator_state,
            restaking_config,
            snapshot,
            ncn,
            operator_pubkey,
            operator_admin,
            account_payer,
            g1_pubkey,
            g2_pubkey,
            signature,
        )
        .await
    }

    /// Sends a transaction to register an operator with BLS keys.
    #[allow(clippy::too_many_arguments)]
    pub async fn register_operator(
        &mut self,
        config: Pubkey,
        ncn_operator_account: Pubkey,
        ncn_operator_state: Pubkey,
        restaking_config: Pubkey,
        snapshot: Pubkey,
        ncn: Pubkey,
        operator_pubkey: Pubkey,
        operator_admin: &Keypair,
        account_payer: Pubkey,
        g1_pubkey: [u8; G1_COMPRESSED_POINT_SIZE],
        g2_pubkey: [u8; G2_COMPRESSED_POINT_SIZE],
        signature: [u8; 64],
    ) -> TestResult<()> {
        let ix = RegisterOperatorBuilder::new()
            .config(config)
            .ncn_operator_account(ncn_operator_account)
            .ncn(ncn)
            .operator(operator_pubkey)
            .operator_admin(operator_admin.pubkey())
            .ncn_operator_state(ncn_operator_state)
            .snapshot(snapshot)
            .restaking_config(restaking_config)
            .account_payer(account_payer)
            .system_program(system_program::id())
            .g1_pubkey(g1_pubkey)
            .g2_pubkey(g2_pubkey)
            .signature(signature)
            .instruction();

        let compute_budget_ix = ComputeBudgetInstruction::set_compute_unit_limit(1_000_000);

        let blockhash = self.banks_client.get_latest_blockhash().await?;
        self.process_transaction(&Transaction::new_signed_with_payer(
            &[ix, compute_budget_ix],
            Some(&self.payer.pubkey()),
            &[&self.payer, operator_admin],
            blockhash,
        ))
        .await
    }

    /// Removes an operator from the snapshot (tombstone + APK subtract +
    /// generation bump), signed by the given admin (NCN or operator admin).
    pub async fn do_remove_operator(
        &mut self,
        ncn: Pubkey,
        operator_pubkey: Pubkey,
        admin: &Keypair,
    ) -> TestResult<()> {
        let config = NcnConfig::find_program_address(&ncn_program::id(), &ncn).0;
        let snapshot = Snapshot::find_program_address(&ncn_program::id(), &ncn).0;

        let ix = RemoveOperatorBuilder::new()
            .config(config)
            .ncn(ncn)
            .operator(operator_pubkey)
            .admin(admin.pubkey())
            .snapshot(snapshot)
            .instruction();

        let blockhash = self.banks_client.get_latest_blockhash().await?;
        self.process_transaction(&Transaction::new_signed_with_payer(
            &[ix],
            Some(&self.payer.pubkey()),
            &[&self.payer, admin],
            blockhash,
        ))
        .await
    }

    /// Updates an operator's BLS keys with simplified parameters
    pub async fn do_update_operator_bn128_keys(
        &mut self,
        ncn: Pubkey,
        operator_pubkey: Pubkey,
        operator_admin: &Keypair,
        g1_pubkey: [u8; G1_COMPRESSED_POINT_SIZE],
        g2_pubkey: [u8; G2_COMPRESSED_POINT_SIZE],
        signature: [u8; 64],
    ) -> TestResult<()> {
        let config = NcnConfig::find_program_address(&ncn_program::id(), &ncn).0;
        let ncn_operator_account =
            NCNOperatorAccount::find_program_address(&ncn_program::id(), &ncn, &operator_pubkey).0;
        let snapshot = Snapshot::find_program_address(&ncn_program::id(), &ncn).0;

        self.update_operator_bn128_keys(
            config,
            ncn_operator_account,
            snapshot,
            ncn,
            operator_pubkey,
            operator_admin,
            g1_pubkey,
            g2_pubkey,
            signature,
        )
        .await
    }

    /// Updates an operator's BLS keys in the operator registry with full parameter control
    #[allow(clippy::too_many_arguments)]
    pub async fn update_operator_bn128_keys(
        &mut self,
        config: Pubkey,
        ncn_operator_account: Pubkey,
        snapshot: Pubkey,
        ncn: Pubkey,
        operator_pubkey: Pubkey,
        operator_admin: &Keypair,
        g1_pubkey: [u8; G1_COMPRESSED_POINT_SIZE],
        g2_pubkey: [u8; G2_COMPRESSED_POINT_SIZE],
        signature: [u8; 64],
    ) -> TestResult<()> {
        let ix = UpdateOperatorBN128KeysBuilder::new()
            .config(config)
            .ncn_operator_account(ncn_operator_account)
            .snapshot(snapshot)
            .ncn(ncn)
            .operator(operator_pubkey)
            .operator_admin(operator_admin.pubkey())
            .g1_pubkey(g1_pubkey)
            .g2_pubkey(g2_pubkey)
            .signature(signature)
            .instruction();

        let compute_budget_ix = ComputeBudgetInstruction::set_compute_unit_limit(1_000_000);

        let blockhash = self.banks_client.get_latest_blockhash().await?;
        self.process_transaction(&Transaction::new_signed_with_payer(
            &[ix, compute_budget_ix],
            Some(&self.payer.pubkey()),
            &[&self.payer, operator_admin],
            blockhash,
        ))
        .await
    }

    /// Updates an operator's IP address and port with simplified parameters
    pub async fn do_update_operator_ip_port(
        &mut self,
        ncn: Pubkey,
        operator_pubkey: Pubkey,
        operator_admin: &Keypair,
        ip_address: [u8; 4],
        port: u16,
    ) -> TestResult<()> {
        let config = NcnConfig::find_program_address(&ncn_program::id(), &ncn).0;
        let ncn_operator_account =
            NCNOperatorAccount::find_program_address(&ncn_program::id(), &ncn, &operator_pubkey).0;

        self.update_operator_ip_port(
            config,
            ncn_operator_account,
            ncn,
            operator_pubkey,
            operator_admin,
            ip_address,
            port,
        )
        .await
    }

    /// Updates an operator's IP address and port with full parameter control
    #[allow(clippy::too_many_arguments)]
    pub async fn update_operator_ip_port(
        &mut self,
        config: Pubkey,
        ncn_operator_account: Pubkey,
        ncn: Pubkey,
        operator_pubkey: Pubkey,
        operator_admin: &Keypair,
        ip_address: [u8; 4],
        port: u16,
    ) -> TestResult<()> {
        let ix = UpdateOperatorIpPortBuilder::new()
            .config(config)
            .ncn_operator_account(ncn_operator_account)
            .ncn(ncn)
            .operator(operator_pubkey)
            .operator_admin(operator_admin.pubkey())
            .ip_address(ip_address)
            .port(port)
            .instruction();

        let compute_budget_ix = ComputeBudgetInstruction::set_compute_unit_limit(1_000_000);

        let blockhash = self.banks_client.get_latest_blockhash().await?;
        self.process_transaction(&Transaction::new_signed_with_payer(
            &[ix, compute_budget_ix],
            Some(&self.payer.pubkey()),
            &[&self.payer, operator_admin],
            blockhash,
        ))
        .await
    }
}

/// Asserts that a TestResult contains a specific NCNProgramError.
#[inline(always)]
#[track_caller]
pub fn assert_ncn_program_error<T>(
    test_error: Result<T, TestError>,
    ncn_program_error: NCNProgramError,
    instruction_index: Option<u8>,
) {
    assert!(test_error.is_err());
    assert_eq!(
        test_error.err().unwrap().to_transaction_error().unwrap(),
        TransactionError::InstructionError(
            instruction_index.unwrap_or(0),
            InstructionError::Custom(ncn_program_error as u32)
        )
    );
}
