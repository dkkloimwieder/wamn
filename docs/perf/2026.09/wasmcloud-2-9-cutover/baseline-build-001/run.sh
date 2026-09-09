#!/usr/bin/env bash
set -uo pipefail
cd /home/kaalin/.cache/wamn-lanes/wasmcloud-2-8-baseline-20260909 || exit 1
wamn_evidence=/home/kaalin/dev/wamn/docs/perf/2026.09/wasmcloud-2-9-cutover/baseline-build-001
export CARGO_BUILD_JOBS=4
export CARGO_TARGET_DIR=/home/kaalin/.cache/wamn-lanes/wasmcloud-2-8-baseline-20260909/target
export RUSTC_WRAPPER=
git rev-parse HEAD > "$wamn_evidence/source.txt"
git status --porcelain > "$wamn_evidence/source-status.txt"
if test -s "$wamn_evidence/source-status.txt"; then exit 2; fi
cat /proc/loadavg > "$wamn_evidence/load-before.txt"
date -u +%FT%TZ > "$wamn_evidence/started.txt"
wamn_status=0
wamn_build() {
  local step=$1
  shift
  printf '%s\n' "$*" > "$wamn_evidence/$step-command.txt"
  "$@" > "$wamn_evidence/$step.log" 2>&1
  local result=$?
  printf '%s\n' "$result" > "$wamn_evidence/$step-exit-code.txt"
  return "$result"
}
wamn_build guests tools/build-components m1 &&
wamn_build host cargo build --locked --release -p wamn-host &&
wamn_build tools cargo build --locked -p wamn-ctl -p wamn-cdc-reader -p wamn-scenario-worker &&
wamn_build gates cargo build --locked --offline -p wamn-gates || wamn_status=$?
printf '%s\n' "$wamn_status" > "$wamn_evidence/exit-code.txt"
date -u +%FT%TZ > "$wamn_evidence/finished.txt"
cat /proc/loadavg > "$wamn_evidence/load-after.txt"
git status --porcelain > "$wamn_evidence/source-status-after.txt"
exit "$wamn_status"
