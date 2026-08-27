#!/usr/bin/env bash
set -euo pipefail

export LC_ALL=C
umask 077

# Test doubles may override these with paths to deterministic local executables.
ctl_bin=${WAMN_CTL_BIN:-wamn-ctl}
kubectl_bin=${KUBECTL_BIN:-kubectl}
psql_bin=${PSQL_BIN:-psql}

org=
project=
env_name=
tenant=
namespace=${WAMN_NAMESPACE:-wamn-system}
system_database_url=
target_admin_database_url=
rotate_effect_writer_generation=
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
        --org | --project | --env | --tenant | --namespace | --system-database-url | \
        --target-admin-database-url | --rotate-effect-writer-generation)
            require_value "$argument" "$((index + 1))"
            value=${args[index + 1]}
            case "$argument" in
                --org) org=$value ;;
                --project) project=$value ;;
                --env) env_name=$value ;;
                --tenant) tenant=$value ;;
                --namespace) namespace=$value ;;
                --system-database-url) system_database_url=$value ;;
                --target-admin-database-url) target_admin_database_url=$value ;;
                --rotate-effect-writer-generation) rotate_effect_writer_generation=$value ;;
            esac
            ((index += 1))
            ;;
        --org=*) org=${argument#*=} ;;
        --project=*) project=${argument#*=} ;;
        --env=*) env_name=${argument#*=} ;;
        --tenant=*) tenant=${argument#*=} ;;
        --namespace=*) namespace=${argument#*=} ;;
        --system-database-url=*) system_database_url=${argument#*=} ;;
        --target-admin-database-url=*) target_admin_database_url=${argument#*=} ;;
        --rotate-effect-writer-generation=*) rotate_effect_writer_generation=${argument#*=} ;;
        --emit-role-sql|--emit-role-sql=*|\
        --emit-management-author-pat-secret|--emit-management-author-pat-secret=*|\
        --emit-route-caller-pat-secret|--emit-route-caller-pat-secret=*|\
        --revoke-pat-prefix|--revoke-pat-prefix=*|\
        --prepare-effect-writer-generation|--prepare-effect-writer-generation=*|\
        --retire-effect-writer-generation|--retire-effect-writer-generation=*|\
        --abort-effect-writer-generation|--abort-effect-writer-generation=*|\
        --emit-effect-writer-secret|--emit-effect-writer-secret=*|\
        --prepare-control-author-generation|--prepare-control-author-generation=*|\
        --retire-control-author-generation|--retire-control-author-generation=*|\
        --abort-control-author-generation|--abort-control-author-generation=*|\
        --emit-control-author-secret|--emit-control-author-secret=*)
            echo "bootstrap: $argument is wrapper-owned and must not be supplied" >&2
            exit 2
            ;;
    esac
done

[[ -n $org ]] || { echo "bootstrap: --org is required" >&2; exit 2; }
[[ -n $project ]] || { echo "bootstrap: --project is required" >&2; exit 2; }
[[ -n $env_name ]] || { echo "bootstrap: --env is required" >&2; exit 2; }
[[ -n $tenant ]] || { echo "bootstrap: --tenant is required" >&2; exit 2; }
if [[ -n $rotate_effect_writer_generation ]]; then
    [[ $rotate_effect_writer_generation == a || $rotate_effect_writer_generation == b ]] || {
        echo "bootstrap: --rotate-effect-writer-generation must be a or b" >&2
        exit 2
    }
    [[ -n $target_admin_database_url ]] || {
        echo "bootstrap: rotation requires --target-admin-database-url" >&2
        exit 2
    }
elif [[ -n $target_admin_database_url ]]; then
    echo "bootstrap: --target-admin-database-url is valid only with writer rotation" >&2
    exit 2
fi

work_dir=$(mktemp -d)
role_sql_path=$work_dir/role.sql
management_path=$work_dir/management-author.json
route_path=$work_dir/route-caller.json
management_new_prefix=
route_new_prefix=
effect_writer_prepared_generation=

effect_writer_secret_name=wamn-effect-writer-$org--$project--$env_name
effect_writer_secret_path=$work_dir/effect-writer.json

effect_writer_template='{{.metadata.name}}|{{.metadata.namespace}}|'
effect_writer_template+='{{with .metadata.labels}}{{index . "app.kubernetes.io/managed-by"}}{{end}}|'
effect_writer_template+='{{with .metadata.labels}}{{index . "app.kubernetes.io/component"}}{{end}}|'
effect_writer_template+='{{with .metadata.labels}}{{index . "wamn.org"}}{{end}}|'
effect_writer_template+='{{with .metadata.labels}}{{index . "wamn.project"}}{{end}}|'
effect_writer_template+='{{with .metadata.labels}}{{index . "wamn.env"}}{{end}}|'
effect_writer_template+='{{with .metadata.annotations}}{{index . "wamn.io/tenant"}}{{end}}|'
effect_writer_template+='{{with .metadata.annotations}}{{index . "wamn.io/credential-id"}}{{end}}|'
effect_writer_template+='{{with .metadata.annotations}}{{index . "wamn.io/credential-generation"}}{{end}}|'
effect_writer_template+='{{with .metadata.annotations}}{{index . "wamn.io/database-role"}}{{end}}|'
effect_writer_template+='{{with .metadata.annotations}}{{index . "wamn.io/predecessor-database-role"}}{{end}}|'
effect_writer_template+='{{with .metadata.annotations}}{{index . "wamn.io/issued-at"}}{{end}}|'
effect_writer_template+='{{with .metadata.annotations}}{{index . "wamn.io/not-before"}}{{end}}|'
effect_writer_template+='{{with .metadata.annotations}}{{index . "wamn.io/expires-at"}}{{end}}|'
effect_writer_template+='{{range $key, $value := .metadata.annotations}}{{if eq $key "wamn.io/revoked-at"}}present={{$value}}{{end}}{{end}}'

effect_writer_manifest_metadata() {
    "$kubectl_bin" create --dry-run=client --validate=false -f "$1" \
        -o "go-template=$effect_writer_template"
}

installed_effect_writer_metadata() {
    "$kubectl_bin" get secret "$effect_writer_secret_name" --namespace "$namespace" \
        --ignore-not-found \
        -o "go-template=$effect_writer_template"
}

effect_writer_role_login() {
    "$psql_bin" "$target_admin_database_url" -X -A -t -v "role=$1" \
        -c "SELECT rolcanlogin FROM pg_roles WHERE rolname = :'role'"
}

effect_writer_role_sessions() {
    "$psql_bin" "$target_admin_database_url" -X -A -t -v "role=$1" \
        -c "SELECT count(*) FROM pg_stat_activity WHERE usename = :'role'"
}

canonical_utc() {
    local value=$1 parsed
    [[ $value =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$ ]] || return 1
    parsed=$(date -u --date="$value" +%Y-%m-%dT%H:%M:%SZ 2>/dev/null) || return 1
    [[ $parsed == "$value" ]]
}

validate_effect_writer_metadata_structure() {
    local record=$1 revoked_timestamp
    local actual_name actual_namespace managed_by component actual_org actual_project actual_env
    local actual_tenant credential_id generation role predecessor_role
    local issued_at not_before expires_at revoked_at
    IFS='|' read -r actual_name actual_namespace managed_by component actual_org actual_project \
        actual_env actual_tenant credential_id generation role predecessor_role issued_at \
        not_before expires_at revoked_at <<<"$record"
    [[ $actual_name == "$effect_writer_secret_name" && $actual_namespace == "$namespace" &&
        $managed_by == wamn && $component == effect-writer-credentials &&
        $actual_org == "$org" && $actual_project == "$project" &&
        $actual_env == "$env_name" &&
        ( -z $actual_tenant || $actual_tenant == "$tenant" ) &&
        $credential_id =~ ^[0-9a-f]{32}$ &&
        ( $generation == a || $generation == b ) &&
        $role =~ ^wamn_effect_writer_[0-9a-f]{40}_$generation$ ]] || return 1
    if [[ -n $predecessor_role ]]; then
        [[ $predecessor_role =~ ^wamn_effect_writer_[0-9a-f]{40}_[ab]$ &&
            ${predecessor_role##*_} != "$generation" ]] || return 1
    fi
    canonical_utc "$issued_at" && canonical_utc "$not_before" && \
        canonical_utc "$expires_at" || return 1
    if [[ -n $revoked_at ]]; then
        [[ $revoked_at == present=* ]] || return 1
        revoked_timestamp=${revoked_at#present=}
        canonical_utc "$revoked_timestamp" || return 1
    fi
    [[ ! $issued_at > $not_before && $not_before < $expires_at ]]
}

effect_writer_metadata_is_current() {
    local record=$1 now
    local _name _namespace _managed _component _org _project _env _tenant _credential_id
    local _generation _role _predecessor _issued_at not_before expires_at revoked_at
    IFS='|' read -r _name _namespace _managed _component _org _project _env _tenant _credential_id \
        _generation _role _predecessor _issued_at not_before expires_at revoked_at <<<"$record"
    now=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    [[ -z $revoked_at && ! $now < $not_before && $now < $expires_at ]]
}

abort_prepared_effect_writer() {
    local generation=$1
    "$ctl_bin" provision-project-env \
        --org "$org" --project "$project" --env "$env_name" --tenant "$tenant" \
        --namespace "$namespace" \
        --target-admin-database-url "$target_admin_database_url" \
        --abort-effect-writer-generation "$generation"
}

abort_prepared_effect_writer_then_fail() {
    local generation=$1 message=$2
    effect_writer_prepared_generation=$generation
    if ! abort_prepared_effect_writer "$generation"; then
        echo "bootstrap: $message; abort of the unpublished generation also failed" >&2
        exit 1
    fi
    effect_writer_prepared_generation=
    echo "bootstrap: $message; aborted the unpublished generation" >&2
    exit 1
}

rotate_effect_writer() {
    local desired=$rotate_effect_writer_generation current prior current_role= current_generation=
    local current_tenant= current_predecessor= legacy_scope=false
    local new new_role new_generation new_tenant new_predecessor= old_role= old_generation=
    local old_login sessions login installed
    if ! current=$(installed_effect_writer_metadata); then
        echo "bootstrap: failed to inspect the installed effect-writer Secret" >&2
        exit 1
    fi
    if [[ -n $current ]]; then
        if ! validate_effect_writer_metadata_structure "$current"; then
            echo "bootstrap: installed effect-writer Secret metadata is invalid" >&2
            exit 1
        fi
        IFS='|' read -r _ _ _ _ _ _ _ current_tenant _ current_generation current_role \
            current_predecessor _ <<<"$current"
        [[ -z $current_tenant ]] && legacy_scope=true
    fi

    if $legacy_scope && [[ $current_generation == "$desired" ]]; then
        echo "bootstrap: legacy effect-writer migration must target the opposite generation" >&2
        exit 1
    fi

    if [[ $current_generation == "$desired" ]]; then
        if ! effect_writer_metadata_is_current "$current"; then
            echo "bootstrap: requested installed effect-writer generation is outside its valid unrevoked window" >&2
            exit 1
        fi
        new=$current
        new_role=$current_role
        new_generation=$current_generation
        if [[ -n $current_predecessor ]]; then
            old_role=$current_predecessor
            old_generation=${old_role##*_}
        else
            old_generation=$([[ $desired == a ]] && printf b || printf a)
            old_role=${new_role%_?}_$old_generation
        fi
    else
        prior=$current
        if [[ -n $current_role ]]; then
            old_role=$current_role
            old_generation=$current_generation
        fi
        "$ctl_bin" provision-project-env \
            --org "$org" --project "$project" --env "$env_name" --tenant "$tenant" \
            --namespace "$namespace" \
            --target-admin-database-url "$target_admin_database_url" \
            --prepare-effect-writer-generation "$desired" \
            --emit-effect-writer-secret "$effect_writer_secret_path"
        effect_writer_prepared_generation=$desired
        if ! new=$(effect_writer_manifest_metadata "$effect_writer_secret_path"); then
            abort_prepared_effect_writer_then_fail "$desired" \
                "could not inspect ctl's prepared effect-writer Secret"
        fi
        if ! validate_effect_writer_metadata_structure "$new" ||
            ! effect_writer_metadata_is_current "$new"; then
            abort_prepared_effect_writer_then_fail "$desired" \
                "ctl emitted invalid effect-writer Secret metadata"
        fi
        IFS='|' read -r _ _ _ _ _ _ _ new_tenant _ new_generation new_role new_predecessor \
            _ <<<"$new"
        [[ $new_generation == "$desired" ]] || {
            abort_prepared_effect_writer_then_fail "$desired" \
                "ctl emitted the wrong effect-writer generation"
        }
        [[ $new_tenant == "$tenant" ]] || {
            abort_prepared_effect_writer_then_fail "$desired" \
                "ctl emitted an effect-writer credential for the wrong tenant"
        }
        if [[ -n $old_role && $legacy_scope == false && ${new_role%_?} != "${old_role%_?}" ]]; then
            abort_prepared_effect_writer_then_fail "$desired" \
                "ctl emitted a role outside the installed credential scope"
        fi
        if [[ -n $old_role && -n $new_predecessor && $new_predecessor != "$old_role" ]]; then
            abort_prepared_effect_writer_then_fail "$desired" \
                "ctl observed a predecessor different from the installed credential"
        fi
        if [[ -z $old_role ]]; then
            old_role=$new_predecessor
            [[ -z $old_role ]] || old_generation=${old_role##*_}
        fi
        if ! login=$(effect_writer_role_login "$new_role"); then
            abort_prepared_effect_writer_then_fail "$desired" \
                "failed to verify the prepared effect-writer generation"
        fi
        [[ $login == t ]] || {
            abort_prepared_effect_writer_then_fail "$desired" \
                "prepared effect-writer generation is not LOGIN-capable"
        }
        # From this point publication may succeed even when the client observes
        # failure. EXIT cleanup must no longer guess; the exact reread below
        # classifies whether an explicit abort remains safe.
        effect_writer_prepared_generation=
        if ! "$kubectl_bin" apply -f "$effect_writer_secret_path"; then
            if ! installed=$(installed_effect_writer_metadata); then
                echo "bootstrap: effect-writer Secret apply failed and its installed state is unreadable; manual reconciliation is required" >&2
                exit 1
            fi
            if [[ $installed == "$new" ]]; then
                : # Ambiguous apply succeeded before the client observed failure.
            elif [[ $installed == "$prior" ]]; then
                abort_prepared_effect_writer_then_fail "$desired" \
                    "effect-writer Secret apply did not publish the prepared generation"
            else
                echo "bootstrap: effect-writer Secret apply failed into a third metadata state; manual reconciliation is required" >&2
                exit 1
            fi
        else
            if ! installed=$(installed_effect_writer_metadata); then
                echo "bootstrap: could not verify the effect-writer Secret after apply; manual reconciliation is required" >&2
                exit 1
            fi
            if [[ $installed != "$new" ]]; then
                if [[ $installed == "$prior" ]]; then
                    abort_prepared_effect_writer_then_fail "$desired" \
                        "effect-writer Secret apply did not publish the prepared generation"
                fi
                echo "bootstrap: effect-writer Secret apply produced mismatched metadata; manual reconciliation is required" >&2
                exit 1
            fi
        fi
    fi

    "$kubectl_bin" rollout restart deployment/executor --namespace "$namespace"
    "$kubectl_bin" rollout status deployment/executor --namespace "$namespace" --timeout=120s
    sessions=$(effect_writer_role_sessions "$new_role")
    [[ $sessions =~ ^[0-9]+$ && $sessions -gt 0 ]] || {
        echo "bootstrap: replacement effect-writer generation has no live private-pool session" >&2
        exit 1
    }

    if [[ -n $old_role ]]; then
        if ! old_login=$(effect_writer_role_login "$old_role"); then
            echo "bootstrap: failed to verify old effect-writer generation" >&2
            exit 1
        fi
        [[ -z $old_login || $old_login == t || $old_login == f ]] || {
            echo "bootstrap: old effect-writer generation returned an invalid LOGIN state" >&2
            exit 1
        }
        if [[ $old_login == t ]]; then
            "$ctl_bin" provision-project-env \
                --org "$org" --project "$project" --env "$env_name" --tenant "$tenant" \
                --namespace "$namespace" \
                --target-admin-database-url "$target_admin_database_url" \
                --retire-effect-writer-generation "$old_generation"
        fi
    fi
    login=$(effect_writer_role_login "$new_role")
    [[ $login == t ]] || {
        echo "bootstrap: replacement effect-writer generation was not retained" >&2
        exit 1
    }
    if [[ -n $old_role ]]; then
        if ! old_login=$(effect_writer_role_login "$old_role"); then
            echo "bootstrap: failed to verify retired effect-writer generation" >&2
            exit 1
        fi
        [[ -z $old_login || $old_login == t || $old_login == f ]] || {
            echo "bootstrap: retired effect-writer generation returned an invalid LOGIN state" >&2
            exit 1
        }
        if [[ $old_login == t ]]; then
            echo "bootstrap: old effect-writer generation remains LOGIN-capable" >&2
            exit 1
        fi
    fi
}

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
    if [[ -n $effect_writer_prepared_generation ]]; then
        if ! abort_prepared_effect_writer "$effect_writer_prepared_generation"; then
            echo "bootstrap: failed to abort an unpublished prepared effect-writer generation" >&2
            cleanup_failed=true
        fi
        effect_writer_prepared_generation=
    fi
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

if [[ -n $rotate_effect_writer_generation ]]; then
    rotate_effect_writer
    exit 0
fi

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
