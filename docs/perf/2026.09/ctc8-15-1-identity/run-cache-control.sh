#!/usr/bin/env bash
set -euo pipefail

run_id=${1:?provide a new evidence run name}
test_name=${2:-}
[[ "$run_id" =~ ^[a-z0-9][a-z0-9-]*$ ]]
[[ -z "$test_name" || "$test_name" =~ ^[a-z][a-z0-9_]*$ ]]
source_tree=/home/kaalin/.cache/wamn-lanes/ctc8-15-session-foundation-20260908
evidence=/home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-15-1-identity/$run_id
mkdir "$evidence"
finish() {
    local status=$?
    trap - EXIT
    printf '%s\n' "$status" > "$evidence/exit"
    exit "$status"
}
trap finish EXIT
cd "$source_tree"
git rev-parse HEAD > "$evidence/source-base"
git diff --binary > "$evidence/source.patch"
sha256sum crates/platform/runtime/src/session_keys.rs \
    crates/platform/runtime/tests/session_keys.rs \
    crates/control/provision/src/identity_issuer.rs \
    crates/identity/platform/src/session_keys.rs > "$evidence/source.sha256"
export RUSTC_WRAPPER=sccache
command=(timeout --kill-after=5s 300s cargo test --locked --offline \
    -p wamn-runtime --features test-util --test session_keys)
if [[ -n "$test_name" ]]; then
    command+=("$test_name" -- --exact --nocapture --test-threads=1)
else
    command+=(-- --nocapture --test-threads=1)
fi
printf '%q ' "${command[@]}" > "$evidence/test.command"
printf '\n' >> "$evidence/test.command"
"${command[@]}" > "$evidence/test.log" 2>&1
