#!/usr/bin/env bash
set -euo pipefail
readonly evidence_dir=/home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-20-pat-service/native-006
cd /home/kaalin/.cache/wamn-lanes/ctc8-20-pat-service-20260910
test -z "$(git status --porcelain)"
git rev-parse HEAD >"$evidence_dir/source.commit"
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
run_gate unit cargo test --locked --offline -p wamn-identity -p wamn-ctl \
  -p wamn-control-provision --lib --no-fail-fast -- --test-threads=1
run_gate session-token cargo test --locked --offline -p wamn-platform-identity --test session_token
run_gate session-cache cargo test --locked --offline -p wamn-runtime --features test-util \
  --test session_keys -- --test-threads=1
run_gate identity-binary cargo build --locked --offline -p wamn-identity --bin wamn-identity
git diff --exit-code >"$evidence_dir/source.restore.log"
