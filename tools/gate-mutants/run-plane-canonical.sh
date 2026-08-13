#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-l5i9.73"
readonly OUTCOME="run-plane drift plans and applies the canonical helpers, checks, and actions"

readonly CAMPAIGN="run-plane-canonical"
readonly BEAD="wamn-l5i9.73"
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
    check-catalog-is-not-planned \
    missing-helper-is-accepted \
    effect-shell-does-not-apply
}

load_mutation() {
  local id="$1"
  case "$id" in
    check-catalog-is-not-planned)
      TARGET="crates/schema/control/src/run_plane.rs"
      EXPECTED_SHA="3f8c9af4d11b1692060b30c480bb92e3bdeb7e13fdf2debc782c5d158bd10280"
      NEEDLE='for spec in CHECK_SPECS {
        if spec.table == "runs" && spec.name == "runs_check" {'
      REPLACEMENT='for spec in &[] as &[CheckSpec] {
        if spec.table == "runs" && spec.name == "runs_check" {'
      GATE="run_plane::tests::drifted_and_missing_checks_plan_exact_repairs"
      TEST_ARGV=(cargo test --locked -p wamn-schema-control "$GATE" -- --exact)
      ;;
    missing-helper-is-accepted)
      TARGET="crates/schema/control/src/run_plane.rs"
      EXPECTED_SHA="3f8c9af4d11b1692060b30c480bb92e3bdeb7e13fdf2debc782c5d158bd10280"
      NEEDLE='.helper_functions
            .get(spec.name)
            .is_none_or(|definition| {
                normalize_observed_schema(definition, schema) != spec.definition.as_ref()
            })'
      REPLACEMENT='.helper_functions
            .get(spec.name)
            .is_some_and(|definition| {
                normalize_observed_schema(definition, schema) != spec.definition.as_ref()
            })'
      GATE="run_plane::tests::missing_helpers_and_trigger_are_repaired_for_present_runs"
      TEST_ARGV=(cargo test --locked -p wamn-schema-control "$GATE" -- --exact)
      ;;
    effect-shell-does-not-apply)
      TARGET="services/ctl/src/reconcile_run_plane.rs"
      EXPECTED_SHA="1d830da8fb767ff12f4c3fbb1228690c70ea98daf9169f4ca6ea33e8efa7de22"
      NEEDLE='for action in &plan.actions[applied..] {
            client
                .batch_execute(&action.sql)'
      REPLACEMENT='for action in &plan.actions[applied..] {
            client
                .batch_execute("SELECT 1")'
      GATE="run_plane_reconcile_live"
      TEST_ARGV=(cargo test --locked -p wamn-ctl --test run_plane_live "$GATE" -- --exact --nocapture)
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
