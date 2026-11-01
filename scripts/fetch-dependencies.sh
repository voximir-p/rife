## Copyright (C) 2026 Voximir.
# SPDX-License-Identifier: GPL-3.0-only

!/bin/sh
set -eu
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
fetch() {
    name=$1 url=$2 tag=$3 expected=$4
    destination="$root/third_party/$name"
    if [ ! -e "$destination" ]; then
        git clone --depth 1 --branch "$tag" "$url" "$destination"
    fi
    actual=$(git -C "$destination" rev-parse HEAD)
    if [ "$actual" != "$expected" ]; then
        printf 'Unexpected %s revision; expected %s. Existing files were left intact.\n' "$name" "$expected" >&2
        exit 1
    fi
}
while IFS='|' read -r name url tag commit submodules; do
    [ "$name" = Name ] && continue
    [ -n "$name" ] || continue
    fetch "$name" "$url" "$tag" "$commit"
    if [ -n "$submodules" ]; then
        git -C "$root/third_party/$name" submodule update --init --depth 1 -- "$submodules"
    fi
done < "$root/third_party/dependencies.lock"
printf 'Pinned native sources are ready. Run cargo build --release.\n'
