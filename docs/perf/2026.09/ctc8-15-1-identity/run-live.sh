#!/usr/bin/env bash
set -euo pipefail

suite=${1:?name one of keys, issuer, surface, cli, control-storage, protected-update, protected-check}
run_id=${2:?provide a new run identifier}
[[ "$run_id" =~ ^[a-z0-9][a-z0-9-]*$ ]]
case "$suite" in
    keys|issuer|surface|cli|control-storage|protected-update|protected-check) ;;
    *) exit 64 ;;
esac
source_tree=/home/kaalin/.cache/wamn-lanes/ctc8-15-session-foundation-20260908
evidence=/home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-15-1-identity/live-$suite-$run_id
container=wamn-ctc8-15-1-$suite-$run_id
mkdir "$evidence"
cd "$source_tree"
git rev-parse HEAD > "$evidence/source-base"
git diff --binary > "$evidence/source.patch"
export RUSTC_WRAPPER=sccache
while pgrep -f '^/home/kaalin/.rustup/.*/bin/cargo ' > /dev/null; do
    sleep 5
done
docker container inspect "$container" > /dev/null 2>&1 && exit 65
docker run -d --name "$container" -e POSTGRES_PASSWORD=identity-proof \
    -e POSTGRES_DB=wamn_system -p 127.0.0.1::5432 postgres:18 \
    > "$evidence/container-id"
cleanup() {
    local status=$?
    trap - EXIT
    docker logs "$container" > "$evidence/postgres.log" 2>&1 || true
    docker rm -fv "$container" > "$evidence/cleanup.log" 2>&1 || status=1
    printf '%s\n' "$status" > "$evidence/exit"
    exit "$status"
}
trap cleanup EXIT
port=$(docker inspect --format '{{(index (index .NetworkSettings.Ports "5432/tcp") 0).HostPort}}' "$container")
[[ "$port" =~ ^[0-9]+$ ]]
database_url=postgres://postgres:identity-proof@127.0.0.1:$port/wamn_system
ready=0
for attempt in {1..60}; do
    if psql "$database_url" -XAtqc 'select 1' > /dev/null 2>&1; then
        ready=1
        break
    fi
    sleep 1
done
test "$ready" = 1
psql "$database_url" -XAtqc 'select version(), current_database()' > "$evidence/server"
case "$suite" in
    keys)
        export WAMN_SESSION_KEYS_PG_URL=$database_url WAMN_SESSION_KEYS_ALLOW_SCHEMA_RESET=1
        command=(cargo test --locked --offline -p wamn-platform-identity --test session_keys_live)
        ;;
    issuer)
        export WAMN_IDENTITY_ISSUER_PG_URL=$database_url WAMN_IDENTITY_ISSUER_ALLOW_SCHEMA_RESET=1
        command=(cargo test --locked --offline -p wamn-control-provision --test identity_issuer_live)
        ;;
    surface)
        export WAMN_IDENTITY_SERVICE_PG_URL=$database_url WAMN_IDENTITY_SERVICE_ALLOW_SCHEMA_RESET=1
        command=(cargo test --locked --offline -p wamn-identity --test https_surface)
        ;;
    cli)
        export WAMN_IDENTITY_ISSUER_CLI_PG_URL=$database_url WAMN_IDENTITY_ISSUER_CLI_ALLOW_SCHEMA_RESET=1
        command=(cargo test --locked --offline -p wamn-ctl --test identity_issuer_live)
        ;;
    control-storage)
        export WAMN_REGISTRY_PG_URL=$database_url
        command=(cargo test --locked --offline -p wamn-control-provision --test control_storage)
        ;;
    protected-*)
        export WAMN_CTL_PG_URL=$database_url
        if [[ "$suite" == protected-update ]]; then
            export WAMN_UPDATE_PROTECTED_RELATIONS=1
        else
            unset WAMN_UPDATE_PROTECTED_RELATIONS
        fi
        command=(cargo test --locked --offline -p wamn-ctl --features ops \
            --test protected_relations_live protected_relations_match_reconciled_postgres)
        ;;
esac
command+=(-- --include-ignored --nocapture --test-threads=1)
printf '%q ' "${command[@]}" > "$evidence/test.command"
printf '\n' >> "$evidence/test.command"
"${command[@]}" > "$evidence/test.log" 2>&1
