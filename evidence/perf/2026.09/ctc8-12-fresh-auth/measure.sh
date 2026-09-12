#!/usr/bin/env bash
# Run the existing journey from an exact clean source snapshot.
set -euo pipefail
evidence=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source_tree=${1:?source worktree required}
expected=${2:?expected source commit required}
run_name=${3:?before-NNN or after-NNN required}
[[ $run_name =~ ^(before|after)-[0-9]{3}$ ]]
cd -- "$source_tree"
[[ $(git rev-parse HEAD) == "$expected" ]]
mkdir -m 0700 -- "$evidence/$run_name"
trap 'result=$?; printf "%s\n" "$result" >"$evidence/$run_name/exit"; exit "$result"' EXIT
git rev-parse HEAD
date -u
uptime
for proof in tools/journey-*-proof; do "$proof"; done
tools/receiving-cluster-journey-run --apply --fresh-auth-bench \
  --evidence-dir "$evidence/$run_name/journey"
date -u
uptime
