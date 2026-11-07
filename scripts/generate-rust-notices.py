#!/usr/bin/env python3
# Copyright (C) 2026 Voximir.
# SPDX-License-Identifier: GPL-3.0-only
"""Copy or verify notices for the standard library of the release compiler."""
import argparse
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--check", action="store_true")
args = parser.parse_args()
root = Path(__file__).resolve().parent.parent
sysroot = Path(subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip())
source = sysroot / "share/doc/rust/COPYRIGHT-library.html"
if not source.is_file():
    parser.error("standard-library notices missing; run rustup component add rust-docs")
output = root / "LICENSES/RUST-STANDARD-LIBRARY.html"
content = source.read_text(encoding="utf-8").replace("\r\n", "\n")
if args.check:
    if not output.is_file() or output.read_text(encoding="utf-8") != content:
        parser.error("Rust library notices changed; run python3 scripts/generate-rust-notices.py")
else:
    output.write_text(content, encoding="utf-8")
