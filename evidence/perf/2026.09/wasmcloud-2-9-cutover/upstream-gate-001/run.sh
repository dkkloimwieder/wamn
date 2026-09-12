#!/usr/bin/env bash
set -uo pipefail
cd /home/kaalin/.cache/wamn-lanes/wasmcloud-2-9-20260909 || exit 1
export RUSTC_WRAPPER=sccache
export SCCACHE_CACHE_SIZE=50G
export CARGO_BUILD_JOBS=4
wamn_evidence=docs/perf/2026.09/wasmcloud-2-9-cutover/upstream-gate-001
date -u +%FT%TZ > "$wamn_evidence/started.txt"
tools/wasmcloud-release-check run /home/kaalin/.cache/wamn-lanes/upstream-wasmcloud-2-9-20260909 > "$wamn_evidence/release-check.log" 2>&1
wamn_status=$?
printf '%s\n' "$wamn_status" > "$wamn_evidence/exit-code.txt"
date -u +%FT%TZ > "$wamn_evidence/finished.txt"
exit "$wamn_status"
