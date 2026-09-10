#!/usr/bin/env bash
set -euo pipefail
readonly evidence_dir=/home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-20-pat-service/helm-001
readonly lane=/home/kaalin/.cache/wamn-lanes/ctc8-20-pat-service-20260910
cd "$lane"
export RUSTC_WRAPPER=
name=$1
shift
test ! -e "$evidence_dir/$name.log"
git rev-parse HEAD >"$evidence_dir/$name.source"
git diff --binary >"$evidence_dir/$name.patch"
sha256sum deploy/platform/identity/templates/deployment.yaml >"$evidence_dir/$name.template.sha256"
printf '%q ' "$@" >"$evidence_dir/$name.command"
printf '\n' >>"$evidence_dir/$name.command"
set +e
"$@" >"$evidence_dir/$name.log" 2>&1
status=$?
set -e
printf '%s\n' "$status" >"$evidence_dir/$name.exit"
exit "$status"
