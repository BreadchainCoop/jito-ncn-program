//! Buffer PDA helpers — the DA channel for large payloads (INTERFACES.md §4).
//!
//! Layout contract: the staged content occupies `data[..content_len]` starting
//! at offset 0 (NO header — settle verifies `sha256(buffer.data[..len]) ==
//! story_sha256` against the raw account bytes). Program metadata lives in a
//! fixed-size TRAILER at the end of the account data:
//! `payer: Pubkey (32) ‖ max_content: u32 LE (4)`.
//!
//! Accounts created/grown via CPI are capped at `MAX_PERMITTED_DATA_INCREASE`
//! (10,240 bytes) per instruction, so buffers larger than that are grown
//! incrementally by successive `WriteBuffer` calls toward
//! `max_content + BUFFER_TRAILER_LEN`; the trailer is relocated to the new
//! end on each growth step (rent for the full target is funded at creation).

use solana_program::pubkey::Pubkey;

use crate::error::SettlementError;

/// Buffer PDA seed prefix: `[b"gk_buffer", state_pda, transition_index_le]`.
pub const GK_BUFFER_SEED: &[u8] = b"gk_buffer";

/// Trailer: rent payer pubkey (32) + max content length (4).
pub const BUFFER_TRAILER_LEN: usize = 36;

/// Max stageable content (Solana account cap minus the trailer).
pub const MAX_BUFFER_CONTENT_LEN: usize = 10 * 1024 * 1024 - BUFFER_TRAILER_LEN;

pub fn buffer_seeds(state_pda: &Pubkey, transition_index: u64) -> Vec<Vec<u8>> {
    vec![
        GK_BUFFER_SEED.to_vec(),
        state_pda.to_bytes().to_vec(),
        transition_index.to_le_bytes().to_vec(),
    ]
}

pub fn find_buffer_program_address(
    program_id: &Pubkey,
    state_pda: &Pubkey,
    transition_index: u64,
) -> (Pubkey, u8, Vec<Vec<u8>>) {
    let seeds = buffer_seeds(state_pda, transition_index);
    let (address, bump) = Pubkey::find_program_address(
        &seeds.iter().map(|s| s.as_slice()).collect::<Vec<_>>(),
        program_id,
    );
    (address, bump, seeds)
}

/// Content region length for a buffer account of `data_len` total bytes.
pub fn buffer_content_len(data_len: usize) -> Result<usize, SettlementError> {
    data_len
        .checked_sub(BUFFER_TRAILER_LEN)
        .ok_or(SettlementError::InvalidBufferBounds)
}

/// Reads the trailer: `(payer, max_content)`.
pub fn read_buffer_trailer(data: &[u8]) -> Result<(Pubkey, u32), SettlementError> {
    let start = buffer_content_len(data.len())?;
    let trailer = data
        .get(start..)
        .ok_or(SettlementError::InvalidBufferBounds)?;
    let payer: [u8; 32] = trailer
        .get(..32)
        .and_then(|s| s.try_into().ok())
        .ok_or(SettlementError::InvalidBufferBounds)?;
    let max_content: [u8; 4] = trailer
        .get(32..)
        .and_then(|s| s.try_into().ok())
        .ok_or(SettlementError::InvalidBufferBounds)?;
    Ok((
        Pubkey::new_from_array(payer),
        u32::from_le_bytes(max_content),
    ))
}

/// Writes the trailer (`payer`, `max_content`) at the end of `data`.
pub fn write_buffer_trailer(
    data: &mut [u8],
    payer: &Pubkey,
    max_content: u32,
) -> Result<(), SettlementError> {
    let start = buffer_content_len(data.len())?;
    let trailer = data
        .get_mut(start..)
        .ok_or(SettlementError::InvalidBufferBounds)?;
    trailer
        .get_mut(..32)
        .ok_or(SettlementError::InvalidBufferBounds)?
        .copy_from_slice(payer.as_ref());
    trailer
        .get_mut(32..)
        .ok_or(SettlementError::InvalidBufferBounds)?
        .copy_from_slice(&max_content.to_le_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_seeds_bind_state_and_index() {
        let state = Pubkey::new_unique();
        let program_id = Pubkey::new_unique();
        let (a0, _, seeds) = find_buffer_program_address(&program_id, &state, 0);
        let (a1, _, _) = find_buffer_program_address(&program_id, &state, 1);
        let (b0, _, _) = find_buffer_program_address(&program_id, &Pubkey::new_unique(), 0);
        assert_ne!(a0, a1);
        assert_ne!(a0, b0);
        assert_eq!(seeds[0], GK_BUFFER_SEED.to_vec());
        assert_eq!(seeds[2], 0u64.to_le_bytes().to_vec());
    }

    #[test]
    fn trailer_roundtrip() {
        let payer = Pubkey::new_unique();
        let mut data = vec![0u8; 100 + BUFFER_TRAILER_LEN];
        write_buffer_trailer(&mut data, &payer, 5000).unwrap();
        let (read_payer, max_content) = read_buffer_trailer(&data).unwrap();
        assert_eq!(read_payer, payer);
        assert_eq!(max_content, 5000);
        assert_eq!(buffer_content_len(data.len()).unwrap(), 100);
        // content region untouched
        assert!(data[..100].iter().all(|b| *b == 0));
    }

    #[test]
    fn trailer_bounds_are_checked() {
        let short = vec![0u8; BUFFER_TRAILER_LEN - 1];
        assert_eq!(
            buffer_content_len(short.len()),
            Err(SettlementError::InvalidBufferBounds)
        );
        assert!(read_buffer_trailer(&short).is_err());
    }
}
