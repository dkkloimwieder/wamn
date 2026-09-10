#!/usr/bin/env bash
set -uo pipefail
readonly evidence_root=/home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-20-pat-service
cd /home/kaalin/.cache/wamn-lanes/ctc8-20-pat-service-20260910 || exit 2
tools/receiving-cluster-journey-run --apply --receiving-correctness \
  --evidence-dir "$evidence_root/receiving-001" \
  >"$evidence_root/post-p3-001/receiving.console.log" 2>&1
status=$?
printf '%s\n' "$status" >"$evidence_root/post-p3-001/receiving.exit"
exit "$status"
