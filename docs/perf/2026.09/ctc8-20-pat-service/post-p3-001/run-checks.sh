#!/usr/bin/env bash
set -euo pipefail
readonly evidence_dir=/home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-20-pat-service/post-p3-001
readonly lane=/home/kaalin/.cache/wamn-lanes/ctc8-20-pat-service-20260910
cd "$lane"
test -z "$(git status --porcelain)"
git rev-parse HEAD >"$evidence_dir/source.commit"
git rev-parse origin/main >"$evidence_dir/main.commit"
export RUSTC_WRAPPER=
run_gate() {
  local name=$1
  shift
  set +e
  "$@" >"$evidence_dir/$name.log" 2>&1
  local status=$?
  set -e
  printf '%s\n' "$status" >"$evidence_dir/$name.exit"
  return "$status"
}
run_gate syntax bash -n tools/receiving-cluster-journey-run tools/wms-cluster-journey-run
run_gate dry-run tools/receiving-cluster-journey-run --receiving-correctness \
  --evidence-dir /home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-20-pat-service/receiving-001
run_gate unit cargo test --locked --offline -p wamn-identity -p wamn-ctl \
  -p wamn-control-provision --lib --no-fail-fast -- --test-threads=1
run_gate workspace cargo check --workspace --all-targets --locked --offline --keep-going
