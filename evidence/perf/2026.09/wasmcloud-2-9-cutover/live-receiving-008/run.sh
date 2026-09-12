#!/usr/bin/env bash
set -uo pipefail
cd /home/kaalin/.cache/wamn-lanes/wasmcloud-2-9-20260909 || exit 1
wamn_evidence=/home/kaalin/dev/wamn/docs/perf/2026.09/wasmcloud-2-9-cutover/live-receiving-008
export CARGO_BUILD_JOBS=4
git rev-parse HEAD > "$wamn_evidence/source.txt"
git status --porcelain > "$wamn_evidence/source-status.txt"
cat /proc/loadavg > "$wamn_evidence/load-before.txt"
date -u +%FT%TZ > "$wamn_evidence/started.txt"
printf '%s\n' 'tools/receiving-cluster-journey-run --apply --evidence-dir /home/kaalin/dev/wamn/docs/perf/2026.09/wasmcloud-2-9-cutover/live-receiving-008/journey' > "$wamn_evidence/command.txt"
tools/receiving-cluster-journey-run --apply --evidence-dir "$wamn_evidence/journey" > "$wamn_evidence/journey.log" 2>&1
wamn_status=$?
printf '%s\n' "$wamn_status" > "$wamn_evidence/exit-code.txt"
date -u +%FT%TZ > "$wamn_evidence/finished.txt"
cat /proc/loadavg > "$wamn_evidence/load-after.txt"
exit "$wamn_status"
