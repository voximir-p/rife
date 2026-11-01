# Copyright (C) 2026 Voximir.
# SPDX-License-Identifier: GPL-3.0-only

$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$sources = Import-Csv -LiteralPath (Join-Path $projectRoot 'third_party/dependencies.lock') -Delimiter '|'
foreach ($source in $sources) {
    $destination = Join-Path $projectRoot "third_party/$($source.Name)"
    if (-not (Test-Path -LiteralPath $destination)) {
        & git clone --depth 1 --branch $source.Tag $source.Url $destination
        if ($LASTEXITCODE -ne 0) { throw "Failed to fetch $($source.Name)" }
    }
    $commit = & git -C $destination rev-parse HEAD
    if ($LASTEXITCODE -ne 0 -or $commit -ne $source.Commit) { throw "Unexpected $($source.Name) revision; expected $($source.Commit). Existing files were left intact." }
    if ($source.Submodules) {
        & git -C $destination submodule update --init --depth 1 -- $source.Submodules
        if ($LASTEXITCODE -ne 0) { throw "Failed to fetch pinned $($source.Submodules)" }
    }
}
Write-Output 'Pinned native sources are ready. Run cargo build --release.'
