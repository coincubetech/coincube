#!/usr/bin/env python3
"""Check that a signed release manifest covers exactly the published artifacts."""

import hashlib
from pathlib import Path
import re
import sys


TARGETS = {
    "aarch64-apple-darwin.dmg",
    "x86_64-apple-darwin.dmg",
    "x86_64-pc-windows-msvc.msi",
    "x86_64-unknown-linux-gnu.tar.gz",
}


def verify(directory: Path, version: str) -> None:
    expected = {f"tenshu-{version}-{target}" for target in TARGETS}
    manifest_name = f"SHA256SUMS-{version}.txt"
    signature_name = f"{manifest_name}.asc"
    actual = {path.name for path in directory.iterdir() if path.is_file()}
    required = expected | {manifest_name, signature_name}
    if actual != required:
        raise ValueError(f"release assets differ: missing={sorted(required - actual)}, extra={sorted(actual - required)}")

    checksums = {}
    for line in (directory / manifest_name).read_text().splitlines():
        match = re.fullmatch(r"([0-9a-fA-F]{64})  ([^/\\]+)", line)
        if not match or match[2] in checksums:
            raise ValueError(f"invalid or duplicate manifest entry: {line!r}")
        checksums[match[2]] = match[1].lower()
    if set(checksums) != expected:
        raise ValueError(f"manifest entries differ: missing={sorted(expected - checksums.keys())}, extra={sorted(checksums.keys() - expected)}")

    for name in sorted(expected):
        hasher = hashlib.sha256()
        with (directory / name).open("rb") as asset:
            for chunk in iter(lambda: asset.read(1024 * 1024), b""):
                hasher.update(chunk)
        digest = hasher.hexdigest()
        if digest != checksums[name]:
            raise ValueError(f"checksum mismatch: {name}")
    print(f"Verified signed-manifest coverage and checksums for {len(expected)} release assets")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        raise SystemExit("usage: verify-release-assets.py ASSET_DIR VERSION")
    try:
        verify(Path(sys.argv[1]), sys.argv[2])
    except (OSError, ValueError) as error:
        raise SystemExit(str(error)) from error
