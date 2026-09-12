#!/usr/bin/env bash
set -euo pipefail
evidence=/home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-15-1-identity/targeted-003
mkdir "$evidence"
cd /home/kaalin/.cache/wamn-lanes/ctc8-15-session-foundation-20260908
export RUSTC_WRAPPER=sccache
trap 'printf "%s\n" "$?" > "$evidence/exit"' EXIT
run() {
    local name=$1 status=0
    shift
    while pgrep -f '^/home/kaalin/.rustup/.*/bin/cargo ' > /dev/null; do sleep 5; done
    printf '%q ' "$@" > "$evidence/$name.command"
    printf '\n' >> "$evidence/$name.command"
    "$@" > "$evidence/$name.log" 2>&1 || status=$?
    printf '%s\n' "$status" > "$evidence/$name.exit"
    printf '%s exit=%s\n' "$name" "$status"
    return "$status"
}
git rev-parse HEAD > "$evidence/source-base"
git diff --binary > "$evidence/source.patch"
run tiers cargo test --locked --offline -p wamn-proof-conformance --test workspace_tiers
run docker cargo test --locked --offline -p wamn-proof-conformance --lib docker_component_provenance
run gate-router cargo build --locked --offline -p wamn-gates
