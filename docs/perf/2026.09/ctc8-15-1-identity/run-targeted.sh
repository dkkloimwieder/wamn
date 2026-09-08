#!/usr/bin/env bash
set -euo pipefail

source_tree=/home/kaalin/.cache/wamn-lanes/ctc8-15-session-foundation-20260908
evidence=/home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-15-1-identity/targeted-001
mkdir "$evidence"
cd "$source_tree"
git rev-parse HEAD > "$evidence/source-base"
git diff --binary > "$evidence/source.patch"
export RUSTC_WRAPPER=sccache
run() {
    local name=$1 status=0
    shift
    while pgrep -f '^/home/kaalin/.rustup/.*/bin/cargo ' > /dev/null; do
        sleep 5
    done
    printf '%q ' "$@" > "$evidence/$name.command"
    printf '\n' >> "$evidence/$name.command"
    "$@" > "$evidence/$name.log" 2>&1 || status=$?
    printf '%s\n' "$status" > "$evidence/$name.exit"
    printf '%s exit=%s\n' "$name" "$status"
    if ((status != 0)); then
        printf '%s\n' "$status" > "$evidence/exit"
        exit "$status"
    fi
}
run metadata cargo metadata --offline --format-version 1
run tokens cargo test --locked --offline -p wamn-platform-identity --test session_token
run issuer cargo test --locked --offline -p wamn-control-provision --lib identity_issuer
run service cargo test --locked --offline -p wamn-identity --all-targets
run cache cargo test --locked --offline -p wamn-runtime --features test-util \
    --test session_keys -- --test-threads=1
run cli cargo test --locked --offline -p wamn-ctl --lib identity_issuer
run inventories cargo test --locked --offline -p wamn-proof-conformance \
    --test package_architecture --test profile_selectors \
    --test retained_root_outcomes --test repo_lint --test workspace_tiers \
    --test state_ownership -- --include-ignored
run docker cargo test --locked --offline -p wamn-proof-conformance \
    --lib docker_component_provenance -- --include-ignored
printf '0\n' > "$evidence/exit"
