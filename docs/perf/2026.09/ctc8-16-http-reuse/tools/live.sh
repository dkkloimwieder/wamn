#!/usr/bin/env bash
# Invoke with bash. Each invocation owns one fresh PostgreSQL and OCI registry.
set +x
set -euo pipefail
umask 077

fail() { printf '%s\n' "$1" >&2; exit 1; }
[[ $# == 1 ]] || fail 'usage: bash live.sh EXACT_TEST_NAME'
test_name=$1
nested=0
case "$test_name" in
    trusted_http_route::tests::real_http_guest_reuses_connections_without_reusing_authority) ;;
    trusted_http_route::tests::nested_http_authorizes_child_and_preserves_original_caller) nested=1 ;;
    *) fail 'name one of the two trusted_http_route::tests live tests exactly' ;;
esac

for tool in docker psql curl openssl rg timeout sha256sum git readlink; do
    command -v "$tool" >/dev/null || fail "required tool is absent: $tool"
done
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
source_tree=$(git -C "$script_dir" rev-parse --show-toplevel)
cd -- "$source_tree"
[[ -f tests/integration/src/trusted_http_route.rs ]] || fail 'integration test source is absent'

# An explicit binary is useful when a debug target has several Cargo hashes.
# Otherwise require exactly one executable, not the newest or an arbitrary one.
test_binary=${WAMN_HTTP_REUSE_TEST_BINARY:-}
target_dir=${CARGO_TARGET_DIR:-$source_tree/target}
if [[ -z "$test_binary" ]]; then
    [[ -d "$target_dir/debug/deps" ]] || fail 'build the integration test binary before this runner'
    candidates=()
    while IFS= read -r candidate; do
        [[ ${candidate##*/} =~ ^wamn_integration_tests-[0-9a-f]+$ && -x "$candidate" ]] || continue
        candidates+=("$candidate")
    done < <(rg --files --hidden --no-ignore "$target_dir/debug/deps" -g 'wamn_integration_tests-*')
    [[ ${#candidates[@]} == 1 ]] || fail 'set WAMN_HTTP_REUSE_TEST_BINARY to the exact built integration test binary'
    test_binary=${candidates[0]}
fi
test_binary=$(readlink -f -- "$test_binary")
[[ -f "$test_binary" && -x "$test_binary" && "$test_binary" == */debug/deps/wamn_integration_tests-* ]] ||
    fail 'WAMN_HTTP_REUSE_TEST_BINARY must name the built debug integration libtest binary'

component_wasm=${WAMN_HTTP_REUSE_COMPONENT_WASM:-}
if [[ -z "$component_wasm" ]]; then
    for candidate in \
        "$source_tree/components/no-std/target/wasm32-wasip2/debug/http_request.wasm" \
        "$target_dir/wasm32-wasip2/debug/http_request.wasm"; do
        if [[ -f "$candidate" ]]; then component_wasm=$candidate; break; fi
    done
fi
[[ -n "$component_wasm" ]] || fail 'build the no-std http-request guest before this runner'
component_wasm=$(readlink -f -- "$component_wasm")
[[ -f "$component_wasm" && "$component_wasm" == */wasm32-wasip2/debug/http_request.wasm ]] ||
    fail 'WAMN_HTTP_REUSE_COMPONENT_WASM must name the built debug http_request.wasm'

docker image inspect postgres:18 >/dev/null 2>&1 || fail 'postgres:18 must already be available locally'
docker image inspect registry:2 >/dev/null 2>&1 || fail 'registry:2 must already be available locally'
private_dir=$(mktemp -d /tmp/wamn-http-reuse.XXXXXXXX)
run_name=${private_dir##*/}
pg_name=${run_name}-pg
registry_name=${run_name}-registry
pg_id=''
registry_id=''
test_pid=''
owned_volumes=()

remove_owned() {
    local id=$1 expected_name=$2 actual
    [[ -n "$id" ]] || return 0
    [[ "$id" =~ ^[0-9a-f]{64}$ ]] || return 1
    actual=$(docker inspect --format '{{.Name}}|{{index .Config.Labels "wamn.test.run"}}' "$id") || return 1
    [[ "$actual" == "/$expected_name|$run_name" ]] || {
        printf 'refuse cleanup: container identity changed: %s\n' "$expected_name" >&2
        return 1
    }
    docker rm --force --volumes "$id" >/dev/null || return 1
    printf 'removed owned container and anonymous volumes: %s\n' "$expected_name"
}

cleanup() {
    local result=$? cleanup_result=0 volume
    trap - EXIT INT TERM
    if [[ -n "$test_pid" ]]; then
        kill -TERM "$test_pid" 2>/dev/null || true
        wait "$test_pid" 2>/dev/null || true
    fi
    remove_owned "$registry_id" "$registry_name" || cleanup_result=1
    remove_owned "$pg_id" "$pg_name" || cleanup_result=1
    docker info --format '{{.ServerVersion}}' >/dev/null 2>&1 || cleanup_result=1
    for volume in "${owned_volumes[@]}"; do
        if docker volume inspect "$volume" >/dev/null 2>&1; then
            printf 'owned anonymous volume remains: %s\n' "$volume" >&2
            cleanup_result=1
        fi
    done
    rm -f -- "$private_dir/password" "$private_dir/postgres.env" \
        "$private_dir/ready.log" "$private_dir/registry.log" "$private_dir/test.log"
    rmdir -- "$private_dir" || cleanup_result=1
    [[ $result != 0 || $cleanup_result == 0 ]] || result=$cleanup_result
    exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

openssl rand -hex 24 > "$private_dir/password"
read -r task_password < "$private_dir/password"
printf 'POSTGRES_PASSWORD=%s\nPOSTGRES_DB=http_reuse\n' "$task_password" > "$private_dir/postgres.env"
pg_id=$(docker create --pull=never --name "$pg_name" --label "wamn.test.run=$run_name" \
    --env-file "$private_dir/postgres.env" -p 127.0.0.1::5432 postgres:18)
registry_id=$(docker create --pull=never --name "$registry_name" --label "wamn.test.run=$run_name" \
    -p 127.0.0.1::5000 registry:2)
for id in "$pg_id" "$registry_id"; do
    [[ "$id" =~ ^[0-9a-f]{64}$ ]] || fail 'Docker returned an invalid container identity'
    volumes=$(docker inspect --format '{{range .Mounts}}{{if eq .Type "volume"}}{{println .Name}}{{end}}{{end}}' "$id") ||
        fail 'cannot read the owned container volumes'
    while IFS= read -r volume; do
        [[ -n "$volume" ]] || continue
        [[ "$volume" =~ ^[0-9a-f]{64}$ ]] || fail 'fixture has an unexpected non-anonymous volume'
        owned_volumes+=("$volume")
    done <<< "$volumes"
done
docker start "$pg_id" "$registry_id" >/dev/null
pg_address=$(docker port "$pg_id" 5432/tcp)
registry_address=$(docker port "$registry_id" 5000/tcp)
[[ "$pg_address" =~ ^127\.0\.0\.1:([0-9]+)$ ]] || fail 'PostgreSQL did not bind one loopback port'
pg_port=${BASH_REMATCH[1]}
[[ "$registry_address" =~ ^127\.0\.0\.1:[0-9]+$ ]] || fail 'registry did not bind one loopback port'

# Only fixture startup retries. The test and its HTTP mutations run once.
ready=0
for attempt in {1..40}; do
    if PGPASSWORD="$task_password" PGCONNECT_TIMEOUT=1 psql -X -w -h 127.0.0.1 \
        -p "$pg_port" -U postgres -d http_reuse -Atqc 'SELECT 1' > "$private_dir/ready.log" 2>&1; then
        [[ $(< "$private_dir/ready.log") == 1 ]] || fail 'PostgreSQL readiness returned an unexpected value'
        ready=1
        break
    fi
    sleep 0.25
done
[[ $ready == 1 ]] || fail 'fresh PostgreSQL did not become ready; no tests ran'
curl --noproxy '*' --silent --show-error --fail --retry 20 --retry-connrefused --retry-delay 1 \
    --retry-max-time 30 --max-time 2 "http://$registry_address/v2/" > "$private_dir/registry.log" 2>&1 ||
    fail 'fresh registry did not become ready; no tests ran'
if [[ $nested == 1 ]]; then
    PGPASSWORD="$task_password" PGCONNECT_TIMEOUT=5 psql -X -w -v ON_ERROR_STOP=1 \
        -h 127.0.0.1 -p "$pg_port" -U postgres -d http_reuse -c 'CREATE DATABASE wamnsystem' >/dev/null
fi

printf 'test=%s\npostgres=%s\nregistry=%s\n' "$test_name" "$pg_name" "$registry_name"
docker inspect --format '{{.Name}} {{.Image}}' "$pg_id" "$registry_id"
sha256sum -- "$test_binary" "$component_wasm" "$script_dir/live.sh"
test_environment=(env -i "PATH=$PATH" \
    WAMN_HTTP_REUSE_ALLOW_SCHEMA_RESET=1 \
    "WAMN_HTTP_REUSE_PG_URL=postgresql://postgres:$task_password@127.0.0.1:$pg_port/http_reuse" \
    "WAMN_HTTP_REUSE_ARTIFACT_BASE=$registry_address/http-reuse" \
    "WAMN_HTTP_REUSE_COMPONENT_WASM=$component_wasm")
if [[ $nested == 1 ]]; then
    test_environment+=("WAMN_HTTP_REUSE_SYSTEM_PG_URL=postgresql://postgres:$task_password@127.0.0.1:$pg_port/wamnsystem")
fi
test_exit=0
timeout --kill-after=5s 240s "${test_environment[@]}" "$test_binary" "$test_name" \
    --include-ignored --exact --nocapture --test-threads=1 > "$private_dir/test.log" 2>&1 &
test_pid=$!
wait "$test_pid" || test_exit=$?
test_pid=''
unset task_password test_environment

password_scan=0
rg --quiet --fixed-strings -f "$private_dir/password" "$private_dir/test.log" || password_scan=$?
credential_scan=0
rg --quiet '(postgres(ql)?://[^[:space:]]+:[^[:space:]]+@|wamn_pat_[A-Za-z0-9_-]+|eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+|BEGIN ([A-Z0-9 ]+ )?PRIVATE KEY)' \
    "$private_dir/test.log" || credential_scan=$?
[[ $password_scan == 1 && $credential_scan == 1 ]] || fail 'credential scan refused log publication; raw log withheld'
sed -n '1,$p' "$private_dir/test.log"
[[ $test_exit == 0 ]] || exit "$test_exit"
[[ $(rg -c '^test result:' "$private_dir/test.log") == 1 ]] || fail 'expected exactly one test result'
rg --quiet --fixed-strings --line-regexp "test $test_name ... ok" "$private_dir/test.log" ||
    fail 'the exact named test did not report success'
rg --quiet '^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured;' "$private_dir/test.log" ||
    fail 'expected one passing test with none ignored'
skip_scan=0
rg --quiet --ignore-case 'skipping|self.skip' "$private_dir/test.log" || skip_scan=$?
[[ $skip_scan == 1 ]] || fail 'self-skip scan did not pass; no pass is claimed'
printf 'PASS exact named live test: %s\n' "$test_name"
