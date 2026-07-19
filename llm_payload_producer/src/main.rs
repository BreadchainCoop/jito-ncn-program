//! CLI: turn a real Gas Killer LLM EVM simulation result into the §4/§6
//! `SettlementPayload` JSON fixture.
//!
//! Example (values from an actual `tellStory` run on anvil under the unbounded
//! gas environment — see `fixtures/tell_story_once_upon_a_time.json`):
//!
//! ```text
//! llm-payload-producer \
//!   --prompt "Once upon a time" \
//!   --story-file story.txt \
//!   --new-root 0xee0dd4fb...b7d6 \
//!   --transition-index 0 \
//!   --state-pda <32-byte hex> \
//!   --ix-discriminator <8-byte hex> \
//!   --buffer <32-byte hex> \
//!   --sim-command "anvil ... && forge script ... && cast send ... tellStory ..." \
//!   --solidity-sdk-commit <hash> \
//!   --out fixtures/tell_story_once_upon_a_time.json
//! ```

use std::path::PathBuf;

use anyhow::Context as _;
use clap::Parser;
use llm_payload_producer::{FixtureSource, ProducerInputs, make_fixture, verify_fixture};

/// Emit the §4 SettlementPayload fixture for one Gas Killer LLM state transition.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// UTF-8 prompt fed to GasKillerLLM.tellStory
    #[arg(long)]
    prompt: String,

    /// File containing the generated story bytes (UTF-8), exactly as produced by the sim
    #[arg(long)]
    story_file: PathBuf,

    /// The consumer's new commitment root (single-slot value), 32-byte hex
    #[arg(long)]
    new_root: String,

    /// The state PDA's transition_count BEFORE this transition
    #[arg(long)]
    transition_index: u64,

    /// The consumer app's state PDA, 32-byte hex
    #[arg(long)]
    state_pda: String,

    /// The settle instruction's 8-byte discriminator, hex
    #[arg(long)]
    ix_discriminator: String,

    /// The story buffer account for this transition, 32-byte hex
    #[arg(long)]
    buffer: String,

    /// The exact EVM simulation command(s) that produced the story + root
    #[arg(long)]
    sim_command: String,

    /// The gas-killer/solidity-sdk commit the simulation ran at
    #[arg(long)]
    solidity_sdk_commit: String,

    /// Output path for the JSON fixture
    #[arg(long)]
    out: PathBuf,
}

fn parse_hex<const N: usize>(name: &str, value: &str) -> anyhow::Result<[u8; N]> {
    let raw = hex::decode(value.trim_start_matches("0x"))
        .with_context(|| format!("{name}: invalid hex"))?;
    let mut out = [0u8; N];
    anyhow::ensure!(
        raw.len() == N,
        "{name}: expected {N} bytes, got {}",
        raw.len()
    );
    out.copy_from_slice(&raw);
    Ok(out)
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let story = std::fs::read(&args.story_file)
        .with_context(|| format!("reading {}", args.story_file.display()))?;

    let inputs = ProducerInputs {
        prompt: &args.prompt,
        story: &story,
        new_root: parse_hex::<32>("--new-root", &args.new_root)?,
        transition_index: args.transition_index,
        state_pda: parse_hex::<32>("--state-pda", &args.state_pda)?,
        ix_discriminator: parse_hex::<8>("--ix-discriminator", &args.ix_discriminator)?,
        buffer: parse_hex::<32>("--buffer", &args.buffer)?,
    };
    let source = FixtureSource {
        sim_command: args.sim_command,
        solidity_sdk_commit: args.solidity_sdk_commit,
    };

    let fixture = make_fixture(&inputs, source)?;
    let verified = verify_fixture(&fixture).context("self-check failed")?;

    std::fs::write(&args.out, serde_json::to_string_pretty(&fixture)? + "\n")
        .with_context(|| format!("writing {}", args.out.display()))?;

    println!("fixture written:   {}", args.out.display());
    println!("story bytes:       {}", story.len());
    println!("story_sha256_hex:  {}", fixture.story_sha256_hex);
    println!("digest_hex:        {}", fixture.digest_hex);
    println!("transition_index:  {}", verified.payload.transition_index);
    println!("new_root:          0x{}", hex::encode(verified.new_root));
    Ok(())
}
