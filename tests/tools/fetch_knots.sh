#!/usr/bin/env bash
# Fetch a Bitcoin Knots release for this host, verify it against the
# PGP-signed SHA256SUMS with tests/tools/knots_verify (same trust anchor as the
# desktop installer), and print the path of the extracted `bitcoind`.
#
#   tests/tools/fetch_knots.sh <version> [cache-dir]
#   e.g. tests/tools/fetch_knots.sh 29.4.1.knots20260508
#
# Nothing is extracted unless verification succeeds. The cache dir defaults to
# tests/tools/knots/ (gitignored) and keeps the archive, the manifest and its
# signature next to the extraction.
#
# Reuse is never taken on trust. There is no "verified" marker: on every run
# the cached archive is verified again with the *current* knots_verify (so a
# change to the verifier or to the vendored signing key is applied to archives
# downloaded under the old one), and the extracted `bitcoind` is reused only if
# its SHA-256 equals the digest of the archive's own `bin/bitcoind` member,
# recomputed by streaming it out of the just-verified archive. A cached file
# that no longer verifies is discarded and fetched once more; if that copy does
# not verify either, the script fails and prints no path.
#
# KNOTS_TEST_BASE_URL and KNOTS_VERIFY_PATH exist for tests/test_btcb2_tools.py
# (a file:// release directory and a stand-in verifier); the harness never sets
# them.
set -euo pipefail

version="${1:?usage: fetch_knots.sh <version> [cache-dir]}"
tools_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cache_dir="${2:-$tools_dir/knots}"
major="${version%%.*}"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64)   suffix="arm64-apple-darwin" ;;
  Darwin-x86_64)  suffix="x86_64-apple-darwin" ;;
  Linux-x86_64)   suffix="x86_64-linux-gnu" ;;
  Linux-aarch64)  suffix="aarch64-linux-gnu" ;;
  *) echo "unsupported host $(uname -s)-$(uname -m)" >&2; exit 2 ;;
esac

base_url="${KNOTS_TEST_BASE_URL:-https://bitcoinknots.org/files/${major}.x/${version}}"
if [ -n "${KNOTS_TEST_BASE_URL:-}" ]; then
  echo "TEST MODE: release directory overridden; this is not a bitcoinknots.org release" >&2
fi
archive="bitcoin-${version}-${suffix}.tar.gz"
member="bitcoin-${version}/bin/bitcoind"
dest="$cache_dir/$version"
bitcoind="$dest/$member"

verify_bin="${KNOTS_VERIFY_PATH:-}"
if [ -z "$verify_bin" ]; then
  for candidate in \
      "${CARGO_TARGET_DIR:-$tools_dir/knots_verify/target}/release/knots_verify" \
      "$tools_dir/knots_verify/target/release/knots_verify"; do
    if [ -x "$candidate" ]; then verify_bin="$candidate"; break; fi
  done
fi
if [ -z "$verify_bin" ]; then
  echo "knots_verify not built: (cd tests/tools/knots_verify && cargo build --release)" >&2
  exit 2
fi

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}
sha256_stdin() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum | cut -d' ' -f1
  else shasum -a 256 | cut -d' ' -f1; fi
}

download_missing() {
  for f in "$archive" SHA256SUMS SHA256SUMS.asc; do
    # Download to a temporary name and rename on success: a transfer that fails
    # after its retries must not leave a partial file that a later run accepts.
    if [ ! -f "$f" ]; then
      rm -f "$f.part"
      curl -fsSL --retry 3 -o "$f.part" "$base_url/$f"
      mv "$f.part" "$f"
    fi
  done
}

mkdir -p "$dest"
cd "$dest"
download_missing

# Verify with the current verifier on every run, cached or fresh. A cached set
# that fails is thrown away and fetched once more; a fresh set that fails is
# final.
if ! "$verify_bin" "$archive" SHA256SUMS SHA256SUMS.asc >&2; then
  echo "cached release files for $version no longer verify; fetching them again" >&2
  rm -f "$archive" SHA256SUMS SHA256SUMS.asc "$archive.part"
  rm -rf "bitcoin-$version"
  download_missing
  "$verify_bin" "$archive" SHA256SUMS SHA256SUMS.asc >&2
fi

# Reuse the extraction only if its bitcoind is byte-identical to the member of
# the archive that just verified; otherwise extract again from that archive.
expected="$(tar -xzOf "$archive" "$member" | sha256_stdin)"
if [ ! -x "$bitcoind" ] || [ "$(sha256_of "$bitcoind")" != "$expected" ]; then
  rm -rf "bitcoin-$version"
  tar -xzf "$archive"
  [ -x "$bitcoind" ] || { echo "no $member in $archive" >&2; exit 1; }
  [ "$(sha256_of "$bitcoind")" = "$expected" ] || { echo "extracted bitcoind does not match the archive member" >&2; exit 1; }
fi
echo "$bitcoind"
