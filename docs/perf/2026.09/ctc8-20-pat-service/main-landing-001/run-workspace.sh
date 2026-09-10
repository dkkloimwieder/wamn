#!/usr/bin/env bash
set -euo pipefail
cd /home/kaalin/dev/wamn
readonly evidence_dir=docs/perf/2026.09/ctc8-20-pat-service/main-landing-001
test ! -e "$evidence_dir/workspace.log"
git rev-parse HEAD >"$evidence_dir/workspace.source"
git diff --binary -- . ':!.beads' >"$evidence_dir/workspace.patch"
export RUSTC_WRAPPER=
printf '%s\n' 'cargo test --workspace --no-fail-fast --locked --offline -- --include-ignored --nocapture --test-threads=1 --skip regenerate_checked_in_journey_schema --skip regenerate_checked_in_dev_config_schema' >"$evidence_dir/workspace.command"
set +e
cargo test --workspace --no-fail-fast --locked --offline -- \
  --include-ignored --nocapture --test-threads=1 \
  --skip regenerate_checked_in_journey_schema \
  --skip regenerate_checked_in_dev_config_schema >"$evidence_dir/workspace.log" 2>&1
status=$?
set -e
printf '%s\n' "$status" >"$evidence_dir/workspace.exit"
git diff --binary -- . ':!.beads' >"$evidence_dir/workspace.after.patch"
exit "$status"
