#!/usr/bin/env bash
# Capture the combined-tree gates without inherited live-service credentials.
set -euo pipefail
evidence=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source_tree=${1:?integration worktree required}
expected=${2:?source commit required}
run_name=${3:?integration-NNN required}
[[ $run_name =~ ^integration-[0-9]{3}$ ]]
cd -- "$source_tree"
[[ $(git rev-parse HEAD) == "$expected" ]]
[[ -z $(git status --porcelain) ]]
mkdir -m 0700 -- "$evidence/$run_name"
output=$evidence/$run_name
trap 'result=$?; printf "%s\n" "$result" >"$output/exit"; exit "$result"' EXIT
trap 'exit 130' HUP INT TERM
git rev-parse HEAD >"$output/source-head"
date -u >"$output/started"
uptime >"$output/load-start"
failure=0
clean_env=(env -i "HOME=$HOME" "PATH=$PATH" "USER=${USER:-kaalin}" LANG=C.UTF-8 RUSTC_WRAPPER=sccache)
run_gate() {
  local label=$1 status=0
  shift
  if ps -eo comm= | awk '$1 == "cargo" || $1 == "rustc" || $1 == "oha" || $1 == "pgbench" { found=1 } END { exit !found }'; then
    printf 'Build/load slot occupied before %s\n' "$label" >&2
    return 75
  fi
  printf '%q ' "${clean_env[@]}" "$@" >>"$output/commands.log"
  printf '\n' >>"$output/commands.log"
  date -u >"$output/$label.started"
  "${clean_env[@]}" "$@" >"$output/$label.log" 2>&1 || status=$?
  printf '%s\n' "$status" >"$output/$label.exit"
  date -u >"$output/$label.finished"
  printf '%s exit=%s\n' "$label" "$status"
  if (( status != 0 )); then failure=1; fi
}
run_gate workspace cargo test --workspace --all-targets --no-fail-fast --locked --offline -- --include-ignored --nocapture
run_gate doctests cargo test --workspace --doc --no-fail-fast --locked --offline -- --include-ignored --nocapture
run_gate contracts tools/contract-diff run
run_gate focused bash "$evidence/quality.sh" "$source_tree" "$expected" quality-001
date -u >"$output/finished"
uptime >"$output/load-end"
exit "$failure"
