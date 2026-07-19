use borsh::{BorshDeserialize, BorshSerialize};
use shank::ShankInstruction;

use crate::config::ConfigAdminRole;

/// Represents all instructions supported by the NCN Program
/// Each instruction specifies the accounts it requires and any parameters
/// The instruction variants are organized into logical sections:
/// - Global: Program initialization and configuration
/// - Snapshot: Creating snapshots of validator and operator state
/// - Vote: Consensus voting mechanism
/// - Admin: Administrative operations
#[rustfmt::skip]
#[derive(Debug, BorshSerialize, BorshDeserialize, ShankInstruction)]
pub enum NCNProgramInstruction {

    // ---------------------------------------------------- //
    //                         GLOBAL                       //
    // ---------------------------------------------------- //
    /// Initialize the config account for the NCN program
    /// Sets up the basic program parameters
    #[account(0, writable, name = "config")]
    #[account(1, name = "ncn")]
    #[account(2, name = "ncn_fee_wallet")]
    #[account(3, signer, name = "ncn_admin")]
    #[account(4, name = "tie_breaker_admin")]
    #[account(5, writable, name = "account_payer")]
    #[account(6, name = "system_program")]
    InitializeConfig {
        /// Number of epochs before voting is considered stalled
        epochs_before_stall: u64,
        /// Number of epochs after consensus before accounts can be closed
        epochs_after_consensus_before_close: u64,
        /// Number of slots after consensus where voting is still valid
        valid_slots_after_consensus: u64,
        /// Minimum stake for a validator to be considered valid
        minimum_stake: u128,
        /// NCN fee basis points (bps) for the NCN program
        ncn_fee_bps: u16,
    },

    /// Initializes the vault registry account to track validator vaults
    #[account(0, name = "config")]
    #[account(1, writable, name = "vault_registry")]
    #[account(2, name = "ncn")]
    #[account(3, writable, name = "account_payer")]
    #[account(4, name = "system_program")]
    InitializeVaultRegistry,


    /// Registers a vault to the vault registry
    #[account(0, name = "config")]
    #[account(1, writable, name = "vault_registry")]
    #[account(2, name = "ncn")]
    #[account(3, name = "vault")]
    #[account(4, name = "ncn_vault_ticket")]
    RegisterVault,

    /// Registers an operator by creating an individual operator PDA
    #[account(0, name = "config")]
    #[account(1, writable, name = "ncn_operator_account")]
    #[account(2, name = "ncn")]
    #[account(3, name = "operator")]
    #[account(4, signer, name = "operator_admin")]
    #[account(5, name = "ncn_operator_state")]
    #[account(6, writable, name = "snapshot")]
    #[account(7, name = "restaking_config")]
    #[account(8, writable, name = "account_payer")]
    #[account(9, name = "system_program")]
    RegisterOperator {
        /// G1 public key (compressed, 32 bytes)
        g1_pubkey: [u8; 32],
        /// G2 public key (compressed, 64 bytes)  
        g2_pubkey: [u8; 64],
        /// BLS signature of G1 pubkey by G2 private key (uncompressed G1 point, 64 bytes)
        signature: [u8; 64],
    },

    /// Updates an operator's BLS keys in their individual operator PDA
    #[account(0, name = "config")]
    #[account(1, writable, name = "ncn_operator_account")]
    #[account(2, name = "ncn")]
    #[account(3, name = "operator")]
    #[account(4, signer, name = "operator_admin")]
    #[account(5, writable, name = "snapshot")]
    UpdateOperatorBN128Keys {
        /// New G1 public key (compressed, 32 bytes)
        g1_pubkey: [u8; 32],
        /// New G2 public key (compressed, 64 bytes)  
        g2_pubkey: [u8; 64],
        /// BLS signature of the new G1 pubkey signed by the new G2 private key (uncompressed G1 point, 64 bytes)
        signature: [u8; 64],
    },

    /// Removes an operator from the snapshot: tombstones its index, subtracts
    /// its G1 from the running APK, and bumps the snapshot generation.
    /// Signer must be the NCN admin or the operator's admin.
    #[account(0, name = "config")]
    #[account(1, name = "ncn")]
    #[account(2, name = "operator")]
    #[account(3, signer, name = "admin")]
    #[account(4, writable, name = "snapshot")]
    RemoveOperator,

    /// Updates an operator's IP address and port in their individual operator PDA
    #[account(0, name = "config")]
    #[account(1, writable, name = "ncn_operator_account")]
    #[account(2, name = "ncn")]
    #[account(3, name = "operator")]
    #[account(4, signer, name = "operator_admin")]
    UpdateOperatorIpPort {
        /// New IP address (IPv4 format, 16 bytes)
        ip_address: [u8; 4],
        /// New port (16 bytes)
        port: u16,
    },

    // ---------------------------------------------------- //
    //                       SNAPSHOT                       //
    // ---------------------------------------------------- //




    /// Initializes the Snapshot
    #[account(0, name = "ncn")]
    #[account(1, writable, name = "snapshot")]
    #[account(2, writable, name = "account_payer")]
    #[account(3, name = "system_program")]
    InitializeSnapshot{},

    /// Reallocates the snapshot account to its full size
    #[account(0, name = "ncn")]
    #[account(1, name = "config")]
    #[account(2, writable, name = "snapshot")]
    #[account(3, writable, name = "account_payer")]
    #[account(4, name = "system_program")]
    ReallocSnapshot {},

    /// Snapshots the vault operator delegation
    #[account(0, name = "config")]
    #[account(1, name = "restaking_config")]
    #[account(2, name = "ncn")]
    #[account(3, name = "operator")]
    #[account(4, name = "vault")]
    #[account(5, name = "vault_ncn_ticket")]
    #[account(6, name = "ncn_vault_ticket")]
    #[account(7, name = "ncn_operator_state")]
    #[account(8, name = "vault_operator_delegation")]
    #[account(9, writable, name = "snapshot")]
    SnapshotVaultOperatorDelegation{},

    // ---------------------------------------------------- //
    //                        VERIFY                        //
    // ---------------------------------------------------- //
    /// Stateless certificate verification (docs/INTERFACES.md par.2).
    /// Mutates nothing; succeeds iff the aggregate BLS signature over `digest`
    /// carries >= Config.consensus_threshold_bps of the snapshot's total stake
    /// at `expected_generation`.
    #[account(0, name = "ncn_config")]
    #[account(1, name = "ncn")]
    #[account(2, name = "snapshot")]
    #[account(3, name = "restaking_config")]
    VerifyCertificate {
        /// The signed 32-byte message digest
        digest: [u8; 32],
        /// Aggregate G2 public key of the signers (compressed, 64 bytes)
        aggregated_g2: [u8; 64],
        /// Aggregate G1 signature (compressed, 32 bytes)
        aggregated_signature: [u8; 32],
        /// LSB-first signer bitmap per operator index (byte i>>3, bit i&7, 1 = signed)
        operators_signature_bitmap: Vec<u8>,
        /// Snapshot operator-set generation the certificate was assembled against
        expected_generation: u64,
    },


    // ---------------------------------------------------- //
    //                        ADMIN                         //
    // ---------------------------------------------------- //
    /// Updates NCN Config parameters
    #[account(0, writable, name = "config")]
    #[account(1, name = "ncn")]
    #[account(2, signer, name = "ncn_admin")]
    AdminSetParameters {
        starting_valid_epoch: Option<u64>,
        epochs_before_stall: Option<u64>,
        epochs_after_consensus_before_close: Option<u64>,
        valid_slots_after_consensus: Option<u64>,
        minimum_stake: Option<u128>,
        /// Stake-weighted consensus threshold in bps of total snapshot stake (1..=10000)
        consensus_threshold_bps: Option<u16>,
    },


    /// Sets a new secondary admin for the NCN
    #[account(0, writable, name = "config")]
    #[account(1, name = "ncn")]
    #[account(2, signer, name = "ncn_admin")]
    #[account(3, name = "new_admin")]
    AdminSetNewAdmin {
        role: ConfigAdminRole,
    },

    /// Registers a new ST mint in the Vault Registry
    #[account(0, name = "config")]
    #[account(1, name = "ncn")]
    #[account(2, name = "st_mint")]
    #[account(3, writable, name = "vault_registry")]
    #[account(4, signer, writable, name = "admin")]
    AdminRegisterStMint{ },
}
