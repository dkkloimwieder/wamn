#!/usr/bin/env bash
set -uo pipefail
cd /home/kaalin/.cache/wamn-lanes/wasmcloud-2-9-20260909 || exit 1
export RUSTC_WRAPPER=sccache
export SCCACHE_CACHE_SIZE=50G
export CARGO_BUILD_JOBS=4
wamn_evidence=docs/perf/2026.09/wasmcloud-2-9-cutover/validation-001
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
date -u +%FT%TZ > "$wamn_evidence/started.txt"
run_leg source-inputs python3 "$wamn_evidence/capture-source.py"
run_leg host-build cargo build -p wamn-host --bin wamn-host --locked
run_leg host-arguments python3 "$wamn_evidence/exercise-chart-arguments.py"
run_leg executor-build cargo build -p wamn-executor --bin wamn-run-worker --locked
run_leg native-http-probe cargo check --manifest-path tools/probes/ctc8-13-native-http/Cargo.toml --all-targets --locked
run_leg wasi-http-probe cargo check --manifest-path tools/probes/ctc8-14-wasi-http/Cargo.toml -p ctc8-14-wasi-http-probe --all-targets --locked
run_leg workspace-sweep cargo test --workspace --locked --offline --no-fail-fast -- --include-ignored --nocapture --test-threads=1 --skip regenerate_checked_in_journey_schema --skip regenerate_checked_in_dev_config_schema
printf '%s\n' "$wamn_result" > "$wamn_evidence/exit-code.txt"
date -u +%FT%TZ > "$wamn_evidence/finished.txt"
exit "$wamn_result"
