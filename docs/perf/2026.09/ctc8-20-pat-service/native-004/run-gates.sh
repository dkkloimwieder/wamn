#!/usr/bin/env bash
set -uo pipefail
readonly evidence_dir=/home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-20-pat-service/native-004
cd /home/kaalin/.cache/wamn-lanes/ctc8-20-pat-service-20260910 || exit 2
export RUSTC_WRAPPER=
cargo test --locked --offline -p wamn-identity -p wamn-ctl \
  -p wamn-control-provision --lib --no-fail-fast -- --test-threads=1 \
  >"$evidence_dir/unit.log" 2>&1
unit_status=$?
printf '%s\n' "$unit_status" >"$evidence_dir/unit.exit"
cargo check --workspace --all-targets --locked --offline --keep-going \
  >"$evidence_dir/workspace.log" 2>&1
workspace_status=$?
printf '%s\n' "$workspace_status" >"$evidence_dir/workspace.exit"
cargo clippy --locked --offline -p wamn-identity -p wamn-ctl \
  -p wamn-control-provision --all-targets --no-deps \
  >"$evidence_dir/clippy.log" 2>&1
clippy_status=$?
printf '%s\n' "$clippy_status" >"$evidence_dir/clippy.exit"
if (( unit_status || workspace_status || clippy_status )); then exit 1; fi
