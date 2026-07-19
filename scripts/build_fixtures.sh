#!/usr/bin/env bash
#
# build_fixtures.sh — build the Jito restaking + vault fixture programs FROM SOURCE
# instead of trusting the checked-in binaries under integration_tests/tests/fixtures/.
#
# What it does:
#   1. Clones https://github.com/jito-foundation/restaking at a pinned release tag
#      into a cache directory (reused across runs; safe to cache in CI).
#   2. Builds jito_restaking_program.so and jito_vault_program.so with cargo-build-sbf.
#   3. Copies the resulting .so files over the checked-in fixtures.
#
# Requirements: git, cargo-build-sbf (Agave/Solana toolchain 4.1.x).
#
# Env overrides:
#   JITO_RESTAKING_TAG        pinned tag to build (default below)
#   JITO_FIXTURE_CACHE_DIR    where to clone/build (default: ~/.cache/jito-restaking-fixtures)
#
# NOTE ON THE PIN: v1.0.0-cli is the latest release tag on jito-foundation/restaking
# (2025-05-01, commit 448ed84; program workspace version 0.0.5). It is nominally a CLI
# release, but it is the newest tagged tree and its program sources supersede v0.0.4
# (2024-12-23, workspace version 0.0.3). Be aware that this workspace's Rust deps track
# the (since-deleted) `v2.1-upgrade` branch of the same repo pinned in Cargo.lock at
# 358fbc3c (2025-02-11), so the on-chain fixture binaries and the client crates are not
# built from the same commit. If account-layout drift ever breaks integration tests,
# change JITO_RESTAKING_TAG/JITO_RESTAKING_COMMIT here (single source of truth).

set -euo pipefail

# ---------------------------------------------------------------------------
# Pinned source of the fixture programs
# ---------------------------------------------------------------------------
JITO_RESTAKING_TAG="${JITO_RESTAKING_TAG:-v1.0.0-cli}"
# Expected commit for the default tag; guards against tag rewrites. Set to "" to skip
# the check (required when overriding JITO_RESTAKING_TAG without updating this).
JITO_RESTAKING_COMMIT="${JITO_RESTAKING_COMMIT-448ed840dbed8522b15b8179082106c39b167bdd}"
JITO_RESTAKING_REPO_URL="${JITO_RESTAKING_REPO_URL:-https://github.com/jito-foundation/restaking.git}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$SCRIPT_DIR")"
FIXTURES_DIR="$REPO_ROOT/integration_tests/tests/fixtures"

CACHE_ROOT="${JITO_FIXTURE_CACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/jito-restaking-fixtures}"
SRC_DIR="$CACHE_ROOT/restaking-$JITO_RESTAKING_TAG"

# ---------------------------------------------------------------------------
# Toolchain
# ---------------------------------------------------------------------------
SOLANA_BIN="$HOME/.local/share/solana/install/active_release/bin"
if ! command -v cargo-build-sbf >/dev/null 2>&1; then
  if [[ -x "$SOLANA_BIN/cargo-build-sbf" ]]; then
    export PATH="$SOLANA_BIN:$PATH"
  else
    echo "error: cargo-build-sbf not found on PATH and not in $SOLANA_BIN" >&2
    echo "Install the Agave toolchain, e.g.:" >&2
    echo '  sh -c "$(curl -sSfL https://release.anza.xyz/v4.1.1/install)"' >&2
    exit 1
  fi
fi
echo "Using $(cargo-build-sbf --version 2>&1 | head -n1)"

# ---------------------------------------------------------------------------
# Clone (or reuse cached clone) at the pinned tag
# ---------------------------------------------------------------------------
clone_pinned() {
  rm -rf "$SRC_DIR"
  mkdir -p "$CACHE_ROOT"
  git clone --depth 1 --branch "$JITO_RESTAKING_TAG" "$JITO_RESTAKING_REPO_URL" "$SRC_DIR"
}

if [[ -d "$SRC_DIR/.git" ]]; then
  echo "Reusing cached clone at $SRC_DIR"
else
  echo "Cloning $JITO_RESTAKING_REPO_URL @ $JITO_RESTAKING_TAG"
  clone_pinned
fi

HEAD_COMMIT="$(git -C "$SRC_DIR" rev-parse HEAD)"
if [[ -n "$JITO_RESTAKING_COMMIT" && "$HEAD_COMMIT" != "$JITO_RESTAKING_COMMIT" ]]; then
  echo "Cached clone HEAD $HEAD_COMMIT != pinned commit $JITO_RESTAKING_COMMIT; re-cloning"
  clone_pinned
  HEAD_COMMIT="$(git -C "$SRC_DIR" rev-parse HEAD)"
  if [[ "$HEAD_COMMIT" != "$JITO_RESTAKING_COMMIT" ]]; then
    echo "error: tag $JITO_RESTAKING_TAG resolves to $HEAD_COMMIT, expected $JITO_RESTAKING_COMMIT" >&2
    echo "The upstream tag may have moved. Update JITO_RESTAKING_COMMIT after verifying." >&2
    exit 1
  fi
fi
echo "Building fixtures from restaking @ $JITO_RESTAKING_TAG ($HEAD_COMMIT)"

# ---------------------------------------------------------------------------
# Build the two fixture programs
# ---------------------------------------------------------------------------
build_program() {
  local manifest="$1"
  echo "==> cargo-build-sbf --manifest-path $manifest"
  (cd "$SRC_DIR" && cargo-build-sbf --manifest-path "$manifest")
}

build_program restaking_program/Cargo.toml
build_program vault_program/Cargo.toml

DEPLOY_DIR="$SRC_DIR/target/deploy"
for so in jito_restaking_program.so jito_vault_program.so; do
  if [[ ! -f "$DEPLOY_DIR/$so" ]]; then
    echo "error: expected build artifact $DEPLOY_DIR/$so not found" >&2
    exit 1
  fi
done

# ---------------------------------------------------------------------------
# Install over the checked-in fixtures, reporting size deltas
# ---------------------------------------------------------------------------
file_size() { wc -c <"$1" | tr -d ' '; }

mkdir -p "$FIXTURES_DIR"
for so in jito_restaking_program.so jito_vault_program.so; do
  new="$DEPLOY_DIR/$so"
  dst="$FIXTURES_DIR/$so"
  if [[ -f "$dst" ]]; then
    old_size="$(file_size "$dst")"
  else
    old_size=0
  fi
  new_size="$(file_size "$new")"
  cp "$new" "$dst"
  echo "installed $so -> $dst"
  echo "  previous: $old_size bytes | built from source: $new_size bytes | delta: $((new_size - old_size)) bytes"
done

echo "Fixture build complete (restaking @ $JITO_RESTAKING_TAG / $HEAD_COMMIT)."
