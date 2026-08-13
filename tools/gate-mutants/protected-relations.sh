#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-0h0g.13.33"
readonly OUTCOME="the generated protected-relation table matches live catalog authority"

repository=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
table="$repository/architecture/protected-writes.json"
live_test="$repository/services/ctl/tests/protected_relations_live.rs"
static_test="$repository/tests/conformance/tests/protected_relations.rs"
expected_table_sha=5603f40e12455e40a325ff44f9a1f46eee3ecde4b6813c3c8ecaaaae5856e4a7
expected_live_sha=57a114672b628d0f99bbd664e71f324f69ef7e5f349b5090e71cab81a951a01c
expected_static_sha=5813f727402cd68698e97d14d28752b3797ffaded27fe49804fbe17931a97219
target_dir=${CARGO_TARGET_DIR:-/tmp/wamn-target-0h0g-13-33}

sha() {
    sha256sum "$1" | cut -d' ' -f1
}

assert_baseline() {
    [[ $(sha "$table") == "$expected_table_sha" ]] || {
        echo "protected relation table SHA drifted" >&2
        return 1
    }
    [[ $(sha "$live_test") == "$expected_live_sha" ]] || {
        echo "protected relation live test SHA drifted" >&2
        return 1
    }
    [[ $(sha "$static_test") == "$expected_static_sha" ]] || {
        echo "protected relation static test SHA drifted" >&2
        return 1
    }
}

static_gate() {
    CARGO_TARGET_DIR="$target_dir" CARGO_INCREMENTAL=0 \
        cargo test --locked --offline -p wamn-proof-conformance \
        --test protected_relations -- --nocapture
}

live_gate() {
    : "${WAMN_CTL_PG_URL:?set WAMN_CTL_PG_URL to a disposable PostgreSQL 18 database}"
    CARGO_TARGET_DIR="$target_dir" CARGO_INCREMENTAL=0 \
        cargo test --locked --offline -p wamn-ctl \
        --test protected_relations_live -- --nocapture --test-threads=1
}

mutate() {
    local filter=$1
    local temporary
    temporary=$(mktemp "${TMPDIR:-/tmp}/protected-relations.XXXXXX")
    jq "$filter" "$table" >"$temporary"
    chmod --reference="$table" "$temporary"
    mv "$temporary" "$table"
}

run_mutant() {
    local name=$1
    local filter=$2
    local gate=$3
    cp "$baseline" "$table"
    mutate "$filter"
    if "$gate" >"$log" 2>&1; then
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
        jq empty "$table"
        echo "protected relation mutation anchors check clean"
        ;;
    green-all)
        assert_baseline
        static_gate
        live_gate
        ;;
    run-all)
        assert_baseline
        baseline=$(mktemp "${TMPDIR:-/tmp}/protected-relations-baseline.XXXXXX")
        log=$(mktemp "${TMPDIR:-/tmp}/protected-relations-log.XXXXXX")
        cp "$table" "$baseline"
        cleanup() {
            cp "$baseline" "$table"
            rm -f "$baseline" "$log"
        }
        trap cleanup EXIT
        run_mutant missing-relation \
            'del(.rows[] | select(.relation == "wamn_run.runs"))' static_gate
        run_mutant installer-drift \
            '(.rows[] | select(.relation == "wamn_run.runs") | .installer) = "wrong-installer"' static_gate
        run_mutant owner-drift \
            '(.rows[] | select(.relation == "wamn_run.runs") | .owner) = "wrong-owner"' static_gate
        run_mutant author-exposure-drift \
            '(.rows[] | select(.relation == "wamn_run.runs") | ."author-reachable") = "no"' static_gate
        run_mutant missing-effect-writer-role \
            'del(.rows[] | select(.relation == "wamn_run.effect_attempt_dispatches") | .roles[] | select(.role == "wamn_effect_writer"))' live_gate
        run_mutant missing-cascade \
            'del(.rows[] | select(.relation == "wamn_run.run_queue") | .mechanisms[] | select(startswith("foreign-key:delete:cascade")))' live_gate
        run_mutant missing-immutability-guard \
            'del(.rows[] | select(.relation == "wamn_run.effect_attempt_dispatches") | .guards[] | select(startswith("trigger:effect_attempt_dispatches_update_immutable")))' live_gate
        cp "$baseline" "$table"
        assert_baseline
        echo "protected relation mutation campaign: 7/7 killed"
        ;;
    *)
        echo "usage: $0 {check|green-all|run-all}" >&2
        exit 2
        ;;
esac
