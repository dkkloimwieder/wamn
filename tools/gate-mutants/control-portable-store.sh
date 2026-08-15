#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-0h0g.9.9"
readonly OUTCOME="control portable storage preserves exact bytes, local integrity, owner-only ACL, and replay drift refusal"

repository=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
ddl="$repository/deploy/sql/control-portable-store.sql"
test_file="$repository/crates/control/provision/tests/control_portable_store.rs"
target_dir=${CARGO_TARGET_DIR:-/home/kaalin/dev/wamn/target/plane-wave8-9-9}
expected_ddl_sha=9e44be7b03737163727d2a3a34680ced15c3268f3787b5a338a96d668fcf8278
expected_test_sha=26a9bad642511788638fcbefd691e999aa3a743d581f6c69e03ea1de1b07fabc

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
        run_mutant retained-drift-acceptance \
            "fb752b794bed00e6180f3b621349fb0257bf099b0e1c740d3e0a3c12993a9edb" \
            "0000000000000000000000000000000000000000000000000000000000000000"
        cp "$baseline" "$ddl"
        assert_baseline
        echo "control portable-store mutation campaign: 5/5 killed and baseline restored"
        ;;
    *)
        echo "usage: $0 {check|green|run-all}" >&2
        exit 2
        ;;
esac
