#![allow(clippy::integer_division)]
use std::{collections::HashMap, mem::size_of, str::FromStr};

use crate::{
    args::{Args, ProgramCommand},
    getters::{
        get_account_payer, get_all_ncn_operator_accounts, get_all_operators_in_ncn,
        get_all_tickets, get_all_vaults_in_ncn, get_ncn, get_ncn_operator_state,
        get_ncn_program_config, get_ncn_vault_ticket, get_operator_snapshot, get_snapshot,
        get_total_epoch_rent_cost, get_vault_ncn_ticket, get_vault_operator_delegation,
        get_vault_registry,
    },
    instructions::{
        admin_create_config, admin_fund_account_payer, admin_register_st_mint, admin_set_new_admin,
        admin_set_parameters, crank_register_vaults, crank_snapshot, crank_snapshot_unupdated,
        create_snapshot, create_vault_registry, full_vault_update, register_operator,
        register_vault, remove_operator, snapshot_vault_operator_delegation,
        update_operator_ip_port, verify_certificate,
    },
    keeper::keeper_loop::startup_ncn_keeper,
};
use anyhow::{anyhow, Result};
use base64::{engine::general_purpose, Engine};
use log::info;
use ncn_program_core::account_payer::AccountPayer;
use solana_account_decoder::{UiAccountEncoding, UiDataSliceConfig};
use solana_client::{
    rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
    rpc_filter::{Memcmp, MemcmpEncodedBytes, RpcFilterType},
};
use solana_rpc_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::{
    commitment_config::CommitmentConfig,
    native_token::lamports_to_sol,
    pubkey::Pubkey,
    signature::{read_keypair_file, Keypair},
};

pub struct CliHandler {
    pub rpc_url: String,
    pub commitment: CommitmentConfig,
    pub keypair: Option<Keypair>,
    pub restaking_program_id: Pubkey,
    pub vault_program_id: Pubkey,
    pub ncn_program_id: Pubkey,
    pub token_program_id: Pubkey,
    pub ncn: Option<Pubkey>,
    pub vault: Pubkey,
    pub epoch: u64,
    pub rpc_client: RpcClient,
    pub retries: u64,
    pub priority_fee_micro_lamports: u64,
}

impl CliHandler {
    pub async fn from_args(args: &Args) -> Result<Self> {
        let rpc_url = args.rpc_url.clone();
        CommitmentConfig::confirmed();

        let commitment = CommitmentConfig::from_str(&args.commitment)?;

        let keypair = match &args.keypair_path {
            Some(path) => Some(
                read_keypair_file(path)
                    .map_err(|e| anyhow!("Failed to read keypair file: {}", e))?,
            ),
            None => None,
        };

        let restaking_program_id = Pubkey::from_str(&args.restaking_program_id)?;

        let vault_program_id = Pubkey::from_str(&args.vault_program_id)?;

        let ncn_program_id = Pubkey::from_str(&args.ncn_program_id)?;

        let token_program_id = Pubkey::from_str(&args.token_program_id)?;

        let ncn = args
            .ncn
            .clone()
            .map(|id| Pubkey::from_str(&id))
            .transpose()?;

        let vault = Pubkey::from_str(&args.vault.clone())?;

        let rpc_client = RpcClient::new_with_commitment(rpc_url.clone(), commitment);

        let mut handler = Self {
            rpc_url,
            commitment,
            keypair,
            restaking_program_id,
            vault_program_id,
            ncn_program_id,
            token_program_id,
            ncn,
            vault,
            epoch: u64::MAX,
            rpc_client,
            retries: args.transaction_retries,
            priority_fee_micro_lamports: args.priority_fee_micro_lamports,
        };

        handler.epoch = {
            if let Some(epoch) = args.epoch {
                epoch
            } else {
                let client = handler.rpc_client();
                let epoch_info = client.get_epoch_info().await?;
                epoch_info.epoch
            }
        };

        Ok(handler)
    }

    pub const fn rpc_client(&self) -> &RpcClient {
        &self.rpc_client
    }

    pub fn get_rpc_program_accounts_with_config<T: jito_bytemuck::Discriminator>(
        &self,
        account_pubkey: &Pubkey,
    ) -> anyhow::Result<RpcProgramAccountsConfig> {
        let data_size = size_of::<T>() + 8;
        let encoded_discriminator = general_purpose::STANDARD.encode(account_pubkey.to_bytes());
        let size_filter = RpcFilterType::DataSize(data_size as u64);
        let ncn_filter = RpcFilterType::Memcmp(Memcmp::new(
            8,                                                 // offset
            MemcmpEncodedBytes::Base64(encoded_discriminator), // encoded bytes
        ));

        let config = RpcProgramAccountsConfig {
            filters: Some(vec![size_filter, ncn_filter]),
            account_config: RpcAccountInfoConfig {
                encoding: Some(UiAccountEncoding::Base64),
                data_slice: Some(UiDataSliceConfig {
                    offset: 0,
                    length: data_size,
                }),
                commitment: Some(self.commitment),
                min_context_slot: None,
            },
            with_context: Some(false),
            sort_results: Some(false),
        };

        Ok(config)
    }

    pub fn keypair(&self) -> Result<&Keypair> {
        self.keypair.as_ref().ok_or_else(|| anyhow!("No keypair"))
    }

    pub fn ncn(&self) -> Result<&Pubkey> {
        self.ncn.as_ref().ok_or_else(|| anyhow!("No NCN address"))
    }

    pub fn vault(&self) -> &Pubkey {
        &self.vault
    }

    #[allow(clippy::large_stack_frames)]
    pub async fn handle(&self, action: ProgramCommand) -> Result<()> {
        match action {
            // Keepers
            // Ncn Keeper
            ProgramCommand::RunKeeper {
                loop_timeout_ms,
                error_timeout_ms,
            } => startup_ncn_keeper(self, loop_timeout_ms, error_timeout_ms).await,

            // Cranks
            ProgramCommand::CrankRegisterVaults {} => crank_register_vaults(self).await,

            ProgramCommand::CrankSnapshot {} => crank_snapshot(self, self.epoch).await,

            ProgramCommand::CrankSnapshotUnupdated { verbose } => {
                crank_snapshot_unupdated(self, self.epoch, verbose).await
            }

            // Admin
            ProgramCommand::AdminCreateConfig {
                ncn_fee_wallet,
                ncn_fee_bps,
                epochs_before_stall,
                valid_slots_after_consensus,
                epochs_after_consensus_before_close,
                tie_breaker_admin,
                minimum_stake,
            } => {
                let tie_breaker = if let Some(admin_str) = tie_breaker_admin {
                    Some(
                        Pubkey::from_str(&admin_str)
                            .map_err(|e| anyhow!("Error parsing tie breaker admin: {}", e))?,
                    )
                } else {
                    None
                };

                let ncn_fee_wallet = Pubkey::from_str(&ncn_fee_wallet)
                    .map_err(|e| anyhow!("Error parsing NCN fee wallet: {}", e))?;

                admin_create_config(
                    self,
                    ncn_fee_wallet,
                    ncn_fee_bps.try_into()?,
                    tie_breaker,
                    epochs_before_stall,
                    valid_slots_after_consensus,
                    epochs_after_consensus_before_close,
                    minimum_stake,
                )
                .await
            }
            ProgramCommand::AdminRegisterStMint { weight_bps } => {
                admin_register_st_mint(self, self.vault(), weight_bps).await
            }
            ProgramCommand::AdminSetTieBreaker { weather_status: _ } => {
                // Tie breaker functionality has been removed
                log::info!("Tie breaker functionality has been removed");
                Ok(())
            }
            ProgramCommand::AdminSetParameters {
                epochs_before_stall,
                epochs_after_consensus_before_close,
                valid_slots_after_consensus,
                starting_valid_epoch,
            } => {
                admin_set_parameters(
                    self,
                    epochs_before_stall,
                    epochs_after_consensus_before_close,
                    valid_slots_after_consensus,
                    starting_valid_epoch,
                )
                .await?;
                let config = get_ncn_program_config(self).await?;
                info!("\n\n--- Parameters Set ---\nepochs_before_stall: {}\nepochs_after_consensus_before_close: {}\nvalid_slots_after_consensus: {}\nstarting_valid_epoch: {}\n",
                    config.epochs_before_stall(),
                    config.epochs_after_consensus_before_close(),
                    config.valid_slots_after_consensus(),
                    config.starting_valid_epoch()
                );

                Ok(())
            }
            ProgramCommand::AdminSetNewAdmin {
                new_admin,
                set_tie_breaker_admin,
            } => {
                let new_admin = Pubkey::from_str(&new_admin)
                    .map_err(|e| anyhow!("Error parsing new admin: {}", e))?;
                admin_set_new_admin(self, &new_admin, set_tie_breaker_admin).await
            }
            ProgramCommand::AdminFundAccountPayer { amount_in_sol } => {
                admin_fund_account_payer(self, amount_in_sol).await
            }

            // Instructions
            ProgramCommand::CreateVaultRegistry => create_vault_registry(self).await,

            ProgramCommand::RegisterVault {} => register_vault(self, self.vault()).await,

            ProgramCommand::RemoveOperator { operator } => {
                let operator = Pubkey::from_str(&operator)
                    .map_err(|e| anyhow!("Error parsing operator: {}", e))?;
                remove_operator(self, &operator).await
            }

            ProgramCommand::RegisterOperator {
                operator,
                g1_pubkey,
                g2_pubkey,
                signature,
                keys_file,
            } => {
                let operator = Pubkey::from_str(&operator)
                    .map_err(|e| anyhow!("Error parsing operator: {}", e))?;

                // Handle BLS key generation/loading vs manual input
                let (g1_array, g2_array, sig_array) = match (g1_pubkey, g2_pubkey, signature) {
                    // All keys provided manually
                    (Some(g1), Some(g2), Some(sig)) => {
                        use crate::bls_keys::hex_to_bytes;
                        (
                            hex_to_bytes::<32>(&g1)?,
                            hex_to_bytes::<64>(&g2)?,
                            hex_to_bytes::<64>(&sig)?,
                        )
                    }
                    // G1 and G2 pubkeys provided, generate signature
                    (Some(g1), Some(g2), None) => {
                        use crate::bls_keys::{generate_or_use_keys, generate_signature};

                        let key_set = generate_or_use_keys(
                            &operator,
                            &keys_file,
                            Some(g1.as_str()),
                            Some(g2.as_str()),
                        )?;
                        let signature = generate_signature(&key_set, self.ncn()?)?;

                        (key_set.g1_pubkey, key_set.g2_pubkey, signature)
                    }
                    // No keys provided - generate random keys
                    (None, None, None) => {
                        use crate::bls_keys::{generate_or_use_keys, generate_signature};

                        let key_set = generate_or_use_keys(&operator, &keys_file, None, None)?;
                        let signature = generate_signature(&key_set, self.ncn()?)?;

                        (key_set.g1_pubkey, key_set.g2_pubkey, signature)
                    }
                    // Partial keys provided - not allowed
                    _ => {
                        return Err(anyhow!(
                            "Either provide G1 and G2 pubkeys (--g1-pubkey, --g2-pubkey) or none for auto-generation. Signature is auto-generated."
                        ));
                    }
                };

                register_operator(self, &operator, g1_array, g2_array, sig_array).await
            }

            ProgramCommand::UpdateOperatorIpPort {
                operator,
                ip_address,
                port,
            } => {
                let operator = Pubkey::from_str(&operator)
                    .map_err(|e| anyhow!("Error parsing operator: {}", e))?;

                update_operator_ip_port(self, &operator, &ip_address, port).await
            }

            ProgramCommand::CreateSnapshot => create_snapshot(self, self.epoch).await,
            ProgramCommand::SnapshotVaultOperatorDelegation { operator } => {
                let operator = Pubkey::from_str(&operator)
                    .map_err(|e| anyhow!("Error parsing operator: {}", e))?;
                snapshot_vault_operator_delegation(self, self.vault(), &operator, self.epoch).await
            }

            ProgramCommand::VerifyCertificate {
                digest,
                aggregated_signature,
                aggregated_g2,
                signers_bitmap,
                expected_generation,
            } => {
                use crate::bls_keys::hex_to_bytes;

                let digest_bytes = hex_to_bytes::<32>(&digest)?;
                let agg_sig = hex_to_bytes::<32>(&aggregated_signature)?;
                let apk2 = hex_to_bytes::<64>(&aggregated_g2)?;
                let bitmap_bytes = hex::decode(&signers_bitmap)
                    .map_err(|e| anyhow!("Error parsing signers bitmap: {}", e))?;

                let expected_generation = match expected_generation {
                    Some(generation) => generation,
                    None => get_snapshot(self, self.epoch).await?.generation(),
                };

                verify_certificate(
                    self,
                    digest_bytes,
                    agg_sig,
                    apk2,
                    bitmap_bytes,
                    expected_generation,
                )
                .await
            }

            ProgramCommand::GenerateVoteSignature {
                private_key,
                message,
            } => {
                use crate::bls_keys::{generate_signature_from_private_key, hex_to_bytes};

                let priv_key_bytes = hex_to_bytes::<32>(&private_key)?;
                let message_bytes = hex_to_bytes::<32>(&message)?;

                let signature =
                    generate_signature_from_private_key(&priv_key_bytes, &message_bytes)?;
                info!("Generated signature: {}", hex::encode(signature));
                Ok(())
            }

            ProgramCommand::AggregateSignatures {
                signatures,
                g1_public_keys,
                g2_public_keys,
                signers_bitmap,
            } => {
                use crate::bls_keys::aggregate_signatures_and_keys;

                let result = aggregate_signatures_and_keys(
                    &signatures,
                    &g1_public_keys,
                    &g2_public_keys,
                    &signers_bitmap,
                )?;

                info!(
                    "Aggregated signature: {}",
                    hex::encode(result.aggregated_signature)
                );
                info!(
                    "Aggregated G2 public key: {}",
                    hex::encode(result.aggregated_g2)
                );
                info!("Signers bitmap: {}", hex::encode(&result.signers_bitmap));
                Ok(())
            }

            // Getters
            ProgramCommand::GetNcn => {
                let ncn = get_ncn(self).await?;
                info!("NCN: {:?}", ncn);
                Ok(())
            }
            ProgramCommand::GetNcnOperatorState { operator } => {
                let operator = Pubkey::from_str(&operator)
                    .map_err(|e| anyhow!("Error parsing operator: {}", e))?;
                let ncn_operator_state = get_ncn_operator_state(self, &operator).await?;
                info!("NCN Operator State: {:?}", ncn_operator_state);
                Ok(())
            }
            ProgramCommand::GetVaultNcnTicket {} => {
                let ncn_ticket = get_vault_ncn_ticket(self, self.vault()).await?;
                info!("Vault NCN Ticket: {:?}", ncn_ticket);
                Ok(())
            }
            ProgramCommand::GetNcnVaultTicket {} => {
                let ncn_ticket = get_ncn_vault_ticket(self, self.vault()).await?;
                info!("NCN Vault Ticket: {:?}", ncn_ticket);
                Ok(())
            }
            ProgramCommand::GetVaultOperatorDelegation { operator } => {
                let operator = Pubkey::from_str(&operator)
                    .map_err(|e| anyhow!("Error parsing operator: {}", e))?;

                let vault_operator_delegation =
                    get_vault_operator_delegation(self, self.vault(), &operator).await?;

                info!("Vault Operator Delegation: {:?}", vault_operator_delegation);
                Ok(())
            }
            ProgramCommand::GetAllOperatorsInNcn => {
                let operators = get_all_operators_in_ncn(self).await?;

                info!("Operators: {:?}", operators);
                Ok(())
            }
            ProgramCommand::GetAllVaultsInNcn => {
                let vaults = get_all_vaults_in_ncn(self).await?;
                info!("Vaults: {:?}", vaults);
                Ok(())
            }
            ProgramCommand::GetAllNCNOperatorAccounts => {
                let ncn_operator_accounts = get_all_ncn_operator_accounts(self).await?;

                info!(
                    "Found {} NCN Operator Accounts:",
                    ncn_operator_accounts.len()
                );
                for (address, account) in ncn_operator_accounts.iter() {
                    info!("NCN Operator Account Address: {}", address);
                    info!("  NCN: {}", account.ncn());
                    info!("  Operator: {}", account.operator_pubkey());
                    info!("  G1 Pubkey: {}", hex::encode(account.g1_pubkey()));
                    info!("  G2 Pubkey: {}", hex::encode(account.g2_pubkey()));
                    info!("  NCN Operator Index: {}", account.ncn_operator_index());
                    info!("  Slot Registered: {}", account.slot_registered());
                    info!("  Bump: {}", account.bump);
                    info!("op: {}", account);
                    info!("---");
                }
                Ok(())
            }
            ProgramCommand::GetAllTickets => {
                let all_tickets = get_all_tickets(self).await?;

                for tickets in all_tickets.iter() {
                    info!("Tickets: {}", tickets);
                }

                Ok(())
            }
            ProgramCommand::GetNCNProgramConfig => {
                let config = get_ncn_program_config(self).await?;
                info!("{}", config);
                Ok(())
            }
            ProgramCommand::GetVaultRegistry => {
                let vault_registry = get_vault_registry(self).await?;
                info!("{}", vault_registry);
                Ok(())
            }

            ProgramCommand::GetSnapshot => {
                let snapshot = get_snapshot(self, self.epoch).await?;
                info!("{}", snapshot);
                Ok(())
            }
            ProgramCommand::GetOperatorSnapshot { operator } => {
                let operator = Pubkey::from_str(&operator)
                    .map_err(|e| anyhow!("Error parsing operator: {}", e))?;
                let operator_snapshot = get_operator_snapshot(self, &operator, self.epoch).await?;
                info!("{}", operator_snapshot);
                Ok(())
            }
            ProgramCommand::GetAccountPayer => {
                let account_payer = get_account_payer(self).await?;
                let (account_payer_address, _, _) =
                    AccountPayer::find_program_address(&self.ncn_program_id, self.ncn()?);
                info!(
                    "\n\n--- Account Payer ---\n{}\nBalance: {}\n",
                    account_payer_address,
                    lamports_to_sol(account_payer.lamports)
                );
                Ok(())
            }
            ProgramCommand::GetTotalEpochRentCost => {
                let total_epoch_rent_cost = get_total_epoch_rent_cost(self).await?;
                info!(
                    "\n\n--- Total Epoch Rent Cost ---\nCost: {}\n",
                    lamports_to_sol(total_epoch_rent_cost)
                );
                Ok(())
            }

            ProgramCommand::GetOperatorStakes => {
                // Get snapshot for total stake
                let snapshot = get_snapshot(self, self.epoch).await?;

                let operators = get_all_operators_in_ncn(self).await?;
                // For each fully activated operator, get their operator snapshot
                let mut operator_stakes = Vec::new();
                for operator in operators.iter() {
                    let operator_snapshot = get_operator_snapshot(self, operator, self.epoch).await;
                    if let Ok(operator_snapshot) = operator_snapshot {
                        operator_stakes
                            .push((operator, operator_snapshot.stake_weight().stake_weight()));
                    } else if let Err(e) = operator_snapshot {
                        log::warn!("Failed to get operator snapshot for {}: {}", operator, e);
                    }
                }

                // Sort operator stakes by stake weight descending
                operator_stakes.sort_by(|(_, a), (_, b)| b.cmp(a));

                for (operator, stake_weight) in operator_stakes.iter() {
                    println!(
                        "Operator: {}, Stake Weight: {}.{:02}%",
                        operator,
                        stake_weight * 10000 / snapshot.minimum_stake().stake_weight() / 100,
                        stake_weight * 10000 / snapshot.minimum_stake().stake_weight() % 100
                    );
                }

                Ok(())
            }

            ProgramCommand::GetVaultStakes => {
                let operators = get_all_operators_in_ncn(self).await?;
                let snapshot = get_snapshot(self, self.epoch).await?;
                let vault_stakes: HashMap<Pubkey, u128> = HashMap::new();
                for operator in operators.iter() {
                    let operator_snapshot = get_operator_snapshot(self, operator, self.epoch).await;
                    if let Ok(operator_snapshot) = operator_snapshot {
                        // Note: This functionality needs to be reimplemented based on the actual data structure
                        // For now, we'll use the operator's total stake weight
                        let total_stake_weight = operator_snapshot.stake_weight().stake_weight();
                        if total_stake_weight > 0 {
                            // This is a simplified approach - in reality, you'd need to get vault-specific weights
                            // from the vault operator delegations
                            log::warn!(
                                "Vault-specific stake weights not implemented in this version"
                            );
                        }
                    } else if let Err(e) = operator_snapshot {
                        log::warn!("Failed to get operator snapshot for {}: {}", operator, e);
                    }
                }

                let mut vault_stakes = vault_stakes.into_iter().collect::<Vec<_>>();
                vault_stakes.sort_by(|(_, a), (_, b)| b.cmp(a));

                for (vault, stake_weight) in vault_stakes.iter() {
                    println!(
                        "Vault: {}, Stake Weight: {}.{:02}%",
                        vault,
                        stake_weight * 10000 / snapshot.minimum_stake().stake_weight() / 100,
                        stake_weight * 10000 / snapshot.minimum_stake().stake_weight() % 100
                    );
                }

                Ok(())
            }

            ProgramCommand::GetVaultOperatorStakes => {
                let operators = get_all_operators_in_ncn(self).await?;
                let snapshot = get_snapshot(self, self.epoch).await?;
                let vault_operator_stakes: HashMap<Pubkey, HashMap<Pubkey, u128>> = HashMap::new();

                // Collect stakes for each vault-operator pair
                for operator in operators.iter() {
                    let operator_snapshot = get_operator_snapshot(self, operator, self.epoch).await;
                    if let Ok(operator_snapshot) = operator_snapshot {
                        // Note: This functionality needs to be reimplemented based on the actual data structure
                        // For now, we'll use the operator's total stake weight
                        let total_stake_weight = operator_snapshot.stake_weight().stake_weight();
                        if total_stake_weight > 0 {
                            // This is a simplified approach - in reality, you'd need to get vault-specific weights
                            // from the vault operator delegations
                            log::warn!(
                                "Vault-specific stake weights not implemented in this version"
                            );
                        }
                    } else if let Err(e) = operator_snapshot {
                        log::warn!("Failed to get operator snapshot for {}: {}", operator, e);
                    }
                }

                // Calculate total stake weight for percentage calculations
                let total_stake_weight = snapshot.minimum_stake().stake_weight();

                // Sort vaults by total stake
                let mut vaults: Vec<_> = vault_operator_stakes.iter().collect();
                vaults.sort_by(|(_, a_ops), (_, b_ops)| {
                    let a_total: u128 = a_ops.values().sum();
                    let b_total: u128 = b_ops.values().sum();
                    b_total.cmp(&a_total)
                });

                for (vault, operator_stakes) in vaults {
                    let vault_total: u128 = operator_stakes.values().sum();
                    if vault_total == 0 {
                        continue;
                    }
                    println!(
                        "Vault: {}, % of Total Stake: {}.{:02}%",
                        vault,
                        vault_total * 10000 / total_stake_weight / 100,
                        vault_total * 10000 / total_stake_weight % 100
                    );

                    let mut operators: Vec<_> = operator_stakes.iter().collect();
                    operators.sort_by(|(_, a), (_, b)| b.cmp(a));

                    for (operator, stake) in operators {
                        if *stake == 0 {
                            continue;
                        }
                        println!(
                            "  Operator: {}, Stake: {}.{:02}%",
                            operator,
                            stake * 10000 / vault_total / 100,
                            stake * 10000 / vault_total % 100
                        );
                    }
                    println!();
                }

                Ok(())
            }
            ProgramCommand::FullUpdateVault => {
                full_vault_update(self, self.vault()).await?;
                Ok(())
            }
        }
    }
}
