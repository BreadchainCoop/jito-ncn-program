# Cross-track interface contract

Frozen 2026-07-19 for the parallel build. Every track builds against THIS
document; changes require updating this file on `main` first. The signature
domain (§1) is already implemented on `main` in `ncn-program-core` — import it,
do not reimplement.

## 1. Signature domain (implemented, frozen)

- `MessageDigest([u8; 32])` — the only signable input (`core::schemes`).
- Hash-to-curve: `Sha256Normalized` — sha256(`HASH_TO_CURVE_DOMAIN` ‖ digest ‖ counter),
  reject-and-retry above `NORMALIZE_MODULUS`, x-candidate decompress.
  `HASH_TO_CURVE_DOMAIN = b"JITO-NCN-BN254-G1-SHA256NORM-V01"`.
- Certificate challenge: `compute_certificate_gamma(digest, apk1, apk_g2, sigma)`
  = keccak256 mod Fr, byte-exact to eigenlayer-middleware BLSSignatureChecker.sol#L214.
- Registration PoP: sign `pop_message_digest(ncn, operator, g1_compressed)`;
  program verifies via `verify_operator_registeration` (pop gamma).
- Certificate wire form: `agg_sig` = G1 compressed 32B, `agg_g2` = G2 compressed 64B,
  `bitmap` = LSB-first per operator index, byte `i>>3`, bit `i&7`, 1 = signed.
  Non-signers are subtracted from the snapshot's running APK on-chain.

## 2. VerifyCertificate (Phase 1 target; replaces CastVote)

Instruction args (borsh, shank-generated):
```
digest: [u8; 32]
aggregated_g2: [u8; 64]        // compressed
aggregated_signature: [u8; 32] // compressed
operators_signature_bitmap: Vec<u8>
expected_generation: u64
```
Accounts: `[ncn_config, ncn, snapshot, restaking_config]` — ALL read-only.
STATELESS: mutates nothing. Checks, in order: bitmap length vs
`operators_registered`; snapshot `generation == expected_generation`;
snapshot slot-freshness (`last_snapshot_slot` within `Config.valid_slots_after_consensus`
of current slot); per-signer minimum stake; signed stake (bps of total snapshot
stake) `>= Config.consensus_threshold_bps` (NEW config field, admin-settable,
default 6667); single challenge-combined pairing via
`verify_aggregated_signature::<Sha256Normalized>`.
Also exposed as `pub fn verify_certificate_readonly(...)` in the processor
module so the settlement program can call it inline (D-VERIFY option B) —
keep the core verification in `ncn-program-core` so both programs share it.
Errors keep the existing NCNProgramError variants; add
`SnapshotGenerationMismatch`, `InsufficientStakeBps`.
The demo VoteCounter + CastVote move OUT of the NCN program (deleted there);
the counter demo returns later as a separate consumer if needed.

## 3. Snapshot changes (Phase 1)

- `generation: u64` — bumped on operator register, remove, key rotation.
  Certificates verify only against their generation.
- `RemoveOperator` instruction (ncn admin or operator admin): tombstones the
  operator's index (index NOT reused within the same epoch), subtracts its G1
  from the running APK, bumps generation.
- Multi-vault: `MAX_VAULTS` 1 → 16, `MAX_ST_MINTS` 1 → 16; `VaultRegistry`
  entries carry `weight_bps: u16` (sums need not be 10_000; weights scale
  per-vault delegation into stake weight). Snapshot accumulates per-operator
  stake across vaults: `sum(delegation_i * weight_bps_i / 10_000)`.
- Epoch staleness: unchanged semantics (has_minimum_stake_now decay).

## 4. gaskiller-settlement program (Track C)

New workspace crates: `settlement_program/` (program), types in
`settlement_core/` (borsh, shared with off-chain).

State PDA (per consumer app): seeds `[b"gk_state", ncn, app_id: [u8;32]]`:
```
discriminator, ncn: Pubkey, app_id: [u8;32],
commitment_root: [u8;32], transition_count: u64,
sim_profile_id: [u8;32], env_commitment: [u8;32], bump
```

Payload + digest (MUST match Track D exactly):
```rust
#[derive(BorshSerialize, BorshDeserialize)]
pub enum StateUpdate {            // enum tag = 1 byte borsh
    Store { data: [u8; 32] },     // 0: the new commitment root
    Event { discriminant: [u8; 8], payload: Vec<u8> }, // 1
}
#[derive(BorshSerialize, BorshDeserialize)]
pub struct SettlementPayload {
    pub transition_index: u64,
    pub state_pda: [u8; 32],
    pub ix_discriminator: [u8; 8],   // settle ix's 8-byte discriminator
    pub updates: Vec<StateUpdate>,
}
// digest = sha256(borsh(SettlementPayload))  -> MessageDigest
```

Settle instruction: accounts `[state_pda (writable), ncn_config, ncn, snapshot,
restaking_config, optional buffer]` + args `{payload: SettlementPayload,
aggregated_g2, aggregated_signature, bitmap, expected_generation}`. Flow:
recompute digest from payload; require `payload.transition_index + 1 ==
state.transition_count + 1` (i.e. `transition_index == state.transition_count`);
require `payload.state_pda == state_pda.key`; inline-verify the certificate
(§2 readonly path); apply: exactly ONE write — `commitment_root =` the single
`Store.data` (exactly one Store required); emit each `Event` via self-CPI
(instruction data = `discriminant ‖ payload`, ≤10KiB); `transition_count += 1`.

Large payloads (LLM story): the story does NOT ride the transaction. Event
`discriminant = sha256("gk:story_meta")[..8]`, `payload = borsh { story_sha256:
[u8;32], buffer: Pubkey, len: u32 }`. Buffer account: PDA
`[b"gk_buffer", state_pda, transition_index.to_le_bytes()]`, created/appended by
untrusted writer ixs before settle; settle (when a story_meta event is present)
requires the buffer account, verifies `sha256(buffer.data[..len]) == story_sha256`.
Buffer stays open (retain-until-indexed); close ix refunds rent to payer, callable
by payer any time after settle.

## 5. Router backend (Track B, commonware-restaking branch `jito-backend`)

- New crates additive: `jito/` (peer of `eigenlayer/`), scheme in
  `core/src/jito_bn254/` or `jito/src/scheme.rs` — implements
  `commonware_cryptography::certificate::Scheme`; sign/verify MUST call into
  `ncn-program-core` (git dependency on BreadchainCoop/jito-ncn-program `main`)
  for hash-to-curve + gamma — no reimplementation.
- QuorumInfo: operators from `getProgramAccounts` memcmp on NCNOperatorAccount
  (ncn field), sockets from ip/port fields; stake + APK + generation from the
  Snapshot PDA. All reads at `confirmed` minimum.
- JitoSubmitter: certificate -> §2 VerifyCertificate ix (standalone demo) or
  §4 settle ix (settlement handler trait). Resolution{Executed} only at
  `finalized`; blockhash expiry => rebuild and resend.
- Startup quorum reconciliation: assert min total stake over all (N−f)-sized
  signer subsets >= consensus_threshold_bps of total, else refuse to start.
- Zero deletions of the EVM path; PR stays additive.

## 6. LLM task producer (Track D)

Runs the REAL EVM simulation (gas-killer/solidity-sdk tooling, revm/foundry,
UnboundedV1 profile) of `GasKillerLLM.tellStory(prompt, maxNewTokens)`, then:
EVM sketch -> `SettlementPayload`: `Store{data = new commitment root}` (the
single-slot consumer pattern: the root IS the commitment slot value);
story bytes -> buffer content + `story_meta` event (§4); emits JSON fixture
`{prompt, story, payload_borsh_base64, digest_hex, story_sha256}` consumed by
Track B/C tests. Deliverable = crate `llm-payload-producer` + >=1 checked-in
REAL fixture generated by an actual run (record the sim command + commit of
the solidity-sdk used).

## 7. Repos / branches

| Track | Repo | Branch | Merges into |
|---|---|---|---|
| A Phase 1 | BreadchainCoop/jito-ncn-program | `phase1-dmsg` | main (first) |
| B Router | BreadchainCoop/commonware-restaking | `jito-backend` | upstream PR (draft) |
| C Settlement | BreadchainCoop/jito-ncn-program | `settlement-program` | main (rebase after A) |
| D Producer | BreadchainCoop/jito-ncn-program | `llm-producer` | main |
| E Frontend | BreadchainCoop/jito-ncn-program | `frontend` | main |
| F Devnet warmup | (on-chain devnet only) | — | .context + docs/DEVNET.md |

Conflicts: C rebases on A for snapshot layout; everything else is file-disjoint.
Coordinator (main session) owns all merges to `main`.
