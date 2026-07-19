use std::time::Duration;

use crate::{
    getters::{
        get_account, get_all_operators_in_ncn, get_all_sorted_operators_for_vault,
        get_all_vaults_in_ncn, get_current_slot, get_operator_snapshot,
        get_or_create_vault_registry, get_restaking_config, get_snapshot, get_vault,
        get_vault_config, get_vault_registry, get_vault_update_state_tracker,
    },
    handler::CliHandler,
    log::boring_progress_bar,
};
use anyhow::{anyhow, Ok, Result};
use jito_restaking_core::{
    config::Config as RestakingConfig, ncn_operator_state::NcnOperatorState,
    ncn_vault_ticket::NcnVaultTicket,
};
use jito_vault_client::{
    instructions::{
        CloseVaultUpdateStateTrackerBuilder, CrankVaultUpdateStateTrackerBuilder,
        InitializeVaultUpdateStateTrackerBuilder,
    },
    types::WithdrawalAllocationMethod,
};
use jito_vault_core::{
    config::Config as VaultConfig, vault_ncn_ticket::VaultNcnTicket,
    vault_operator_delegation::VaultOperatorDelegation,
    vault_update_state_tracker::VaultUpdateStateTracker,
};
use log::info;
use ncn_program_client::{
    instructions::{
        AdminRegisterStMintBuilder, AdminSetNewAdminBuilder, AdminSetParametersBuilder,
        CastVoteBuilder, InitializeConfigBuilder as InitializeNCNProgramConfigBuilder,
        InitializeSnapshotBuilder, InitializeVaultRegistryBuilder, InitializeVoteCounterBuilder,
        ReallocSnapshotBuilder, RegisterOperatorBuilder, RegisterVaultBuilder,
        SnapshotVaultOperatorDelegationBuilder, UpdateOperatorIpPortBuilder,
    },
    types::ConfigAdminRole,
};
use ncn_program_core::{
    account_payer::AccountPayer, config::Config as NCNProgramConfig, constants::MAX_REALLOC_BYTES,
    ncn_operator_account::NCNOperatorAccount, snapshot::Snapshot, utils::get_epoch,
    vault_registry::VaultRegistry, vote_counter::VoteCounter,
};
use solana_client::rpc_config::RpcSendTransactionConfig;

use hex;

use solana_sdk::{
    compute_budget::ComputeBudgetInstruction,
    instruction::Instruction,
    msg,
    native_token::sol_to_lamports,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    signer::Signer,
    system_instruction::transfer,
    system_program,
    transaction::Transaction,
};
use tokio::time::sleep;

// --------------------- ADMIN ------------------------------
#[allow(clippy::too_many_arguments)]
pub async fn admin_create_config(
    handler: &CliHandler,
    ncn_fee_wallet: Pubkey,
    ncn_fee_bps: u16,
    tie_breaker_admin: Option<Pubkey>,
    epochs_before_stall: u64,
    valid_slots_after_consensus: u64,
    epochs_after_consensus_before_close: u64,
    minimum_stake: u128,
) -> Result<()> {
    let keypair = handler.keypair()?;
    let client = handler.rpc_client();

    let ncn = *handler.ncn()?;

    let (config, _, _) = NCNProgramConfig::find_program_address(&handler.ncn_program_id, &ncn);

    let (account_payer, _, _) = AccountPayer::find_program_address(&handler.ncn_program_id, &ncn);
    println!("Account Payer: {}", account_payer);

    let tie_breaker_admin = tie_breaker_admin.unwrap_or_else(|| keypair.pubkey());

    let initialize_config_ix = InitializeNCNProgramConfigBuilder::new()
        .config(config)
        .ncn(ncn)
        .ncn_fee_wallet(ncn_fee_wallet)
        .ncn_admin(keypair.pubkey())
        .tie_breaker_admin(tie_breaker_admin)
        .account_payer(account_payer)
        .system_program(system_program::id())
        .epochs_before_stall(epochs_before_stall)
        .epochs_after_consensus_before_close(epochs_after_consensus_before_close)
        .valid_slots_after_consensus(valid_slots_after_consensus)
        .minimum_stake(minimum_stake)
        .ncn_fee_bps(ncn_fee_bps)
        .instruction();

    let program = client.get_account(&handler.ncn_program_id).await?;

    info!(
        "\n\n----------------------\nProgram: {:?}\n\nProgram Account:\n{:?}\n\nIX:\n{:?}\n----------------------\n",
        &handler.ncn_program_id, program, &initialize_config_ix
    );

    send_and_log_transaction(
        handler,
        &[initialize_config_ix],
        &[],
        "Created NCN Program Config",
        &[
            format!("NCN: {:?}", ncn),
            format!("Ncn Admin: {:?}", keypair.pubkey()),
            format!("Tie Breaker Admin: {:?}", tie_breaker_admin),
            format!(
                "Valid Slots After Consensus: {:?}",
                valid_slots_after_consensus
            ),
        ],
    )
    .await?;

    Ok(())
}

pub async fn admin_register_st_mint(handler: &CliHandler, vault: &Pubkey) -> Result<()> {
    let keypair = handler.keypair()?;

    let ncn = *handler.ncn()?;

    let (config, _, _) = NCNProgramConfig::find_program_address(&handler.ncn_program_id, &ncn);

    let (vault_registry, _, _) = VaultRegistry::find_program_address(&handler.ncn_program_id, &ncn);

    let vault_account = get_vault(handler, vault).await?;

    let mut register_st_mint_builder = AdminRegisterStMintBuilder::new();

    register_st_mint_builder
        .config(config)
        .admin(keypair.pubkey())
        .vault_registry(vault_registry)
        .ncn(ncn)
        .st_mint(vault_account.supported_mint);

    let register_st_mint_ix = register_st_mint_builder.instruction();

    send_and_log_transaction(
        handler,
        &[register_st_mint_ix],
        &[],
        "Registered ST Mint",
        &[
            format!("NCN: {:?}", ncn),
            format!("ST Mint: {:?}", vault_account.supported_mint),
        ],
    )
    .await?;

    Ok(())
}

pub async fn admin_set_new_admin(
    handler: &CliHandler,
    new_admin: &Pubkey,
    set_tie_breaker_admin: bool,
) -> Result<()> {
    let keypair = handler.keypair()?;
    let ncn = *handler.ncn()?;

    let config_pda = NCNProgramConfig::find_program_address(&handler.ncn_program_id, &ncn).0;

    let roles = [(set_tie_breaker_admin, ConfigAdminRole::TieBreakerAdmin)];

    for (should_set, role) in roles.iter() {
        if !should_set {
            continue;
        }

        let mut ix = AdminSetNewAdminBuilder::new();
        ix.config(config_pda)
            .ncn(ncn)
            .ncn_admin(keypair.pubkey())
            .new_admin(*new_admin)
            .role(*role);

        send_and_log_transaction(
            handler,
            &[ix.instruction()],
            &[],
            "Admin Set New Admin",
            &[
                format!("NCN: {:?}", ncn),
                format!("New Admin: {:?}", new_admin),
                format!("Role: {:?}", role),
            ],
        )
        .await?;
    }

    Ok(())
}

pub async fn admin_set_parameters(
    handler: &CliHandler,
    epochs_before_stall: Option<u64>,
    epochs_after_consensus_before_close: Option<u64>,
    valid_slots_after_consensus: Option<u64>,
    starting_valid_epoch: Option<u64>,
) -> Result<()> {
    let keypair = handler.keypair()?;
    let ncn = *handler.ncn()?;

    let config_pda = NCNProgramConfig::find_program_address(&handler.ncn_program_id, &ncn).0;

    let mut ix = AdminSetParametersBuilder::new();
    ix.config(config_pda).ncn(ncn).ncn_admin(keypair.pubkey());

    if let Some(epochs) = epochs_before_stall {
        ix.epochs_before_stall(epochs);
    }

    if let Some(epochs) = epochs_after_consensus_before_close {
        ix.epochs_after_consensus_before_close(epochs);
    }

    if let Some(slots) = valid_slots_after_consensus {
        ix.valid_slots_after_consensus(slots);
    }

    if let Some(epoch) = starting_valid_epoch {
        ix.starting_valid_epoch(epoch);
    }

    send_and_log_transaction(
        handler,
        &[ix.instruction()],
        &[],
        "Set Parameters",
        &[
            format!("NCN: {:?}", ncn),
            format!("Epochs Before Stall: {:?}", epochs_before_stall),
            format!(
                "Epochs After Consensus Before Close: {:?}",
                epochs_after_consensus_before_close
            ),
            format!(
                "Valid Slots After Consensus: {:?}",
                valid_slots_after_consensus
            ),
        ],
    )
    .await?;

    Ok(())
}

pub async fn admin_fund_account_payer(handler: &CliHandler, amount: f64) -> Result<()> {
    let keypair = handler.keypair()?;
    let ncn = *handler.ncn()?;

    let (account_payer, _, _) = AccountPayer::find_program_address(&handler.ncn_program_id, &ncn);

    let transfer_ix = transfer(&keypair.pubkey(), &account_payer, sol_to_lamports(amount));

    send_and_log_transaction(
        handler,
        &[transfer_ix],
        &[],
        "Fund Account Payer",
        &[
            format!("NCN: {:?}", ncn),
            format!("Amount: {:?} SOL", amount),
        ],
    )
    .await?;

    Ok(())
}

// --------------------- NCN Program ------------------------------

// ----------------------- Keeper ---------------------------------

pub async fn create_vote_counter(handler: &CliHandler) -> Result<()> {
    let ncn = *handler.ncn()?;

    let (config, _, _) = NCNProgramConfig::find_program_address(&handler.ncn_program_id, &ncn);

    let (vote_counter, _, _) = VoteCounter::find_program_address(&handler.ncn_program_id, &ncn);

    let (account_payer, _, _) = AccountPayer::find_program_address(&handler.ncn_program_id, &ncn);

    let vote_counter_account = get_account(handler, &vote_counter).await?;

    // Skip if vote counter already exists
    if vote_counter_account.is_none() {
        let initialize_vote_counter_ix = InitializeVoteCounterBuilder::new()
            .config(config)
            .vote_counter(vote_counter)
            .ncn(ncn)
            .account_payer(account_payer)
            .system_program(system_program::id())
            .instruction();

        send_and_log_transaction(
            handler,
            &[initialize_vote_counter_ix],
            &[],
            "Created Vote Counter",
            &[format!("NCN: {:?}", ncn)],
        )
        .await?;
    } else {
        info!("Vote counter already exists for NCN: {:?}", ncn);
    }

    Ok(())
}

pub async fn create_vault_registry(handler: &CliHandler) -> Result<()> {
    let ncn = *handler.ncn()?;

    let (config, _, _) = NCNProgramConfig::find_program_address(&handler.ncn_program_id, &ncn);

    let (vault_registry, _, _) = VaultRegistry::find_program_address(&handler.ncn_program_id, &ncn);

    let (account_payer, _, _) = AccountPayer::find_program_address(&handler.ncn_program_id, &ncn);

    let vault_registry_account = get_account(handler, &vault_registry).await?;

    // Skip if vault registry already exists
    if vault_registry_account.is_none() {
        let initialize_vault_registry_ix = InitializeVaultRegistryBuilder::new()
            .config(config)
            .account_payer(account_payer)
            .ncn(ncn)
            .vault_registry(vault_registry)
            .instruction();

        send_and_log_transaction(
            handler,
            &[initialize_vault_registry_ix],
            &[],
            "Created Vault Registry",
            &[format!("NCN: {:?}", ncn)],
        )
        .await?;
    }

    // Number of reallocations needed based on VaultRegistry::SIZE
    let num_reallocs =
        ((VaultRegistry::SIZE as f64 / MAX_REALLOC_BYTES as f64).ceil() as u64 - 1).max(1);

    // ReallocVaultRegistryBuilder not available - functionality may have been removed
    // let realloc_vault_registry_ix = ReallocVaultRegistryBuilder::new()
    //     .config(config)
    //     .vault_registry(vault_registry)
    //     .ncn(ncn)
    //     .account_payer(account_payer)
    //     .system_program(system_program::id())
    //     .instruction();

    let mut realloc_ixs = Vec::with_capacity(num_reallocs as usize);
    realloc_ixs.push(ComputeBudgetInstruction::set_compute_unit_limit(1_400_000));
    for _ in 0..num_reallocs {
        // realloc_ixs.push(realloc_vault_registry_ix.clone()); // ReallocVaultRegistryBuilder not available
    }

    send_and_log_transaction(
        handler,
        &realloc_ixs,
        &[],
        "Reallocated Vault Registry",
        &[
            format!("NCN: {:?}", ncn),
            format!("Number of reallocations: {:?}", num_reallocs),
        ],
    )
    .await?;

    Ok(())
}

pub async fn register_vault(handler: &CliHandler, vault: &Pubkey) -> Result<()> {
    let ncn = *handler.ncn()?;
    let vault = *vault;

    let (ncn_program_config, _, _) =
        NCNProgramConfig::find_program_address(&handler.ncn_program_id, &ncn);

    let (vault_registry, _, _) = VaultRegistry::find_program_address(&handler.ncn_program_id, &ncn);

    let (ncn_vault_ticket, _, _) =
        NcnVaultTicket::find_program_address(&handler.restaking_program_id, &ncn, &vault);

    let register_vault_ix = RegisterVaultBuilder::new()
        .config(ncn_program_config)
        .vault_registry(vault_registry)
        .vault(vault)
        .ncn(ncn)
        .ncn_vault_ticket(ncn_vault_ticket)
        .instruction();

    send_and_log_transaction(
        handler,
        &[register_vault_ix],
        &[],
        "Registered Vault",
        &[format!("NCN: {:?}", ncn), format!("Vault: {:?}", vault)],
    )
    .await?;

    Ok(())
}

pub async fn register_operator(
    handler: &CliHandler,
    operator: &Pubkey,
    g1_pubkey: [u8; 32],
    g2_pubkey: [u8; 64],
    signature: [u8; 64],
) -> Result<()> {
    let keypair = handler.keypair()?;
    let ncn = *handler.ncn()?;
    let operator = *operator;

    let (config, _, _) = NCNProgramConfig::find_program_address(&handler.ncn_program_id, &ncn);

    let (ncn_operator_account, _, _) =
        NCNOperatorAccount::find_program_address(&handler.ncn_program_id, &ncn, &operator);

    let (account_payer, _, _) = AccountPayer::find_program_address(&handler.ncn_program_id, &ncn);

    let (ncn_operator_state, _, _) =
        NcnOperatorState::find_program_address(&handler.restaking_program_id, &ncn, &operator);

    let (restaking_config, _, _) =
        RestakingConfig::find_program_address(&handler.restaking_program_id);

    let (snapshot, _, _) = Snapshot::find_program_address(&handler.ncn_program_id, &ncn);

    let register_operator_ix = RegisterOperatorBuilder::new()
        .config(config)
        .ncn_operator_account(ncn_operator_account)
        .ncn(ncn)
        .operator(operator)
        .operator_admin(keypair.pubkey())
        .snapshot(snapshot)
        .ncn_operator_state(ncn_operator_state)
        .restaking_config(restaking_config)
        .account_payer(account_payer)
        .system_program(system_program::id())
        .g1_pubkey(g1_pubkey)
        .g2_pubkey(g2_pubkey)
        .signature(signature)
        .instruction();

    let compute_budget_ix = ComputeBudgetInstruction::set_compute_unit_limit(1_400_000);

    send_and_log_transaction(
        handler,
        &[register_operator_ix, compute_budget_ix],
        &[],
        "Registered Operator",
        &[
            format!("NCN: {:?}", ncn),
            format!("Operator: {:?}", operator),
            format!("G1 Public Key: {}", hex::encode(g1_pubkey)),
            format!("G2 Public Key: {}", hex::encode(g2_pubkey)),
        ],
    )
    .await?;

    Ok(())
}

pub async fn update_operator_ip_port(
    handler: &CliHandler,
    operator: &Pubkey,
    ip_address: &str,
    port: u16,
) -> Result<()> {
    let keypair = handler.keypair()?;
    let ncn = *handler.ncn()?;

    // Parse IP address from string to bytes
    let ip_bytes = parse_ip_address(ip_address)?;

    let (config, _, _) = NCNProgramConfig::find_program_address(&handler.ncn_program_id, &ncn);
    let (ncn_operator_account, _, _) =
        NCNOperatorAccount::find_program_address(&handler.ncn_program_id, &ncn, operator);

    let update_operator_ip_port_ix = UpdateOperatorIpPortBuilder::new()
        .config(config)
        .ncn_operator_account(ncn_operator_account)
        .ncn(ncn)
        .operator(*operator)
        .operator_admin(keypair.pubkey())
        .ip_address(ip_bytes)
        .port(port)
        .instruction();

    let compute_budget_ix = ComputeBudgetInstruction::set_compute_unit_limit(400_000);

    send_and_log_transaction(
        handler,
        &[update_operator_ip_port_ix, compute_budget_ix],
        &[],
        "Updated Operator IP and Port",
        &[
            format!("NCN: {:?}", ncn),
            format!("Operator: {:?}", operator),
            format!("IP Address: {}", ip_address),
            format!("Port: {}", port),
        ],
    )
    .await?;

    Ok(())
}

/// Parse IPv4 address from string to 16-byte array (IPv4-mapped IPv6 format)
fn parse_ip_address(ip_str: &str) -> Result<[u8; 4]> {
    let parts: Vec<&str> = ip_str.split('.').collect();
    if parts.len() != 4 {
        return Err(anyhow!(
            "Invalid IPv4 address format. Expected format: a.b.c.d"
        ));
    }

    let mut ip_bytes = [0u8; 4];

    for (i, part) in parts.iter().enumerate() {
        let octet: u8 = part
            .parse()
            .map_err(|_| anyhow!("Invalid IPv4 octet: {}", part))?;
        ip_bytes[i] = octet;
    }

    Ok(ip_bytes)
}

pub async fn create_snapshot(handler: &CliHandler, epoch: u64) -> Result<()> {
    let ncn = *handler.ncn()?;

    let (config, _, _) = NCNProgramConfig::find_program_address(&handler.ncn_program_id, &ncn);

    let (snapshot, _, _) = Snapshot::find_program_address(&handler.ncn_program_id, &ncn);

    let (account_payer, _, _) = AccountPayer::find_program_address(&handler.ncn_program_id, &ncn);

    // First, initialize the snapshot account with minimal size
    let initialize_snapshot_ix = InitializeSnapshotBuilder::new()
        .ncn(ncn)
        .snapshot(snapshot)
        .account_payer(account_payer)
        .system_program(system_program::id())
        .instruction();

    send_and_log_transaction(
        handler,
        &[initialize_snapshot_ix],
        &[],
        "Initialized Snapshot",
        &[format!("NCN: {:?}", ncn), format!("Epoch: {:?}", epoch)],
    )
    .await?;

    // Then, reallocate the snapshot account to full size and initialize the data
    // Calculate number of reallocations needed based on Snapshot::SIZE
    let num_reallocs =
        ((Snapshot::SIZE as f64 / MAX_REALLOC_BYTES as f64).ceil() as u64 - 1).max(1);

    let mut realloc_ixs = Vec::with_capacity(num_reallocs as usize + 1);
    realloc_ixs.push(ComputeBudgetInstruction::set_compute_unit_limit(1_400_000));

    let realloc_snapshot_ix = ReallocSnapshotBuilder::new()
        .ncn(ncn)
        .config(config)
        .snapshot(snapshot)
        .account_payer(account_payer)
        .system_program(system_program::id())
        .instruction();

    for _ in 0..num_reallocs {
        realloc_ixs.push(realloc_snapshot_ix.clone());
    }

    send_and_log_transaction(
        handler,
        &realloc_ixs,
        &[],
        "Reallocated and Initialized Snapshot",
        &[
            format!("NCN: {:?}", ncn),
            format!("Epoch: {:?}", epoch),
            format!("Number of reallocations: {:?}", num_reallocs),
        ],
    )
    .await?;

    Ok(())
}

pub async fn snapshot_vault_operator_delegation(
    handler: &CliHandler,
    vault: &Pubkey,
    operator: &Pubkey,
    epoch: u64,
) -> Result<()> {
    let ncn = *handler.ncn()?;

    let vault = *vault;
    let operator = *operator;

    let (config, _, _) = NCNProgramConfig::find_program_address(&handler.ncn_program_id, &ncn);

    let (restaking_config, _, _) =
        RestakingConfig::find_program_address(&handler.restaking_program_id);

    let (vault_ncn_ticket, _, _) =
        VaultNcnTicket::find_program_address(&handler.vault_program_id, &vault, &ncn);

    let (ncn_vault_ticket, _, _) =
        NcnVaultTicket::find_program_address(&handler.restaking_program_id, &ncn, &vault);

    let (vault_operator_delegation, _, _) =
        VaultOperatorDelegation::find_program_address(&handler.vault_program_id, &vault, &operator);

    let (ncn_operator_state, _, _) =
        NcnOperatorState::find_program_address(&handler.restaking_program_id, &ncn, &operator);

    let (snapshot, _, _) = Snapshot::find_program_address(&handler.ncn_program_id, &ncn);

    let snapshot_vault_operator_delegation_ix = SnapshotVaultOperatorDelegationBuilder::new()
        .restaking_config(restaking_config)
        .config(config)
        .ncn(ncn)
        .operator(operator)
        .vault(vault)
        .vault_ncn_ticket(vault_ncn_ticket)
        .ncn_operator_state(ncn_operator_state)
        .ncn_vault_ticket(ncn_vault_ticket)
        .vault_operator_delegation(vault_operator_delegation)
        .snapshot(snapshot)
        .instruction();

    send_and_log_transaction(
        handler,
        &[snapshot_vault_operator_delegation_ix],
        &[],
        "Snapshotted Vault Operator Delegation",
        &[
            format!("NCN: {:?}", ncn),
            format!("Vault: {:?}", vault),
            format!("Operator: {:?}", operator),
            format!("Epoch: {:?}", epoch),
        ],
    )
    .await?;

    Ok(())
}

// --------------------- operator ------------------------------

pub async fn cast_vote(
    handler: &CliHandler,
    epoch: u64,
    agg_sig: [u8; 32],
    apk2: [u8; 64],
    signers_bitmap: Vec<u8>,
    message: [u8; 32],
) -> Result<()> {
    let ncn = *handler.ncn()?;

    let (config, _, _) = NCNProgramConfig::find_program_address(&handler.ncn_program_id, &ncn);

    let (snapshot, _, _) = Snapshot::find_program_address(&handler.ncn_program_id, &ncn);

    let (restaking_config, _, _) =
        RestakingConfig::find_program_address(&handler.restaking_program_id);

    let (vote_counter, _, _) = VoteCounter::find_program_address(&handler.ncn_program_id, &ncn);

    let cast_vote_ix = CastVoteBuilder::new()
        .config(config)
        .ncn(ncn)
        .snapshot(snapshot)
        .restaking_config(restaking_config)
        .vote_counter(vote_counter)
        .aggregated_signature(agg_sig)
        .aggregated_g2(apk2)
        .operators_signature_bitmap(signers_bitmap)
        .instruction();

    send_and_log_transaction(
        handler,
        &[cast_vote_ix],
        &[],
        "Cast Vote",
        &[
            format!("NCN: {:?}", ncn),
            format!("Epoch: {:?}", epoch),
            format!("Message: {}", hex::encode(message)),
        ],
    )
    .await?;

    Ok(())
}

// --------------------- MIDDLEWARE ------------------------------

// Consensus result functionality has been removed from the program

pub const CREATE_TIMEOUT_MS: u64 = 2000;
pub const CREATE_GET_RETRIES: u64 = 3;
pub async fn check_created(handler: &CliHandler, address: &Pubkey) -> Result<()> {
    let mut retries = 0;
    let mut account = get_account(handler, address).await?;
    while account.is_none() && retries < CREATE_GET_RETRIES {
        sleep(Duration::from_millis(CREATE_TIMEOUT_MS * (retries + 1))).await;
        account = get_account(handler, address).await?;
        retries += 1;
    }

    if account.is_none() {
        return Err(anyhow!(
            "Failed to get account after creation {:?}",
            address
        ));
    }

    Ok(())
}

pub async fn full_vault_update(handler: &CliHandler, vault: &Pubkey) -> Result<()> {
    let payer = handler.keypair()?;

    // Get Epoch Info
    let current_slot = get_current_slot(handler).await?;
    let (ncn_epoch, epoch_length) = {
        let vault_config = get_vault_config(handler).await?;
        let ncn_epoch = vault_config.get_epoch_from_slot(current_slot)?;
        let epoch_length = vault_config.epoch_length();
        (ncn_epoch, epoch_length)
    };

    // Check Vault
    let vault_account = get_vault(handler, vault).await?;
    let current_slot = get_current_slot(handler).await?;

    if !vault_account.is_update_needed(current_slot, epoch_length)? {
        return Ok(());
    }

    // Initialize Vault Update State Tracker
    let (vault_config, _, _) = VaultConfig::find_program_address(&handler.vault_program_id);

    let (vault_update_state_tracker, _, _) =
        VaultUpdateStateTracker::find_program_address(&handler.vault_program_id, vault, ncn_epoch);

    let vault_update_state_tracker_account =
        get_account(handler, &vault_update_state_tracker).await?;

    msg!(
        "Full Vault Update for Vault: {:?} at NCN Epoch: {:?}",
        vault,
        ncn_epoch
    );

    if vault_update_state_tracker_account.is_none() {
        let initialize_vault_update_state_tracker_ix =
            InitializeVaultUpdateStateTrackerBuilder::new()
                .vault(*vault)
                .vault_update_state_tracker(vault_update_state_tracker)
                .system_program(system_program::id())
                .withdrawal_allocation_method(WithdrawalAllocationMethod::Greedy)
                .payer(payer.pubkey())
                .config(vault_config)
                .instruction();

        let result = send_and_log_transaction(
            handler,
            &[initialize_vault_update_state_tracker_ix],
            &[payer],
            "Initialize Vault Update State Tracker",
            &[
                format!("VAULT: {:?}", vault),
                format!("Vault Epoch: {:?}", ncn_epoch),
            ],
        )
        .await;

        if result.is_err() {
            log::error!(
                "Failed to initialize Vault Update State Tracker for Vault: {:?} at NCN Epoch: {:?} with error: {:?}",
                vault,
                ncn_epoch,
                result.err().unwrap()
            );
        }
    }

    // Crank Vault Update State Tracker
    let all_operators = get_all_sorted_operators_for_vault(handler, vault).await?;

    if !all_operators.is_empty() {
        let starting_index = {
            let vault_update_state_tracker_account =
                get_vault_update_state_tracker(handler, vault, ncn_epoch).await?;
            let last_updated_index = vault_update_state_tracker_account.last_updated_index();

            if last_updated_index == u64::MAX {
                ncn_epoch % all_operators.len() as u64
            } else {
                (last_updated_index + 1) % all_operators.len() as u64
            }
        };

        for index in 0..all_operators.len() {
            let current_index = (starting_index as usize + index) % all_operators.len();
            let operator = all_operators.get(current_index).unwrap();

            let (vault_operator_delegation, _, _) = VaultOperatorDelegation::find_program_address(
                &handler.vault_program_id,
                vault,
                operator,
            );

            let crank_vault_update_state_tracker_ix = CrankVaultUpdateStateTrackerBuilder::new()
                .vault(*vault)
                .operator(*operator)
                .config(vault_config)
                .vault_operator_delegation(vault_operator_delegation)
                .vault_update_state_tracker(vault_update_state_tracker)
                .instruction();

            let result = send_and_log_transaction(
                handler,
                &[crank_vault_update_state_tracker_ix],
                &[payer],
                "Crank Vault Update State Tracker",
                &[
                    format!("VAULT: {:?}", vault),
                    format!("Operator: {:?}", operator),
                    format!("Vault Epoch: {:?}", ncn_epoch),
                ],
            )
            .await;

            if result.is_err() {
                log::error!(
                "Failed to crank Vault Update State Tracker for Vault: {:?} and Operator: {:?} at NCN Epoch: {:?} with error: {:?}",
                vault,
                operator,
                ncn_epoch,
                result.err().unwrap()
            );
            }
        }
    }

    // Close Update State Tracker
    let vault_update_state_tracker_account =
        get_account(handler, &vault_update_state_tracker).await?;

    if vault_update_state_tracker_account.is_some() {
        let close_vault_update_state_tracker_ix = CloseVaultUpdateStateTrackerBuilder::new()
            .vault(*vault)
            .vault_update_state_tracker(vault_update_state_tracker)
            .payer(payer.pubkey())
            .config(vault_config)
            .ncn_epoch(ncn_epoch)
            .instruction();

        let result = send_and_log_transaction(
            handler,
            &[close_vault_update_state_tracker_ix],
            &[payer],
            "Close Vault Update State Tracker",
            &[
                format!("VAULT: {:?}", vault),
                format!("Vault Epoch: {:?}", ncn_epoch),
            ],
        )
        .await;

        if result.is_err() {
            log::error!(
                "Failed to close Vault Update State Tracker for Vault: {:?} at NCN Epoch: {:?} with error: {:?}",
                vault,
                ncn_epoch,
                result.err().unwrap()
            );
        }
    }

    Ok(())
}

pub async fn get_or_create_snapshot(handler: &CliHandler, epoch: u64) -> Result<Snapshot> {
    let ncn = *handler.ncn()?;
    let (snapshot, _, _) = Snapshot::find_program_address(&handler.ncn_program_id, &ncn);

    if get_account(handler, &snapshot)
        .await?
        .is_none_or(|snapshot| snapshot.data.len() < Snapshot::SIZE)
    {
        create_snapshot(handler, epoch).await?;
        check_created(handler, &snapshot).await?;
    }

    get_snapshot(handler, epoch).await
}

// --------------------- CRANKERS ------------------------------

pub async fn crank_register_vaults(handler: &CliHandler) -> Result<()> {
    let all_ncn_vaults = get_all_vaults_in_ncn(handler).await?;
    let vault_registry = get_or_create_vault_registry(handler).await?;
    let all_registered_vaults: Vec<Pubkey> = vault_registry
        .get_valid_vault_entries()
        .iter()
        .map(|entry| *entry.vault())
        .collect();

    let vaults_to_register: Vec<Pubkey> = all_ncn_vaults
        .iter()
        .filter(|vault| !all_registered_vaults.contains(vault))
        .copied()
        .collect();

    for vault in vaults_to_register.iter() {
        let result = register_vault(handler, vault).await;

        if let Err(err) = result {
            log::error!(
                "Failed to register vault: {:?} with error: {:?}",
                vault,
                err
            );
        }
    }

    Ok(())
}

pub async fn crank_snapshot(handler: &CliHandler, epoch: u64) -> Result<()> {
    let vault_registry = get_vault_registry(handler).await?;

    let operators = get_all_operators_in_ncn(handler).await?;
    let all_vaults: Vec<Pubkey> = vault_registry
        .get_valid_vault_entries()
        .iter()
        .map(|entry| *entry.vault())
        .collect();

    let _snapshot = get_or_create_snapshot(handler, epoch).await?;

    // Initialize operator snapshot progress tracking in epoch state
    let _ncn = *handler.ncn()?;

    let vault = &all_vaults[0];
    let result = full_vault_update(handler, vault).await;
    if let Err(err) = result {
        log::error!(
            "Failed to update the vault: {:?} with error: {:?}",
            vault,
            err
        );
    }

    for operator in operators.iter() {
        // Create Vault Operator Delegation
        {
            let result = get_operator_snapshot(handler, operator, epoch).await;

            if result.is_err() {
                log::error!(
                "Failed to get or create operator snapshot for operator: {:?} in epoch: {:?} with error: {:?}",
                operator,
                epoch,
                result.err().unwrap()
            );
                continue;
            };
        }

        let result = snapshot_vault_operator_delegation(handler, vault, operator, epoch).await;
        if let Err(err) = result {
            log::error!(
                    "Failed to snapshot vault operator delegation for vault: {:?} and operator: {:?} in epoch: {:?} with error: {:?}",
                    vault,
                    operator,
                    epoch,
                    err
                );
        }
    }

    Ok(())
}

pub async fn crank_snapshot_unupdated(
    handler: &CliHandler,
    epoch: u64,
    verbose: bool,
) -> Result<()> {
    let vault_registry = get_vault_registry(handler).await?;
    let all_vaults: Vec<Pubkey> = vault_registry
        .get_valid_vault_entries()
        .iter()
        .map(|entry| *entry.vault())
        .collect();

    if all_vaults.is_empty() {
        info!("No vaults found in registry");
        return Ok(());
    }

    let snapshot = get_snapshot(handler, epoch).await?;

    // We'll use the first vault for snapshotting (similar to crank_snapshot)
    let vault = &all_vaults[0];
    let result = full_vault_update(handler, vault).await;
    if let Err(err) = result {
        log::error!(
            "Failed to update the vault: {:?} with error: {:?}",
            vault,
            err
        );
    }

    let mut operators_to_snapshot = Vec::new();
    let mut already_snapshotted = Vec::new();

    let config = get_restaking_config(handler).await?;

    // Check which operators need snapshotting
    for operator_snapshot in snapshot.operator_snapshots().iter() {
        let last_snapshot_epoch = get_epoch(
            operator_snapshot.last_snapshot_slot(),
            config.epoch_length(),
        )?;
        // Check if it has been snapshotted this epoch
        if operator_snapshot.is_active() && last_snapshot_epoch < epoch {
            operators_to_snapshot.push(*operator_snapshot.operator());
            if verbose {
                info!(
                    "Operator {} needs snapshotting",
                    operator_snapshot.operator()
                );
            }
        } else {
            already_snapshotted.push(*operator_snapshot.operator());
            if verbose {
                info!(
                    "Operator {} already snapshotted this epoch",
                    operator_snapshot.operator()
                );
            }
        }
    }

    info!(
        "Found {} operators total: {} already snapshotted, {} need snapshotting",
        snapshot.operators_registered(),
        already_snapshotted.len(),
        operators_to_snapshot.len()
    );

    if operators_to_snapshot.is_empty() {
        info!("All operators are already snapshotted for epoch {}", epoch);
        return Ok(());
    }

    let mut successful_snapshots = 0;
    let mut failed_snapshots = 0;

    for operator in operators_to_snapshot.iter() {
        if verbose {
            info!("Processing operator: {}", operator);
        }

        let result = snapshot_vault_operator_delegation(handler, vault, operator, epoch).await;
        if let Err(err) = result {
            log::error!(
                "Failed to snapshot vault operator delegation for vault: {:?} and operator: {:?} in epoch: {:?} with error: {:?}",
                vault,
                operator,
                epoch,
                err
            );
            failed_snapshots += 1;
        } else {
            successful_snapshots += 1;
            if verbose {
                info!("Successfully snapshotted operator: {}", operator);
            }
        }
    }

    info!(
        "Snapshot operation completed: {} successful, {} failed out of {} operators that needed snapshotting",
        successful_snapshots,
        failed_snapshots,
        operators_to_snapshot.len()
    );

    Ok(())
}

// --------------------- HELPERS -------------------------

pub async fn send_and_log_transaction(
    handler: &CliHandler,
    instructions: &[Instruction],
    signing_keypairs: &[&Keypair],
    title: &str,
    log_items: &[String],
) -> Result<()> {
    sleep(Duration::from_secs(1)).await;

    let signature = send_transactions(handler, instructions, signing_keypairs).await?;

    log_transaction(title, signature, log_items);

    Ok(())
}

pub async fn send_transactions(
    handler: &CliHandler,
    instructions: &[Instruction],
    signing_keypairs: &[&Keypair],
) -> Result<Signature> {
    let client = handler.rpc_client();
    let keypair = handler.keypair()?;
    let retries = handler.retries;
    let priority_fee_micro_lamports = handler.priority_fee_micro_lamports;

    let mut all_instructions = vec![];

    all_instructions.push(ComputeBudgetInstruction::set_compute_unit_price(
        priority_fee_micro_lamports,
    ));

    all_instructions.extend_from_slice(instructions);

    for iteration in 0..retries {
        let blockhash = client.get_latest_blockhash().await?;

        // Create a vector that combines all signing keypairs
        let mut all_signers = vec![keypair];
        all_signers.extend(signing_keypairs.iter());

        let tx = Transaction::new_signed_with_payer(
            &all_instructions,
            Some(&keypair.pubkey()),
            &all_signers, // Pass the reference to the vector of keypair references
            blockhash,
        );

        let config = RpcSendTransactionConfig {
            skip_preflight: true,
            ..RpcSendTransactionConfig::default()
        };
        let result = client
            .send_and_confirm_transaction_with_spinner_and_config(&tx, client.commitment(), config)
            .await;

        if result.is_err() {
            info!(
                "Retrying transaction after {}s {}/{}",
                (1 + iteration),
                iteration,
                retries
            );

            boring_progress_bar((1 + iteration) * 1000).await;
            continue;
        }

        return Ok(result?);
    }

    // last retry
    let blockhash = client.get_latest_blockhash().await?;

    // Create a vector that combines all signing keypairs
    let mut all_signers = vec![keypair];
    all_signers.extend(signing_keypairs.iter());

    let tx = Transaction::new_signed_with_payer(
        instructions,
        Some(&keypair.pubkey()),
        &all_signers, // Pass the reference to the vector of keypair references
        blockhash,
    );

    let result = client.send_and_confirm_transaction(&tx).await;

    if let Err(e) = result {
        return Err(anyhow!("\nError: \n\n{:?}\n\n", e));
    }

    Ok(result?)
}

pub fn log_transaction(title: &str, signature: Signature, log_items: &[String]) {
    let mut log_message = format!(
        "\n\n---------- {} ----------\nSignature: {:?}",
        title, signature
    );

    for item in log_items {
        log_message.push_str(&format!("\n{}", item));
    }

    // msg!(log_message.clone());

    log_message.push('\n');
    info!("{}", log_message);
}
