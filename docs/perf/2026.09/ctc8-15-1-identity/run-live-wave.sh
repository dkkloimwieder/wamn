#!/usr/bin/env bash
set -euo pipefail
evidence=/home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-15-1-identity
prerequisite=${1:?provide the completed targeted run name}
wave=${2:?provide a new wave identifier}
[[ "$prerequisite" =~ ^targeted-[0-9]+$ && "$wave" =~ ^[0-9]+$ ]]
test ! -e "$evidence/live-wave-$wave.exit"
trap 'printf "%s\n" "$?" > "$evidence/live-wave-$wave.exit"' EXIT
while [[ ! -f "$evidence/$prerequisite/exit" ]]; do sleep 5; done
test "$(<"$evidence/$prerequisite/exit")" = 0
for suite in keys issuer surface cli protected-update protected-check; do
    bash "$evidence/run-live.sh" "$suite" 001
    printf '%s exit=0\n' "$suite"
done
