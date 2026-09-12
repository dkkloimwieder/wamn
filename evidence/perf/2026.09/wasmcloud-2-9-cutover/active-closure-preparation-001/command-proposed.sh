#!/usr/bin/env bash
# PREPARED ONLY. Root runs after WMS ends, both patches are reviewed and committed,
# and the serialized build/live slot is free. This command has not run.
set -euo pipefail
: "${CUTOVER_ACTIVE_CLOSURE_SOURCE:?set the full fixed source commit after integration}"
python3 /home/kaalin/dev/wamn/docs/perf/2026.09/wasmcloud-2-9-cutover/virtualization-preparation-001/run-virtualization.py \
  --repo /home/kaalin/.cache/wamn-lanes/wasmcloud-2-9-20260909 \
  --source "$CUTOVER_ACTIVE_CLOSURE_SOURCE" \
  --evidence-dir /home/kaalin/dev/wamn/docs/perf/2026.09/wasmcloud-2-9-cutover/virtualization-live-003
