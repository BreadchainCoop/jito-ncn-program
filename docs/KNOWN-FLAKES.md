# Known flaky integration tests (tracked)

Status 2026-07-20: three tests fail intermittently (~1-in-6 full parallel runs;
one ~1-in-12 even in isolation). Program logic is verified correct — these are
test-harness artifacts of solana-program-test's wall-clock PoH.

1. `test_remove_operator_twice_fails` — the two removals build byte-identical
   transactions; when both land under one recent blockhash the second is a
   duplicate signature and BanksClient returns the first tx's cached Ok
   (status-cache artifact). The on-chain double-removal guard itself is
   correct: tombstoned slots cannot be re-removed or double-subtracted.
2. `test_operator_snapshot_delegation_next_epoch_calculations`
   (asserts 11000, reads 1000) — DIAGNOSED (branch `test-determinism-crank`):
   the same status-cache artifact as #1, surfacing through the crank path.
   The snapshot round after `add_delegation` re-cranks the same
   (vault, operator) pair with a transaction byte-identical to the previous
   round's; when both land under one recent blockhash the second is a
   duplicate signature answered from the status cache (the first crank's
   cached Ok) without executing, so the snapshot silently keeps the stale
   pre-add delegation (1000). Slots never drift by wall clock in
   solana-program-test (only warps move the clock; the PohService loop only
   registers blockhashes), so the visible "slot race" is really a blockhash
   race. Reproduced 1-in-20 in isolation with the fresh-blockhash cursor
   disabled; not reproduced in 50+ runs with it active. Fix: the snapshot
   helper warps to a fresh slot per crank round (a warp force-registers a
   new blockhash, so cross-round cranks can never be signature duplicates)
   and verifies the vault's update state is current at the executing slot
   before each snapshot.
3. `test_multi_vault_weighted_stake_accumulation` — not reproducible in
   isolation even with the fresh-blockhash cursor disabled (20/20 clean):
   its only identical-transaction pair is the deliberate re-crank, whose
   silent duplicate-swallow left the asserted values unchanged (a vacuous
   pass, not a failure). Historical full-suite failures were load-side
   client errors on the long crank transaction chain (recent-blockhash
   expiry while preflight simulation delayed submission — mitigated by the
   non-preflight path on main). The test now warps before the re-crank and
   asserts `last_snapshot_slot` advanced, proving the upsert executed.

Partially mitigated on main (non-preflight submission + 500ms leak grace).
Systemic fixes in flight on branches `test-determinism-blockhash` (unique
blockhash per submitted tx in the shared fixture clients) and
`test-determinism-crank` (deterministic vault-update cranking).
Acceptance bar to close this file: 20 consecutive clean full-suite runs plus
20 isolation runs per listed test.
