//! Shared types for the `gaskiller-settlement` program (docs/INTERFACES.md §4).
//!
//! Everything here is host + sbf compatible: borsh wire types (shared byte-form
//! with Track D's payload producer and Track B's submitter), the payload digest,
//! event discriminants, PDA derivations, and the on-chain account layouts.

#![allow(unexpected_cfgs)]

pub mod buffer;
pub mod error;
pub mod instruction;
pub mod payload;
pub mod state;

pub use buffer::*;
pub use error::SettlementError;
pub use payload::*;
pub use state::GkState;
