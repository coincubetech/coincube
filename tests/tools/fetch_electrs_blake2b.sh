#!/usr/bin/env bash
# Build the BLAKE2b-aware Esplora indexer for the BTCB2 regtest harness:
# retropex/electrs, branch `mempool`, commit 4453cac ("Support BLAKE2b"), the
# indexer Connect runs for Bitcoin Blake2b (company-brain
# plans/bitcoin-blake2b decision 12). Prints the path of the built binary.
#
#   tests/tools/fetch_electrs_blake2b.sh [cache-dir]
#
# The commit is pinned and re-checked after clone; a checkout at any other
# commit is refused rather than built.
set -euo pipefail

tools_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cache_dir="${1:-$tools_dir/electrs-blake2b}"
repo_url="https://github.com/retropex/electrs.git"
commit="4453cac61979322c0260f4b90e899379ae606206"
src="$cache_dir/src"
target="${ELECTRS_BLAKE2B_TARGET_DIR:-$cache_dir/target}"
bin="$target/release/electrs"

if [ -x "$bin" ] && [ -f "$cache_dir/.built-$commit" ]; then
  echo "$bin"
  exit 0
fi

mkdir -p "$cache_dir"
if [ ! -d "$src/.git" ]; then
  git clone --quiet --branch mempool "$repo_url" "$src" >&2
fi
git -C "$src" fetch --quiet origin "$commit" >&2 || true
git -C "$src" checkout --quiet "$commit" >&2
actual="$(git -C "$src" rev-parse HEAD)"
if [ "$actual" != "$commit" ]; then
  echo "retropex/electrs checkout is at $actual, expected $commit" >&2
  exit 1
fi

# `--locked` keeps the dependency graph at the upstream Cargo.lock.
(cd "$src" && CARGO_TARGET_DIR="$target" cargo build --release --locked --bin electrs >&2)
[ -x "$bin" ] || { echo "electrs binary not produced" >&2; exit 1; }
touch "$cache_dir/.built-$commit"
echo "$bin"
