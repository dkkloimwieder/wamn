#!/usr/bin/env bash
set -euo pipefail

export LC_ALL=C
umask 077

# Test doubles may override these with paths to deterministic local executables.
ctl_bin=${WAMN_CTL_BIN:-wamn-ctl}
kubectl_bin=${KUBECTL_BIN:-kubectl}

org=
project=
env_name=
namespace=${WAMN_NAMESPACE:-wamn-system}
system_database_url=
args=("$@")

require_value() {
    if (($2 >= ${#args[@]})); then
        echo "bootstrap: $1 requires a value" >&2
        exit 2
    fi
}

for ((index = 0; index < ${#args[@]}; index++)); do
    argument=${args[index]}
    case "$argument" in
        --org | --project | --env | --namespace | --system-database-url)
            require_value "$argument" "$((index + 1))"
            value=${args[index + 1]}
            case "$argument" in
                --org) org=$value ;;
                --project) project=$value ;;
                --env) env_name=$value ;;
                --namespace) namespace=$value ;;
                --system-database-url) system_database_url=$value ;;
            esac
            ((index += 1))
            ;;
        --org=*) org=${argument#*=} ;;
        --project=*) project=${argument#*=} ;;
        --env=*) env_name=${argument#*=} ;;
        --namespace=*) namespace=${argument#*=} ;;
        --system-database-url=*) system_database_url=${argument#*=} ;;
        --emit-role-sql|--emit-role-sql=*|\
        --emit-management-author-pat-secret|--emit-management-author-pat-secret=*|\
        --emit-route-caller-pat-secret|--emit-route-caller-pat-secret=*|\
        --revoke-pat-prefix|--revoke-pat-prefix=*)
            echo "bootstrap: $argument is wrapper-owned and must not be supplied" >&2
            exit 2
            ;;
    esac
done

[[ -n $org ]] || { echo "bootstrap: --org is required" >&2; exit 2; }
[[ -n $project ]] || { echo "bootstrap: --project is required" >&2; exit 2; }
[[ -n $env_name ]] || { echo "bootstrap: --env is required" >&2; exit 2; }

work_dir=$(mktemp -d)
role_sql_path=$work_dir/role.sql
management_path=$work_dir/management-author.json
route_path=$work_dir/route-caller.json
management_new_prefix=
route_new_prefix=

revoke_pat() {
    local prefix=$1
    local revoke_connection=()
    if [[ -n $system_database_url ]]; then
        revoke_connection=(--system-database-url "$system_database_url")
    fi
    "$ctl_bin" provision-project-env "${revoke_connection[@]}" --revoke-pat-prefix "$prefix"
}

cleanup() {
    local status=$? cleanup_failed=false prefix
    trap - EXIT
    set +e
    for prefix in "$management_new_prefix" "$route_new_prefix"; do
        [[ -z $prefix ]] && continue
        if ! revoke_pat "$prefix"; then
            echo "bootstrap: failed to revoke an uninstalled newly issued PAT" >&2
            cleanup_failed=true
        fi
    done
    rm -rf -- "$work_dir"
    if [[ $status -eq 0 && $cleanup_failed == true ]]; then
        status=1
    fi
    exit "$status"
}
trap cleanup EXIT

secret_template='{{.metadata.name}}|{{.metadata.namespace}}|'
secret_template+='{{with .metadata.labels}}{{with index . "app.kubernetes.io/managed-by"}}{{.}}{{end}}{{end}}|'
secret_template+='{{with .metadata.labels}}{{with index . "app.kubernetes.io/component"}}{{.}}{{end}}{{end}}|'
secret_template+='{{with .metadata.labels}}{{with index . "wamn.org"}}{{.}}{{end}}{{end}}|'
secret_template+='{{with .metadata.labels}}{{with index . "wamn.project"}}{{.}}{{end}}{{end}}|'
secret_template+='{{with .metadata.labels}}{{with index . "wamn.env"}}{{.}}{{end}}{{end}}|'
secret_template+='{{with .metadata.annotations}}{{with index . "wamn.io/credential-purpose"}}{{.}}{{end}}{{end}}|'
secret_template+='{{with .metadata.annotations}}{{with index . "wamn.io/principal-id"}}{{.}}{{end}}{{end}}|'
secret_template+='{{with .metadata.annotations}}{{with index . "wamn.io/principal-kind"}}{{.}}{{end}}{{end}}|'
secret_template+='{{with .metadata.annotations}}{{with index . "wamn.io/principal-subject"}}{{.}}{{end}}{{end}}|'
secret_template+='{{with .metadata.annotations}}{{with index . "wamn.io/project-role"}}{{.}}{{end}}{{end}}|'
secret_template+='{{with .metadata.annotations}}{{with index . "wamn.io/pat-prefix"}}{{.}}{{end}}{{end}}|'
secret_template+='{{with .metadata.annotations}}{{with index . "wamn.io/pat-expires-at"}}{{.}}{{end}}{{end}}|'
secret_template+='{{with .metadata.annotations}}{{with index . "wamn.io/pending-issued-pat-prefix"}}{{.}}{{end}}{{end}}|'
secret_template+='{{with .metadata.annotations}}{{with index . "wamn.io/pending-revoke-pat-prefix"}}{{.}}{{end}}{{end}}|'
secret_template+='{{with .data}}{{if index . "token"}}present{{else}}absent{{end}}{{else}}absent{{end}}'
secret_state=
secret_prefix=
secret_pending_issued=
secret_pending_revoke=

inspect_secret() {
    local purpose=$1 secret_name=$2 subject=$3 role=$4
    local found record expires_at parsed_expiry now token_state
    local identity_valid=false expiry_canonical=false expiry_future=false
    local actual_name actual_namespace managed_by component actual_org actual_project actual_env
    local actual_purpose principal_id principal_kind actual_subject actual_role prefix
    local pending_issued pending_revoke

    found=$("$kubectl_bin" get secret "$secret_name" --namespace "$namespace" \
        --ignore-not-found -o 'jsonpath={.metadata.name}')
    if [[ -z $found ]]; then
        secret_state=absent
        secret_prefix=
        secret_pending_issued=
        secret_pending_revoke=
        return
    fi

    record=$("$kubectl_bin" get secret "$secret_name" --namespace "$namespace" \
        -o "go-template=$secret_template")
    IFS='|' read -r actual_name actual_namespace managed_by component actual_org \
        actual_project actual_env actual_purpose principal_id principal_kind \
        actual_subject actual_role prefix expires_at pending_issued pending_revoke \
        token_state <<<"$record"

    if [[ $actual_name == "$secret_name" && $actual_namespace == "$namespace" &&
        $managed_by == wamn && $component == project-env-pat &&
        $actual_org == "$org" && $actual_project == "$project" &&
        $actual_env == "$env_name" && $actual_purpose == "$purpose" &&
        $principal_id =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ &&
        $principal_kind == service && $actual_subject == "$subject" &&
        $actual_role == "$role" && $prefix =~ ^[0-9a-f]{16}$ ]]; then
        identity_valid=true
    fi
    if [[ $expires_at =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$ ]] &&
        parsed_expiry=$(date -u --date="$expires_at" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null) &&
        [[ $parsed_expiry == "$expires_at" ]]; then
        expiry_canonical=true
    fi
    now=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    if [[ $expiry_canonical == true && $expires_at > "$now" ]]; then
        expiry_future=true
    fi

    if [[ -z $pending_issued && -z $pending_revoke ]]; then
        if [[ $identity_valid == true && $expiry_future == true && $token_state == present ]]; then
            secret_state=valid
        elif [[ $prefix =~ ^[0-9a-f]{16}$ ]]; then
            secret_state=invalid
        else
            secret_state=corrupt
        fi
    elif [[ ! $pending_issued =~ ^[0-9a-f]{16}$ ||
        ( -n $pending_revoke && ! $pending_revoke =~ ^[0-9a-f]{16}$ ) ||
        $pending_issued == "$pending_revoke" || ! $prefix =~ ^[0-9a-f]{16}$ ||
        ( $token_state != present && $token_state != absent ) ]]; then
        secret_state=corrupt
    elif [[ $prefix == "$pending_issued" && $token_state == absent ]]; then
        if [[ -z $pending_revoke && $identity_valid == true && $expiry_canonical == true ]]; then
            secret_state=stub
        else
            secret_state=corrupt
        fi
    elif [[ $prefix == "$pending_issued" && $token_state == present ]]; then
        if [[ $identity_valid != true || $expiry_canonical != true ]]; then
            secret_state=corrupt
        elif [[ $expiry_future == true ]]; then
            secret_state=pending
        else
            secret_state=pending_expired
        fi
    elif [[ $prefix == "$pending_revoke" ]]; then
        secret_state=staged
    elif [[ $token_state == present && $identity_valid == true &&
        $expiry_canonical == true && $expiry_future != true ]]; then
        # A fresh issue was staged over an already-expired pending replacement.
        secret_state=staged
    else
        secret_state=corrupt
    fi

    secret_prefix=$prefix
    secret_pending_issued=$pending_issued
    secret_pending_revoke=$pending_revoke
}

capture_management() {
    management_state=$secret_state
    management_prefix=$secret_prefix
    management_pending_issued=$secret_pending_issued
    management_pending_revoke=$secret_pending_revoke
}

capture_route() {
    route_state=$secret_state
    route_prefix=$secret_prefix
    route_pending_issued=$secret_pending_issued
    route_pending_revoke=$secret_pending_revoke
}

clear_pending() {
    local secret_name=$1 pending_revoke=$2
    local annotations=('wamn.io/pending-issued-pat-prefix-')
    if [[ -n $pending_revoke ]]; then
        annotations+=('wamn.io/pending-revoke-pat-prefix-')
    fi
    "$kubectl_bin" annotate secret "$secret_name" --namespace "$namespace" \
        "${annotations[@]}"
}

restore_expired_pending() {
    local secret_name=$1 prefix=$2
    "$kubectl_bin" annotate secret "$secret_name" --namespace "$namespace" --overwrite \
        "wamn.io/pending-issued-pat-prefix=$prefix"
}

reconcile_recovery() {
    local state=$1 secret_name=$2 prefix=$3 pending_issued=$4 pending_revoke=$5

    case $state in
        pending)
            # Only a currently valid/unexpired installed replacement may retire
            # the fallback token.
            if [[ -n $pending_revoke ]]; then
                revoke_pat "$pending_revoke"
            fi
            clear_pending "$secret_name" "$pending_revoke"
            ;;
        stub)
            # The final-name stub contains no token, so its pending issue is not
            # installed and is safe to revoke before removing the temporary state.
            revoke_pat "$pending_issued"
            "$kubectl_bin" delete secret "$secret_name" --namespace "$namespace" \
                --ignore-not-found
            ;;
        staged)
            # The current Secret does not contain pending_issued. Retire that
            # uninstalled issue, then restore the prior recovery state.
            revoke_pat "$pending_issued"
            if [[ $prefix == "$pending_revoke" ]]; then
                clear_pending "$secret_name" "$pending_revoke"
            else
                restore_expired_pending "$secret_name" "$prefix"
            fi
            ;;
        absent|valid|invalid|pending_expired) ;;
        *) echo "bootstrap: internal unhandled Secret state $state" >&2; exit 1 ;;
    esac
}

management_name=wamn-pat-management-author-$org--$project--$env_name
management_subject=wamn-management-author-$org--$project--$env_name
route_name=wamn-pat-route-caller-$org--$project--$env_name
route_subject=wamn-route-caller-$org--$project--$env_name

# Global read-only preflight: neither credential may mutate until both current
# Secrets and both recovery-marker sets have been inspected and classified.
inspect_secret management-author "$management_name" "$management_subject" project-author
capture_management
inspect_secret route-caller "$route_name" "$route_subject" route-caller
capture_route
if [[ $management_state == corrupt || $route_state == corrupt ]]; then
    echo "bootstrap: corrupt PAT Secret or recovery metadata; refusing all mutation" >&2
    exit 1
fi

reconcile_recovery "$management_state" "$management_name" "$management_prefix" \
    "$management_pending_issued" "$management_pending_revoke"
reconcile_recovery "$route_state" "$route_name" "$route_prefix" \
    "$route_pending_issued" "$route_pending_revoke"

# Re-read after recovery actions to establish the issuance inputs.
inspect_secret management-author "$management_name" "$management_subject" project-author
capture_management
inspect_secret route-caller "$route_name" "$route_subject" route-caller
capture_route
for state in "$management_state" "$route_state"; do
    case $state in
        absent|valid|invalid|pending_expired) ;;
        *) echo "bootstrap: PAT recovery did not converge" >&2; exit 1 ;;
    esac
done

management_old_prefix=
route_old_prefix=
case $management_state in
    invalid) management_old_prefix=$management_prefix ;;
    pending_expired) management_old_prefix=$management_pending_revoke ;;
esac
case $route_state in
    invalid) route_old_prefix=$route_prefix ;;
    pending_expired) route_old_prefix=$route_pending_revoke ;;
esac

issue_args=()
if [[ $management_state != valid ]]; then
    issue_args+=(--emit-management-author-pat-secret "$management_path")
else
    echo "bootstrap: $management_name is valid; skipping issuance"
fi
if [[ $route_state != valid ]]; then
    issue_args+=(--emit-route-caller-pat-secret "$route_path")
else
    echo "bootstrap: $route_name is valid; skipping issuance"
fi

manifest_prefix() {
    "$kubectl_bin" create --dry-run=client --validate=false -f "$1" \
        -o 'jsonpath={.metadata.annotations.wamn\.io/pat-prefix}'
}

track_manifest() {
    local path=$1 variable=$2 prefix
    [[ -f $path ]] || return 0
    prefix=$(manifest_prefix "$path")
    if [[ ! $prefix =~ ^[0-9a-f]{16}$ ]]; then
        echo "bootstrap: issued PAT manifest has an invalid lookup prefix" >&2
        exit 1
    fi
    printf -v "$variable" '%s' "$prefix"
}

# ctl authenticates every newly issued PAT before it writes the Secret manifest.
# The role SQL always goes to a wrapper-owned 0600 path, never stdout.
if ! "$ctl_bin" provision-project-env "${args[@]}" \
    --emit-role-sql "$role_sql_path" "${issue_args[@]}"; then
    track_manifest "$management_path" management_new_prefix
    track_manifest "$route_path" route_new_prefix
    exit 1
fi
track_manifest "$management_path" management_new_prefix
track_manifest "$route_path" route_new_prefix

decorate_manifest() {
    local path=$1 new_prefix=$2 old_prefix=$3 decorated=${1%.json}-decorated.json
    local annotations=("wamn.io/pending-issued-pat-prefix=$new_prefix")
    if [[ -n $old_prefix ]]; then
        annotations+=("wamn.io/pending-revoke-pat-prefix=$old_prefix")
    fi
    "$kubectl_bin" annotate --local --overwrite -f "$path" "${annotations[@]}" \
        -o json >"$decorated"
    mv -- "$decorated" "$path"
}

stage_recovery() {
    local state=$1 path=$2 secret_name=$3 purpose=$4 new_prefix=$5 old_prefix=$6
    local stub=${path%.json}-stub.json

    decorate_manifest "$path" "$new_prefix" "$old_prefix"
    if [[ $state == absent ]]; then
        "$kubectl_bin" patch --local -f "$path" --type=merge \
            -p '{"data":null,"stringData":null}' -o json >"$stub"
        "$kubectl_bin" apply -f "$stub"
    else
        local annotations=("wamn.io/pending-issued-pat-prefix=$new_prefix")
        if [[ -n $old_prefix ]]; then
            annotations+=("wamn.io/pending-revoke-pat-prefix=$old_prefix")
        fi
        "$kubectl_bin" annotate secret "$secret_name" --namespace "$namespace" --overwrite \
            "${annotations[@]}"
    fi

    # The staging write cannot install the new token. Require its durable marker
    # before entering the ambiguous final apply.
    case $purpose in
        management-author)
            inspect_secret "$purpose" "$secret_name" "$management_subject" project-author
            ;;
        route-caller)
            inspect_secret "$purpose" "$secret_name" "$route_subject" route-caller
            ;;
    esac
    if [[ $state == absent ]]; then
        [[ $secret_state == stub && $secret_pending_issued == "$new_prefix" ]] || {
            echo "bootstrap: tokenless issuance stub failed reread validation" >&2
            exit 1
        }
    else
        [[ $secret_state == staged && $secret_pending_issued == "$new_prefix" &&
            $secret_pending_revoke == "$old_prefix" ]] || {
            echo "bootstrap: pre-apply recovery marker failed reread validation" >&2
            exit 1
        }
    fi
}

apply_verify_revoke() {
    local purpose=$1 path=$2 secret_name=$3 subject=$4 role=$5 prior_state=$6
    local old_prefix=$7 new_prefix_variable=$8 new_prefix=${!8}

    stage_recovery "$prior_state" "$path" "$secret_name" "$purpose" \
        "$new_prefix" "$old_prefix"

    # From this point the final apply is ambiguous: a nonzero client exit may
    # still have installed the new token. Its durable marker owns all recovery.
    printf -v "$new_prefix_variable" ''
    "$kubectl_bin" apply -f "$path"

    inspect_secret "$purpose" "$secret_name" "$subject" "$role"
    if [[ $secret_state != pending || $secret_prefix != "$new_prefix" ||
        $secret_pending_issued != "$new_prefix" ||
        $secret_pending_revoke != "$old_prefix" ]]; then
        echo "bootstrap: applied $secret_name failed pending replacement validation" >&2
        exit 1
    fi
    if [[ -n $old_prefix ]]; then
        revoke_pat "$old_prefix"
    fi
    clear_pending "$secret_name" "$old_prefix"
    inspect_secret "$purpose" "$secret_name" "$subject" "$role"
    if [[ $secret_state != valid || $secret_prefix != "$new_prefix" ]]; then
        echo "bootstrap: $secret_name failed post-revocation reread validation" >&2
        exit 1
    fi
}

if [[ $management_state != valid ]]; then
    apply_verify_revoke management-author "$management_path" "$management_name" \
        "$management_subject" project-author "$management_state" "$management_old_prefix" \
        management_new_prefix
fi
if [[ $route_state != valid ]]; then
    apply_verify_revoke route-caller "$route_path" "$route_name" \
        "$route_subject" route-caller "$route_state" "$route_old_prefix" route_new_prefix
fi
