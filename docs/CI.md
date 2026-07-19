# CI

`.github/workflows/ci.yml` runs two jobs on pushes to `main`, all PRs, and manual dispatch.

## lint

- `cargo fmt --check` over the six first-party packages with `--style-edition 2021`.
  - Why not `--all`? The `commonware-avs-router-solana` submodule is formatted with
    style edition 2024, while this repo's tree is formatted 2021-style. The workspace
    declares `edition = "2024"`, which would otherwise flip rustfmt into 2024 import
    ordering and flag ~46 files that are fine under the convention the tree actually
    uses. If you rename or add a workspace crate, update the `-p` list in the workflow.
- `cargo clippy --workspace --exclude ncn-program-bls-router -- -D warnings
  -D clippy::arithmetic_side_effects -D clippy::integer_division`.
  - `ncn-program-bls-router` (the `commonware-avs-router-solana` submodule) is excluded:
    it is third-party-managed and not held to this repo's lint bar.

## test

1. Installs the Agave toolchain pinned by `AGAVE_VERSION` (same 4.1.x family as local dev)
   via the `release.anza.xyz` installer.
2. Runs `scripts/build_fixtures.sh`, which builds `jito_restaking_program.so` and
   `jito_vault_program.so` **from source** at the release tag pinned by
   `JITO_RESTAKING_TAG` (also the default at the top of the script) and installs them
   over `integration_tests/tests/fixtures/*.so`, so tests never trust the checked-in
   binaries. The clone + build is cached with `actions/cache` keyed on the tag and the
   Agave version.
3. Builds the NCN program for SBF into the fixtures dir:
   `cargo build-sbf --manifest-path program/Cargo.toml --sbf-out-dir integration_tests/tests/fixtures`.
4. Runs `SBF_OUT_DIR=integration_tests/tests/fixtures cargo nextest run -p ncn-program-integration-tests`
   (setting `SBF_OUT_DIR` is what makes `test_builder.rs` load the real `.so` programs
   instead of in-process processors).

## Submodule checkout

`.gitmodules` pins SSH URLs. Both jobs rewrite `git@github.com:` to `https://github.com/`
and init only `commonware-avs-router-solana` — it is a workspace member, so no cargo
command can even load the workspace manifest without it. (`local-test-validator` is not
needed by CI.)

## Bumping the fixture pin

Edit `JITO_RESTAKING_TAG` in the workflow env **and** `JITO_RESTAKING_TAG` /
`JITO_RESTAKING_COMMIT` at the top of `scripts/build_fixtures.sh` (the commit check
guards against upstream tag rewrites). Note the workspace's `jito-*` Rust crates are
pinned in `Cargo.lock` to the deleted upstream branch `v2.1-upgrade`
(commit `358fbc3c`), so the fixture binaries and the client crates are not built from
the same commit; if account-layout drift breaks integration tests, that is the first
place to look.
