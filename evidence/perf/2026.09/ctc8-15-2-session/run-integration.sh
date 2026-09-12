#!/usr/bin/env bash
# Invoke with bash. All results stay in the evidence directory, including detached runs.
set +x
set -euo pipefail
umask 077
exec </dev/null

source_tree=/home/kaalin/.cache/wamn-lanes/ctc8-15-2-session-exchange-20260908
evidence_root=/home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-15-2-session
increment=${1:-}
[[ $# == 1 && "$increment" =~ ^[0-9]{3}$ ]] || {
    printf '%s\n' 'usage: bash run-integration.sh NNN (a new three-digit increment)' >&2
    exit 2
}
evidence=$evidence_root/integration-$increment
[[ ! -e "$evidence" && ! -L "$evidence" ]] || {
    printf '%s\n' 'evidence directory already exists; choose a new increment' >&2
    exit 2
}
mkdir -- "$evidence"
overall=0
finish() {
    local shell_exit=$? capture_exit=0
    trap - EXIT
    set +e
    if [[ $overall == 0 && $shell_exit != 0 ]]; then overall=$shell_exit; fi
    git -C "$source_tree" rev-parse HEAD > "$evidence/after-source.base" || capture_exit=$?
    git -C "$source_tree" status --porcelain=v1 --untracked-files=all > "$evidence/after-source.status" || capture_exit=$?
    if [[ -f "$evidence/source.base" && -f "$evidence/source.status" ]]; then
        cmp "$evidence/source.base" "$evidence/after-source.base" > "$evidence/source-head-change.log" 2>&1 || capture_exit=$?
        cmp "$evidence/source.status" "$evidence/after-source.status" > "$evidence/source-status-change.log" 2>&1 || capture_exit=$?
    fi
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
sha256sum -- "$evidence_root/run-integration.sh" > "$evidence/runner.sha256"
git rev-parse HEAD > "$evidence/source.base"
git status --porcelain=v1 --untracked-files=all > "$evidence/source.status"
git rev-parse main > "$evidence/main.base"

# Clear inherited live-fixture authority without recording any values.
while IFS= read -r variable; do
    if [[ "$variable" == WAMN_* ]]; then unset "$variable"; fi
done < <(compgen -e)
unset PGHOST PGPORT PGUSER PGPASSWORD PGDATABASE PGSERVICE PGSERVICEFILE DATABASE_URL NATS_URL
export RUSTC_WRAPPER=sccache
export CARGO_TERM_COLOR=never
printf 'cwd=%s\nRUSTC_WRAPPER=sccache\nCARGO_TARGET_DIR must be unset\nWAMN_* and named database/NATS variables cleared\n' \
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

cargo_preflight() {
    [[ -z ${CARGO_TARGET_DIR+x} ]] || {
        printf '%s\n' 'Unset CARGO_TARGET_DIR; this lane must use its own default target.'
        return 1
    }
    command -v sccache >/dev/null || return 1
    # Record shared load without blocking independent worktrees.
    # The commands below remain serial and use this worktree's default target.
    ps -eo pid=,comm= | awk '$2 == "cargo" || $2 == "rustc" {print}'
}

run main-ancestry git merge-base --is-ancestor "$(< "$evidence/main.base")" HEAD
[[ $(< "$evidence/main-ancestry.exit") == 0 ]] || exit "$overall"
run cargo-preflight cargo_preflight
[[ $(< "$evidence/cargo-preflight.exit") == 0 ]] || exit "$overall"
run workspace cargo test --locked --offline --workspace --no-fail-fast -- --include-ignored --test-threads=1
run contract-diff tools/contract-diff run
exit "$overall"
