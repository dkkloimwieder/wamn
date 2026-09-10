#!/usr/bin/env bash
set -uo pipefail
readonly evidence_dir=/home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-20-pat-service/native-001
cd /home/kaalin/.cache/wamn-lanes/ctc8-20-pat-service-20260910 || exit 2
RUSTC_WRAPPER= cargo test --locked --offline \
  -p wamn-identity -p wamn-ctl -p wamn-control-provision \
  --all-targets --no-run >"$evidence_dir/build.log" 2>&1
status=$?
printf '%s\n' "$status" >"$evidence_dir/build.exit"
exit "$status"
