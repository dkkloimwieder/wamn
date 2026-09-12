#!/usr/bin/env bash
# Capture the existing deployed membership gate from a clean source snapshot.
set -euo pipefail
evidence=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
source_tree=${1:?source worktree required}
expected=${2:?expected source commit required}
run_name=${3:?membership-NNN required}
[[ $run_name =~ ^membership-[0-9]{3}$ ]]
cd -- "$source_tree"
[[ $(git rev-parse HEAD) == "$expected" ]]
[[ -z $(git status --porcelain) ]]
mkdir -m 0700 -- "$evidence/$run_name"
trap 'result=$?; printf "%s\n" "$result" >"$evidence/$run_name/exit"; exit "$result"' EXIT
git rev-parse HEAD
date -u
tools/receiving-cluster-journey-run --apply --membershipproof \
  --evidence-dir "$evidence/$run_name/journey"
date -u
