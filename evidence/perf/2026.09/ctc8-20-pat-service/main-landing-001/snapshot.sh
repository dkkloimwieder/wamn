#!/usr/bin/env bash
set -euo pipefail
cd /home/kaalin/dev/wamn
readonly evidence_dir=docs/perf/2026.09/ctc8-20-pat-service/main-landing-001
name=$1
test ! -e "$evidence_dir/$name.files"
git rev-parse HEAD >"$evidence_dir/$name.commit"
git diff --cached --binary >"$evidence_dir/$name.index.patch"
git status --porcelain=v1 --untracked-files=all >"$evidence_dir/$name.status"
git ls-files --modified --others --exclude-standard -z |
  while IFS= read -r -d '' path; do
    case "$path" in
      docs/perf/2026.09/ctc8-20-pat-service/*|docs/perf/2026.09/claim-transaction/*) continue ;;
    esac
    stat --printf='%a ' -- "$path"
    sha256sum -- "$path"
  done | sort >"$evidence_dir/$name.files"
