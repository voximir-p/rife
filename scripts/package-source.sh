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
"""Build the release Corresponding Source bundle from committed sources."""

import argparse
import sys
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile

ROOT = Path(sys.argv.pop(1)).resolve()


def run(*args, cwd=ROOT, **kwargs):
    return subprocess.check_output(args, cwd=cwd, **kwargs)


def export_git(repository, destination):
    destination.mkdir(parents=True, exist_ok=True)
    subprocess.run(["tar", "-xf", "-", "-C", str(destination)],
                   input=run("git", "archive", "HEAD", cwd=repository), check=True)


def main():
    parser = argparse.ArgumentParser(prog="package-source.sh", description=__doc__)
    parser.add_argument("--output", type=Path, default=ROOT / "target/rife-source.tar.gz")
    args = parser.parse_args()
    # cargo-dist's stdout redirection creates an untracked dist-manifest.json
    # before this script starts. git archive excludes untracked files; require
    # tracked sources to match HEAD without rejecting generated build outputs.
    changes = run("git", "status", "--porcelain", "--untracked-files=no").decode(
        errors="replace"
    ).rstrip()
    if changes:
        parser.error("commit or restore tracked release changes before packaging:\n" + changes)
    # Fetch only the revisions in the lock manifest. Never ship machine-local
    # runtime files, model weights, build products, Git history, or credentials.
    run("sh", str(ROOT / "scripts/fetch-dependencies.sh"))
    dependencies = []
    for line in (ROOT / "third_party/dependencies.lock").read_text().splitlines()[1:]:
        if not line:
            continue
        name, _, _, commit, submodules = line.split("|")
        path = ROOT / "third_party" / name
        if run("git", "rev-parse", "HEAD", cwd=path).decode().strip() != commit:
            raise RuntimeError(f"wrong revision for {name}")
        if run("git", "status", "--porcelain", "--untracked-files=all", cwd=path).strip():
            raise RuntimeError(f"modified native source: {name}")
        dependencies.append((name, path))
        for submodule in submodules.split(",") if submodules else []:
            for state in run("git", "submodule", "status", "--recursive", "--", submodule,
                             cwd=path).decode().splitlines():
                if not state.startswith(" "):
                    raise RuntimeError(f"unverified submodule: {state}")
                subpath = state.split()[1]
                dependencies.append((f"{name}/{subpath}", path / subpath))
    output = args.output.resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="rife-source-") as temporary:
        source = Path(temporary) / "rife-source"
        source.mkdir()
        # Git provides the exact committed project sources, including all build
        # scripts and notices. Dependency trees are added separately below.
        export_git(ROOT, source)
        for name, path in dependencies:
            export_git(path, source / "third_party" / name)
        config = run("cargo", "vendor", "--locked", "--versioned-dirs", "vendor", cwd=source)
        with (source / ".cargo/config.toml").open("a") as config_file:
            config_file.write("\n" + config.decode())
        revision = run("git", "rev-parse", "HEAD").decode().strip()
        (source / "SOURCE-REVISION").write_text(revision + "\n")
        (source / "BUILD-SOURCE.md").write_text(
            "# Building the release source\n\n"
            "This bundle includes locked Rust crates in vendor/ and pinned native sources\n"
            "in third_party/. Their original license and notice files are retained.\n"
            "Install Rust 1.88+, CMake, Ninja and the platform C++/Vulkan toolchain\n"
            "described in CONTRIBUTING.md, then run cargo build --release --locked --offline.\n"
            "The dependency fetch scripts are unnecessary for this extracted bundle.\n"
            "FFmpeg/FFprobe and models are user-supplied runtime inputs; see README.md.\n")
        # Fail if the vendored graph cannot resolve without the publisher's cache.
        empty_home = Path(temporary) / "cargo-home"
        empty_home.mkdir()
        run("cargo", "metadata", "--locked", "--offline", "--format-version=1",
            cwd=source, env={**os.environ, "CARGO_HOME": str(empty_home)})
        staged = Path(temporary) / "source.tar.gz"
        with tarfile.open(staged, "w:gz") as archive:
            archive.add(source, arcname="rife-source")
        shutil.copyfile(staged, output)
    print(output)


if __name__ == "__main__":
    main()

PYTHON
