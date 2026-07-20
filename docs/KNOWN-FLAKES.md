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
   (asserts 11000, reads 1000) and
3. `test_multi_vault_weighted_stake_accumulation` — vault-delegation crank vs
   slot-clock races.

Partially mitigated on main (non-preflight submission + 500ms leak grace).
Systemic fixes in flight on branches `test-determinism-blockhash` (unique
blockhash per submitted tx in the shared fixture clients) and
`test-determinism-crank` (deterministic vault-update cranking).
Acceptance bar to close this file: 20 consecutive clean full-suite runs plus
20 isolation runs per listed test.
