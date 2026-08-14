#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-0h0g.8.4"
readonly OUTCOME="durable test orchestration pins exact run maps and schema ownership"
readonly CAMPAIGN="test-orchestration"
readonly BEAD="wamn-0h0g.8.4"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name the serialized debug target directory" >&2
  exit 2
fi

declare TARGET EXPECTED_SHA NEEDLE REPLACEMENT GATE
declare -a TEST_ARGV

mutation_ids() {
  printf '%s\n' \
    named-node-plan-join-bypass \
    schema-control-drops-case-runs \
    from-zero-skips-post-helper-triggers
}

load_mutation() {
  local id="$1"
  case "$id" in
    named-node-plan-join-bypass)
      TARGET="services/scenario-worker/src/store/test_orchestration.rs"
      EXPECTED_SHA="2dc1ee0e0c508b433adeca3f9856982a76a7ec41ff7eea77bd827b4bed9e0f9f"
      NEEDLE='AND resolution.execution_bundle_hash = node.current_plan_hash \'
      REPLACEMENT='AND true \'
      GATE="store::test_orchestration::tests::statements_pin_normalized_idempotency_and_named_node_projection"
      TEST_ARGV=(cargo test --locked -p wamn-scenario-worker --lib "$GATE" -- --exact)
      ;;
    schema-control-drops-case-runs)
      TARGET="crates/schema/control/src/run_plane.rs"
      EXPECTED_SHA="25f00a4ba18ef08cd05d0b761378a7423801046aa3cf9f70ef772480dce54ae0"
      NEEDLE='            record_tables(AUTHORING_TESTS_SQL, "wamn_run"),
            [
                "authoring_test_sets",
                "authoring_test_run_reservations",
                "authoring_test_case_runs",
                "authoring_test_reports",'
      REPLACEMENT='            record_tables(AUTHORING_TESTS_SQL, "wamn_run"),
            [
                "authoring_test_sets",
                "authoring_test_run_reservations",
                "authoring_test_reports",'
      GATE="run_plane::tests::record_tables_are_pinned"
      TEST_ARGV=(cargo test --locked -p wamn-schema-control --lib "$GATE" -- --exact)
      ;;
    from-zero-skips-post-helper-triggers)
      TARGET="crates/schema/control/src/run_plane.rs"
      EXPECTED_SHA="25f00a4ba18ef08cd05d0b761378a7423801046aa3cf9f70ef772480dce54ae0"
      NEEDLE='        if !obs.tables.contains_key(&spec.table)
            && table_section_carries_trigger(&spec.table, &spec.name)
        {'
      REPLACEMENT='        if !obs.tables.contains_key(&spec.table) {'
      GATE="run_plane::tests::from_zero_plans_the_full_set_in_order"
      TEST_ARGV=(cargo test --locked -p wamn-schema-control --lib "$GATE" -- --exact)
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

assert_precondition() {
  local actual
  actual="$(sha256 "$TARGET")"
  if [[ "$actual" != "$EXPECTED_SHA" ]]; then
    echo "$TARGET hash mismatch: expected $EXPECTED_SHA, got $actual" >&2
    return 2
  fi
  NEEDLE="$NEEDLE" perl -0ne \
    '$count += () = /\Q$ENV{NEEDLE}\E/g; END { exit($count == 1 ? 0 : 1) }' \
    "$TARGET" || {
      echo "$TARGET must contain the mutation anchor exactly once" >&2
      return 2
    }
}

replace_once() {
  NEEDLE="$NEEDLE" REPLACEMENT="$REPLACEMENT" perl -0pi -e \
    's/\Q$ENV{NEEDLE}\E/$ENV{REPLACEMENT}/' "$TARGET"
}

run_gate() {
  "${TEST_ARGV[@]}"
}

run_green() {
  local id="$1"
  load_mutation "$id"
  assert_precondition
  echo "GREEN campaign=$CAMPAIGN bead=$BEAD id=$id gate=$GATE target=$TARGET command=${TEST_ARGV[*]}"
  run_gate
}

run_mutant() (
  local id="$1"
  local backup_dir backup restored_sha mutant_sha exit_code
  load_mutation "$id"
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

  echo "MUTANT campaign=$CAMPAIGN bead=$BEAD id=$id gate=$GATE target=$TARGET baseline_sha256=$EXPECTED_SHA mutant_sha256=$mutant_sha command=${TEST_ARGV[*]}"
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
    assert_precondition
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
    while IFS= read -r id; do run_green "$id"; done < <(mutation_ids)
    ;;
  run)
    [[ $# -eq 2 ]] || { usage; exit 2; }
    run_mutant "$2"
    ;;
  run-all)
    [[ $# -eq 1 ]] || { usage; exit 2; }
    while IFS= read -r id; do run_mutant "$id"; done < <(mutation_ids)
    ;;
  *)
    usage
    exit 2
    ;;
esac
