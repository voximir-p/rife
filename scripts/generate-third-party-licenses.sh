#!/bin/sh
# Copyright (C) 2026 Voximir.
# SPDX-License-Identifier: GPL-3.0-only
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"
check=false
case "${1-}" in
    --check) check=true; shift ;;
    "") ;;
    *) printf 'Usage: %s [--check]\n' "$0" >&2; exit 2 ;;
esac
[ "$#" -eq 0 ] || { printf 'Usage: %s [--check]\n' "$0" >&2; exit 2; }
mkdir -p target
cargo about generate --workspace --locked --fail --output-file target/THIRD-PARTY-LICENSES.html about.hbs
if "$check"; then
    [ -f THIRD-PARTY-LICENSES.html ] || { printf '%s\n' 'THIRD-PARTY-LICENSES.html is missing; run sh scripts/generate-third-party-licenses.sh' >&2; exit 1; }
    # Normalize CRLF checkouts and generated output without changing checked-in files.
    checked=$(mktemp target/license-checked.XXXXXX)
    generated=$(mktemp target/license-generated.XXXXXX)
    trap 'rm -f "$checked" "$generated"' EXIT HUP INT TERM
    tr -d '\r' < THIRD-PARTY-LICENSES.html > "$checked"
    tr -d '\r' < target/THIRD-PARTY-LICENSES.html > "$generated"
    cmp -s "$checked" "$generated" || { printf '%s\n' 'THIRD-PARTY-LICENSES.html is stale; run sh scripts/generate-third-party-licenses.sh and commit the result' >&2; exit 1; }
else
    cp target/THIRD-PARTY-LICENSES.html THIRD-PARTY-LICENSES.html
fi
