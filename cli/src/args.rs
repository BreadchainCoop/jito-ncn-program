use std::fmt;

use clap::{Parser, Subcommand, ValueEnum};
use solana_sdk::clock::DEFAULT_SLOTS_PER_EPOCH;

#[derive(Parser)]
#[command(author, version, about = "A CLI for creating and managing the ncn program", long_about = None)]
pub struct Args {
    #[command(subcommand)]
    pub command: ProgramCommand,

    #[arg(
        long,
        global = true,
        env = "RPC_URL",
        default_value = "https://api.mainnet-beta.solana.com",
        help = "RPC URL to use"
    )]
    pub rpc_url: String,

    #[arg(
        long,
        global = true,
        env = "COMMITMENT",
        default_value = "confirmed",
        help = "Commitment level"
    )]
    pub commitment: String,

    #[arg(
        long,
        global = true,
        env = "PRIORITY_FEE_MICRO_LAMPORTS",
        default_value_t = 1,
        help = "Priority fee in micro lamports"
    )]
    pub priority_fee_micro_lamports: u64,

    #[arg(
        long,
        global = true,
        env = "TRANSACTION_RETRIES",
        default_value_t = 0,
        help = "Amount of times to retry a transaction"
    )]
    pub transaction_retries: u64,

    #[arg(
        long,
        global = true,
        env = "NCN_PROGRAM_ID",
        default_value_t = ncn_program::id().to_string(),
        help = "NCN program ID"
    )]
    pub ncn_program_id: String,

    #[arg(
        long,
        global = true,
        env = "RESTAKING_PROGRAM_ID",
        default_value_t = jito_restaking_program::id().to_string(),
        help = "Restaking program ID"
    )]
    pub restaking_program_id: String,

    #[arg(
        long,
        global = true,
        env = "VAULT_PROGRAM_ID", 
        default_value_t = jito_vault_program::id().to_string(),
        help = "Vault program ID"
    )]
    pub vault_program_id: String,

    #[arg(
        long,
        global = true,
        env = "TOKEN_PROGRAM_ID",
        default_value_t = spl_token::id().to_string(),
        help = "Token Program ID"
    )]
    pub token_program_id: String,

    #[arg(long, global = true, env = "NCN", help = "NCN Account Address")]
    pub ncn: Option<String>,

    #[arg(long, env = "VAULT", help = "Vault Account Address")]
    pub vault: String,

    #[arg(
        long,
        global = true,
        env = "EPOCH",
        help = "Epoch - defaults to current epoch"
    )]
    pub epoch: Option<u64>,

    #[arg(long, global = true, env = "KEYPAIR_PATH", help = "keypair path")]
    pub keypair_path: Option<String>,

    #[arg(long, global = true, help = "Verbose mode")]
    pub verbose: bool,

    #[arg(long, global = true, hide = true)]
    pub markdown_help: bool,
}

#[derive(Subcommand)]
pub enum ProgramCommand {
    /// NCN Keeper
    RunKeeper {
        #[arg(
            long,
            env,
            default_value_t = 600_000, // 10 minutes
            help = "Maximum time in milliseconds between keeper loop iterations"
        )]
        loop_timeout_ms: u64,
        #[arg(
            long,
            env,
            default_value_t = 10_000, // 10 seconds
            help = "Timeout in milliseconds when an error occurs before retrying"
        )]
        error_timeout_ms: u64,
    },

    /// Crank Functions
    CrankRegisterVaults {},
    CrankSnapshot {},
    CrankSnapshotUnupdated {
        #[arg(long, help = "Show detailed progress information")]
        verbose: bool,
    },

    /// Admin
    AdminCreateConfig {
        #[arg(long, help = "Ncn Fee Wallet Address")]
        ncn_fee_wallet: String,
        #[arg(long, help = "Ncn Fee bps")]
        ncn_fee_bps: u64,

        #[arg(long, default_value_t = 10 as u64, help = "Epochs before tie breaker can set consensus")]
        epochs_before_stall: u64,
        #[arg(long, default_value_t = (DEFAULT_SLOTS_PER_EPOCH as f64 * 0.1) as u64, help = "Valid slots after consensus")]
        valid_slots_after_consensus: u64,
        #[arg(
            long,
            default_value_t = 10,
            help = "Epochs after consensus before accounts can be closed"
        )]
        epochs_after_consensus_before_close: u64,
        #[arg(long, help = "Tie breaker admin address")]
        tie_breaker_admin: Option<String>,
        #[arg(long, help = "Minimum stake required for operators (in lamports)")]
        minimum_stake: u128,
    },
    AdminRegisterStMint {},

    AdminSetTieBreaker {
        #[arg(long, help = "tie breaker for voting")]
        weather_status: u8,
    },
    AdminSetParameters {
        #[arg(long, help = "Epochs before tie breaker can set consensus")]
        epochs_before_stall: Option<u64>,
        #[arg(long, help = "Epochs after consensus before accounts can be closed")]
        epochs_after_consensus_before_close: Option<u64>,
        #[arg(long, help = "Slots to which voting is allowed after consensus")]
        valid_slots_after_consensus: Option<u64>,
        #[arg(long, help = "Starting valid epoch")]
        starting_valid_epoch: Option<u64>,
    },
    AdminSetNewAdmin {
        #[arg(long, help = "New admin address")]
        new_admin: String,
        #[arg(long, help = "Set tie breaker admin")]
        set_tie_breaker_admin: bool,
    },
    AdminFundAccountPayer {
        #[arg(long, help = "Amount of SOL to fund")]
        amount_in_sol: f64,
    },

    /// Instructions
    CreateVaultRegistry,

    RegisterVault {},

    RegisterOperator {
        #[arg(long, help = "Operator address")]
        operator: String,
        #[arg(
            long,
            help = "G1 public key (32 bytes as hex string) - auto-generated if not provided"
        )]
        g1_pubkey: Option<String>,
        #[arg(
            long,
            help = "G2 public key (64 bytes as hex string) - auto-generated if not provided"
        )]
        g2_pubkey: Option<String>,
        #[arg(
            long,
            help = "BLS signature (64 bytes as hex string) - auto-generated if not provided (deprecated, will be auto-generated)"
        )]
        signature: Option<String>,
        #[arg(
            long,
            help = "Path to save/load BLS keys JSON file",
            default_value = "bls-keys.json"
        )]
        keys_file: String,
    },

    UpdateOperatorIpPort {
        #[arg(long, help = "Operator address")]
        operator: String,
        #[arg(
            long,
            help = "IPv4 address in dotted decimal notation (e.g., 192.168.1.100)"
        )]
        ip_address: String,
        #[arg(long, help = "Port number for the port")]
        port: u16,
    },

    CreateSnapshot,

    SnapshotVaultOperatorDelegation {
        #[arg(long, help = "Operator address")]
        operator: String,
    },

    /// Verify a BLS certificate over a message digest (stateless; mutates nothing)
    VerifyCertificate {
        #[arg(long, help = "Message digest the certificate signs (32 bytes hex)")]
        digest: String,
        #[arg(long, help = "Aggregated G1 signature (32 bytes hex, compressed)")]
        aggregated_signature: String,
        #[arg(long, help = "Aggregated G2 public key (64 bytes hex, compressed)")]
        aggregated_g2: String,
        #[arg(long, help = "Bitmap indicating which operators signed (hex string)")]
        signers_bitmap: String,
        #[arg(
            long,
            help = "Snapshot generation the certificate was assembled against (defaults to the current snapshot generation)"
        )]
        expected_generation: Option<u64>,
    },

    /// Generate a BLS signature over a message digest for certificate aggregation
    GenerateVoteSignature {
        #[arg(long, help = "Operator private key (32 bytes hex)")]
        private_key: String,
        #[arg(long, help = "Message digest to sign (32 bytes hex)")]
        message: String,
    },

    /// Aggregate multiple BLS signatures for voting
    AggregateSignatures {
        #[arg(long, help = "Comma-separated list of signatures (64 bytes hex each)")]
        signatures: String,
        #[arg(
            long,
            help = "Comma-separated list of G1 public keys (32 bytes hex each)"
        )]
        g1_public_keys: String,
        #[arg(
            long,
            help = "Comma-separated list of G2 public keys (64 bytes hex each)"
        )]
        g2_public_keys: String,
        #[arg(long, help = "Bitmap indicating which operators signed (hex string)")]
        signers_bitmap: String,
    },

    /// Getters
    GetNcn,
    GetNcnOperatorState {
        #[arg(long, env = "OPERATOR", help = "Operator Account Address")]
        operator: String,
    },
    GetVaultNcnTicket {},
    GetNcnVaultTicket {},
    GetVaultOperatorDelegation {
        #[arg(long, env = "OPERATOR", help = "Operator Account Address")]
        operator: String,
    },
    GetAllTickets,
    GetAllOperatorsInNcn,
    GetAllVaultsInNcn,
    GetAllNCNOperatorAccounts,
    GetNCNProgramConfig,
    GetVaultRegistry,

    GetSnapshot,
    GetOperatorSnapshot {
        #[arg(long, env = "OPERATOR", help = "Operator Account Address")]
        operator: String,
    },
    GetAccountPayer,
    GetTotalEpochRentCost,

    GetOperatorStakes,
    GetVaultStakes,
    GetVaultOperatorStakes,

    FullUpdateVault,
}

#[rustfmt::skip]
impl fmt::Display for Args {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "\n NCN Program CLI Configuration")?;
        writeln!(f, "═══════════════════════════════════════")?;

        // Network Configuration
        writeln!(f, "\n📡 Network Settings:")?;
        writeln!(f, "  • RPC URL:     {}", self.rpc_url)?;
        writeln!(f, "  • Commitment:  {}", self.commitment)?;

        // Program IDs
        writeln!(f, "\n🔑 Program IDs:")?;
        writeln!(f, "  • NCN Program:       {}", self.ncn_program_id)?;
        writeln!(f, "  • Restaking:         {}", self.restaking_program_id)?;
        writeln!(f, "  • Vault program:     {}", self.vault_program_id)?;
        writeln!(f, "  • Token:             {}", self.token_program_id)?;

        // Solana Settings
        writeln!(f, "\n◎  Solana Settings:")?;
        writeln!(f, "  • Keypair Path:  {}", self.keypair_path.as_deref().unwrap_or("Not Set"))?;
        writeln!(f, "  • NCN:  {}", self.ncn.as_deref().unwrap_or("Not Set"))?;
        writeln!(f, "  • Epoch: {}", self.epoch.map_or_else(|| "Current".to_string(), |e| e.to_string()))?;

        // Optional Settings
        writeln!(f, "\n⚙️  Additional Settings:")?;
        writeln!(f, "  • Verbose Mode:  {}", if self.verbose { "Enabled" } else { "Disabled" })?;
        writeln!(f, "  • Markdown Help: {}", if self.markdown_help { "Enabled" } else { "Disabled" })?;

        writeln!(f, "\n")?;

        Ok(())
    }
}

#[derive(ValueEnum, Debug, Clone)]
pub enum Cluster {
    Mainnet,
    Testnet,
    Localnet,
}

impl fmt::Display for Cluster {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Mainnet => write!(f, "mainnet"),
            Self::Testnet => write!(f, "testnet"),
            Self::Localnet => write!(f, "localnet"),
        }
    }
}
