#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-0h0g.4.6"
readonly OUTCOME="one locked, idempotent project-admin transaction terminalizes effect uncertainty and preserves immutable evidence"
readonly CAMPAIGN="operator-terminalize"
readonly BEAD="wamn-0h0g.4.6"
readonly EXPECTED_PROFILE="debug"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"
: "${CARGO_TARGET_DIR:?CARGO_TARGET_DIR must name the serialized debug target directory}"

declare TARGET EXPECTED_SHA NEEDLE REPLACEMENT GATE EXPECTED_COUNT
declare -a TEST_ARGV

mutation_ids() {
  printf '%s\n' \
    run-lock-removed \
    multiple-started-nodes-accepted \
    divergent-retry-accepted \
    populated-history-dropped \
    operator-action-mutable
}

load_mutation() {
  case "$1" in
    run-lock-removed)
      TARGET="crates/execution/run-state/src/operator_action.rs"
      EXPECTED_SHA="c5be941d4eb785d62dd00bebcb7debed469b9d6afc70fbf6e826cdf1f1906984"
      NEEDLE="SELECT status, fail_kind FROM runs \\
      WHERE tenant_id = \$1 AND run_id = \$2 FOR UPDATE"
      REPLACEMENT="SELECT status, fail_kind FROM runs \\
      WHERE tenant_id = \$1 AND run_id = \$2"
      EXPECTED_COUNT=1
      GATE="operator_action::tests::transaction_statements_pin_load_bearing_order_and_guards"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --lib "$GATE" -- --exact)
      ;;
    multiple-started-nodes-accepted)
      TARGET="services/ctl/src/terminalize_effect_uncertain.rs"
      EXPECTED_SHA="979d25cbe8481523a71159bd02e43a6a5aa598f175424d6e281e256460df050f"
      NEEDLE="if candidates.len() > 1 {"
      REPLACEMENT="if false {"
      EXPECTED_COUNT=1
      GATE="terminalize_effect_uncertain_is_atomic_exact_and_authority_closed_live"
      TEST_ARGV=(cargo test --locked --offline -p wamn-ctl --test terminalize_effect_uncertain_live "$GATE" -- --exact --nocapture --test-threads=1)
      ;;
    divergent-retry-accepted)
      TARGET="services/ctl/src/terminalize_effect_uncertain.rs"
      EXPECTED_SHA="979d25cbe8481523a71159bd02e43a6a5aa598f175424d6e281e256460df050f"
      NEEDLE="prior_actions.len() == 1 && prior_actions[0].is_identical(request, principal)"
      REPLACEMENT="prior_actions.len() == 1"
      EXPECTED_COUNT=1
      GATE="terminalize_effect_uncertain_is_atomic_exact_and_authority_closed_live"
      TEST_ARGV=(cargo test --locked --offline -p wamn-ctl --test terminalize_effect_uncertain_live "$GATE" -- --exact --nocapture --test-threads=1)
      ;;
    populated-history-dropped)
      TARGET="crates/schema/control/src/run_plane.rs"
      EXPECTED_SHA="a7e47b5eec411485e014ea06e25d7b4fad1d2d5ba546e59428d1ba2d1bbc2acb"
      NEEDLE="let preflight = if populated.is_empty() {"
      REPLACEMENT="let preflight = if true || populated.is_empty() {"
      EXPECTED_COUNT=1
      GATE="retired_effect_disposition_cutover_live"
      TEST_ARGV=(cargo test --locked --offline -p wamn-ctl --test run_plane_live "$GATE" -- --exact --nocapture --test-threads=1)
      ;;
    operator-action-mutable)
      TARGET="crates/schema/control/src/run_plane.rs"
      EXPECTED_SHA="a7e47b5eec411485e014ea06e25d7b4fad1d2d5ba546e59428d1ba2d1bbc2acb"
      NEEDLE="RAISE EXCEPTION USING
        ERRCODE = '55000',
        MESSAGE = 'operator-run-action-immutable';"
      REPLACEMENT="RETURN OLD;"
      EXPECTED_COUNT=1
      GATE="terminalize_effect_uncertain_is_atomic_exact_and_authority_closed_live"
      TEST_ARGV=(cargo test --locked --offline -p wamn-ctl --test terminalize_effect_uncertain_live "$GATE" -- --exact --nocapture --test-threads=1)
      ;;
    *) echo "unknown mutant: $1" >&2; return 2 ;;
  esac
}

sha256() { sha256sum "$1" | cut -d ' ' -f 1; }

assert_precondition() {
  local actual count
  actual="$(sha256 "$TARGET")"
  [[ "$actual" == "$EXPECTED_SHA" ]] || {
    echo "$TARGET hash mismatch: expected $EXPECTED_SHA, got $actual" >&2
    return 2
  }
  count="$(TARGET="$TARGET" NEEDLE="$NEEDLE" perl -0ne '$n += s/\Q$ENV{NEEDLE}\E/$&/g; END { print $n }' "$TARGET")"
  [[ "$count" == "$EXPECTED_COUNT" ]] || {
    echo "$TARGET mutation anchor count: expected $EXPECTED_COUNT, got $count" >&2
    return 2
  }
}

replace_once() {
  TARGET="$TARGET" NEEDLE="$NEEDLE" REPLACEMENT="$REPLACEMENT" perl -0pi -e \
    'BEGIN {$done=0} $done ||= s/\Q$ENV{NEEDLE}\E/$ENV{REPLACEMENT}/' "$TARGET"
}

run_gate() {
  if [[ "$GATE" == "retired_effect_disposition_cutover_live" ]]; then
    : "${WAMN_CTL_PG_URL:?set WAMN_CTL_PG_URL to disposable PostgreSQL 18}"
  elif [[ "$GATE" == "terminalize_effect_uncertain_is_atomic_exact_and_authority_closed_live" ]]; then
    : "${WAMN_OPERATOR_TERMINALIZE_PG18_URL:?set WAMN_OPERATOR_TERMINALIZE_PG18_URL to disposable PostgreSQL 18}"
  fi
  "${TEST_ARGV[@]}"
}

run_green() {
  load_mutation "$1"
  assert_precondition
  echo "GREEN id=$1 gate=$GATE target=$TARGET command=${TEST_ARGV[*]}"
  run_gate
}

run_mutant() (
  load_mutation "$1"
  assert_precondition
  local backup_dir backup mutant_sha restored_sha exit_code
  backup_dir="$(mktemp -d)"
  backup="$backup_dir/original"
  cp "$TARGET" "$backup"
  restore() {
    cp "$backup" "$TARGET"
    restored_sha="$(sha256 "$TARGET")"
    rm -f "$backup"
    rmdir "$backup_dir"
    [[ "$restored_sha" == "$EXPECTED_SHA" ]] || exit 3
  }
  trap restore EXIT INT TERM
  replace_once
  mutant_sha="$(sha256 "$TARGET")"
  [[ "$mutant_sha" != "$EXPECTED_SHA" ]] || exit 3
  echo "MUTANT id=$1 gate=$GATE target=$TARGET baseline_sha256=$EXPECTED_SHA mutant_sha256=$mutant_sha command=${TEST_ARGV[*]}"
  set +e
  run_gate
  exit_code=$?
  set -e
  [[ $exit_code -ne 0 ]] || { echo "SURVIVED id=$1 gate=$GATE" >&2; exit 1; }
  echo "KILLED id=$1 gate=$GATE exit_code=$exit_code"
)

check_campaign() {
  while IFS= read -r id; do
    load_mutation "$id"
    assert_precondition
    echo "CHECKED id=$id gate=$GATE target=$TARGET sha256=$EXPECTED_SHA"
  done < <(mutation_ids)
}

usage() { echo "usage: $0 list | check | green MUTANT | green-all | run MUTANT | run-all" >&2; }

case "${1:-}" in
  list) mutation_ids ;;
  check) check_campaign ;;
  green) [[ $# -eq 2 ]] || { usage; exit 2; }; run_green "$2" ;;
  green-all) while IFS= read -r id; do run_green "$id"; done < <(mutation_ids) ;;
  run) [[ $# -eq 2 ]] || { usage; exit 2; }; run_mutant "$2" ;;
  run-all) while IFS= read -r id; do run_mutant "$id"; done < <(mutation_ids) ;;
  *) usage; exit 2 ;;
esac
