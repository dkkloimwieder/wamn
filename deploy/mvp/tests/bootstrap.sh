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

case $1 in
    get)
        name=$3
        file=$MOCK_STATE/$name
        check_namespace "$@"
        if [[ $* == *ignore-not-found* ]]; then
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
        printf '%s' "$prefix"
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
            echo "apply:$purpose" >>"$MOCK_LOG"
            [[ ${MOCK_APPLY_FAIL:-} != "$purpose" ]] || exit 1
            save_record "$MOCK_STATE/$name"
            [[ ${MOCK_APPLY_AMBIGUOUS:-} != "$purpose" ]] || exit 1
            if [[ ${MOCK_REREAD_FAIL:-} == "$purpose" ]]; then
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
namespace=${WAMN_NAMESPACE:-wamn-system}
while (($#)); do
    case $1 in
        --emit-management-author-pat-secret) management=$2; shift 2 ;;
        --emit-route-caller-pat-secret) route=$2; shift 2 ;;
        --emit-role-sql) role_sql=$2; shift 2 ;;
        --revoke-pat-prefix) revoke=$2; shift 2 ;;
        --namespace) namespace=$2; shift 2 ;;
        --namespace=*) namespace=${1#*=}; shift ;;
        *) shift ;;
    esac
done

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
chmod +x "$test_dir/bin/kubectl" "$test_dir/bin/wamn-ctl" "$bootstrap"

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
        "$bootstrap" --org acme --project billing --env dev \
        --system-database-url 'postgres://admin:URL_SECRET_SENTINEL@sys/db' \
        --emit-secret "$test_dir/db.json" "$@"
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
        $output == *ROLE_SQL_PASSWORD_SENTINEL* ]]; then
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
        --app-password ROLE_SQL_PASSWORD_SENTINEL --emit-secret "$test_dir/db.json" 2>&1)
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

# The wrapper owns every credential-bearing output flag, including role SQL.
for forbidden in \
    '--emit-role-sql=-' \
    '--emit-management-author-pat-secret=/tmp/forbidden' \
    '--emit-route-caller-pat-secret=/tmp/forbidden' \
    '--revoke-pat-prefix=0123456789abcdef'; do
    if run_bootstrap "$forbidden" >/dev/null 2>&1; then
        echo "expected wrapper-owned flag rejection: $forbidden" >&2
        exit 1
    fi
done

echo 'bootstrap wrapper tests passed'
