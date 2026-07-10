#!/usr/bin/env bash
# Produce vendor/wasmcloud: the wasmCloud monorepo at the rev pinned in
# Cargo.toml, with our carried patches (patches/*.patch) applied. The root
# Cargo.toml [patch] section points wash-runtime at this checkout, so run this
# once before building (locally and in the Dockerfile). Idempotent: reruns
# reset the checkout and reapply the patches. See patches/README.md.
set -euo pipefail

REPO_URL=https://github.com/wasmCloud/wasmCloud.git
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VENDOR="$ROOT/vendor/wasmcloud"

# Single source of truth for the pin: the wash-runtime rev in Cargo.toml.
REV="$(sed -n 's/^wash-runtime = {.* rev = "\([0-9a-f]\{40\}\)".*/\1/p' "$ROOT/Cargo.toml")"
if [ -z "$REV" ]; then
    echo "error: could not read a 40-hex wash-runtime rev from $ROOT/Cargo.toml" >&2
    exit 1
fi
echo "wasmCloud rev (from Cargo.toml): $REV"

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
