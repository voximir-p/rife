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
sysroot = Path(subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip())
source = sysroot / "share/doc/rust/COPYRIGHT-library.html"
if not source.is_file():
    parser.error("standard-library notices missing; run rustup component add rust-docs")
output = root / "LICENSES/RUST-STANDARD-LIBRARY.html"
content = source.read_text(encoding="utf-8").replace("\r\n", "\n")
if args.check:
    if not output.is_file() or output.read_text(encoding="utf-8") != content:
        parser.error("Rust library notices changed; run sh scripts/generate-rust-notices.sh")
else:
    output.write_text(content, encoding="utf-8")

PYTHON
