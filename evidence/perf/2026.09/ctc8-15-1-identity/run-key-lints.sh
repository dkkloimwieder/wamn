#!/usr/bin/env bash
set -euo pipefail
evidence=/home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-15-1-identity/key-lints-001
mkdir "$evidence"
cd /home/kaalin/.cache/wamn-lanes/ctc8-15-session-foundation-20260908
export RUSTC_WRAPPER=sccache
trap 'printf "%s\n" "$?" > "$evidence/exit"' EXIT
while pgrep -f '^/home/kaalin/.rustup/.*/bin/cargo ' > /dev/null; do sleep 5; done
git rev-parse HEAD > "$evidence/source-base"
git diff --binary > "$evidence/source.patch"
cargo clippy --locked --offline -p wamn-platform-identity -p wamn-identity --lib \
  > "$evidence/clippy.log" 2>&1
