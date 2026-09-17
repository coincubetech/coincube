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
# must be clean — no modified, staged, untracked or ignored file (a dropped-in
# source file or ignored build input changes the build as surely as an edit). A dirty tree is *refused*, not
# reset: a developer's deliberate local patch is left in place and the message
# says how to restore the tree or use another cache dir.
#
# Cache reuse is not self-certifying. The marker written after a build records
# the commit, the binary's SHA-256 and the binary's own `--version` line, and
# it is only ever written after the source-state checks above passed. On reuse
# the script re-verifies the source state (clone present, HEAD at the commit,
# tree clean), the binary against the recorded digest, and the binary's
# `--version` against the recorded line. Upstream's build.rs stamps that line
# with the commit and suffixes "(dirty)" when it built from an unclean tree —
# but only when GIT_HASH is not already set in the build environment, since
# build.rs returns early if it is and the stamp is then whatever the
# environment said. Neither this script nor the workflow sets GIT_HASH, so
# here the stamp is a secondary check; the clean-checkout requirement above is
# the gate. Markers of an older format are ignored, which forces one rebuild
# through the checked path.
#
# The cache dir defaults to $XDG_CACHE_HOME/coincube/electrs-blake2b
# (~/.cache/...): it must live outside this repository, or Cargo treats the
# checkout as a member of Coincube's workspace and refuses to build it. The
# build target defaults to <cache>/target and may be moved with
# ELECTRS_BLAKE2B_TARGET_DIR, but never into the checkout: a target equal to
# or below <cache>/src — by any spelling, relative, absolute or through a
# symlink — is refused before anything is created, since Cargo's output there
# would fail the clean-checkout gate above on every later run.
#
# ELECTRS_BLAKE2B_TEST_REPO / ELECTRS_BLAKE2B_TEST_COMMIT / ELECTRS_BLAKE2B_TEST_BRANCH
# exist only for tests/test_btcb2_tools.py, which exercises the source checks
# against a throwaway repository, and are honoured only together with
# ELECTRS_BLAKE2B_DRY_RUN=1, which never builds: an override that leaks into a
# real build environment is refused, so the production path cannot be pointed
# at another repository or commit.
set -euo pipefail

cache_dir="${1:-${XDG_CACHE_HOME:-$HOME/.cache}/coincube/electrs-blake2b}"
if [ -n "${ELECTRS_BLAKE2B_TEST_REPO:-}${ELECTRS_BLAKE2B_TEST_COMMIT:-}${ELECTRS_BLAKE2B_TEST_BRANCH:-}" ] \
   && [ "${ELECTRS_BLAKE2B_DRY_RUN:-0}" != "1" ]; then
  echo "ELECTRS_BLAKE2B_TEST_* overrides are only accepted with ELECTRS_BLAKE2B_DRY_RUN=1 (test mode); refusing to build from an overridden repository or commit" >&2
  exit 2
fi
repo_url="${ELECTRS_BLAKE2B_TEST_REPO:-https://github.com/retropex/electrs.git}"
branch="${ELECTRS_BLAKE2B_TEST_BRANCH:-mempool}"
commit="${ELECTRS_BLAKE2B_TEST_COMMIT:-4453cac61979322c0260f4b90e899379ae606206}"
if [ -n "${ELECTRS_BLAKE2B_TEST_REPO:-}${ELECTRS_BLAKE2B_TEST_COMMIT:-}${ELECTRS_BLAKE2B_TEST_BRANCH:-}" ]; then
  echo "TEST MODE: repository/commit overridden; this is not the pinned indexer" >&2
fi
# Both directories are used from inside the checkout later (`cd "$src" && …
# cargo build`), so a relative cache dir or a relative ELECTRS_BLAKE2B_TARGET_DIR
# would make Cargo write under "$src/<relative>/…" while the binary is looked
# for at "<cache>/target/…" — a full build that then "produced no binary", and
# a stray untracked tree inside the checkout that the next run's clean-tree
# check refuses. A target that resolves *inside* the checkout by any other
# spelling (absolute, or through a symlink to it) fails the same way one build
# later, and on a fresh cache earlier still: creating it makes $src non-empty
# before `git clone`. So both paths are resolved to physical absolute paths
# without creating anything, a target equal to or below the checkout is
# refused, and only then does the script touch the filesystem. (No CI path
# passes either: the workflow and README call the script with no argument.)
#
# Physical absolute path for $1 without creating it: resolve the deepest
# existing ancestor (`cd -P`: symlinks before `..`, as the kernel will when
# Cargo opens the path) and re-append the missing remainder verbatim.
physical_path() {
  local path="$1" rest="" parent
  while [ ! -d "$path" ]; do
    parent="$(dirname -- "$path")"
    if [ "$parent" = "$path" ]; then
      echo "cannot resolve $1: no existing ancestor directory" >&2
      return 1
    fi
    rest="$(basename -- "$path")${rest:+/$rest}"
    path="$parent"
  done
  path="$(cd -P -- "$path" && pwd -P)"
  path="${path%/}${rest:+/$rest}"
  printf '%s\n' "${path:-/}"
}

cache_dir="$(physical_path "$cache_dir")"
src="$(physical_path "$cache_dir/src")"
if [ -n "${ELECTRS_BLAKE2B_TARGET_DIR:-}" ]; then
  target="$(physical_path "$ELECTRS_BLAKE2B_TARGET_DIR")"
else
  target="$cache_dir/target"
fi
case "$target" in
  "$src"|"$src"/*)
    echo "ELECTRS_BLAKE2B_TARGET_DIR=${ELECTRS_BLAKE2B_TARGET_DIR:-<unset>} resolves to $target, inside the source checkout $src; Cargo's output there would make the checkout dirty and every later run would refuse it. Use a directory outside the checkout (the default is $cache_dir/target). Nothing was created or removed." >&2
    exit 2
    ;;
esac
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
  # Ignored files count too: an ignored build input (a stale .cargo/config.toml,
  # say) changes what Cargo compiles just as a tracked edit does, and this
  # script never creates ignored files here — the target dir lives outside.
  local dirty
  dirty="$(git -C "$src" status --porcelain --untracked-files=all --ignored)"
  if [ -n "$dirty" ]; then
    echo "retropex/electrs checkout at $src has local changes; refusing to build or reuse the pinned commit from a modified tree:" >&2
    echo "$dirty" >&2
    echo "(restore it with: git -C '$src' checkout -- . && git -C '$src' clean -fdx; or point the script at a fresh cache dir)" >&2
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

# Secondary check (see the header: valid because GIT_HASH is not preset here):
# upstream's build.rs stamps the commit into the version string and appends
# "(dirty)" when it built from an unclean tree.
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
