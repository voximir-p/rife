# Copyright (C) 2026 Voximir.
# SPDX-License-Identifier: GPL-3.0-only

param([switch] $Check)

$ErrorActionPreference = 'Stop'
$projectRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$report = Join-Path $projectRoot 'THIRD-PARTY-LICENSES.html'
$generated = Join-Path $projectRoot 'target/THIRD-PARTY-LICENSES.html'
$template = Join-Path $projectRoot 'about.hbs'

New-Item -ItemType Directory -Force -Path (Split-Path -Parent $generated) | Out-Null
& cargo about generate --workspace --locked --fail --output-file $generated $template
if ($LASTEXITCODE -ne 0) {
    throw 'cargo-about license report generation failed'
}

if ($Check) {
    if (-not (Test-Path -LiteralPath $report)) {
        throw 'THIRD-PARTY-LICENSES.html is missing; run scripts/generate-third-party-licenses.ps1'
    }
    # Git may convert checked-in text to CRLF on Windows; compare content, not line endings.
    $checkedInContent = [IO.File]::ReadAllText($report) -replace '\r\n?', "`n"
    $generatedContent = [IO.File]::ReadAllText($generated) -replace '\r\n?', "`n"
    if (-not [string]::Equals($checkedInContent, $generatedContent, [StringComparison]::Ordinal)) {
        throw 'THIRD-PARTY-LICENSES.html is stale; run scripts/generate-third-party-licenses.ps1 and commit the result'
    }
} else {
    Copy-Item -LiteralPath $generated -Destination $report -Force
}
