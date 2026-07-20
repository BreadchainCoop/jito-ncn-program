//! Instruction wire format + builders for the gaskiller-settlement program.
//!
//! Encoding: `8-byte discriminator ‖ borsh(args)`. The settle payload commits
//! to the settle discriminator (`SettlementPayload.ix_discriminator`), which
//! is why this program dispatches on 8-byte discriminators rather than the
//! NCN program's borsh-enum instruction encoding. Discriminators are
//! `sha256("gk:ix:<name>")[..8]`, pinned by tests below.

use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    system_program,
};

use crate::{error::SettlementError, payload::SettlementPayload};

/// `sha256("gk:ix:initialize_state")[..8]`
pub const INITIALIZE_STATE_DISCRIMINATOR: [u8; 8] =
    [0xce, 0xcc, 0x93, 0x83, 0x20, 0x31, 0xf7, 0x2a];
/// `sha256("gk:ix:write_buffer")[..8]`
pub const WRITE_BUFFER_DISCRIMINATOR: [u8; 8] = [0x4d, 0x1a, 0xa9, 0x4f, 0x1d, 0x0c, 0x02, 0x21];
/// `sha256("gk:ix:settle")[..8]`
pub const SETTLE_DISCRIMINATOR: [u8; 8] = [0x35, 0xf8, 0xe2, 0x1f, 0x3e, 0xe1, 0x4e, 0xae];
/// `sha256("gk:ix:close_buffer")[..8]`
pub const CLOSE_BUFFER_DISCRIMINATOR: [u8; 8] = [0x9c, 0xf0, 0x8a, 0x7a, 0x98, 0xf5, 0x99, 0x96];

/// Event-authority PDA seed: the self-CPI event branch requires this PDA as a
/// signer, so only the program itself (via `invoke_signed`) can emit events.
pub const EVENT_AUTHORITY_SEED: &[u8] = b"gk_event_authority";

pub fn find_event_authority(program_id: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[EVENT_AUTHORITY_SEED], program_id)
}

/// `InitializeState { app_id, sim_profile_id, env_commitment }`
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct InitializeStateArgs {
    pub app_id: [u8; 32],
    pub sim_profile_id: [u8; 32],
    pub env_commitment: [u8; 32],
}

/// `WriteBuffer { transition_index, offset, bytes }` (+ `max_size`, only read
/// on the first write, when the buffer account is created).
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct WriteBufferArgs {
    pub transition_index: u64,
    pub offset: u32,
    pub bytes: Vec<u8>,
    /// Content capacity, used only when the buffer does not exist yet.
    pub max_size: u32,
}

/// `Settle { payload, aggregated_g2, aggregated_signature, bitmap,
/// expected_generation }` (INTERFACES.md §4/§2 wire form).
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct SettleArgs {
    pub payload: SettlementPayload,
    /// Aggregated signer G2 key, compressed (64 bytes).
    pub aggregated_g2: [u8; 64],
    /// Aggregated G1 signature, compressed (32 bytes).
    pub aggregated_signature: [u8; 32],
    /// LSB-first per operator index: byte `i>>3`, bit `i&7`, 1 = signed.
    pub operators_signature_bitmap: Vec<u8>,
    /// Snapshot generation the certificate was produced against.
    pub expected_generation: u64,
}

/// `CloseBuffer { transition_index }`
#[derive(Debug, Clone, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub struct CloseBufferArgs {
    pub transition_index: u64,
}

fn encode(discriminator: &[u8; 8], args: &impl BorshSerialize) -> Result<Vec<u8>, SettlementError> {
    let mut data = discriminator.to_vec();
    args.serialize(&mut data)
        .map_err(|_| SettlementError::SerializationError)?;
    Ok(data)
}

/// Accounts: `[state (w), ncn, payer (s, w), system_program]`
pub fn initialize_state_ix(
    program_id: &Pubkey,
    state: &Pubkey,
    ncn: &Pubkey,
    payer: &Pubkey,
    args: &InitializeStateArgs,
) -> Result<Instruction, SettlementError> {
    Ok(Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new(*state, false),
            AccountMeta::new_readonly(*ncn, false),
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data: encode(&INITIALIZE_STATE_DISCRIMINATOR, args)?,
    })
}

/// Accounts: `[state, buffer (w), payer (s, w), system_program]`
pub fn write_buffer_ix(
    program_id: &Pubkey,
    state: &Pubkey,
    buffer: &Pubkey,
    payer: &Pubkey,
    args: &WriteBufferArgs,
) -> Result<Instruction, SettlementError> {
    Ok(Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new_readonly(*state, false),
            AccountMeta::new(*buffer, false),
            AccountMeta::new(*payer, true),
            AccountMeta::new_readonly(system_program::id(), false),
        ],
        data: encode(&WRITE_BUFFER_DISCRIMINATOR, args)?,
    })
}

/// Accounts (§4 order plus the two mechanically-required self-CPI accounts):
/// `[state (w), ncn_config, ncn, snapshot, restaking_config,
///   event_authority, settlement_program, optional buffer]`
#[allow(clippy::too_many_arguments)]
pub fn settle_ix(
    program_id: &Pubkey,
    state: &Pubkey,
    ncn_config: &Pubkey,
    ncn: &Pubkey,
    snapshot: &Pubkey,
    restaking_config: &Pubkey,
    buffer: Option<&Pubkey>,
    args: &SettleArgs,
) -> Result<Instruction, SettlementError> {
    let (event_authority, _) = find_event_authority(program_id);
    let mut accounts = vec![
        AccountMeta::new(*state, false),
        AccountMeta::new_readonly(*ncn_config, false),
        AccountMeta::new_readonly(*ncn, false),
        AccountMeta::new_readonly(*snapshot, false),
        AccountMeta::new_readonly(*restaking_config, false),
        AccountMeta::new_readonly(event_authority, false),
        AccountMeta::new_readonly(*program_id, false),
    ];
    if let Some(buffer) = buffer {
        accounts.push(AccountMeta::new_readonly(*buffer, false));
    }
    Ok(Instruction {
        program_id: *program_id,
        accounts,
        data: encode(&SETTLE_DISCRIMINATOR, args)?,
    })
}

/// Accounts: `[state, buffer (w), payer (s, w)]`
pub fn close_buffer_ix(
    program_id: &Pubkey,
    state: &Pubkey,
    buffer: &Pubkey,
    payer: &Pubkey,
    args: &CloseBufferArgs,
) -> Result<Instruction, SettlementError> {
    Ok(Instruction {
        program_id: *program_id,
        accounts: vec![
            AccountMeta::new_readonly(*state, false),
            AccountMeta::new(*buffer, false),
            AccountMeta::new(*payer, true),
        ],
        data: encode(&CLOSE_BUFFER_DISCRIMINATOR, args)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha8(name: &str) -> [u8; 8] {
        let hash = solana_nostd_sha256::hashv(&[name.as_bytes()]);
        let mut out = [0u8; 8];
        out.copy_from_slice(&hash[..8]);
        out
    }

    #[test]
    fn discriminators_are_sha256_prefixes() {
        assert_eq!(
            INITIALIZE_STATE_DISCRIMINATOR,
            sha8("gk:ix:initialize_state")
        );
        assert_eq!(WRITE_BUFFER_DISCRIMINATOR, sha8("gk:ix:write_buffer"));
        assert_eq!(SETTLE_DISCRIMINATOR, sha8("gk:ix:settle"));
        assert_eq!(CLOSE_BUFFER_DISCRIMINATOR, sha8("gk:ix:close_buffer"));
    }

    #[test]
    fn discriminators_are_distinct() {
        let all = [
            INITIALIZE_STATE_DISCRIMINATOR,
            WRITE_BUFFER_DISCRIMINATOR,
            SETTLE_DISCRIMINATOR,
            CLOSE_BUFFER_DISCRIMINATOR,
            crate::payload::STORY_META_DISCRIMINANT,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i.checked_add(1).unwrap()) {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn encode_prefixes_discriminator() {
        let args = CloseBufferArgs {
            transition_index: 3,
        };
        let data = encode(&CLOSE_BUFFER_DISCRIMINATOR, &args).unwrap();
        assert_eq!(&data[..8], &CLOSE_BUFFER_DISCRIMINATOR);
        assert_eq!(&data[8..], &3u64.to_le_bytes());
    }

    #[test]
    fn settle_ix_accounts_order() {
        let program_id = Pubkey::new_unique();
        let keys: Vec<Pubkey> = (0..6).map(|_| Pubkey::new_unique()).collect();
        let args = SettleArgs {
            payload: SettlementPayload {
                transition_index: 0,
                state_pda: keys[0].to_bytes(),
                ix_discriminator: SETTLE_DISCRIMINATOR,
                updates: vec![],
            },
            aggregated_g2: [0; 64],
            aggregated_signature: [0; 32],
            operators_signature_bitmap: vec![],
            expected_generation: 0,
        };
        let ix = settle_ix(
            &program_id,
            &keys[0],
            &keys[1],
            &keys[2],
            &keys[3],
            &keys[4],
            Some(&keys[5]),
            &args,
        )
        .unwrap();
        assert_eq!(ix.accounts.len(), 8);
        assert_eq!(ix.accounts[0].pubkey, keys[0]);
        assert!(ix.accounts[0].is_writable);
        assert_eq!(ix.accounts[5].pubkey, find_event_authority(&program_id).0);
        assert_eq!(ix.accounts[6].pubkey, program_id);
        assert_eq!(ix.accounts[7].pubkey, keys[5]);
        assert_eq!(&ix.data[..8], &SETTLE_DISCRIMINATOR);
    }
}
