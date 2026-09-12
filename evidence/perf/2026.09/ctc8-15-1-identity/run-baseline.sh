#!/usr/bin/env bash
set -euo pipefail

source_tree=/home/kaalin/.cache/wamn-lanes/ctc8-15-baseline-20260908
evidence=/home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-15-1-identity/baseline-001
mkdir "$evidence"
cd "$source_tree"
test "$(git rev-parse HEAD)" = 3a48ea1538adfec591179035dd98bfab12383969
test -z "$(git status --porcelain)"
git rev-parse HEAD > "$evidence/source-commit"
export RUSTC_WRAPPER=sccache
overall=0
run() {
    local name=$1 status=0
    shift
    printf '%q ' "$@" > "$evidence/$name.command"
    printf '\n' >> "$evidence/$name.command"
    "$@" > "$evidence/$name.log" 2>&1 || status=$?
    printf '%s\n' "$status" > "$evidence/$name.exit"
    printf '%s exit=%s\n' "$name" "$status"
    if ((status != 0)); then overall=1; fi
}
run metadata cargo metadata --locked --offline --no-deps --format-version 1
run inventories cargo test --locked --offline -p wamn-proof-conformance \
    --test package_architecture --test profile_selectors \
    --test retained_root_outcomes --test repo_lint --test workspace_tiers \
    --no-fail-fast -- --include-ignored
run docker cargo test --locked --offline -p wamn-proof-conformance \
    --lib docker_component_provenance -- --include-ignored
run platform cargo test --locked --offline -p wamn-proof-system \
    --test deploy_platform_inventory -- --include-ignored
printf '%s\n' "$overall" > "$evidence/exit"
exit "$overall"
