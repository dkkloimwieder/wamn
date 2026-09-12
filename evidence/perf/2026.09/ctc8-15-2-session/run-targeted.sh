#!/usr/bin/env bash
set -euo pipefail
source_tree=${1:?provide the identity worktree}
evidence=/home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-15-2-session/${2:?provide a new targeted run name}
[[ "$source_tree" == /home/kaalin/.cache/wamn-lanes/ctc8-15-2-session-exchange-20260908 ]]
[[ "$evidence" =~ /targeted-[0-9]+$ && ! -e "$evidence/exit" ]]
mkdir -p "$evidence"
cd "$source_tree"
trap 'printf "%s\n" "$?" > "$evidence/exit"' EXIT
export RUSTC_WRAPPER=sccache
git rev-parse HEAD > "$evidence/source-base"
git diff --binary > "$evidence/source.patch"
run() {
    local name=$1 task_exit=0
    shift
    printf '%q ' "$@" > "$evidence/$name.command"
    printf '\n' >> "$evidence/$name.command"
    "$@" > "$evidence/$name.log" 2>&1 || task_exit=$?
    printf '%s\n' "$task_exit" > "$evidence/$name.exit"
    printf '%s exit=%s\n' "$name" "$task_exit"
    return "$task_exit"
}
if [[ "${3:-full}" == full ]]; then
    run metadata cargo metadata --offline --format-version 1
    run provision cargo test --locked --offline -p wamn-control-provision --lib
    run target cargo test --locked --offline -p wamn-control-provision --test session_target
fi
run identity cargo test --locked --offline -p wamn-identity --lib
run issuer-cli cargo test --locked --offline -p wamn-ctl --lib identity_issuer
run reader-cli cargo test --locked --offline -p wamn-ctl --lib session_reader
run ctl-binary cargo build --locked --offline -p wamn-ctl --bin wamn-ctl
run observer cargo test --locked --offline -p wamn-proof-integration --lib identity_session_proof
