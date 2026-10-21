#!/usr/bin/env bash
set -euo pipefail

prototype_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
cargo build --release --locked --manifest-path "$prototype_root/Cargo.toml"
python3 - "$prototype_root" <<'PY'
from pathlib import Path
import os
import plistlib
import shutil
import subprocess
import sys

root = Path(sys.argv[1])
bundle = root / 'target/TenshuLauncherLab.app'
executable = bundle / 'Contents/MacOS/tenshu-launcher-3d'
executable.parent.mkdir(parents=True, exist_ok=True)
temporary = executable.with_suffix('.new')
shutil.copy2(root / 'target/release/tenshu-launcher-3d', temporary)
os.replace(temporary, executable)
with (bundle / 'Contents/Info.plist').open('wb') as output:
    plistlib.dump({
        'CFBundleExecutable': 'tenshu-launcher-3d',
        'CFBundleIdentifier': 'io.coincube.tenshu.launcherlab',
        'CFBundleName': 'Tenshu Launcher Lab',
        'CFBundleDisplayName': 'Tenshu Launcher Lab',
        'CFBundlePackageType': 'APPL',
        'CFBundleVersion': '1',
        'NSHighResolutionCapable': True,
    }, output)
subprocess.run(['codesign', '--force', '--sign', '-', str(bundle)], check=True)
print(bundle)
PY
