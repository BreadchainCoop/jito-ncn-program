//! CLI: turn a real Gas Killer LLM EVM simulation result into the §4/§6
//! `SettlementPayload` JSON fixture.
//!
//! Two modes (`--mode`, default `story`):
//!
//! - `story` (§4/§6): a `tellStory` run — `Store{root}` + a `story_meta` event
//!   whose bytes ride a buffer account.
//!
//!   ```text
//!   llm-payload-producer \
//!     --prompt "Once upon a time" --story-file story.txt \
//!     --new-root 0xee0dd4fb...b7d6 --transition-index 0 \
//!     --state-pda <hex> --ix-discriminator <hex> --buffer <hex> \
//!     --sim-command "..." --solidity-sdk-commit <hash> \
//!     --out fixtures/tell_story_once_upon_a_time.json
//!   ```
//!
//! - `qwen` (§8): a real Qwen3-0.6B chat answer — `Store{commitment_root}` + a
//!   `qwen_answer` event carrying the prompt/answer token ids (inline, no
//!   buffer). The answer ids come from an actual sharded engine run.
//!
//!   ```text
//!   llm-payload-producer --mode qwen \
//!     --prompt "What is the capital of France?" \
//!     --prompt-ids 151644,872,... --answer-ids 785,6722,... \
//!     --answer-text "The capital of France is Paris." \
//!     --manifest 0x23216cb9...c4a7ae9 --new-root 0x<chat-root> \
//!     --transition-index 0 --state-pda <hex> --ix-discriminator <hex> \
//!     --sim-command "sharded_infer.py --real ..." --solidity-sdk-commit <hash> \
//!     --out fixtures/qwen06_capital_of_france.json
//!   ```

use std::path::PathBuf;

use anyhow::{Context as _, bail};
use clap::Parser;
use llm_payload_producer::{
    FixtureSource, ProducerInputs, QwenFixtureSource, QwenInputs, make_fixture, make_qwen_fixture,
    verify_fixture, verify_qwen_fixture,
};

/// Emit the §4/§8 SettlementPayload fixture for one Gas Killer LLM state
/// transition.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Payload flavour: `story` (§4, buffered) or `qwen` (§8, inline ids).
    #[arg(long, default_value = "story")]
    mode: String,

    /// UTF-8 prompt (story: fed to tellStory; qwen: pre-template chat prompt).
    #[arg(long)]
    prompt: String,

    // --- story mode ---
    /// [story] File with the generated story bytes (UTF-8).
    #[arg(long)]
    story_file: Option<PathBuf>,
    /// [story] The story buffer account for this transition, 32-byte hex.
    #[arg(long)]
    buffer: Option<String>,

    // --- qwen mode ---
    /// [qwen] Comma-separated prompt token ids.
    #[arg(long)]
    prompt_ids: Option<String>,
    /// [qwen] Comma-separated answer token ids (from the real engine run).
    #[arg(long)]
    answer_ids: Option<String>,
    /// [qwen] The engine's detokenized answer text.
    #[arg(long)]
    answer_text: Option<String>,
    /// [qwen] Overlay manifest, 32-byte hex.
    #[arg(long)]
    manifest: Option<String>,
    /// [qwen] Model tag: 0 = qwen3-0.6b, 1 = qwen3.5-35b.
    #[arg(long, default_value_t = 0)]
    model: u8,

    // --- shared ---
    /// The consumer's new commitment root (single-slot value), 32-byte hex.
    #[arg(long)]
    new_root: String,
    /// The state PDA's transition_count BEFORE this transition.
    #[arg(long)]
    transition_index: u64,
    /// The consumer app's state PDA, 32-byte hex.
    #[arg(long)]
    state_pda: String,
    /// The settle instruction's 8-byte discriminator, hex.
    #[arg(long)]
    ix_discriminator: String,
    /// The exact simulation command that produced the answer/story.
    #[arg(long)]
    sim_command: String,
    /// The gas-killer/solidity-sdk commit the simulation ran at.
    #[arg(long)]
    solidity_sdk_commit: String,
    /// Output path for the JSON fixture.
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

fn parse_u32_list(name: &str, value: &str) -> anyhow::Result<Vec<u32>> {
    value
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<u32>()
                .with_context(|| format!("{name}: bad u32 `{s}`"))
        })
        .collect()
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    match args.mode.as_str() {
        "story" => run_story(args),
        "qwen" => run_qwen(args),
        other => bail!("unknown --mode `{other}` (expected `story` or `qwen`)"),
    }
}

fn run_story(args: Args) -> anyhow::Result<()> {
    let story_file = args
        .story_file
        .context("story mode requires --story-file")?;
    let buffer = args.buffer.context("story mode requires --buffer")?;
    let story =
        std::fs::read(&story_file).with_context(|| format!("reading {}", story_file.display()))?;

    let inputs = ProducerInputs {
        prompt: &args.prompt,
        story: &story,
        new_root: parse_hex::<32>("--new-root", &args.new_root)?,
        transition_index: args.transition_index,
        state_pda: parse_hex::<32>("--state-pda", &args.state_pda)?,
        ix_discriminator: parse_hex::<8>("--ix-discriminator", &args.ix_discriminator)?,
        buffer: parse_hex::<32>("--buffer", &buffer)?,
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

fn run_qwen(args: Args) -> anyhow::Result<()> {
    let prompt_ids = parse_u32_list(
        "--prompt-ids",
        &args.prompt_ids.context("qwen mode requires --prompt-ids")?,
    )?;
    let answer_ids = parse_u32_list(
        "--answer-ids",
        &args.answer_ids.context("qwen mode requires --answer-ids")?,
    )?;
    let answer_text = args
        .answer_text
        .context("qwen mode requires --answer-text")?;
    let manifest = parse_hex::<32>(
        "--manifest",
        &args.manifest.context("qwen mode requires --manifest")?,
    )?;
    anyhow::ensure!(!answer_ids.is_empty(), "answer-ids must be non-empty");
    anyhow::ensure!(
        answer_ids.len() <= 24,
        "answer rides the event inline; {} ids exceeds the 24-token cap (§8)",
        answer_ids.len()
    );

    let inputs = QwenInputs {
        prompt: &args.prompt,
        model: args.model,
        prompt_ids,
        answer_ids,
        manifest,
        new_root: parse_hex::<32>("--new-root", &args.new_root)?,
        transition_index: args.transition_index,
        state_pda: parse_hex::<32>("--state-pda", &args.state_pda)?,
        ix_discriminator: parse_hex::<8>("--ix-discriminator", &args.ix_discriminator)?,
    };
    let source = QwenFixtureSource {
        cmd: args.sim_command,
        sdk_commit: args.solidity_sdk_commit,
    };

    let fixture = make_qwen_fixture(&inputs, &answer_text, source)?;
    let verified = verify_qwen_fixture(&fixture).context("self-check failed")?;

    std::fs::write(&args.out, serde_json::to_string_pretty(&fixture)? + "\n")
        .with_context(|| format!("writing {}", args.out.display()))?;

    println!("qwen fixture:      {}", args.out.display());
    println!("model:             {}", verified.answer.model);
    println!(
        "prompt_ids:        {} tokens",
        verified.answer.prompt_ids.len()
    );
    println!("answer_ids:        {:?}", verified.answer.answer_ids);
    println!("answer_text:       {}", fixture.answer_text);
    println!("commitment_root:   0x{}", hex::encode(verified.new_root));
    println!("manifest:          0x{}", fixture.manifest);
    println!("digest_hex:        {}", fixture.digest_hex);
    println!("transition_index:  {}", verified.payload.transition_index);
    Ok(())
}
