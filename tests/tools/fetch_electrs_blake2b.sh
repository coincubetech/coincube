#!/usr/bin/env bash
# Build the BLAKE2b-aware Esplora indexer for the BTCB2 regtest harness:
# retropex/electrs, branch `mempool`, commit 4453cac ("Support BLAKE2b"), the
# indexer Connect runs for Bitcoin Blake2b (company-brain
# plans/bitcoin-blake2b decision 12). Prints the path of the built binary.
#
#   tests/tools/fetch_electrs_blake2b.sh [cache-dir]
#
# The commit is pinned and re-checked after clone; a checkout at any other
# commit is refused rather than built. The cache dir defaults to
# $XDG_CACHE_HOME/coincube/electrs-blake2b (~/.cache/...): it must live
# outside this repository, or Cargo treats the checkout as a member of
# Coincube's workspace and refuses to build it.
set -euo pipefail

cache_dir="${1:-${XDG_CACHE_HOME:-$HOME/.cache}/coincube/electrs-blake2b}"
repo_url="https://github.com/retropex/electrs.git"
commit="4453cac61979322c0260f4b90e899379ae606206"
src="$cache_dir/src"
target="${ELECTRS_BLAKE2B_TARGET_DIR:-$cache_dir/target}"
bin="$target/release/electrs"
# The "built from the pinned commit" marker lives next to the binary it
# vouches for and records that binary's SHA-256: a reused cache entry is only
# trusted if the executable still hashes to what this script built, so a
# replaced or stale binary is rebuilt from the checked-out commit instead of
# being handed back as the pinned one.
marker="$target/.built-$commit"

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

if [ -x "$bin" ] && [ -f "$marker" ] && [ "$(cat "$marker")" = "$(sha256_of "$bin")" ]; then
  echo "$bin"
  exit 0
fi
# Remove the binary as well as the marker: Cargo could otherwise judge the
# crate fresh, leave a replaced executable in place, and the digest written
# below would then vouch for the stand-in.
rm -f "$marker" "$bin"

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
sha256_of "$bin" > "$marker"
echo "$bin"
