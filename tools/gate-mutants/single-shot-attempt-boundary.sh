#!/usr/bin/env bash
set -euo pipefail

readonly CAMPAIGN="single-shot-attempt-boundary"
readonly BEAD="wamn-0h0g.4.3"
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
    restore-recovery-literal \
    restore-predecessor-lineage \
    restore-current-attempt-pointer \
    allow-second-attempt-per-occurrence \
    rewrite-existing-attempt-history \
    accept-active-mutable-only-attempt \
    drop-effect-ledger-immutability \
    drop-cdc-source-lineage \
    drop-cdc-root-depth-immutability
}

load_mutation() {
  local id="$1"
  case "$id" in
    restore-recovery-literal)
      TARGET="deploy/sql/run-state.sql"
      EXPECTED_SHA="5ed55948b298ef52a127d1d2a4f7bd1144254564f06ae2a7b12a1614346712b6"
      NEEDLE='    attempt_key text,
    created_at'
      REPLACEMENT='    attempt_key text,
    recovery_class text,
    created_at'
      GATE="transitions::tests::effect_attempt_schema_has_one_identity_and_no_successor_shape"
      ;;
    restore-predecessor-lineage)
      TARGET="deploy/sql/run-state.sql"
      EXPECTED_SHA="5ed55948b298ef52a127d1d2a4f7bd1144254564f06ae2a7b12a1614346712b6"
      NEEDLE='    attempt_key text,
    created_at'
      REPLACEMENT='    attempt_key text,
    predecessor_attempt_id uuid,
    created_at'
      GATE="transitions::tests::effect_attempt_schema_has_one_identity_and_no_successor_shape"
      ;;
    restore-current-attempt-pointer)
      TARGET="deploy/sql/run-state.sql"
      EXPECTED_SHA="5ed55948b298ef52a127d1d2a4f7bd1144254564f06ae2a7b12a1614346712b6"
      NEEDLE='CREATE TABLE wamn_run.node_runs (
    tenant_id'
      REPLACEMENT='CREATE TABLE wamn_run.node_runs (
    current_effect_attempt_id uuid,
    tenant_id'
      GATE="transitions::tests::effect_attempt_schema_has_one_identity_and_no_successor_shape"
      ;;
    allow-second-attempt-per-occurrence)
      TARGET="deploy/sql/run-state.sql"
      EXPECTED_SHA="5ed55948b298ef52a127d1d2a4f7bd1144254564f06ae2a7b12a1614346712b6"
      NEEDLE='        UNIQUE (tenant_id, run_id, node_id, occurrence),'
      REPLACEMENT='        UNIQUE (tenant_id, run_id, node_id, occurrence, attempt_id),'
      GATE="transitions::tests::effect_attempt_schema_has_one_identity_and_no_successor_shape"
      ;;
    rewrite-existing-attempt-history)
      TARGET="crates/schema/control/src/run_plane.rs"
      EXPECTED_SHA="8c02af3a0a99023f46b1aa31210fcf72b09e4e2407b8443e41d92b47875243cd"
      NEEDLE='LOCK TABLE {schema}.effect_attempts IN SHARE ROW EXCLUSIVE MODE;'
      REPLACEMENT='UPDATE {schema}.effect_attempts SET attempt_key = attempt_key;
LOCK TABLE {schema}.effect_attempts IN SHARE ROW EXCLUSIVE MODE;'
      GATE="run_plane::tests::upgraded_attempt_rows_are_preserved_but_retired"
      ;;
    accept-active-mutable-only-attempt)
      TARGET="crates/schema/control/src/run_plane.rs"
      EXPECTED_SHA="8c02af3a0a99023f46b1aa31210fcf72b09e4e2407b8443e41d92b47875243cd"
      NEEDLE="WHERE n.status = 'started'"
      REPLACEMENT="WHERE n.status = 'completed'"
      GATE="run_plane::tests::unsafe_legacy_attempt_upgrade_refuses"
      ;;
    drop-effect-ledger-immutability)
      TARGET="deploy/sql/run-state.sql"
      EXPECTED_SHA="5ed55948b298ef52a127d1d2a4f7bd1144254564f06ae2a7b12a1614346712b6"
      NEEDLE='CREATE TRIGGER effect_attempts_update_immutable'
      REPLACEMENT='CREATE TRIGGER effect_attempts_update_mutable'
      GATE="transitions::tests::effect_ledger_and_cdc_lineage_remain_immutable"
      ;;
    drop-cdc-source-lineage)
      TARGET="deploy/sql/run-state.sql"
      EXPECTED_SHA="5ed55948b298ef52a127d1d2a4f7bd1144254564f06ae2a7b12a1614346712b6"
      NEEDLE="event_source_run_id IS NOT NULL AND event_source_run_id <> ''"
      REPLACEMENT="event_source_run_id IS NULL"
      GATE="transitions::tests::effect_ledger_and_cdc_lineage_remain_immutable"
      ;;
    drop-cdc-root-depth-immutability)
      TARGET="deploy/sql/run-state.sql"
      EXPECTED_SHA="5ed55948b298ef52a127d1d2a4f7bd1144254564f06ae2a7b12a1614346712b6"
      NEEDLE='CREATE TRIGGER runs_event_lineage_immutable
BEFORE UPDATE OF event_source_run_id, event_root_run_id, event_depth'
      REPLACEMENT='CREATE TRIGGER runs_event_lineage_immutable
BEFORE UPDATE OF event_source_run_id'
      GATE="transitions::tests::effect_ledger_and_cdc_lineage_remain_immutable"
      ;;
    *)
      echo "unknown mutant: $id" >&2
      return 2
      ;;
  esac
  TEST_ARGV=(cargo test --locked -p wamn-run-state "$GATE" -- --exact)
  if [[ "$TARGET" == "crates/schema/control/src/run_plane.rs" ]]; then
    TEST_ARGV=(cargo test --locked -p wamn-schema-control "$GATE" -- --exact)
  fi
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
  assert_precondition
  echo "GREEN campaign=$CAMPAIGN bead=$BEAD profile=$EXPECTED_PROFILE id=$id gate=$GATE target=$TARGET command=${TEST_ARGV[*]}"
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

  echo "MUTANT campaign=$CAMPAIGN id=$id gate=$GATE target=$TARGET baseline_sha256=$EXPECTED_SHA mutant_sha256=$mutant_sha command=${TEST_ARGV[*]}"
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
