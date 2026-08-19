#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
bootstrap=$repo_root/deploy/mvp/bootstrap.sh
test_dir=$(mktemp -d)
trap 'rm -rf -- "$test_dir"' EXIT
mkdir -p "$test_dir/bin" "$test_dir/state"

cat >"$test_dir/bin/kubectl" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail

argument_after() {
    local wanted=$1
    shift
    while (($#)); do
        if [[ $1 == "$wanted" ]]; then
            printf '%s' "$2"
            return
        fi
        shift
    done
    return 1
}

check_namespace() {
    local actual
    actual=$(argument_after --namespace "$@")
    [[ $actual == "$MOCK_EXPECT_NAMESPACE" ]] || {
        echo "unexpected namespace $actual" >&2
        exit 1
    }
}

load_record() {
    IFS='|' read -r name namespace managed component org project env_name purpose \
        principal_id kind subject role prefix expiry pending_issued pending_revoke \
        token_state <"$1"
}

print_record() {
    printf '%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s\n' \
        "$name" "$namespace" "$managed" "$component" "$org" "$project" \
        "$env_name" "$purpose" "$principal_id" "$kind" "$subject" "$role" \
        "$prefix" "$expiry" "$pending_issued" "$pending_revoke" "$token_state"
}

save_record() {
    print_record >"$1"
}

print_writer_metadata() {
    printf '%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s|%s' \
        "$name" "$namespace" "$managed" "$component" "$org" "$project" \
        "$env_name" "$purpose" "$prefix" "$role" "$principal_id" "$kind" \
        "$expiry" "$subject"
}

case $1 in
    get)
        name=$3
        file=$MOCK_STATE/$name
        check_namespace "$@"
        if [[ $* == *wamn.io/credential-id* ]]; then
            if [[ -f $file ]]; then
                load_record "$file"
                print_writer_metadata
            fi
        elif [[ $* == *ignore-not-found* ]]; then
            [[ ! -f $file ]] || printf '%s' "$name"
        else
            failure=$MOCK_STATE/.reread-failure-$name
            if [[ -f $failure ]]; then
                rm "$failure"
                exit 1
            fi
            cat "$file"
        fi
        ;;
    create)
        path=$(argument_after -f "$@")
        load_record "$path"
        if [[ $component == effect-writer-credentials ]]; then
            print_writer_metadata
        else
            printf '%s' "$prefix"
        fi
        ;;
    patch)
        path=$(argument_after -f "$@")
        load_record "$path"
        token_state=absent
        print_record
        echo "stub:$purpose" >>"$MOCK_LOG"
        ;;
    apply)
        path=$(argument_after -f "$@")
        load_record "$path"
        if [[ $token_state == absent ]]; then
            echo "stage-stub:$purpose" >>"$MOCK_LOG"
            save_record "$MOCK_STATE/$name"
        else
            apply_purpose=$purpose
            [[ $component != effect-writer-credentials ]] || apply_purpose=effect-writer
            echo "apply:$apply_purpose" >>"$MOCK_LOG"
            [[ ${MOCK_APPLY_FAIL:-} != "$apply_purpose" ]] || exit 1
            if [[ ${MOCK_APPLY_THIRD:-} == "$apply_purpose" ]]; then
                purpose=cccccccccccccccccccccccccccccccc
                save_record "$MOCK_STATE/$name"
                exit 1
            fi
            if [[ ${MOCK_APPLY_NOOP:-} == "$apply_purpose" ]]; then
                echo "secret/$name configured"
                exit 0
            fi
            save_record "$MOCK_STATE/$name"
            if [[ $apply_purpose == effect-writer ]]; then
                # Client-side kubectl apply adds an unrelated annotation. The
                # wrapper compares only its owned wamn.io metadata fields.
                : >"$MOCK_STATE/.last-applied-$name"
            fi
            [[ ${MOCK_APPLY_AMBIGUOUS:-} != "$apply_purpose" ]] || exit 1
            if [[ ${MOCK_REREAD_FAIL:-} == "$apply_purpose" ]]; then
                : >"$MOCK_STATE/.reread-failure-$name"
            fi
        fi
        echo "secret/$name configured"
        ;;
    annotate)
        if [[ $* == *--local* ]]; then
            path=$(argument_after -f "$@")
            load_record "$path"
            for argument in "$@"; do
                case $argument in
                    wamn.io/pending-issued-pat-prefix=*) pending_issued=${argument#*=} ;;
                    wamn.io/pending-revoke-pat-prefix=*) pending_revoke=${argument#*=} ;;
                esac
            done
            print_record
            tail -n +2 "$path"
            echo "decorate:$purpose" >>"$MOCK_LOG"
        else
            name=$3
            file=$MOCK_STATE/$name
            check_namespace "$@"
            load_record "$file"
            operation=annotate
            for argument in "$@"; do
                case $argument in
                    wamn.io/pending-issued-pat-prefix=*)
                        pending_issued=${argument#*=}
                        if [[ $pending_issued == "$prefix" ]]; then
                            operation=restore
                        else
                            operation=stage
                        fi
                        ;;
                    wamn.io/pending-revoke-pat-prefix=*) pending_revoke=${argument#*=} ;;
                    wamn.io/pending-issued-pat-prefix-)
                        pending_issued=
                        operation=clear
                        ;;
                    wamn.io/pending-revoke-pat-prefix-) pending_revoke= ;;
                esac
            done
            echo "$operation:$purpose" >>"$MOCK_LOG"
            if [[ $operation == clear && ${MOCK_CLEAR_FAIL:-} == "$purpose" ]]; then
                exit 1
            fi
            if [[ $operation == stage && ${MOCK_STAGE_FAIL:-} == "$purpose" ]]; then
                exit 1
            fi
            save_record "$file"
            echo "secret/$name annotated"
        fi
        ;;
    delete)
        name=$3
        check_namespace "$@"
        load_record "$MOCK_STATE/$name"
        echo "delete:$purpose" >>"$MOCK_LOG"
        rm "$MOCK_STATE/$name"
        ;;
    rollout)
        echo "rollout:$2:$3" >>"$MOCK_LOG"
        if [[ $2 == status ]]; then
            if [[ ${MOCK_WRITER_ROLLOUT_FAIL:-0} == 1 ]]; then
                exit 1
            fi
            : >"$MOCK_STATE/.writer-rollout"
        fi
        ;;
    *) exit 2 ;;
esac
MOCK

cat >"$test_dir/bin/wamn-ctl" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail

management=
route=
role_sql=
revoke=
writer_prepare=
writer_retire=
writer_abort=
writer_secret=
namespace=${WAMN_NAMESPACE:-wamn-system}
while (($#)); do
    case $1 in
        --emit-management-author-pat-secret) management=$2; shift 2 ;;
        --emit-route-caller-pat-secret) route=$2; shift 2 ;;
        --emit-role-sql) role_sql=$2; shift 2 ;;
        --revoke-pat-prefix) revoke=$2; shift 2 ;;
        --prepare-effect-writer-generation) writer_prepare=$2; shift 2 ;;
        --retire-effect-writer-generation) writer_retire=$2; shift 2 ;;
        --abort-effect-writer-generation) writer_abort=$2; shift 2 ;;
        --emit-effect-writer-secret) writer_secret=$2; shift 2 ;;
        --namespace) namespace=$2; shift 2 ;;
        --namespace=*) namespace=${1#*=}; shift ;;
        *) shift ;;
    esac
done

writer_role() {
    printf 'wamn_effect_writer_1111111111111111111111111111111111111111_%s' "$1"
}

if [[ -n $writer_prepare ]]; then
    role=$(writer_role "$writer_prepare")
    echo "prepare:$writer_prepare" >>"$MOCK_LOG"
    printf t >"$MOCK_STATE/.writer-$writer_prepare-login"
    issued_at=2026-01-01T00:00:00Z
    not_before=2026-01-01T00:00:00Z
    expires_at=2099-01-01T00:00:00Z
    revoked_at=
    if [[ $writer_prepare == a ]]; then
        credential_id=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
    else
        credential_id=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
    fi
    case ${MOCK_WRITER_MANIFEST_CORRUPTION:-} in
        validity) not_before=2100-01-01T00:00:00Z ;;
        revoked) revoked_at=present=2026-01-02T00:00:00Z ;;
    esac
    printf 'wamn-effect-writer-acme--billing--dev|%s|wamn|effect-writer-credentials|acme|billing|dev|%s|%s|%s|%s|%s|%s|%s|||present\n' \
        "$namespace" "$credential_id" \
        "$issued_at" "$not_before" "$revoked_at" "$role" "$writer_prepare" "$expires_at" \
        >"$writer_secret"
    exit 0
fi

if [[ -n $writer_retire ]]; then
    echo "retire:$writer_retire" >>"$MOCK_LOG"
    printf f >"$MOCK_STATE/.writer-$writer_retire-login"
    exit 0
fi

if [[ -n $writer_abort ]]; then
    echo "abort:$writer_abort" >>"$MOCK_LOG"
    printf f >"$MOCK_STATE/.writer-$writer_abort-login"
    exit 0
fi

if [[ -n $revoke ]]; then
    echo "revoke:$revoke" >>"$MOCK_LOG"
    [[ ${MOCK_REVOKE_FAIL_PREFIX:-} != "$revoke" ]] || exit 1
    echo "revoked PAT prefix $revoke"
    exit 0
fi

selection=none
[[ -z $management ]] || selection=management-author
if [[ -n $route ]]; then
    [[ $selection == none ]] && selection=route-caller || selection=both
fi
echo "issue:$selection" >>"$MOCK_LOG"

if [[ -z $role_sql ]]; then
    echo 'CREATE ROLE wamn_app PASSWORD ROLE_SQL_PASSWORD_SENTINEL'
else
    printf '%s\n' 'CREATE ROLE wamn_app PASSWORD ROLE_SQL_PASSWORD_SENTINEL' >"$role_sql"
fi
[[ ${MOCK_CTL_FAIL_BEFORE_WRITE:-0} == 0 ]] || exit 1

write_record() {
    local purpose=$1 path=$2 prefix=$3 role=$4 subject_stem=$5 name
    name=wamn-pat-$purpose-acme--billing--dev
    printf '%s|%s|wamn|project-env-pat|acme|billing|dev|%s|01234567-89ab-cdef-0123-456789abcdef|service|%s-acme--billing--dev|%s|%s|2099-01-01T00:00:00Z|||present\n' \
        "$name" "$namespace" "$purpose" "$subject_stem" "$role" "$prefix" >"$path"
    printf 'token=PAT_TOKEN_SENTINEL_%s\n' "$purpose" >>"$path"
}

[[ -z $management ]] || write_record management-author "$management" 1111111111111111 project-author wamn-management-author
[[ ${MOCK_CTL_FAIL_AFTER_MANAGEMENT:-0} == 0 ]] || exit 1
[[ -z $route ]] || write_record route-caller "$route" 2222222222222222 route-caller wamn-route-caller
echo provisioned
MOCK

cat >"$test_dir/bin/psql" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail

role=
query=
while (($#)); do
    case $1 in
        -v) role=${2#role=}; shift 2 ;;
        -c) query=$2; shift 2 ;;
        *) shift ;;
    esac
done
generation=${role##*_}
if [[ $query == *pg_stat_activity* ]]; then
    echo "probe-sessions:$generation" >>"$MOCK_LOG"
    if [[ ${MOCK_WRITER_SESSION_ZERO:-0} == 1 ]]; then
        printf '0\n'
    elif [[ -f $MOCK_STATE/.writer-rollout ]]; then
        printf '1\n'
    else
        printf '0\n'
    fi
else
    echo "probe-login:$generation" >>"$MOCK_LOG"
    if [[ ${MOCK_WRITER_LOGIN_PROBE_FAIL_GENERATION:-} == "$generation" ]]; then
        exit 1
    fi
    if [[ -f $MOCK_STATE/.writer-$generation-login ]]; then
        cat "$MOCK_STATE/.writer-$generation-login"
        printf '\n'
    fi
fi
MOCK
chmod +x "$test_dir/bin/kubectl" "$test_dir/bin/wamn-ctl" "$test_dir/bin/psql" "$bootstrap"

expected_namespace() {
    printf '%s' "${TEST_NAMESPACE:-wamn-system}"
}

record() {
    local purpose=$1 prefix=$2 expiry=$3 role=$4 subject_stem=$5
    local pending_issued=${6:-} pending_revoke=${7:-} token_state=${8:-present}
    local name namespace
    name=wamn-pat-$purpose-acme--billing--dev
    namespace=$(expected_namespace)
    printf '%s|%s|wamn|project-env-pat|acme|billing|dev|%s|01234567-89ab-cdef-0123-456789abcdef|service|%s-acme--billing--dev|%s|%s|%s|%s|%s|%s\n' \
        "$name" "$namespace" "$purpose" "$subject_stem" "$role" "$prefix" "$expiry" \
        "$pending_issued" "$pending_revoke" "$token_state" >"$test_dir/state/$name"
}

run_bootstrap() {
    local namespace
    local -a environment=(env -u WAMN_NAMESPACE)
    : >"$test_dir/log"
    namespace=$(expected_namespace)
    if [[ -n ${TEST_NAMESPACE:-} ]]; then
        environment=(env "WAMN_NAMESPACE=$TEST_NAMESPACE")
    fi
    "${environment[@]}" \
        MOCK_STATE="$test_dir/state" MOCK_LOG="$test_dir/log" \
        MOCK_EXPECT_NAMESPACE="$namespace" \
        MOCK_APPLY_FAIL="${MOCK_APPLY_FAIL:-}" \
        MOCK_APPLY_AMBIGUOUS="${MOCK_APPLY_AMBIGUOUS:-}" \
        MOCK_REREAD_FAIL="${MOCK_REREAD_FAIL:-}" \
        MOCK_REVOKE_FAIL_PREFIX="${MOCK_REVOKE_FAIL_PREFIX:-}" \
        MOCK_CLEAR_FAIL="${MOCK_CLEAR_FAIL:-}" \
        MOCK_STAGE_FAIL="${MOCK_STAGE_FAIL:-}" \
        MOCK_CTL_FAIL_BEFORE_WRITE="${MOCK_CTL_FAIL_BEFORE_WRITE:-0}" \
        MOCK_CTL_FAIL_AFTER_MANAGEMENT="${MOCK_CTL_FAIL_AFTER_MANAGEMENT:-0}" \
        WAMN_CTL_BIN="$test_dir/bin/wamn-ctl" KUBECTL_BIN="$test_dir/bin/kubectl" \
        PSQL_BIN="$test_dir/bin/psql" \
        "$bootstrap" --org acme --project billing --env dev \
        --system-database-url 'postgres://admin:URL_SECRET_SENTINEL@sys/db' \
        --emit-secret "$test_dir/db.json" "$@"
}

run_writer_rotation() {
    : >"$test_dir/log"
    MOCK_STATE="$test_dir/state" MOCK_LOG="$test_dir/log" \
        MOCK_EXPECT_NAMESPACE=wamn-system \
        MOCK_APPLY_FAIL="${MOCK_APPLY_FAIL:-}" \
        MOCK_APPLY_AMBIGUOUS="${MOCK_APPLY_AMBIGUOUS:-}" \
        MOCK_APPLY_THIRD="${MOCK_APPLY_THIRD:-}" \
        MOCK_APPLY_NOOP="${MOCK_APPLY_NOOP:-}" \
        MOCK_WRITER_ROLLOUT_FAIL="${MOCK_WRITER_ROLLOUT_FAIL:-0}" \
        MOCK_WRITER_SESSION_ZERO="${MOCK_WRITER_SESSION_ZERO:-0}" \
        MOCK_WRITER_LOGIN_PROBE_FAIL_GENERATION="${MOCK_WRITER_LOGIN_PROBE_FAIL_GENERATION:-}" \
        MOCK_WRITER_MANIFEST_CORRUPTION="${MOCK_WRITER_MANIFEST_CORRUPTION:-}" \
        WAMN_CTL_BIN="$test_dir/bin/wamn-ctl" KUBECTL_BIN="$test_dir/bin/kubectl" \
        PSQL_BIN="$test_dir/bin/psql" \
        "$bootstrap" --org acme --project billing --env dev \
        --target-admin-database-url 'postgres://admin:WRITER_ADMIN_SENTINEL@pg/wamn-db-acme--billing--dev' \
        --rotate-effect-writer-generation "$1"
}

reset_writer_state() {
    rm -f "$test_dir/state/wamn-effect-writer-acme--billing--dev" \
        "$test_dir/state/.last-applied-wamn-effect-writer-acme--billing--dev" \
        "$test_dir/state/.writer-a-login" "$test_dir/state/.writer-b-login" \
        "$test_dir/state/.writer-rollout"
}

writer_record() {
    local generation=$1 issued_at=${2:-2026-01-01T00:00:00Z}
    local not_before=${3:-2026-01-01T00:00:00Z} expires_at=${4:-2099-01-01T00:00:00Z}
    local revoked_at=${5:-} credential_id role
    if [[ $generation == a ]]; then
        credential_id=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
    else
        credential_id=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
    fi
    role=wamn_effect_writer_1111111111111111111111111111111111111111_$generation
    printf 'wamn-effect-writer-acme--billing--dev|wamn-system|wamn|effect-writer-credentials|acme|billing|dev|%s|%s|%s|%s|%s|%s|%s|||present\n' \
        "$credential_id" "$issued_at" "$not_before" "$revoked_at" "$role" \
        "$generation" "$expires_at" \
        >"$test_dir/state/wamn-effect-writer-acme--billing--dev"
    printf t >"$test_dir/state/.writer-$generation-login"
}

assert_log() {
    local expected=$1 actual
    actual=$(cat "$test_dir/log")
    [[ $actual == "$expected" ]] || {
        printf 'expected log:\n%s\nactual log:\n%s\n' "$expected" "$actual" >&2
        exit 1
    }
}

assert_no_secret_output() {
    local output=$1
    if [[ $output == *PAT_TOKEN_SENTINEL* || $output == *URL_SECRET_SENTINEL* ||
        $output == *ROLE_SQL_PASSWORD_SENTINEL* || $output == *WRITER_ADMIN_SENTINEL* ]]; then
        echo 'bootstrap leaked credential material' >&2
        exit 1
    fi
}

assert_pending() {
    local name=$1 expected_issued=$2 expected_revoke=$3 expected_token=$4
    local issued revoke token
    IFS='|' read -r _ _ _ _ _ _ _ _ _ _ _ _ _ _ issued revoke token \
        <"$test_dir/state/$name"
    [[ $issued == "$expected_issued" && $revoke == "$expected_revoke" &&
        $token == "$expected_token" ]] || {
        echo "$name recovery metadata did not match" >&2
        exit 1
    }
}

assert_no_pending() {
    local name=$1
    assert_pending "$name" '' '' present
}

# Stable exact metadata with a canonical future UTC expiry skips both PATs.
record management-author aaaaaaaaaaaaaaaa 2099-01-01T00:00:00Z project-author wamn-management-author
record route-caller bbbbbbbbbbbbbbbb 2099-01-01T00:00:00Z route-caller wamn-route-caller
output=$(run_bootstrap 2>&1)
assert_log 'issue:none'
assert_no_secret_output "$output"

# Optional real ctl boundary: role SQL must remain in the wrapper-owned file.
if [[ -n ${WAMN_REAL_CTL_BIN:-} ]]; then
    : >"$test_dir/log"
    output=$(env -u WAMN_NAMESPACE \
        MOCK_STATE="$test_dir/state" MOCK_LOG="$test_dir/log" \
        MOCK_EXPECT_NAMESPACE=wamn-system \
        WAMN_CTL_BIN="$WAMN_REAL_CTL_BIN" KUBECTL_BIN="$test_dir/bin/kubectl" \
        "$bootstrap" --org acme --project billing --env dev --cluster wamn-pg \
        --app-password ROLE_SQL_PASSWORD_SENTINEL \
        --dispatch-reader-password ROLE_SQL_PASSWORD_SENTINEL_READER \
        --emit-secret "$test_dir/db.json" 2>&1)
    assert_no_secret_output "$output"
    assert_log ''
fi

# WAMN_NAMESPACE overrides the wamn-system default used above.
TEST_NAMESPACE=tenant-space
record management-author aaaaaaaaaaaaaaaa 2099-01-01T00:00:00Z project-author wamn-management-author
record route-caller bbbbbbbbbbbbbbbb 2099-01-01T00:00:00Z route-caller wamn-route-caller
output=$(run_bootstrap 2>&1)
assert_log 'issue:none'
unset TEST_NAMESPACE
assert_no_secret_output "$output"

# Absent Secrets use transient tokenless final-name stubs, then converge steady.
rm "$test_dir/state/wamn-pat-management-author-acme--billing--dev"
rm "$test_dir/state/wamn-pat-route-caller-acme--billing--dev"
output=$(run_bootstrap 2>&1)
assert_log $'issue:both\ndecorate:management-author\nstub:management-author\nstage-stub:management-author\napply:management-author\nclear:management-author\ndecorate:route-caller\nstub:route-caller\nstage-stub:route-caller\napply:route-caller\nclear:route-caller'
assert_no_pending wamn-pat-management-author-acme--billing--dev
assert_no_pending wamn-pat-route-caller-acme--billing--dev
assert_no_secret_output "$output"

# Semantically impossible UTC expiry rotates instead of comparing lexically.
record management-author aaaaaaaaaaaaaaaa 2099-01-01T00:00:00Z project-author wamn-management-author
record route-caller cccccccccccccccc 2099-02-29T00:00:00Z route-caller wamn-route-caller
output=$(run_bootstrap 2>&1)
assert_log $'issue:route-caller\ndecorate:route-caller\nstage:route-caller\napply:route-caller\nrevoke:cccccccccccccccc\nclear:route-caller'
assert_no_pending wamn-pat-route-caller-acme--billing--dev

# Wrong stable identity metadata rotates only that credential.
record management-author dddddddddddddddd 2099-01-01T00:00:00Z project-author wrong-management-author
record route-caller bbbbbbbbbbbbbbbb 2099-01-01T00:00:00Z route-caller wamn-route-caller
output=$(run_bootstrap 2>&1)
assert_log $'issue:management-author\ndecorate:management-author\nstage:management-author\napply:management-author\nrevoke:dddddddddddddddd\nclear:management-author'

# Failure before the ambiguous apply cannot install new token material, so the
# EXIT cleanup revokes that newly issued PAT and leaves the old Secret untouched.
record management-author aaaaaaaaaaaaaaaa 2099-01-01T00:00:00Z project-author wamn-management-author
record route-caller 6767676767676767 2000-01-01T00:00:00Z route-caller wamn-route-caller
if MOCK_STAGE_FAIL=route-caller run_bootstrap >/dev/null 2>&1; then
    echo 'expected pre-apply staging failure' >&2
    exit 1
fi
assert_log $'issue:route-caller\ndecorate:route-caller\nstage:route-caller\nrevoke:2222222222222222'
assert_no_pending wamn-pat-route-caller-acme--billing--dev

# Non-installing apply failure is still indeterminate: retain new PAT + markers;
# rerun observes the old Secret, safely revokes new, then retries rotation.
record management-author aaaaaaaaaaaaaaaa 2099-01-01T00:00:00Z project-author wamn-management-author
record route-caller eeeeeeeeeeeeeeee 2000-01-01T00:00:00Z route-caller wamn-route-caller
if MOCK_APPLY_FAIL=route-caller run_bootstrap >/dev/null 2>&1; then
    echo 'expected apply failure' >&2
    exit 1
fi
assert_log $'issue:route-caller\ndecorate:route-caller\nstage:route-caller\napply:route-caller'
assert_pending wamn-pat-route-caller-acme--billing--dev 2222222222222222 eeeeeeeeeeeeeeee present
output=$(run_bootstrap 2>&1)
assert_log $'revoke:2222222222222222\nclear:route-caller\nissue:route-caller\ndecorate:route-caller\nstage:route-caller\napply:route-caller\nrevoke:eeeeeeeeeeeeeeee\nclear:route-caller'

# Ambiguous apply copies the desired Secret then fails. Rerun recognizes the
# installed valid replacement and revokes only the old fallback.
record route-caller ffffffffffffffff 2000-01-01T00:00:00Z route-caller wamn-route-caller
if MOCK_APPLY_AMBIGUOUS=route-caller run_bootstrap >/dev/null 2>&1; then
    echo 'expected ambiguous apply failure' >&2
    exit 1
fi
assert_log $'issue:route-caller\ndecorate:route-caller\nstage:route-caller\napply:route-caller'
assert_pending wamn-pat-route-caller-acme--billing--dev 2222222222222222 ffffffffffffffff present
output=$(run_bootstrap 2>&1)
assert_log $'revoke:ffffffffffffffff\nclear:route-caller\nissue:none'
assert_no_pending wamn-pat-route-caller-acme--billing--dev

# Reread failure has the same installed-new recovery and converges on rerun.
record route-caller abababababababab 2000-01-01T00:00:00Z route-caller wamn-route-caller
if MOCK_REREAD_FAIL=route-caller run_bootstrap >/dev/null 2>&1; then
    echo 'expected reread failure' >&2
    exit 1
fi
assert_log $'issue:route-caller\ndecorate:route-caller\nstage:route-caller\napply:route-caller'
output=$(run_bootstrap 2>&1)
assert_log $'revoke:abababababababab\nclear:route-caller\nissue:none'

# Revoke failure retains both markers; rerun retries before accepting steady state.
record route-caller cdcdcdcdcdcdcdcd 2000-01-01T00:00:00Z route-caller wamn-route-caller
if MOCK_REVOKE_FAIL_PREFIX=cdcdcdcdcdcdcdcd run_bootstrap >/dev/null 2>&1; then
    echo 'expected revoke failure' >&2
    exit 1
fi
assert_log $'issue:route-caller\ndecorate:route-caller\nstage:route-caller\napply:route-caller\nrevoke:cdcdcdcdcdcdcdcd'
output=$(run_bootstrap 2>&1)
assert_log $'revoke:cdcdcdcdcdcdcdcd\nclear:route-caller\nissue:none'

# Cleanup failure after successful revoke repeats idempotent revoke, then clears.
record route-caller dededededededede 2000-01-01T00:00:00Z route-caller wamn-route-caller
if MOCK_CLEAR_FAIL=route-caller run_bootstrap >/dev/null 2>&1; then
    echo 'expected pending-marker cleanup failure' >&2
    exit 1
fi
assert_log $'issue:route-caller\ndecorate:route-caller\nstage:route-caller\napply:route-caller\nrevoke:dededededededede\nclear:route-caller'
output=$(run_bootstrap 2>&1)
assert_log $'revoke:dededededededede\nclear:route-caller\nissue:none'

# Global preflight validates both credentials before mutating pending management.
record management-author 3333333333333333 2099-01-01T00:00:00Z project-author \
    wamn-management-author 3333333333333333 aaaaaaaaaaaaaaaa present
record route-caller bbbbbbbbbbbbbbbb 2099-01-01T00:00:00Z route-caller \
    wamn-route-caller malformed bbbbbbbbbbbbbbbb present
if run_bootstrap >/dev/null 2>&1; then
    echo 'expected global corrupt-metadata refusal' >&2
    exit 1
fi
assert_log ''

# An expired installed replacement retains the old fallback. Failed issuance
# performs no revoke; success installs a fresh replacement before old revoke.
record management-author aaaaaaaaaaaaaaaa 2099-01-01T00:00:00Z project-author wamn-management-author
record route-caller 4444444444444444 2000-01-01T00:00:00Z route-caller \
    wamn-route-caller 4444444444444444 5555555555555555 present
if MOCK_CTL_FAIL_BEFORE_WRITE=1 run_bootstrap >/dev/null 2>&1; then
    echo 'expected fresh replacement issuance failure' >&2
    exit 1
fi
assert_log 'issue:route-caller'
assert_pending wamn-pat-route-caller-acme--billing--dev 4444444444444444 5555555555555555 present
output=$(run_bootstrap 2>&1)
assert_log $'issue:route-caller\ndecorate:route-caller\nstage:route-caller\napply:route-caller\nrevoke:5555555555555555\nclear:route-caller'

# Partial ctl failure revokes a new manifest that never reached pre-apply staging.
rm "$test_dir/state/wamn-pat-management-author-acme--billing--dev"
rm "$test_dir/state/wamn-pat-route-caller-acme--billing--dev"
if MOCK_CTL_FAIL_AFTER_MANAGEMENT=1 run_bootstrap >/dev/null 2>&1; then
    echo 'expected partial issuance failure' >&2
    exit 1
fi
assert_log $'issue:both\nrevoke:1111111111111111'

# Missing/malformed prior revoke metadata fails before issuance or mutation.
record management-author aaaaaaaaaaaaaaaa 2099-01-01T00:00:00Z project-author wamn-management-author
record route-caller not-a-prefix 2000-01-01T00:00:00Z route-caller wamn-route-caller
if run_bootstrap >/dev/null 2>&1; then
    echo 'expected malformed prior prefix refusal' >&2
    exit 1
fi
assert_log ''

# Writer generation rotation is wrapper-owned: ctl prepares/authenticates,
# kubectl publishes and rolls the runner, read-only probes prove pool use, and
# only then ctl retires the old generation.
reset_writer_state
if MOCK_APPLY_FAIL=effect-writer run_writer_rotation a >/dev/null 2>&1; then
    echo 'expected non-installing writer Secret apply failure' >&2
    exit 1
fi
assert_log $'prepare:a\nprobe-login:a\napply:effect-writer\nabort:a'
[[ $(cat "$test_dir/state/.writer-a-login") == f ]]
output=$(run_writer_rotation a 2>&1)
assert_log $'prepare:a\nprobe-login:a\napply:effect-writer\nrollout:restart:deployment/runner\nrollout:status:deployment/runner\nprobe-sessions:a\nprobe-login:b\nprobe-login:a\nprobe-login:b'
assert_no_secret_output "$output"
[[ -f "$test_dir/state/.last-applied-wamn-effect-writer-acme--billing--dev" ]]

# Prepared-but-unpublished generations are aborted on every definite failure,
# including invalid ctl metadata before publication and a successful apply that
# provably left the exact prior (absent) state installed.
reset_writer_state
if MOCK_WRITER_MANIFEST_CORRUPTION=validity run_writer_rotation a >/dev/null 2>&1; then
    echo 'expected invalid prepared writer metadata to fail closed' >&2
    exit 1
fi
assert_log $'prepare:a\nabort:a'
[[ $(cat "$test_dir/state/.writer-a-login") == f ]]
reset_writer_state
if MOCK_APPLY_NOOP=effect-writer run_writer_rotation a >/dev/null 2>&1; then
    echo 'expected no-op writer Secret publication to fail closed' >&2
    exit 1
fi
assert_log $'prepare:a\nprobe-login:a\napply:effect-writer\nabort:a'
[[ $(cat "$test_dir/state/.writer-a-login") == f ]]

# An invalid same-generation installed Secret is never silently reused.
reset_writer_state
writer_record a 2026-01-01T00:00:00Z 2026-01-01T00:00:00Z 2026-02-01T00:00:00Z
if run_writer_rotation a >/dev/null 2>&1; then
    echo 'expected invalid installed writer Secret refusal' >&2
    exit 1
fi
assert_log ''

# An expired but structurally exact old Secret may rotate to the opposite slot;
# refusing that path would make ordinary expiry unrecoverable.
output=$(run_writer_rotation b 2>&1)
assert_log $'prepare:b\nprobe-login:b\napply:effect-writer\nrollout:restart:deployment/runner\nrollout:status:deployment/runner\nprobe-sessions:b\nprobe-login:a\nretire:a\nprobe-login:b\nprobe-login:a'
assert_no_secret_output "$output"

reset_writer_state
output=$(MOCK_APPLY_AMBIGUOUS=effect-writer run_writer_rotation a 2>&1)
assert_log $'prepare:a\nprobe-login:a\napply:effect-writer\nrollout:restart:deployment/runner\nrollout:status:deployment/runner\nprobe-sessions:a\nprobe-login:b\nprobe-login:a\nprobe-login:b'
assert_no_secret_output "$output"
output=$(run_writer_rotation a 2>&1)
assert_log $'rollout:restart:deployment/runner\nrollout:status:deployment/runner\nprobe-sessions:a\nprobe-login:b\nprobe-login:a\nprobe-login:b'
assert_no_secret_output "$output"
if MOCK_APPLY_FAIL=effect-writer run_writer_rotation b >/dev/null 2>&1; then
    echo 'expected prior-preserving writer Secret apply failure' >&2
    exit 1
fi
assert_log $'prepare:b\nprobe-login:b\napply:effect-writer\nabort:b'
[[ $(cat "$test_dir/state/.writer-b-login") == f ]]
installed_generation=$(cut -d '|' -f 13 \
    "$test_dir/state/wamn-effect-writer-acme--billing--dev")
[[ $installed_generation == a ]]

# A failed apply that leaves neither the exact prior nor the exact emitted
# metadata is ambiguous and requires manual reconciliation; it is not aborted.
reset_writer_state
if MOCK_APPLY_THIRD=effect-writer run_writer_rotation a >/dev/null 2>&1; then
    echo 'expected third-state writer Secret publication refusal' >&2
    exit 1
fi
assert_log $'prepare:a\nprobe-login:a\napply:effect-writer'
[[ $(cat "$test_dir/state/.writer-a-login") == t ]]

# If the Secret is absent after publication loss while both generations are
# active, the desired generation is republished and the opposite generation is
# still derived and retired before steady state is declared.
reset_writer_state
printf t >"$test_dir/state/.writer-a-login"
printf t >"$test_dir/state/.writer-b-login"
output=$(run_writer_rotation a 2>&1)
assert_log $'prepare:a\nprobe-login:a\napply:effect-writer\nrollout:restart:deployment/runner\nrollout:status:deployment/runner\nprobe-sessions:a\nprobe-login:b\nretire:b\nprobe-login:a\nprobe-login:b'
assert_no_secret_output "$output"

reset_writer_state
printf t >"$test_dir/state/.writer-a-login"
printf t >"$test_dir/state/.writer-b-login"
if MOCK_WRITER_LOGIN_PROBE_FAIL_GENERATION=b run_writer_rotation a >/dev/null 2>&1; then
    echo 'expected old-generation LOGIN probe failure to abort rotation' >&2
    exit 1
fi
assert_log $'prepare:a\nprobe-login:a\napply:effect-writer\nrollout:restart:deployment/runner\nrollout:status:deployment/runner\nprobe-sessions:a\nprobe-login:b'

if MOCK_WRITER_ROLLOUT_FAIL=1 run_writer_rotation b >/dev/null 2>&1; then
    echo 'expected writer rollout failure after Secret publication' >&2
    exit 1
fi
assert_log $'prepare:b\nprobe-login:b\napply:effect-writer\nrollout:restart:deployment/runner\nrollout:status:deployment/runner'
output=$(run_writer_rotation b 2>&1)
assert_log $'rollout:restart:deployment/runner\nrollout:status:deployment/runner\nprobe-sessions:b\nprobe-login:a\nretire:a\nprobe-login:b\nprobe-login:a'
assert_no_secret_output "$output"

if MOCK_WRITER_SESSION_ZERO=1 run_writer_rotation a >/dev/null 2>&1; then
    echo 'expected writer live-session proof failure after Secret publication' >&2
    exit 1
fi
assert_log $'prepare:a\nprobe-login:a\napply:effect-writer\nrollout:restart:deployment/runner\nrollout:status:deployment/runner\nprobe-sessions:a'
output=$(run_writer_rotation a 2>&1)
assert_log $'rollout:restart:deployment/runner\nrollout:status:deployment/runner\nprobe-sessions:a\nprobe-login:b\nretire:b\nprobe-login:a\nprobe-login:b'
assert_no_secret_output "$output"
output=$(run_writer_rotation b 2>&1)
assert_log $'prepare:b\nprobe-login:b\napply:effect-writer\nrollout:restart:deployment/runner\nrollout:status:deployment/runner\nprobe-sessions:b\nprobe-login:a\nretire:a\nprobe-login:b\nprobe-login:a'
assert_no_secret_output "$output"

# The wrapper owns every credential-bearing output flag, including role SQL.
for forbidden in \
    '--emit-role-sql=-' \
    '--emit-management-author-pat-secret=/tmp/forbidden' \
    '--emit-route-caller-pat-secret=/tmp/forbidden' \
    '--revoke-pat-prefix=0123456789abcdef' \
    '--prepare-effect-writer-generation=a' \
    '--retire-effect-writer-generation=b' \
    '--abort-effect-writer-generation=a' \
    '--emit-effect-writer-secret=/tmp/forbidden'; do
    if run_bootstrap "$forbidden" >/dev/null 2>&1; then
        echo "expected wrapper-owned flag rejection: $forbidden" >&2
        exit 1
    fi
done

echo 'bootstrap wrapper tests passed'
