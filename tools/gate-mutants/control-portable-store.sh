#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-0h0g.9.9"
readonly OUTCOME="control portable storage preserves exact bytes, local integrity, owner-only ACL, and replay drift refusal"

repository=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
ddl="$repository/deploy/sql/control-portable-store.sql"
test_file="$repository/crates/control/provision/tests/control_portable_store.rs"
target_dir=${CARGO_TARGET_DIR:-/home/kaalin/dev/wamn/target/plane-wave8-9-9}
expected_ddl_sha=12d38415c256ff1da5af7c2f2e45c82e292ba4e0dcb93585daf995ab81fda9d1
expected_test_sha=659b9e17006e21cb378ea70c9c890a0e66f8f2347018fd0ddc87396f14ab20b8

sha() {
    sha256sum "$1" | cut -d' ' -f1
}

assert_baseline() {
    [[ $(sha "$ddl") == "$expected_ddl_sha" ]] || {
        echo "control portable DDL SHA drifted" >&2
        return 1
    }
    [[ $(sha "$test_file") == "$expected_test_sha" ]] || {
        echo "control portable test SHA drifted" >&2
        return 1
    }
}

gate() {
    CARGO_TARGET_DIR="$target_dir" CARGO_INCREMENTAL=0 \
        cargo test --locked --offline -p wamn-control-provision \
        --test control_portable_store -- --nocapture --test-threads=1
}

mutate() {
    local from=$1
    local to=$2
    perl -0pi -e "s/\\Q$from\\E/$to/" "$ddl"
}

run_mutant() {
    local name=$1
    local from=$2
    local to=$3
    cp "$baseline" "$ddl"
    local before_sha
    before_sha=$(sha "$ddl")
    mutate "$from" "$to"
    [[ $(sha "$ddl") != "$before_sha" ]] || {
        echo "mutation anchor did not change DDL: $name" >&2
        return 2
    }
    if gate >"$log" 2>&1; then
        echo "SURVIVED: $name" >&2
        cat "$log" >&2
        return 1
    fi
    echo "KILLED: $name"
}

mode=${1:-}
case "$mode" in
    check)
        assert_baseline
        echo "control portable-store mutation anchors check clean"
        ;;
    green)
        assert_baseline
        gate
        ;;
    run-all)
        : "${WAMN_CONTROL_PORTABLE_PG_URL:?set the isolated PostgreSQL 18 URL}"
        assert_baseline
        baseline=$(mktemp "${TMPDIR:-/tmp}/control-portable-baseline.XXXXXX")
        log=$(mktemp "${TMPDIR:-/tmp}/control-portable-mutant.XXXXXX")
        cp "$ddl" "$baseline"
        cleanup() {
            cp "$baseline" "$ddl"
            rm -f "$baseline" "$log"
        }
        trap cleanup EXIT
        run_mutant map-hash-bypass \
            "sha256(tested_resolution_map_bytes)" \
            "sha256(convert_to('{}', 'UTF8'))"
        run_mutant byte-retry-bypass \
            "tested_resolution_map_bytes = p_tested_resolution_map_bytes" \
            "tested_resolution_map_bytes IS NOT NULL"
        run_mutant report-fk-removal \
            "REFERENCES wamn_run.authoring_test_reports (tenant_id, report_id)" \
            "REFERENCES wamn_run.authoring_test_run_reservations (tenant_id, report_id)"
        run_mutant production-grant \
            "REVOKE ALL ON ALL TABLES IN SCHEMA catalog, wamn_run FROM PUBLIC;" \
            "GRANT SELECT ON catalog.release_flow_test_evidence TO PUBLIC;"
        run_mutant collation-stability-bypass \
            'pg_get_constraintdef(con.oid, false)) COLLATE "C"' \
            'pg_get_constraintdef(con.oid, false))'
        # The attestation constraint set orders identically under C and
        # en_US.UTF-8, so dropping its COLLATE "C" cannot be killed; reversing
        # the ORDER BY that COLLATE hardens can (wamn-0h0g.15.32).
        run_mutant attestation-order-stability-bypass \
            'pg_get_constraintdef(con.oid, true)) COLLATE "C"' \
            'pg_get_constraintdef(con.oid, true)) COLLATE "C" DESC'
        run_mutant retained-drift-acceptance \
            "b08e42cb2130ae46ebdcbb7c030f566ce37c29a287a5fdcae2ad6fb30cc82d29" \
            "0000000000000000000000000000000000000000000000000000000000000000"
        cp "$baseline" "$ddl"
        assert_baseline
        echo "control portable-store mutation campaign: 7/7 killed and baseline restored"
        ;;
    *)
        echo "usage: $0 {check|green|run-all}" >&2
        exit 2
        ;;
esac
