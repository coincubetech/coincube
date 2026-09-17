#!/usr/bin/env bash
# Build the BLAKE2b-aware Esplora indexer for the BTCB2 regtest harness:
# retropex/electrs, branch `mempool`, commit 4453cac ("Support BLAKE2b"), the
# indexer Connect runs for Bitcoin Blake2b (company-brain
# plans/bitcoin-blake2b decision 12). Prints the path of the built binary.
#
#   tests/tools/fetch_electrs_blake2b.sh [cache-dir]
#
# What gets built is exactly the pinned commit's tree. `git checkout <commit>`
# followed by `rev-parse HEAD` only pins HEAD: with the cache already at the
# commit the checkout is a no-op and edits to tracked files survive it, and
# `--locked` pins dependencies, not source. So before Cargo runs the checkout
# must be clean — no modified, staged or untracked file (a dropped-in source
# file changes the build as surely as an edit). A dirty tree is *refused*, not
# reset: a developer's deliberate local patch is left in place and the message
# says how to restore the tree or use another cache dir.
#
# Cache reuse is not self-certifying. The marker written after a build records
# the commit, the binary's SHA-256 and the binary's own `--version` line, and
# it is only ever written after the source-state checks above passed. On reuse
# the script re-verifies the source state (clone present, HEAD at the commit,
# tree clean), the binary against the recorded digest, and the binary's
# `--version` against the recorded line — which upstream's build.rs stamps with
# the commit and suffixes with "(dirty)" when built from an unclean tree, an
# independent witness of what was compiled. Markers of an older format are
# ignored, which forces one rebuild through the checked path.
#
# The cache dir defaults to $XDG_CACHE_HOME/coincube/electrs-blake2b
# (~/.cache/...): it must live outside this repository, or Cargo treats the
# checkout as a member of Coincube's workspace and refuses to build it.
#
# ELECTRS_BLAKE2B_TEST_REPO / ELECTRS_BLAKE2B_TEST_COMMIT / ELECTRS_BLAKE2B_TEST_BRANCH
# and ELECTRS_BLAKE2B_DRY_RUN=1 exist only for tests/test_btcb2_tools.py, which
# exercises the source checks against a throwaway repository without building.
set -euo pipefail

cache_dir="${1:-${XDG_CACHE_HOME:-$HOME/.cache}/coincube/electrs-blake2b}"
repo_url="${ELECTRS_BLAKE2B_TEST_REPO:-https://github.com/retropex/electrs.git}"
branch="${ELECTRS_BLAKE2B_TEST_BRANCH:-mempool}"
commit="${ELECTRS_BLAKE2B_TEST_COMMIT:-4453cac61979322c0260f4b90e899379ae606206}"
if [ -n "${ELECTRS_BLAKE2B_TEST_REPO:-}${ELECTRS_BLAKE2B_TEST_COMMIT:-}" ]; then
  echo "TEST MODE: repository/commit overridden; this is not the pinned indexer" >&2
fi
src="$cache_dir/src"
target="${ELECTRS_BLAKE2B_TARGET_DIR:-$cache_dir/target}"
bin="$target/release/electrs"
marker="$target/.built-$commit-v2"

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

version_of() { "$1" --version 2>&1 | head -1; }

# Bring the checkout to the pinned commit and verify its state. Every path
# through this script — reuse or build — goes through here.
verify_source() {
  mkdir -p "$cache_dir"
  if [ ! -d "$src/.git" ]; then
    git clone --quiet --branch "$branch" "$repo_url" "$src" >&2
  fi
  git -C "$src" fetch --quiet origin "$commit" >&2 || true
  git -C "$src" checkout --quiet "$commit" >&2
  local actual
  actual="$(git -C "$src" rev-parse HEAD)"
  if [ "$actual" != "$commit" ]; then
    echo "retropex/electrs checkout is at $actual, expected $commit" >&2
    exit 1
  fi
  local dirty
  dirty="$(git -C "$src" status --porcelain --untracked-files=all --ignored=no)"
  if [ -n "$dirty" ]; then
    echo "retropex/electrs checkout at $src has local changes; refusing to build or reuse the pinned commit from a modified tree:" >&2
    echo "$dirty" >&2
    echo "(restore it with: git -C '$src' checkout -- . && git -C '$src' clean -fd; or point the script at a fresh cache dir)" >&2
    exit 1
  fi
}

verify_source

# Reuse only when the source state was just verified *and* the binary and its
# self-reported version still match what this script recorded after a checked
# build.
if [ -x "$bin" ] && [ -f "$marker" ]; then
  recorded_commit="$(sed -n 's/^commit=//p' "$marker")"
  recorded_sha="$(sed -n 's/^sha256=//p' "$marker")"
  recorded_version="$(sed -n 's/^version=//p' "$marker")"
  if [ "$recorded_commit" = "$commit" ] \
     && [ -n "$recorded_sha" ] && [ "$recorded_sha" = "$(sha256_of "$bin")" ] \
     && [ -n "$recorded_version" ] && [ "$recorded_version" = "$(version_of "$bin")" ]; then
    echo "$bin"
    exit 0
  fi
fi
# Remove the binary as well as the marker (and any pre-repair marker): Cargo
# could otherwise judge the crate fresh, leave a replaced executable in place,
# and the digest written below would then vouch for the stand-in.
rm -f "$marker" "$target/.built-$commit" "$bin"

if [ "${ELECTRS_BLAKE2B_DRY_RUN:-0}" = "1" ]; then
  echo "dry-run: would build $commit from a clean checkout at $src" >&2
  echo "$bin"
  exit 0
fi

# `--locked` keeps the dependency graph at the upstream Cargo.lock.
(cd "$src" && CARGO_TARGET_DIR="$target" cargo build --release --locked --bin electrs >&2)
[ -x "$bin" ] || { echo "electrs binary not produced" >&2; exit 1; }

# Independent of the checks above: upstream's build.rs stamps the commit into
# the version string and appends "(dirty)" when it built from an unclean tree.
version="$(version_of "$bin")"
case "$version" in
  *dirty*) echo "built electrs reports a dirty tree: $version" >&2; rm -f "$bin"; exit 1 ;;
esac
case "$version" in
  *"${commit:0:7}"*) ;;
  *) echo "built electrs does not report the pinned commit ${commit:0:7}: $version" >&2; rm -f "$bin"; exit 1 ;;
esac

{
  echo "commit=$commit"
  echo "sha256=$(sha256_of "$bin")"
  echo "version=$version"
} > "$marker"
echo "$bin"
