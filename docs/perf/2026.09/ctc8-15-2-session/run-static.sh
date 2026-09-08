#!/usr/bin/env bash
# Invoke with bash. This capture uses one lane and never waits for Cargo.
set +x
set -euo pipefail
umask 077

source_tree=/home/kaalin/.cache/wamn-lanes/ctc8-15-2-session-exchange-20260908
evidence_root=/home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-15-2-session
increment=${1:-}
[[ $# == 1 && "$increment" =~ ^[0-9]{3}$ ]] || {
    printf '%s\n' 'usage: bash run-static.sh NNN (a new three-digit increment)' >&2
    exit 2
}
evidence=$evidence_root/static-$increment
[[ ! -e "$evidence" && ! -L "$evidence" ]] || {
    printf '%s\n' 'evidence directory already exists; choose a new increment' >&2
    exit 2
}
mkdir -- "$evidence"
overall=0
cargo_ready=false
finish() {
    local result=$?
    trap - EXIT
    [[ $result == 0 ]] || overall=$result
    date -u +%FT%TZ > "$evidence/finished"
    printf '%s\n' "$overall" > "$evidence/exit"
    printf 'STATIC exit=%s evidence=%s\n' "$overall" "$evidence"
    exit "$overall"
}
trap finish EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
cd "$source_tree"
date -u +%FT%TZ > "$evidence/started"
sha256sum -- "$evidence_root/run-static.sh" > "$evidence/runner.sha256"
export RUSTC_WRAPPER=sccache
export CARGO_TERM_COLOR=never
printf 'cwd=%s\nRUSTC_WRAPPER=sccache\nCARGO_TARGET_DIR must be unset\n' "$source_tree" > "$evidence/environment"

capture_source() {
    local prefix=$1 path diff_exit
    git rev-parse HEAD > "$evidence/$prefix.base"
    git status --porcelain=v1 --untracked-files=all > "$evidence/$prefix.status"
    git diff --binary HEAD > "$evidence/$prefix.patch"
    while IFS= read -r -d '' path; do
        diff_exit=0
        git diff --no-index --binary -- /dev/null "$path" >> "$evidence/$prefix.patch" || diff_exit=$?
        [[ $diff_exit -le 1 ]] || return "$diff_exit"
    done < <(git ls-files --others --exclude-standard -z)
    sha256sum -- "$evidence/$prefix.patch" > "$evidence/$prefix.patch.sha256"
}

run() {
    local name=$1 task_exit=0
    shift
    printf '%q ' "$@" > "$evidence/$name.command"
    printf '\n' >> "$evidence/$name.command"
    if [[ $1 == cargo && "$cargo_ready" != true ]]; then
        printf '%s\n' 'NOT RUN: the single Cargo preflight failed.' > "$evidence/$name.log"
        task_exit=125
    else
        "$@" > "$evidence/$name.log" 2>&1 || task_exit=$?
    fi
    printf '%s\n' "$task_exit" > "$evidence/$name.exit"
    printf '%s exit=%s\n' "$name" "$task_exit"
    if [[ $task_exit != 0 && $overall == 0 ]]; then overall=$task_exit; fi
}

cargo_preflight() {
    [[ -z ${CARGO_TARGET_DIR+x} ]] || {
        printf '%s\n' 'Unset CARGO_TARGET_DIR; this lane must use its own default target.'
        return 1
    }
    command -v sccache >/dev/null || return 1
    # Match process names only. Never capture command arguments or credentials.
    ps -eo pid=,comm= | awk '$2 == "cargo" || $2 == "rustc" {busy=1; print} END {exit busy}'
}

capture_source source
run diff-check git diff --check
run bash-syntax bash -n tools/identity-jwks-journey-run
run helm helm template wamn-identity deploy/platform/identity --namespace wamn-system \
    --set-string issuer=https://identity.static-proof.internal \
    --set-string databaseSecret=identity-static-issuer \
    --set-string tlsSecret=identity-static-tls \
    --set-string 'sessionTargetSecrets[0]=identity-static-acme-dev' \
    --set-string 'sessionTargetSecrets[1]=identity-static-other-dev'
printf '%s\n' 'Standalone Secret-list assertion not run: yq was unavailable when this capture was prepared. The actual render is in helm.log; deploy_platform_inventory supplies the existing rendered-manifest checks.' \
    > "$evidence/helm-secret-list.note"
run cargo-preflight cargo_preflight
if [[ $(< "$evidence/cargo-preflight.exit") == 0 ]]; then cargo_ready=true; fi
run architecture cargo test --locked --offline --no-fail-fast -p wamn-proof-conformance \
    --test package_architecture --test workspace_tiers -- --include-ignored --nocapture
run platform cargo test --locked --offline -p wamn-proof-system \
    --test deploy_platform_inventory -- --include-ignored --nocapture
run clippy cargo clippy --locked --offline -p wamn-control-provision -p wamn-identity \
    -p wamn-ctl -p wamn-proof-integration -p wamn-gates --all-targets
capture_source after-source
run source-head-unchanged cmp "$evidence/source.base" "$evidence/after-source.base"
run source-status-unchanged cmp "$evidence/source.status" "$evidence/after-source.status"
run source-diff-unchanged cmp "$evidence/source.patch" "$evidence/after-source.patch"
