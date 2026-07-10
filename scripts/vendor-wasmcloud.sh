#!/usr/bin/env bash
# Produce vendor/wasmcloud: the wasmCloud monorepo at the rev pinned in
# Cargo.toml, with our carried patches (patches/*.patch) applied. The root
# Cargo.toml [patch] section points wash-runtime at this checkout, so run this
# once before building (locally and in the Dockerfile). Idempotent: reruns
# reset the checkout and reapply the patches.
set -euo pipefail

REV=8b53285f33f9d8cdb3748ff75d3f951ba3be4f2f
REPO_URL=https://github.com/wasmCloud/wasmCloud.git
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VENDOR="$ROOT/vendor/wasmcloud"

if [ ! -d "$VENDOR/.git" ]; then
    mkdir -p "$VENDOR"
    git -C "$VENDOR" init -q
    git -C "$VENDOR" remote add origin "$REPO_URL"
fi

if ! git -C "$VENDOR" cat-file -e "$REV^{commit}" 2>/dev/null; then
    git -C "$VENDOR" fetch --depth 1 origin "$REV"
fi

git -C "$VENDOR" checkout -q --force "$REV"
git -C "$VENDOR" clean -qfd

for patch in "$ROOT"/patches/*.patch; do
    echo "applying $(basename "$patch")"
    git -C "$VENDOR" apply --verbose "$patch"
done

echo "vendor/wasmcloud ready at $REV with $(ls "$ROOT"/patches/*.patch | wc -l) patch(es)"
