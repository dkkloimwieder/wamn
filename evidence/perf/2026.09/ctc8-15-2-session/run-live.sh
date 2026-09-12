#!/usr/bin/env bash
# Two-phase, one-use capture. Invoke with bash; no readiness or Cargo wait loops.
set +x
set -euo pipefail
umask 077

source_tree=/home/kaalin/.cache/wamn-lanes/ctc8-15-2-session-exchange-20260908
evidence_root=/home/kaalin/dev/wamn/docs/perf/2026.09/ctc8-15-2-session
action=${1:-}
suite=${2:-}
increment=${3:-}
fail() { printf '%s\n' "$1" >&2; exit 1; }
[[ $# == 3 && "$action" =~ ^(start|run|cleanup)$ && "$increment" =~ ^[0-9]{3,}$ ]] ||
    fail 'usage: bash run-live.sh {start|run|cleanup} SUITE INCREMENT (for example exchange 001)'
arm=''
case "$suite" in
    reader)
        package=wamn-control-provision; test=session_role_reader_live; count=1
        database=wamn_session_role_reader_proof
        url_var=WAMN_SESSION_ROLE_READER_PG_URL; arm=WAMN_SESSION_ROLE_READER_ALLOW_SCHEMA_RESET
        witness=dedicated_session_reader_columns_and_generations_execute_on_postgres ;;
    issuer)
        package=wamn-control-provision; test=identity_issuer_live; count=1
        database=wamn_system
        url_var=WAMN_IDENTITY_ISSUER_PG_URL; arm=WAMN_IDENTITY_ISSUER_ALLOW_SCHEMA_RESET
        witness=scoped_issuer_grants_and_generation_retirement_execute_on_postgres ;;
    surface)
        package=wamn-identity; test=https_surface; count=2; database=wamn_system
        url_var=WAMN_IDENTITY_SERVICE_PG_URL; arm=WAMN_IDENTITY_SERVICE_ALLOW_SCHEMA_RESET
        witness=identity_https_has_only_public_jwks_and_health ;;
    exchange)
        package=wamn-identity; test=session_exchange; count=1; database=wamn_system
        url_var=WAMN_SESSION_EXCHANGE_PG_URL; arm=WAMN_SESSION_EXCHANGE_ALLOW_SCHEMA_RESET
        witness=session_exchange_uses_fresh_scoped_authority_without_session_state ;;
    issuer-cli)
        package=wamn-ctl; test=identity_issuer_live; count=2; database=wamn_system
        url_var=WAMN_IDENTITY_ISSUER_CLI_PG_URL; arm=WAMN_IDENTITY_ISSUER_CLI_ALLOW_SCHEMA_RESET
        witness=compiled_cli_publishes_rolls_back_and_retires_identity_generations ;;
    audience-cli)
        package=wamn-ctl; test=session_audience_live; count=2; database=wamn_system
        url_var=WAMN_SESSION_AUDIENCE_CLI_PG_URL; arm=WAMN_SESSION_AUDIENCE_CLI_ALLOW_SCHEMA_RESET
        witness=compiled_cli_publishes_bound_session_targets_and_rotates_reader_generations ;;
    family-matrix)
        package=wamn-control-provision; test=family_denial_matrix; count=20; database=postgres
        url_var=WAMN_DENIAL_MATRIX_PG_URL
        witness=the_session_role_reader_is_refused_the_other_families_operations ;;
    *) fail 'unknown suite; use reader, issuer, surface, exchange, issuer-cli, audience-cli, or family-matrix' ;;
esac

run_name=live-${suite}-${increment}
evidence=$evidence_root/$run_name
container=wamn-ctc8-15-2-${suite}-${increment}-pg
owner_label=wamn.proof.owner
owner=ctc8-15-2-session-live
container_id=''
private_dir=''
[[ -f "$source_tree/services/identity/src/session.rs" ]] || fail 'fixed source-tree anchor is absent'
for tool in docker psql sha256sum openssl rg awk ps stat git; do
    command -v "$tool" >/dev/null || fail "required tool is absent: $tool"
done

capture_source() {
    local prefix=$1 path diff_exit
    git rev-parse HEAD > "$evidence/$prefix.base"
    git status --porcelain=v1 --untracked-files=all > "$evidence/$prefix.status"
    git diff --binary HEAD > "$evidence/$prefix.patch"
    while IFS= read -r -d '' path; do
        diff_exit=0
        git diff --no-index --binary -- /dev/null "$path" >> "$evidence/$prefix.patch" || diff_exit=$?
        [[ $diff_exit == 1 ]] || fail 'cannot capture untracked source'
    done < <(git ls-files --others --exclude-standard -z)
    # HEAD identifies unchanged files. Hash only the recorded working-tree
    # changes, rather than rereading all historical measurement artifacts.
    while IFS= read -r -d '' path; do
        [[ ! -f "$path" ]] || sha256sum -- "$path"
    done < <({ git diff --name-only -z HEAD; git ls-files --others --exclude-standard -z; } | sort -zu) > "$evidence/$prefix.sha256"
    sha256sum -- "$evidence/$prefix.patch" > "$evidence/$prefix.patch.sha256"
}

owned_container() {
    local actual actual_owner actual_run
    actual=$(docker inspect --format '{{.Id}}' "$container" 2>/dev/null) || return 1
    actual_owner=$(docker inspect --format '{{index .Config.Labels "wamn.proof.owner"}}' "$container")
    actual_run=$(docker inspect --format '{{index .Config.Labels "wamn.proof.run"}}' "$container")
    [[ "$actual" == "$container_id" && "$actual_owner" == "$owner" && "$actual_run" == "$run_name" ]]
}

cleanup_owned() {
    local cleanup_exit=0 volume
    if owned_container; then
        printf 'docker rm --force --volumes %q\n' "$container" > "$evidence/cleanup.command"
        docker rm --force --volumes "$container" > "$evidence/cleanup.log" 2>&1 || cleanup_exit=$?
        if [[ $cleanup_exit == 0 ]]; then
            docker info --format '{{.ServerVersion}}' >/dev/null 2>&1 || cleanup_exit=1
            if docker inspect "$container_id" >/dev/null 2>&1; then cleanup_exit=1; fi
            if [[ -f "$evidence/volume" ]]; then
                read -r volume < "$evidence/volume"
                if docker volume inspect "$volume" >/dev/null 2>&1; then cleanup_exit=1; fi
            fi
        fi
    else
        printf '%s\n' 'owned container identity could not be verified; nothing removed' > "$evidence/cleanup.log"
        cleanup_exit=1
    fi
    printf '%s\n' "$cleanup_exit" > "$evidence/cleanup.exit"
    if [[ $cleanup_exit == 0 && -n "$private_dir" ]]; then
        # Exact files in the mktemp-created directory, never a recursive sweep.
        rm -f -- "$private_dir/password" "$private_dir/container.env" \
            "$private_dir/readiness.raw" "$private_dir/test.raw.log"
        rmdir -- "$private_dir" || cleanup_exit=$?
    fi
    printf '%s\n' "$cleanup_exit" > "$evidence/cleanup.exit"
    return "$cleanup_exit"
}

cd "$source_tree"
if [[ "$action" == start ]]; then
    [[ ! -e "$evidence" ]] || fail 'evidence directory already exists; choose a new increment'
    [[ -z $(docker ps -aq --filter "label=$owner_label=$owner") ]] ||
        fail 'another owned live server exists; run or clean it before starting another suite'
    ! docker container inspect "$container" >/dev/null 2>&1 || fail 'container name already exists'
    docker image inspect postgres:18 >/dev/null 2>&1 || fail 'postgres:18 must already be available locally'
    mkdir -- "$evidence"
    private_dir=$(mktemp -d /tmp/wamn-ctc8-15-2-live.XXXXXXXX)
    printf '%s\n' "$private_dir" > "$evidence/private-directory"
    finish_start() {
        local result=$?
        trap - EXIT
        if [[ $result != 0 && -n "$container_id" ]]; then cleanup_owned || true; fi
        printf '%s\n' "$result" > "$evidence/start.exit"
        exit "$result"
    }
    trap finish_start EXIT
    openssl rand -hex 24 > "$private_dir/password"
    read -r task_password < "$private_dir/password"
    printf 'POSTGRES_PASSWORD=%s\nPOSTGRES_DB=%s\n' "$task_password" "$database" > "$private_dir/container.env"
    unset task_password
    printf 'source=%s\nsuite=%s\ndatabase=%s\ncontainer=%s\n' "$source_tree" "$suite" "$database" "$container" > "$evidence/start.info"
    sha256sum -- "$evidence_root/run-live.sh" > "$evidence/runner.sha256"
    date -u +%FT%TZ > "$evidence/start.time"
    capture_source start-source
    printf 'docker create --pull=never --name %q --label %q --label %q --env-file <private fixture file> -p 127.0.0.1::5432 postgres:18\n' \
        "$container" "$owner_label=$owner" "wamn.proof.run=$run_name" > "$evidence/start.command"
    container_id=$(docker create --pull=never --name "$container" \
        --label "$owner_label=$owner" --label "wamn.proof.run=$run_name" \
        --env-file "$private_dir/container.env" -p 127.0.0.1::5432 postgres:18)
    [[ "$container_id" =~ ^[0-9a-f]{64}$ ]] || fail 'unexpected container identity'
    printf '%s\n' "$container_id" > "$evidence/container-id"
    docker inspect --format '{{.Image}}' "$container" > "$evidence/image-id"
    docker inspect --format '{{range .Mounts}}{{if eq .Type "volume"}}{{println .Name}}{{end}}{{end}}' "$container" > "$evidence/volume"
    [[ $(wc -w < "$evidence/volume") == 1 ]] || fail 'expected one private PostgreSQL anonymous volume'
    read -r volume < "$evidence/volume"
    [[ "$volume" =~ ^[0-9a-f]{64}$ ]] || fail 'expected an anonymous PostgreSQL volume'
    printf 'docker start %q\n' "$container" >> "$evidence/start.command"
    docker start "$container" > "$evidence/start.log" 2>&1
    address=$(docker port "$container" 5432/tcp)
    [[ "$address" =~ ^127\.0\.0\.1:([0-9]+)$ ]] || fail 'PostgreSQL is not bound to one loopback port'
    printf '%s\n' "${BASH_REMATCH[1]}" > "$evidence/port"
    printf 'STARTED suite=%s evidence=%s port=%s; readiness not yet checked\n' "$suite" "$evidence" "${BASH_REMATCH[1]}"
    exit 0
fi

[[ -d "$evidence" && -f "$evidence/start.exit" && ! -e "$evidence/exit" ]] || fail 'missing, failed, or already consumed start evidence'
[[ $(< "$evidence/start.exit") == 0 ]] || fail 'start did not succeed'
read -r container_id < "$evidence/container-id"
read -r private_dir < "$evidence/private-directory"
[[ "$container_id" =~ ^[0-9a-f]{64}$ && "$private_dir" =~ ^/tmp/wamn-ctc8-15-2-live\.[A-Za-z0-9]{8}$ ]] || fail 'invalid owned-resource record'
[[ -d "$private_dir" && ! -L "$private_dir" && $(stat -c %u "$private_dir") == "$UID" && $(stat -c %a "$private_dir") == 700 ]] || fail 'private credential directory failed ownership checks'
owned_container || fail 'container does not match this run identity'
if [[ "$action" == cleanup ]]; then
    result=0
    cleanup_owned || result=$?
    printf 'abandoned-before-test\n' > "$evidence/result"
    printf '%s\n' "$result" > "$evidence/exit"
    exit "$result"
fi

# Match process names, not argv containing the watcher itself or credentials.
busy=$(ps -eo pid=,comm= | awk '$2 == "cargo" || $2 == "rustc" {print $1, $2}')
[[ -z "$busy" ]] || fail 'Cargo/rustc is active; run was not started (no waiting loop)'
[[ -z ${CARGO_TARGET_DIR+x} ]] || fail 'unset CARGO_TARGET_DIR; this source uses its own default target'
if [[ "$suite" == exchange ]]; then
    [[ -x "$source_tree/target/debug/wamn-ctl" ]] || fail 'build this worktree wamn-ctl binary before the exchange suite'
    export WAMN_SESSION_EXCHANGE_CTL_BIN="$source_tree/target/debug/wamn-ctl"
fi
[[ ! -e "$evidence/run.started" ]] || fail 'run has already been attempted; use a fresh increment'
date -u +%FT%TZ > "$evidence/run.started"
finish_run() {
    local result=$? cleanup_exit=0
    trap - EXIT
    cleanup_owned || cleanup_exit=$?
    [[ $result != 0 || $cleanup_exit == 0 ]] || result=$cleanup_exit
    printf '%s\n' "$result" > "$evidence/exit"
    printf 'FINISHED suite=%s exit=%s evidence=%s\n' "$suite" "$result" "$evidence"
    exit "$result"
}
trap finish_run EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
read -r port < "$evidence/port"
[[ "$port" =~ ^[0-9]+$ ]] || fail 'invalid recorded loopback port'
read -r task_password < "$private_dir/password"
printf 'PGPASSWORD=<private fixture password> PGCONNECT_TIMEOUT=5 psql -X -w -h 127.0.0.1 -p %q -U postgres -d %q -Atqc SELECT\\ 1\n' "$port" "$database" > "$evidence/readiness.command"
readiness_exit=0
PGPASSWORD="$task_password" PGCONNECT_TIMEOUT=5 psql -X -w -h 127.0.0.1 -p "$port" -U postgres -d "$database" -Atqc 'SELECT 1' > "$private_dir/readiness.raw" 2>&1 || readiness_exit=$?
printf '%s\n' "$readiness_exit" > "$evidence/readiness.exit"
[[ $readiness_exit == 0 && $(< "$private_dir/readiness.raw") == 1 ]] || fail 'single host-side SQL readiness check failed; no tests ran'
printf '1\n' > "$evidence/readiness.log"
capture_source source
export RUSTC_WRAPPER=sccache
export CARGO_TERM_COLOR=never
export "$url_var=postgres://postgres:$task_password@127.0.0.1:$port/$database"
if [[ -n "$arm" ]]; then export "$arm=1"; fi
command=(cargo test --locked --offline -p "$package" --test "$test" -- --include-ignored --nocapture --test-threads=1)
{
    printf 'cwd=%s\nRUSTC_WRAPPER=sccache\nCARGO_TARGET_DIR=<unset; worktree default>\n%s=<private loopback fixture URL>\n' "$source_tree" "$url_var"
    if [[ -n "$arm" ]]; then printf '%s=1\n' "$arm"; fi
    if [[ "$suite" == exchange ]]; then printf 'WAMN_SESSION_EXCHANGE_CTL_BIN=%s\n' "$WAMN_SESSION_EXCHANGE_CTL_BIN"; fi
    printf '%q ' "${command[@]}"
    printf '\n'
} > "$evidence/test.command"
if [[ "$suite" == exchange ]]; then sha256sum -- "$WAMN_SESSION_EXCHANGE_CTL_BIN" > "$evidence/ctl-binary.sha256"; fi
rustc -Vv > "$evidence/rustc-version"
cargo -V > "$evidence/cargo-version"
test_exit=0
"${command[@]}" > "$private_dir/test.raw.log" 2>&1 || test_exit=$?
printf '%s\n' "$test_exit" > "$evidence/test.exit"
unset "$url_var"
if [[ -n "$arm" ]]; then unset "$arm"; fi
# Preserve raw bytes only when publication is safe. Do not silently rewrite a
# failing receipt. Unknown unlabeled private bytes still require safe test code.
password_scan=0
rg -a -q -F -f "$private_dir/password" "$private_dir/test.raw.log" || password_scan=$?
credential_scan=0
rg -a -q '(postgres(ql)?://[^[:space:]]+:[^[:space:]]+@|wamn_pat_[A-Za-z0-9_-]+|eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+|BEGIN ([A-Z0-9 ]+ )?PRIVATE KEY|"(private_key|private_key_pkcs8|d)"[[:space:]]*:)' "$private_dir/test.raw.log" || credential_scan=$?
if [[ $password_scan != 1 || $credential_scan != 1 ]]; then
    printf 'FAIL output safety scan refused publication; raw log withheld\n' > "$evidence/receipt"
    [[ $test_exit == 0 ]] || exit "$test_exit"
    exit 1
fi
cp -- "$private_dir/test.raw.log" "$evidence/test.log"
sha256sum -- "$evidence/test.log" > "$evidence/test.log.sha256"
binary=$(awk '/^[[:space:]]+Running tests\// {sub(/^.*\(/, ""); sub(/\)$/, ""); print}' "$evidence/test.log")
if [[ "$binary" =~ ^target/debug/deps/${test}-[0-9a-f]+$ && -f "$binary" ]]; then
    sha256sum -- "$binary" > "$evidence/test-binary.sha256"
fi
capture_source after-source
cmp -s "$evidence/source.base" "$evidence/after-source.base" &&
    cmp -s "$evidence/source.sha256" "$evidence/after-source.sha256" &&
    cmp -s "$evidence/source.patch" "$evidence/after-source.patch" || fail 'source changed during execution; result is not attributed to one source snapshot'
[[ $test_exit == 0 ]] || { printf 'FAIL cargo test exit=%s; inspect exact failed test names\n' "$test_exit" > "$evidence/receipt"; exit "$test_exit"; }
[[ $(rg -c '^test result:' "$evidence/test.log") == 1 ]] &&
    rg -q "^test result: ok\\. $count passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;" "$evidence/test.log" &&
    rg -q "^test $witness \\.\\.\\. ok$" "$evidence/test.log" &&
    ! rg -qi 'skipping|self.skip' "$evidence/test.log" || fail 'missing exact armed test receipt; no pass is claimed'
printf 'PASS suite=%s tests=%s ignored=0 named-live-witness=%s\n' "$suite" "$count" "$witness" > "$evidence/receipt"
