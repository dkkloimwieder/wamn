#!/usr/bin/env bash
set -euo pipefail
run_id=${1:?provide a new run identifier}
[[ "$run_id" =~ ^[a-z0-9][a-z0-9-]*$ ]]
cd /home/kaalin/dev/wamn
[[ $(git branch --show-current) == main ]]
evidence=$PWD/docs/perf/2026.09/ctc8-15-1-identity/integration-$run_id
mkdir "$evidence"
# Stateful proofs run separately against their own fresh PostgreSQL servers.
# The broad sweep must not inherit a connection to the frozen environment.
while IFS= read -r variable; do
    case "$variable" in
        WAMN_*|PGHOST|PGPORT|PGUSER|PGPASSWORD|PGDATABASE|PGSERVICE|PGSERVICEFILE|DATABASE_URL|NATS_URL)
            unset "$variable"
            ;;
    esac
done < <(compgen -e)
unset CARGO_TARGET_DIR
export RUSTC_WRAPPER=sccache
overall=0
trap 'printf "%s\n" "$?" > "$evidence/exit"' EXIT
run() {
    local name=$1 status=0
    shift
    while pgrep -f '^/home/kaalin/.rustup/.*/bin/cargo ' > /dev/null; do sleep 5; done
    printf '%q ' "$@" > "$evidence/$name.command"
    printf '\n' >> "$evidence/$name.command"
    "$@" > "$evidence/$name.log" 2>&1 || status=$?
    printf '%s\n' "$status" > "$evidence/$name.exit"
    printf '%s exit=%s\n' "$name" "$status"
    if [[ "$status" != 0 ]]; then overall=1; fi
}
git rev-parse HEAD > "$evidence/source-head"
git status --porcelain > "$evidence/source-status"
run workspace cargo test --locked --offline --workspace --no-fail-fast -- --include-ignored --test-threads=1
run contracts tools/contract-diff run
exit "$overall"
