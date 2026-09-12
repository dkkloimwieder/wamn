#!/usr/bin/env bash
# Capture the fixture test before either measured source snapshot runs.
set -euo pipefail
evidence=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
cd -- "${1:?source worktree required}"
trap 'result=$?; printf "%s\n" "$result" >"$evidence/preflight.exit"; exit "$result"' EXIT
git rev-parse HEAD
date -u
uptime
RUSTC_WRAPPER=sccache cargo test -p wamn-proof-integration --lib --locked --offline \
  membershipproof::tests -- --include-ignored --nocapture --test-threads=1
