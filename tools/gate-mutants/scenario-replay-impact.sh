#!/usr/bin/env bash
set -euo pipefail

readonly CAMPAIGN="scenario-replay-impact"
readonly BEAD="wamn-2jdm.5.3"
readonly EXPECTED_PROFILE="debug"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name a unique debug target directory" >&2
  exit 2
fi

declare TARGET EXPECTED_SHA NEEDLE REPLACEMENT GATE
declare -a TEST_ARGV

mutation_ids() {
  printf '%s\n' \
    assertion-exact-egress \
    testkit-aggregate-fold \
    suiteexec-case-isolation \
    suiteexec-selector-scope \
    suiteproof-forced-rls \
    suiteproof-version-cascade \
    pinproof-secret-scrub \
    pinproof-replay-normalization \
    impact-name-traversal \
    impact-suite-traversal \
    pocsuite-aggregate-fold
}

load_mutation() {
  local id="$1"
  case "$id" in
    assertion-exact-egress)
      TARGET="crates/scenarios/model/src/evaluate.rs"
      EXPECTED_SHA="6129c8c913bd847f8f56af77890c9305c25e4a287bd2ed06f28e1b57779c9466"
      NEEDLE="let ok = unexpected == 0 && unused == 0 && len_ok;"
      REPLACEMENT="let ok = unused == 0;"
      GATE="evaluate::tests::exactly_these_catches_an_extra_call"
      TEST_ARGV=(cargo test --locked -p wamn-scenario-model --lib "$GATE" -- --exact)
      ;;
    testkit-aggregate-fold)
      TARGET="tests/integration/src/testkitbench.rs"
      EXPECTED_SHA="f8c541e02ec19f627275c9cf09fd5f0e41b38f2d9b9d1d1d204cf7708390eb7c"
      NEEDLE="check(ok, &label, r.passed);"
      REPLACEMENT="check(ok, &label, true);"
      GATE="testkitbench::tests::aggregate_fold_turns_red_when_any_stored_assertion_fails"
      TEST_ARGV=(cargo test --locked -p wamn-proof-integration --lib "$GATE" -- --exact)
      ;;
    suiteexec-case-isolation)
      TARGET="services/scenario-worker/src/lib.rs"
      EXPECTED_SHA="dbc9c1ca3828c1e932ec865c57961d2d6aa9cdb30b3a26d524408adb3d4b151c"
      NEEDLE='let schema = template.replace("{ordinal}", &ordinal.to_string());'
      REPLACEMENT='let schema = template.replace("{ordinal}", "0");'
      GATE="tests::execution_schema_template_is_explicit_and_case_isolated"
      TEST_ARGV=(cargo test --locked -p wamn-scenario-worker --lib "$GATE" -- --exact)
      ;;
    suiteexec-selector-scope)
      TARGET="crates/scenarios/catalog/src/sql.rs"
      EXPECTED_SHA="6e429e034f59aa593208618b241529796a2369c8add94364409331252a507ecc"
      NEEDLE='WHERE tenant_id = $1 AND flow_id = $2 AND flow_version = $3 AND suite_id = $4 \'
      REPLACEMENT='WHERE tenant_id = $1 AND flow_id = $2 AND flow_version = $3 \'
      GATE="sql::tests::cases_predicate_is_scoped_by_all_four_keys"
      TEST_ARGV=(cargo test --locked -p wamn-scenario-catalog --lib "$GATE" -- --exact)
      ;;
    suiteproof-forced-rls)
      TARGET="deploy/sql/flow-tests.sql"
      EXPECTED_SHA="4a4c8e4ff49f777fa46610fb210cc8deb32615a4e9399cfc9fa2054fcf84598b"
      NEEDLE="ALTER TABLE wamn_run.test_suites FORCE ROW LEVEL SECURITY;"
      REPLACEMENT="ALTER TABLE wamn_run.test_suites ENABLE ROW LEVEL SECURITY;"
      GATE="suiteproof::tests::canonical_suite_ddl_enforces_tenant_and_version_lifetime"
      TEST_ARGV=(cargo test --locked -p wamn-proof-integration --lib "$GATE" -- --exact)
      ;;
    suiteproof-version-cascade)
      TARGET="deploy/sql/flow-tests.sql"
      EXPECTED_SHA="4a4c8e4ff49f777fa46610fb210cc8deb32615a4e9399cfc9fa2054fcf84598b"
      NEEDLE="REFERENCES wamn_run.flows (tenant_id, flow_id, version) ON DELETE CASCADE"
      REPLACEMENT="REFERENCES wamn_run.flows (tenant_id, flow_id, version) ON DELETE RESTRICT"
      GATE="suiteproof::tests::canonical_suite_ddl_enforces_tenant_and_version_lifetime"
      TEST_ARGV=(cargo test --locked -p wamn-proof-integration --lib "$GATE" -- --exact)
      ;;
    pinproof-secret-scrub)
      TARGET="crates/scenarios/catalog/src/pin.rs"
      EXPECTED_SHA="0cabc5cb8836e37a96cf1f1398823b03ce72877b668dce8d80e2349eb4a3ce99"
      NEEDLE="scrub(&mut output);"
      REPLACEMENT="let _ = &mut output;"
      GATE="pin::tests::pin_full_run_scrubs_secrets"
      TEST_ARGV=(cargo test --locked -p wamn-scenario-catalog --lib "$GATE" -- --exact)
      ;;
    pinproof-replay-normalization)
      TARGET="crates/scenarios/catalog/src/pin.rs"
      EXPECTED_SHA="0cabc5cb8836e37a96cf1f1398823b03ce72877b668dce8d80e2349eb4a3ce99"
      NEEDLE="canonicalize: true,"
      REPLACEMENT="canonicalize: false,"
      GATE="pin::tests::replay_round_trip_tolerates_volatile_but_rejects_real"
      TEST_ARGV=(cargo test --locked -p wamn-scenario-catalog --lib "$GATE" -- --exact)
      ;;
    impact-name-traversal)
      TARGET="crates/schema/control/src/impact/mod.rs"
      EXPECTED_SHA="05aa2dde01126ca9820f090f45a30b8019bc9aa6b9407cae4f4eddc45b7ae7f4"
      NEEDLE="&& names.contains(name)"
      REPLACEMENT="&& false"
      GATE="impact::tests::node_config_edge_keys_on_entity_name_not_id"
      TEST_ARGV=(cargo test --locked -p wamn-schema-control --lib "$GATE" -- --exact)
      ;;
    impact-suite-traversal)
      TARGET="crates/schema/control/src/impact/mod.rs"
      EXPECTED_SHA="05aa2dde01126ca9820f090f45a30b8019bc9aa6b9407cae4f4eddc45b7ae7f4"
      NEEDLE='.filter(|s| affected_flows.contains(&(s.tenant.as_str(), s.flow_id.as_str())))'
      REPLACEMENT='.filter(|_| false)'
      GATE="impact::tests::suites_of_affected_flows_are_enumerated_across_versions"
      TEST_ARGV=(cargo test --locked -p wamn-schema-control --lib "$GATE" -- --exact)
      ;;
    pocsuite-aggregate-fold)
      TARGET="tests/integration/src/pocsuiteproof.rs"
      EXPECTED_SHA="a0a1db7fbe924f4dd8f4891c4ed904b4db9bac785ed60f718d5010a135b7e0e2"
      NEEDLE="check(ok, &label, r.passed);"
      REPLACEMENT="check(ok, &label, true);"
      GATE="pocsuiteproof::tests::aggregate_fold_turns_red_when_a_real_poc_assertion_fails"
      TEST_ARGV=(cargo test --locked -p wamn-proof-integration --lib "$GATE" -- --exact)
      ;;
    *)
      echo "unknown mutant: $id" >&2
      return 2
      ;;
  esac
}

sha256() {
  sha256sum "$1" | cut -d ' ' -f 1
}

assert_clean_target() {
  git diff --quiet -- "$TARGET" || {
    echo "$TARGET has unstaged changes" >&2
    return 2
  }
  git diff --cached --quiet -- "$TARGET" || {
    echo "$TARGET has staged changes" >&2
    return 2
  }
}

assert_precondition() {
  local actual
  actual="$(sha256 "$TARGET")"
  if [[ "$actual" != "$EXPECTED_SHA" ]]; then
    echo "$TARGET hash mismatch: expected $EXPECTED_SHA, got $actual" >&2
    return 2
  fi
  TARGET="$TARGET" NEEDLE="$NEEDLE" python3 -c \
    'import os, pathlib, sys; data=pathlib.Path(os.environ["TARGET"]).read_text(); count=data.count(os.environ["NEEDLE"]); sys.exit(0 if count == 1 else 1)' || {
      echo "$TARGET must contain the mutation anchor exactly once" >&2
      return 2
    }
}

replace_once() {
  TARGET="$TARGET" NEEDLE="$NEEDLE" REPLACEMENT="$REPLACEMENT" python3 -c \
    'import os, pathlib; path=pathlib.Path(os.environ["TARGET"]); data=path.read_text(); path.write_text(data.replace(os.environ["NEEDLE"], os.environ["REPLACEMENT"], 1))'
}

run_gate() {
  "${TEST_ARGV[@]}"
}

run_green() {
  local id="$1"
  load_mutation "$id"
  assert_clean_target
  assert_precondition
  echo "GREEN id=$id gate=$GATE target=$TARGET command=${TEST_ARGV[*]}"
  run_gate
}

run_mutant() (
  local id="$1"
  local backup_dir backup restored_sha mutant_sha exit_code
  load_mutation "$id"
  assert_clean_target
  assert_precondition

  backup_dir="$(mktemp -d)"
  backup="$backup_dir/original"
  cp "$TARGET" "$backup"
  restore() {
    cp "$backup" "$TARGET"
    restored_sha="$(sha256 "$TARGET")"
    rm -f "$backup"
    rmdir "$backup_dir"
    if [[ "$restored_sha" != "$EXPECTED_SHA" ]]; then
      echo "restore failed for $TARGET: expected $EXPECTED_SHA, got $restored_sha" >&2
      exit 3
    fi
  }
  trap restore EXIT INT TERM

  replace_once
  mutant_sha="$(sha256 "$TARGET")"
  if [[ "$mutant_sha" == "$EXPECTED_SHA" ]]; then
    echo "mutation $id did not change $TARGET" >&2
    exit 3
  fi

  echo "MUTANT id=$id gate=$GATE target=$TARGET baseline_sha256=$EXPECTED_SHA mutant_sha256=$mutant_sha command=${TEST_ARGV[*]}"
  set +e
  run_gate
  exit_code=$?
  set -e
  if [[ $exit_code -eq 0 ]]; then
    echo "SURVIVED id=$id gate=$GATE" >&2
    exit 1
  fi
  echo "KILLED id=$id gate=$GATE exit_code=$exit_code"
)

check_campaign() {
  local id
  while IFS= read -r id; do
    load_mutation "$id"
    assert_clean_target
    assert_precondition
    if [[ "${TEST_ARGV[0]}" != "cargo" || "${TEST_ARGV[1]}" != "test" ]]; then
      echo "$id does not use a fixed cargo test command" >&2
      return 2
    fi
    printf 'CHECKED id=%s gate=%s target=%s sha256=%s\n' \
      "$id" "$GATE" "$TARGET" "$EXPECTED_SHA"
  done < <(mutation_ids)
}

usage() {
  echo "usage: $0 list | check | green MUTANT | green-all | run MUTANT | run-all" >&2
}

case "${1:-}" in
  list)
    mutation_ids
    ;;
  check)
    check_campaign
    ;;
  green)
    [[ $# -eq 2 ]] || { usage; exit 2; }
    run_green "$2"
    ;;
  green-all)
    [[ $# -eq 1 ]] || { usage; exit 2; }
    while IFS= read -r id; do
      run_green "$id"
    done < <(mutation_ids)
    ;;
  run)
    [[ $# -eq 2 ]] || { usage; exit 2; }
    run_mutant "$2"
    ;;
  run-all)
    [[ $# -eq 1 ]] || { usage; exit 2; }
    while IFS= read -r id; do
      run_mutant "$id"
    done < <(mutation_ids)
    ;;
  *)
    usage
    exit 2
    ;;
esac
