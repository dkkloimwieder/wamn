#!/usr/bin/env bash
# Invoke with bash. Run only the two corrected inventory assertions, serially.
set +x
set -euo pipefail
umask 077
exec </dev/null

source_tree=/home/kaalin/.cache/wamn-lanes/ctc8-15-2-session-exchange-20260908
evidence_root=/home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-15-2-session
increment=${1:-}
[[ $# == 1 && "$increment" =~ ^[0-9]{3}$ ]] || {
    printf '%s\n' 'usage: bash run-inventory-corrections.sh NNN (a new three-digit increment)' >&2
    exit 2
}
evidence=$evidence_root/inventory-$increment
[[ ! -e "$evidence" && ! -L "$evidence" ]] || {
    printf '%s\n' 'evidence directory already exists; choose a new increment' >&2
    exit 2
}
mkdir -- "$evidence"
overall=0

capture_source() {
    local prefix=$1 path diff_exit
    git -C "$source_tree" rev-parse HEAD > "$evidence/$prefix.base" || return $?
    git -C "$source_tree" status --porcelain=v1 --untracked-files=all > "$evidence/$prefix.status" || return $?
    git -C "$source_tree" diff --binary HEAD > "$evidence/$prefix.patch" || return $?
    git -C "$source_tree" ls-files --others --exclude-standard -z > "$evidence/$prefix.untracked" || return $?
    while IFS= read -r -d '' path; do
        diff_exit=0
        git -C "$source_tree" diff --no-index --binary -- /dev/null "$path" >> "$evidence/$prefix.patch" || diff_exit=$?
        [[ $diff_exit -le 1 ]] || return "$diff_exit"
    done < "$evidence/$prefix.untracked"
    sha256sum -- "$evidence/$prefix.patch" > "$evidence/$prefix.patch.sha256"
}

finish() {
    local shell_exit=$? capture_exit=0
    trap - EXIT
    set +e
    if [[ $overall == 0 && $shell_exit != 0 ]]; then overall=$shell_exit; fi
    capture_source after-source || capture_exit=$?
    printf '%s\n' "$capture_exit" > "$evidence/source-capture.exit"
    if [[ $overall == 0 && $capture_exit != 0 ]]; then overall=$capture_exit; fi
    date -u +%FT%TZ > "$evidence/finished"
    printf '%s\n' "$overall" > "$evidence/exit"
    exit "$overall"
}
trap finish EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
cd "$source_tree"
date -u +%FT%TZ > "$evidence/started"
sha256sum -- "$evidence_root/run-inventory-corrections.sh" > "$evidence/runner.sha256"
capture_source source

# Remove inherited fixture authority without recording its values.
while IFS= read -r variable; do
    if [[ "$variable" == WAMN_* ]]; then unset "$variable"; fi
done < <(compgen -e)
unset PGHOST PGPORT PGUSER PGPASSWORD PGDATABASE PGSERVICE PGSERVICEFILE DATABASE_URL NATS_URL
[[ -z ${CARGO_TARGET_DIR+x} ]] || {
    printf '%s\n' 'Unset CARGO_TARGET_DIR; this lane must use its own default target.' > "$evidence/refusal.log"
    exit 2
}
export RUSTC_WRAPPER=sccache
export CARGO_TERM_COLOR=never
printf 'cwd=%s\nRUSTC_WRAPPER=sccache\nCARGO_TARGET_DIR=<unset; worktree default>\nWAMN_* and named database/NATS variables cleared\n' \
    "$source_tree" > "$evidence/environment"

run() {
    local name=$1 task_exit=0
    shift
    printf '%q ' "$@" > "$evidence/$name.command"
    printf '\n' >> "$evidence/$name.command"
    "$@" > "$evidence/$name.log" 2>&1 || task_exit=$?
    printf '%s\n' "$task_exit" > "$evidence/$name.exit"
    if [[ $task_exit != 0 && $overall == 0 ]]; then overall=$task_exit; fi
}

run platform-grain cargo test --locked --offline -p wamn-control-provision \
    --test deploy_sql_authority the_platform_grain_family_set_is_pinned_and_not_derived \
    -- --include-ignored --exact --test-threads=1
run frozen-label cargo test --locked --offline -p wamn-ctl --lib \
    provision_project_env::tests::every_workload_family_carries_a_distinct_frozen_label \
    -- --include-ignored --exact --test-threads=1
exit "$overall"
