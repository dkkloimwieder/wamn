#!/usr/bin/env bash
set -uo pipefail
readonly evidence_dir=/home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-20-pat-service/native-002
cd /home/kaalin/.cache/wamn-lanes/ctc8-20-pat-service-20260910 || exit 2
export RUSTC_WRAPPER=
cargo test --locked --offline -p wamn-identity -p wamn-ctl \
  -p wamn-control-provision --lib --no-fail-fast -- --test-threads=1 \
  >"$evidence_dir/unit.log" 2>&1
status=$?
printf '%s\n' "$status" >"$evidence_dir/unit.exit"
if (( status != 0 )); then exit "$status"; fi
cargo test --locked --offline -p wamn-ctl --test pat_bootstrap_live --no-run \
  >"$evidence_dir/bootstrap-build.log" 2>&1
status=$?
printf '%s\n' "$status" >"$evidence_dir/bootstrap-build.exit"
if (( status != 0 )); then exit "$status"; fi
cargo build --locked --offline -p wamn-identity -p wamn-ctl --bins \
  >"$evidence_dir/binaries.log" 2>&1
status=$?
printf '%s\n' "$status" >"$evidence_dir/binaries.exit"
exit "$status"
