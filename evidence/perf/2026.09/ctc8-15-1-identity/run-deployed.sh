#!/usr/bin/env bash
set -euo pipefail
run_id=${1:?provide a new run identifier}
[[ "$run_id" =~ ^[a-z0-9][a-z0-9-]*$ ]]
evidence_root=/home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-15-1-identity
trap 'printf "%s\n" "$?" > "$evidence_root/deployed-$run_id.launch.exit"' EXIT
cd /home/kaalin/.cache/wamn-lanes/ctc8-15-session-foundation-20260908
while pgrep -f '^/home/kaalin/.rustup/.*/bin/cargo ' > /dev/null; do sleep 5; done
tools/identity-jwks-journey-run --apply --evidence-dir "$evidence_root/deployed-$run_id"
