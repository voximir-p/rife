#!/bin/sh
# Copyright (C) 2026 Voximir.
# SPDX-License-Identifier: GPL-3.0-only
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
if command -v python3 >/dev/null 2>&1 && python3 --version >/dev/null 2>&1; then
    python=python3
else
    python=python
fi
exec "$python" - "$root" "$@" <<'PYTHON'
"""Copy or verify notices for the standard library of the release compiler."""
import argparse
import sys
from pathlib import Path
import subprocess

project_root = sys.argv.pop(1)
parser = argparse.ArgumentParser(prog="generate-rust-notices.sh", description=__doc__)
parser.add_argument("--check", action="store_true")
args = parser.parse_args()
root = Path(project_root).resolve()
compiler = subprocess.check_output(["rustc", "--version"], text=True).strip()
sysroot = Path(subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip())
source = sysroot / "share/doc/rust/COPYRIGHT-library.html"
if not source.is_file():
    parser.error(
        f"standard-library notices missing for {compiler} at {source}; "
        "reinstall the selected Rust compiler with rustup"
    )
output = root / "LICENSES/RUST-STANDARD-LIBRARY.html"
content = source.read_text(encoding="utf-8").replace("\r\n", "\n")
if args.check:
    if not output.is_file() or output.read_text(encoding="utf-8") != content:
        parser.error(
            f"standard-library notices do not match {compiler}; "
            "use the compiler pinned in rust-toolchain.toml. "
            "When updating that compiler, run sh scripts/generate-rust-notices.sh "
            "and commit the toolchain and notices together"
        )
else:
    output.write_text(content, encoding="utf-8")
print(f"{'Verified' if args.check else 'Updated'} standard-library notices for {compiler}")

PYTHON
