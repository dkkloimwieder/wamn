#!/usr/bin/env bash
set -euo pipefail
run_id=${1:?provide a new run identifier}
[[ "$run_id" =~ ^[a-z0-9][a-z0-9-]*$ ]]
evidence=/home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-15-1-identity/final-targeted-$run_id
mkdir "$evidence"
cd /home/kaalin/.cache/wamn-lanes/ctc8-15-session-foundation-20260908
export RUSTC_WRAPPER=sccache
overall=0
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
    if [[ "$status" != 0 ]]; then overall=1; fi
}
git rev-parse HEAD > "$evidence/source-base"
git diff --binary > "$evidence/source.patch"
run lints cargo clippy --locked --offline -p wamn-platform-identity -p wamn-identity -p wamn-control-provision -p wamn-runtime -p wamn-ctl --lib --bins
run inventories cargo test --locked --offline -p wamn-proof-conformance --test package_architecture --test profile_selectors --test retained_root_outcomes --test repo_lint --test workspace_tiers --test protected_relations
run platform cargo test --locked --offline -p wamn-proof-system --test deploy_platform_inventory
run control-storage bash /home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-15-1-identity/run-live.sh control-storage "$run_id"
exit "$overall"
