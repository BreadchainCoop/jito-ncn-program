# frontend — Solana port of the live LLM demo page

Static, single-file port of <https://llm.gaskiller.xyz> (the Sepolia GasKillerLLM
demo) to the Jito NCN + gaskiller-settlement stack. Same brand system (Chakra
Petch / Space Grotesk, black/zinc/emerald/orange, hard-offset shadows), same
surface — hero, chat panel, on-chain history, Bisection Proof Lab, pipeline
explainer, program address footer — rewired from Ethereum JSON-RPC to Solana
JSON-RPC per [`docs/INTERFACES.md`](../docs/INTERFACES.md) §4.

## Serve locally

No build step. From this directory:

```sh
python3 -m http.server 8000
# open http://localhost:8000/
```

(Any static file server works. `file://` does not — `fetch("config.json")` and
`crypto.subtle` need an http(s) origin; localhost counts as a secure context.)

## config.json

The page is driven entirely by `config.json`, fetched at runtime
(`cache: no-store`, so a deploy can rewrite it without cache busting):

| field | meaning |
|---|---|
| `rpcUrl` | Solana JSON-RPC endpoint (e.g. `https://api.devnet.solana.com`) |
| `ncnProgramId` | the deployed NCN program (this repo's `program/`) |
| `settlementProgramId` | the `gaskiller-settlement` program (Track C) |
| `statePda` | the stories260K consumer's state PDA — seeds `[b"gk_state", ncn, app_id]` |
| `commitment` | RPC commitment (default `confirmed`) |
| `cluster` | explorer cluster query param (default `devnet`) |

**Population:** the devnet deploy (parity-plan Phase 5 / the deploy runbook that
emits `ncn_deploy.json`) writes these four addresses here. Until then the fields
ship empty and the page shows an explicit **"network not deployed yet"** state
everywhere — the chat panel, history, and footer all say so plainly. The page
never fabricates a chain response: every rendered story is read from a live
account and its sha256 is re-checked in the browser.

## What the page reads (INTERFACES.md §4)

- **State PDA** — `discriminator, ncn, app_id, commitment_root,
  transition_count, sim_profile_id, env_commitment, bump`. The decoder infers
  the discriminator header width (8/1/0 bytes) from the account length; the
  repo convention (`jito_bytemuck`) is byte 0 = discriminator, struct at byte 8.
- **Settle events** — self-CPIs of the settlement program found in
  `getTransaction(...).meta.innerInstructions` for signatures on the state PDA.
  Instruction data = `discriminant(8) ‖ borsh payload`. The story event
  discriminant is `sha256("gk:story_meta")[..8]` = `cca755e2a2a25bed`, payload
  `borsh { story_sha256: [u8;32], buffer: Pubkey, len: u32 }`.
- **Story buffers** — PDA `[b"gk_buffer", state_pda, transition_index.to_le]`.
  The story text is `buffer.data[..len]`, and the page verifies
  `sha256(buffer.data[..len]) == story_sha256` before labeling it verified.
  If the RPC's transaction retention no longer covers the settle txs, the page
  falls back to deriving buffer PDAs directly and labels the bytes
  **len UNVERIFIED**.

All RPC is hand-rolled JSON-RPC `fetch` (mirroring the EVM page's `rpc()`
helper). `@solana/web3.js` is loaded from a pinned CDN build
(`1.95.8`, unpkg IIFE) **only** for `PublicKey.findProgramAddressSync` in the
buffer-PDA fallback; if the CDN is unreachable the rest of the page still works.

## Honest differences vs the EVM page

- **No in-browser ask.** Sepolia's page can `eth_call`-simulate against the
  deployed contract; Solana has no analog that could replay the EVM-reference
  inference, and prompts run through the Track D producer + operator committee.
  The chat panel's button is therefore **"Read the chain"** — it reads settled
  state + story buffers, and its copy says exactly that.
- **Bisection Proof Lab** is ported intact (it was always a self-contained
  browser simulation). Gas figures are labeled **EVM reference** (stories260K,
  ~44M gas/token); Solana-side slashing execution is described as roadmap
  (pending Jito's upstream `Slash` instruction), not as shipped.
- Round-in-progress card → **Chain watch** card: polls the state PDA every 15 s
  and announces when `transition_count` advances (the chain is the ground
  truth, same as the EVM page's watcher).

## Assets

`icon.png`, `apple-icon.png`, `gk-wordmark.png`, `gk-eclipse.png`,
`gk-diagram.png` are the live site's own assets (the diagram is kept, labeled
as the EVM reference pipeline). Fonts come from Google Fonts, same as the live
page.
