//! BanksClient wrapper for the settlement program under test.

use jito_bytemuck::AccountDeserialize;
use jito_restaking_core::config::Config as RestakingConfig;
use ncn_program_core::{config::Config as NcnConfig, snapshot::Snapshot};
use settlement_core::{
    buffer::find_buffer_program_address,
    instruction::{
        close_buffer_ix, initialize_state_ix, settle_ix, write_buffer_ix, CloseBufferArgs,
        InitializeStateArgs, SettleArgs, WriteBufferArgs,
    },
    state::GkState,
};
use solana_program::{native_token::sol_to_lamports, pubkey::Pubkey, system_instruction::transfer};
use solana_program_test::{BanksClient, BanksClientError};
use solana_sdk::{
    commitment_config::CommitmentLevel,
    compute_budget::ComputeBudgetInstruction,
    hash::Hash,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

use super::{TestError, TestResult};

pub struct SettlementClient {
    banks_client: BanksClient,
    payer: Keypair,
}

impl SettlementClient {
    pub const fn new(banks_client: BanksClient, payer: Keypair) -> Self {
        Self {
            banks_client,
            payer,
        }
    }

    // NB: intentionally NOT the `_with_preflight_` variant — mirrors the
    // shared fixture clients (see ncn_program_client.rs): preflight simulates
    // the BN254-heavy transaction before validating its blockhash, which
    // nondeterministically evicts under nextest parallelism.
    pub async fn process_transaction(&mut self, tx: &Transaction) -> TestResult<()> {
        self.banks_client
            .process_transaction_with_commitment(tx.clone(), CommitmentLevel::Processed)
            .await?;
        Ok(())
    }

    /// Recent blockhash guaranteed distinct from the last one used by any
    /// client in this test (see `crate::fixtures::fresh_blockhash`).
    async fn fresh_blockhash(&mut self) -> Result<Hash, BanksClientError> {
        crate::fixtures::fresh_blockhash(&mut self.banks_client).await
    }

    pub async fn airdrop(&mut self, to: &Pubkey, sol: f64) -> TestResult<()> {
        let blockhash = self.fresh_blockhash().await?;
        self.process_transaction(&Transaction::new_signed_with_payer(
            &[transfer(&self.payer.pubkey(), to, sol_to_lamports(sol))],
            Some(&self.payer.pubkey()),
            &[&self.payer],
            blockhash,
        ))
        .await
    }

    pub async fn get_state(&mut self, state: &Pubkey) -> TestResult<GkState> {
        let account = self
            .banks_client
            .get_account(*state)
            .await?
            .expect("state account not found");
        Ok(*GkState::try_from_slice_unchecked(account.data.as_slice())
            .map_err(TestError::ProgramError)?)
    }

    pub async fn do_initialize_state(
        &mut self,
        ncn: &Pubkey,
        app_id: [u8; 32],
        sim_profile_id: [u8; 32],
        env_commitment: [u8; 32],
    ) -> TestResult<Pubkey> {
        let (state, _, _) = GkState::find_program_address(&settlement_program::id(), ncn, &app_id);
        let ix = initialize_state_ix(
            &settlement_program::id(),
            &state,
            ncn,
            &self.payer.pubkey(),
            &InitializeStateArgs {
                app_id,
                sim_profile_id,
                env_commitment,
            },
        )
        .unwrap();

        let blockhash = self.fresh_blockhash().await?;
        self.process_transaction(&Transaction::new_signed_with_payer(
            &[ix],
            Some(&self.payer.pubkey()),
            &[&self.payer],
            blockhash,
        ))
        .await?;
        Ok(state)
    }

    /// One WriteBuffer instruction in its own transaction.
    pub async fn do_write_buffer(
        &mut self,
        state: &Pubkey,
        transition_index: u64,
        offset: u32,
        bytes: Vec<u8>,
        max_size: u32,
    ) -> TestResult<Pubkey> {
        let (buffer, _, _) =
            find_buffer_program_address(&settlement_program::id(), state, transition_index);
        let ix = write_buffer_ix(
            &settlement_program::id(),
            state,
            &buffer,
            &self.payer.pubkey(),
            &WriteBufferArgs {
                transition_index,
                offset,
                bytes,
                max_size,
            },
        )
        .unwrap();

        let blockhash = self.fresh_blockhash().await?;
        self.process_transaction(&Transaction::new_signed_with_payer(
            &[ix],
            Some(&self.payer.pubkey()),
            &[&self.payer],
            blockhash,
        ))
        .await?;
        Ok(buffer)
    }

    /// Stages `content` into the transition's buffer across multiple
    /// transactions of `chunk_size` bytes each.
    pub async fn do_write_buffer_chunked(
        &mut self,
        state: &Pubkey,
        transition_index: u64,
        content: &[u8],
        chunk_size: usize,
    ) -> TestResult<Pubkey> {
        let max_size = content.len() as u32;
        let mut buffer = Pubkey::default();
        for (i, chunk) in content.chunks(chunk_size).enumerate() {
            buffer = self
                .do_write_buffer(
                    state,
                    transition_index,
                    (i * chunk_size) as u32,
                    chunk.to_vec(),
                    max_size,
                )
                .await?;
        }
        Ok(buffer)
    }

    fn settle_transaction(
        &self,
        ncn: &Pubkey,
        state: &Pubkey,
        buffer: Option<&Pubkey>,
        args: &SettleArgs,
        blockhash: solana_sdk::hash::Hash,
    ) -> Transaction {
        let ncn_config = NcnConfig::find_program_address(&ncn_program::id(), ncn).0;
        let snapshot = Snapshot::find_program_address(&ncn_program::id(), ncn).0;
        let restaking_config =
            RestakingConfig::find_program_address(&jito_restaking_program::id()).0;

        let compute_budget_ix = ComputeBudgetInstruction::set_compute_unit_limit(1_400_000);
        let ix = settle_ix(
            &settlement_program::id(),
            state,
            &ncn_config,
            ncn,
            &snapshot,
            &restaking_config,
            buffer,
            args,
        )
        .unwrap();

        Transaction::new_signed_with_payer(
            &[compute_budget_ix, ix],
            Some(&self.payer.pubkey()),
            &[&self.payer],
            blockhash,
        )
    }

    pub async fn do_settle(
        &mut self,
        ncn: &Pubkey,
        state: &Pubkey,
        buffer: Option<&Pubkey>,
        args: &SettleArgs,
    ) -> TestResult<()> {
        let blockhash = self.fresh_blockhash().await?;
        self.process_transaction(&self.settle_transaction(ncn, state, buffer, args, blockhash))
            .await
    }

    /// Settles and returns the transaction meta logs (self-CPI evidence) plus
    /// the inner-instruction data captured by simulation.
    pub async fn do_settle_with_meta(
        &mut self,
        ncn: &Pubkey,
        state: &Pubkey,
        buffer: Option<&Pubkey>,
        args: &SettleArgs,
    ) -> TestResult<(Vec<String>, Vec<Vec<u8>>)> {
        let blockhash = self.fresh_blockhash().await?;
        let tx = self.settle_transaction(ncn, state, buffer, args, blockhash);

        // Simulation captures inner instructions (the self-CPI event bytes).
        let simulation = self.banks_client.simulate_transaction(tx.clone()).await?;
        let inner_ix_data: Vec<Vec<u8>> = simulation
            .simulation_details
            .as_ref()
            .and_then(|d| d.inner_instructions.as_ref())
            .map(|per_ix| {
                per_ix
                    .iter()
                    .flatten()
                    .map(|inner| inner.instruction.data.clone())
                    .collect()
            })
            .unwrap_or_default();

        let result = self
            .banks_client
            .process_transaction_with_metadata(tx)
            .await?;
        result.result.map_err(|e| {
            TestError::BanksClientError(solana_program_test::BanksClientError::TransactionError(e))
        })?;
        let logs = result.metadata.map(|m| m.log_messages).unwrap_or_default();

        Ok((logs, inner_ix_data))
    }

    pub async fn do_close_buffer(
        &mut self,
        state: &Pubkey,
        transition_index: u64,
        payer: &Keypair,
    ) -> TestResult<()> {
        let (buffer, _, _) =
            find_buffer_program_address(&settlement_program::id(), state, transition_index);
        let ix = close_buffer_ix(
            &settlement_program::id(),
            state,
            &buffer,
            &payer.pubkey(),
            &CloseBufferArgs { transition_index },
        )
        .unwrap();

        let blockhash = self.fresh_blockhash().await?;
        self.process_transaction(&Transaction::new_signed_with_payer(
            &[ix],
            Some(&payer.pubkey()),
            &[payer],
            blockhash,
        ))
        .await
    }

    pub async fn get_balance(&mut self, key: &Pubkey) -> TestResult<u64> {
        Ok(self
            .banks_client
            .get_account(*key)
            .await?
            .map(|a| a.lamports)
            .unwrap_or(0))
    }

    pub async fn account_exists(&mut self, key: &Pubkey) -> TestResult<bool> {
        Ok(self.banks_client.get_account(*key).await?.is_some())
    }

    pub fn payer_keypair(&self) -> Keypair {
        self.payer.insecure_clone()
    }
}
