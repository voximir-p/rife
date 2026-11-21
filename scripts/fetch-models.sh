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
exec "$python" - "$root" <<'PYTHON'
"""Fetch only the selected model directories at the locked upstream revision."""
from pathlib import Path
import re
import subprocess
import sys
import tempfile

root = Path(sys.argv[1])
models = [line.strip() for line in (root / "third_party/models.txt").read_text().splitlines()
          if line.strip() and not line.lstrip().startswith("#")]
if not models or len(set(models)) != len(models) or any(
        not re.fullmatch(r"[A-Za-z0-9_-][A-Za-z0-9_.-]*", model) or model.endswith(".")
        for model in models):
    sys.exit("invalid or duplicate model names in third_party/models.txt")
name, url, _, revision, _ = next(
    line.split("|") for line in (root / "third_party/dependencies.lock").read_text().splitlines()[1:]
    if line.startswith("rife-ncnn-vulkan-ex|"))
destination = root / "third_party" / name
paths = [f"models/{model}" for model in models]


def git(*args, capture=False):
    result = subprocess.run(["git", "-C", str(destination), *args], check=True,
                            stdout=subprocess.PIPE if capture else None, text=True)
    return result.stdout.strip() if capture else None


def complete():
    return all((destination / path).is_file() and (destination / path).stat().st_size > 0
               for path in ["LICENSE", *(f"{path}/{file}" for path in paths
                                         for file in ["flownet.param", "flownet.bin"])])


if not destination.exists():
    # A failed fetch must not leave an incomplete checkout that blocks retries.
    with tempfile.TemporaryDirectory(prefix=".rife-models-", dir=destination.parent) as temporary:
        final = destination
        destination = Path(temporary) / name
        destination.mkdir()
        git("init")
        git("remote", "add", "origin", url)
        git("config", "remote.origin.promisor", "true")
        git("config", "remote.origin.partialclonefilter", "blob:none")
        git("fetch", "--depth", "1", "--filter=blob:none", "origin", revision)
        git("sparse-checkout", "init", "--cone")
        git("sparse-checkout", "set", "--", *paths)
        git("checkout", "--detach", "FETCH_HEAD")
        if not complete():
            sys.exit("selected model files are missing at the pinned revision")
        destination.rename(final)
        destination = final
elif (destination / ".git").exists():
    if git("rev-parse", "HEAD", capture=True) != revision:
        sys.exit(f"unexpected {name} revision; expected {revision}. Existing files were left intact.")
    if git("status", "--porcelain", "--untracked-files=all", capture=True):
        sys.exit(f"modified model sources in {destination}; existing files were left intact")
    if not complete():
        # Add directories without discarding the user's existing sparse selection.
        cone = subprocess.run(["git", "-C", str(destination), "config", "--bool",
                               "core.sparseCheckoutCone"], capture_output=True, text=True)
        license_path = [] if cone.stdout.strip() == "true" else ["/LICENSE"]
        git("sparse-checkout", "add", "--", *license_path, *paths)
if not complete():
    sys.exit(f"missing model files in {destination}; check third_party/models.txt and the pinned revision")
print("Pinned model assets are ready: " + ", ".join(models))
PYTHON
