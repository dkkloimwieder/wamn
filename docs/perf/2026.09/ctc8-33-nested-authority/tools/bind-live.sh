#!/usr/bin/env bash
# Invoke with bash. Each invocation owns one fresh PostgreSQL with two databases.
set +x
set -euo pipefail
umask 077

fail() { printf '%s\n' "$1" >&2; exit 1; }
[[ $# == 0 ]] || fail 'usage: WAMN_BIND_CONNECTION_TEST_BINARY=/exact/debug/deps/bind_connection_live-HASH bash bind-live.sh'
test_name=bind_connection_round_trips_through_the_plugins_own_resolution
for tool in docker psql openssl rg timeout sha256sum git readlink; do
    command -v "$tool" >/dev/null || fail "required tool is absent: $tool"
done
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
source_tree=$(git -C "$script_dir" rev-parse --show-toplevel)
cd -- "$source_tree"
[[ -f services/ctl/tests/bind_connection_live.rs ]] || fail 'the bind-connection proof source is absent'

test_binary=${WAMN_BIND_CONNECTION_TEST_BINARY:-}
[[ -n "$test_binary" ]] || fail 'set WAMN_BIND_CONNECTION_TEST_BINARY to the exact debug test binary'
test_binary=$(readlink -f -- "$test_binary")
[[ -f "$test_binary" && -x "$test_binary" && "$test_binary" == */debug/deps/* &&
   ${test_binary##*/} =~ ^bind_connection_live-[0-9a-f]+$ ]] ||
    fail 'WAMN_BIND_CONNECTION_TEST_BINARY must name the built debug bind_connection_live test binary'
test_hash=$(sha256sum -- "$test_binary")
docker image inspect postgres:18 >/dev/null 2>&1 || fail 'postgres:18 must already exist locally'

private_dir=$(mktemp -d /tmp/wamn-nested-bind.XXXXXXXX)
run_name=${private_dir##*/}
pg_name=${run_name}-pg
pg_id=''
test_pid=''
owned_volumes=()

remove_owned() {
    local actual
    [[ -n "$pg_id" ]] || return 0
    [[ "$pg_id" =~ ^[0-9a-f]{64}$ ]] || return 1
    actual=$(docker inspect --format '{{.Name}}|{{index .Config.Labels "wamn.proof.run"}}' "$pg_id") || return 1
    [[ "$actual" == "/$pg_name|$run_name" ]] || {
        printf 'cleanup refused: the container identity changed: %s\n' "$pg_name" >&2
        return 1
    }
    docker rm --force --volumes "$pg_id" >/dev/null || return 1
    printf 'removed owned container and anonymous volumes: %s\n' "$pg_name"
}

cleanup() {
    local result=$? cleanup_result=0 volume fixture_dir
    trap - EXIT INT TERM
    if [[ -n "$test_pid" ]]; then
        kill -TERM "$test_pid" 2>/dev/null || true
        wait "$test_pid" 2>/dev/null || true
    fi
    remove_owned || cleanup_result=1
    docker info --format '{{.ServerVersion}}' >/dev/null 2>&1 || cleanup_result=1
    for volume in "${owned_volumes[@]}"; do
        if docker volume inspect "$volume" >/dev/null 2>&1; then
            printf 'an owned anonymous volume remains: %s\n' "$volume" >&2
            cleanup_result=1
        fi
    done
    for fixture_dir in "$private_dir"/wamn-bind-connection-*; do
        [[ -e "$fixture_dir" ]] || continue
        if [[ -d "$fixture_dir" && ! -L "$fixture_dir" &&
              ${fixture_dir##*/} =~ ^wamn-bind-connection-[0-9]+$ ]]; then
            rm -f -- "$fixture_dir/lacking.json" "$fixture_dir/good.json" "$fixture_dir/again.json" || cleanup_result=1
            rmdir -- "$fixture_dir" || cleanup_result=1
        else
            cleanup_result=1
        fi
    done
    rm -f -- "$private_dir/password" "$private_dir/postgres.env" \
        "$private_dir/ready.log" "$private_dir/test.log" || cleanup_result=1
    rmdir -- "$private_dir" || cleanup_result=1
    if [[ $cleanup_result == 0 ]]; then
        printf 'cleanup=pass\n'
    else
        printf 'cleanup=fail\n' >&2
    fi
    [[ $result != 0 || $cleanup_result == 0 ]] || result=$cleanup_result
    exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

openssl rand -hex 24 > "$private_dir/password"
read -r task_password < "$private_dir/password"
printf 'POSTGRES_PASSWORD=%s\nPOSTGRES_DB=bind_project\n' "$task_password" > "$private_dir/postgres.env"
pg_id=$(docker create --pull=never --name "$pg_name" --label "wamn.proof.run=$run_name" \
    --env-file "$private_dir/postgres.env" -p 127.0.0.1::5432 postgres:18)
[[ "$pg_id" =~ ^[0-9a-f]{64}$ ]] || fail 'Docker returned an invalid container identity'
volumes=$(docker inspect --format '{{range .Mounts}}{{if eq .Type "volume"}}{{println .Name}}{{end}}{{end}}' "$pg_id") ||
    fail 'cannot read the owned container volumes'
while IFS= read -r volume; do
    [[ -n "$volume" ]] || continue
    [[ "$volume" =~ ^[0-9a-f]{64}$ ]] || fail 'the fixture has an unexpected named volume'
    owned_volumes+=("$volume")
done <<< "$volumes"
docker start "$pg_id" >/dev/null
pg_address=$(docker port "$pg_id" 5432/tcp)
[[ "$pg_address" =~ ^127\.0\.0\.1:([0-9]+)$ ]] || fail 'PostgreSQL did not bind one loopback port'
pg_port=${BASH_REMATCH[1]}

# Only fixture startup retries. The named proof runs once.
ready=0
for attempt in {1..40}; do
    if PGPASSWORD="$task_password" PGCONNECT_TIMEOUT=1 psql -X -w -h 127.0.0.1 \
        -p "$pg_port" -U postgres -d bind_project -Atqc 'SELECT 1' > "$private_dir/ready.log" 2>&1; then
        [[ $(< "$private_dir/ready.log") == 1 ]] || fail 'PostgreSQL readiness returned an unexpected value'
        ready=1
        break
    fi
    sleep 0.25
done
[[ $ready == 1 ]] || fail 'fresh PostgreSQL did not become ready; no tests ran'
PGPASSWORD="$task_password" PGCONNECT_TIMEOUT=5 psql -X -w -v ON_ERROR_STOP=1 \
    -h 127.0.0.1 -p "$pg_port" -U postgres -d bind_project -c 'CREATE DATABASE bind_control' >/dev/null
pg_version=$(PGPASSWORD="$task_password" PGCONNECT_TIMEOUT=5 psql -X -w -v ON_ERROR_STOP=1 \
    -h 127.0.0.1 -p "$pg_port" -U postgres -d bind_project -Atqc 'SHOW server_version_num')
[[ "$pg_version" =~ ^18[0-9]{4}$ ]] || fail 'the fixture must run PostgreSQL 18'

printf 'test=%s\npostgres=%s\npostgres_version_num=%s\n' "$test_name" "$pg_name" "$pg_version"
printf 'command=%s %s --include-ignored --exact --nocapture --test-threads=1\n' "$test_binary" "$test_name"
docker inspect --format '{{.Name}} {{.Image}}' "$pg_id"
printf '%s\n' "$test_hash"
sha256sum -- "$script_dir/bind-live.sh"
test_exit=0
timeout --kill-after=5s 240s env -i "PATH=$PATH" "TMPDIR=$private_dir" \
    "WAMN_BIND_CONNECTION_PROJECT_PG_URL=postgresql://postgres:$task_password@127.0.0.1:$pg_port/bind_project" \
    "WAMN_BIND_CONNECTION_CONTROL_PG_URL=postgresql://postgres:$task_password@127.0.0.1:$pg_port/bind_control" \
    "$test_binary" "$test_name" --include-ignored --exact --nocapture --test-threads=1 \
    > "$private_dir/test.log" 2>&1 &
test_pid=$!
wait "$test_pid" || test_exit=$?
test_pid=''
unset task_password

password_scan=0
rg --quiet --fixed-strings -f "$private_dir/password" "$private_dir/test.log" || password_scan=$?
credential_scan=0
rg --quiet '(postgres(ql)?://[^[:space:]]+:[^[:space:]]+@|wamn_pat_[A-Za-z0-9_-]+|eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+|BEGIN ([A-Z0-9 ]+ )?PRIVATE KEY)' \
    "$private_dir/test.log" || credential_scan=$?
[[ $password_scan == 1 && $credential_scan == 1 ]] || fail 'credential scan refused log publication; raw log withheld'
sed -n '1,$p' "$private_dir/test.log"
printf 'test_exit=%s\n' "$test_exit"
[[ $test_exit == 0 ]] || exit "$test_exit"
[[ $(sha256sum -- "$test_binary") == "$test_hash" ]] || fail 'the test binary changed during the proof'
[[ $(rg -c '^test result:' "$private_dir/test.log") == 1 ]] || fail 'expected exactly one test result'
[[ $(rg -c '^running 1 test$' "$private_dir/test.log") == 1 ]] || fail 'expected exactly one running test'
# The package verb prints between the test name and libtest's final result.
[[ $(rg -c "^test $test_name \\.\\.\\." "$private_dir/test.log") == 1 ]] ||
    fail 'the exact named test did not run once'
rg --quiet '^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured;' "$private_dir/test.log" ||
    fail 'expected one passing test with none ignored'
skip_scan=0
rg --quiet --ignore-case 'skipping|self.skip' "$private_dir/test.log" || skip_scan=$?
[[ $skip_scan == 1 ]] || fail 'the log contains a self-skip or the scan failed'
printf 'PASS exact named live test: %s\n' "$test_name"
