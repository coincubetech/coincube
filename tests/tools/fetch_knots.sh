#!/usr/bin/env bash
# Fetch a Bitcoin Knots release for this host, verify it against the
# PGP-signed SHA256SUMS with tests/tools/knots_verify (same trust anchor as the
# desktop installer), and print the path of the extracted `bitcoind`.
#
#   tests/tools/fetch_knots.sh <version> [cache-dir]
#   e.g. tests/tools/fetch_knots.sh 29.4.1.knots20260508
#
# Nothing is extracted unless verification succeeds. The cache dir defaults to
# tests/tools/knots/ (gitignored). A re-run reuses the extraction only if the
# extracted bitcoind still hashes to what was recorded at verification time
# (".verified"); otherwise the cached archive is verified and extracted again.
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

base_url="https://bitcoinknots.org/files/${major}.x/${version}"
archive="bitcoin-${version}-${suffix}.tar.gz"
dest="$cache_dir/$version"
bitcoind="$dest/bitcoin-$version/bin/bitcoind"

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

if [ -x "$bitcoind" ] && [ -f "$dest/.verified" ] \
   && [ "$(cat "$dest/.verified")" = "$(sha256_of "$bitcoind")" ]; then
  echo "$bitcoind"
  exit 0
fi

mkdir -p "$dest"
cd "$dest"
rm -rf ".verified" "bitcoin-$version"
for f in "$archive" SHA256SUMS SHA256SUMS.asc; do
  # Download to a temporary name and rename on success: a transfer that fails
  # after its retries must not leave a partial file the `-f` check accepts.
  if [ ! -f "$f" ]; then
    rm -f "$f.part"
    curl -fsSL --retry 3 -o "$f.part" "$base_url/$f"
    mv "$f.part" "$f"
  fi
done
"$verify_bin" "$archive" SHA256SUMS SHA256SUMS.asc >&2
tar -xzf "$archive"
[ -x "$bitcoind" ] || { echo "no bitcoind in $archive" >&2; exit 1; }
sha256_of "$bitcoind" > .verified
echo "$bitcoind"
