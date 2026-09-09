#!/usr/bin/env bash
set -uo pipefail
cd /home/kaalin/.cache/wamn-lanes/wasmcloud-2-9-20260909 || exit 1
export RUSTC_WRAPPER=sccache
export SCCACHE_CACHE_SIZE=50G
export CARGO_BUILD_JOBS=4
wamn_evidence=docs/perf/2026.09/wasmcloud-2-9-cutover/validation-003
wamn_result=0
run_leg() {
  local wamn_label="$1"
  shift
  printf '%q ' "$@" > "$wamn_evidence/$wamn_label-command.txt"
  printf '\n' >> "$wamn_evidence/$wamn_label-command.txt"
  date -u +%FT%TZ > "$wamn_evidence/$wamn_label-started.txt"
  "$@" > "$wamn_evidence/$wamn_label.log" 2>&1
  local wamn_status=$?
  printf '%s\n' "$wamn_status" > "$wamn_evidence/$wamn_label-exit-code.txt"
  date -u +%FT%TZ > "$wamn_evidence/$wamn_label-finished.txt"
  if [[ "$wamn_status" != 0 ]]; then wamn_result="$wamn_status"; fi
}
run_leg lifecycle-tests cargo test -p wamn-runtime --lib lifecycle::tests --locked --offline -- --include-ignored --nocapture --test-threads=1
run_leg runtime-clippy cargo clippy -p wamn-runtime --all-targets --locked --offline
printf '%s\n' "$wamn_result" > "$wamn_evidence/exit-code.txt"
