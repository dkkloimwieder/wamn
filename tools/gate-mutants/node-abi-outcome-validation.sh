#!/usr/bin/env bash
set -euo pipefail

# The three node-ABI outcome validators are the engine's fail-closed boundary
# between a node's self-reported result and the run lifecycle: a request/event
# entry may only re-emit its admitted input on `main` with no context write, and
# a `fail` node may only return the authored terminal detail. Each mutant here
# either stops `Plan::apply` from consulting one validator or weakens one clause
# inside it, and requires the named gate to fail.
#
# Successor campaign (wamn-0h0g.15.124): e05636b8 moved these validators from
# components/execution/flowrunner/src/lib.rs to the engine, and wamn-0h0g.15.122
# retired the four node-abi gate-mutants that guarded them at the old address.
# This is a fresh anchor against engine.rs, not a resurrection of those scripts —
# the crate changed with the move, so the gates run under `-p wamn-runner`.
#
# Network-free: the gates are pure reducer integration tests, no cluster, no DB.

readonly OWNER="bd:wamn-0h0g.15.124"
readonly OUTCOME="node outcomes are validated against the authored contract before any lifecycle mutation"
readonly CAMPAIGN="node-abi-outcome-validation"
readonly BEAD="wamn-0h0g.15.124"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name the serialized debug target directory" >&2
  exit 2
fi
export CARGO_INCREMENTAL=0
export CARGO_BUILD_JOBS=2

declare TARGET EXPECTED_SHA NEEDLE REPLACEMENT EXPECTED_COUNT SUITE GATE
declare -a TEST_ARGV

mutation_ids() {
  printf '%s\n' \
    request-emission-unvalidated \
    event-emission-unvalidated \
    fail-outcome-unvalidated \
    request-context-write-permitted \
    event-context-write-permitted \
    fail-message-defaults-to-empty
}

load_mutation() {
  local id="$1"
  TARGET="crates/execution/flow-engine/src/engine.rs"
  EXPECTED_SHA="70d812b3b2c92a9924cc76bc106686152b19f589f7631799e718c445572202c5"
  EXPECTED_COUNT=1
  case "$id" in
    request-emission-unvalidated)
      NEEDLE='validate_request_outcome(dispatch, &outcome)?;'
      REPLACEMENT='let _ = validate_request_outcome(dispatch, &outcome);'
      SUITE="lifecycle_node_abi_prototype"
      GATE="malformed_abi_results_cannot_authorize_engine_lifecycle_mutation"
      ;;
    event-emission-unvalidated)
      NEEDLE='validate_event_outcome(dispatch, &outcome)?;'
      REPLACEMENT='let _ = validate_event_outcome(dispatch, &outcome);'
      SUITE="lifecycle_node_abi_prototype"
      GATE="malformed_event_emission_cannot_advance_the_entry_token"
      ;;
    fail-outcome-unvalidated)
      NEEDLE='validate_fail_outcome(dispatch, &outcome)?;'
      REPLACEMENT='let _ = validate_fail_outcome(dispatch, &outcome);'
      SUITE="reserved"
      GATE="fail_refuses_non_terminal_or_mismatched_results_without_lifecycle_mutation"
      ;;
    # The two entry arms are byte-identical apart from their refusal variant, so
    # each needle carries the arm's tail to stay unique inside the file.
    request-context-write-permitted)
      NEEDLE=$'&& context.is_none() => {\n            Ok(())\n        }\n        NodeOutcome::Error(_) => Ok(()),\n        NodeOutcome::Success { .. } => Err(ApplyError::InvalidRequestEmission),'
      REPLACEMENT=$'&& (context.is_none() || context.is_some()) => {\n            Ok(())\n        }\n        NodeOutcome::Error(_) => Ok(()),\n        NodeOutcome::Success { .. } => Err(ApplyError::InvalidRequestEmission),'
      SUITE="lifecycle_node_abi_prototype"
      GATE="request_emissions_that_replace_context_or_leave_the_main_port_are_refused"
      ;;
    event-context-write-permitted)
      NEEDLE=$'&& context.is_none() => {\n            Ok(())\n        }\n        NodeOutcome::Error(_) => Ok(()),\n        NodeOutcome::Success { .. } => Err(ApplyError::InvalidEventEmission),'
      REPLACEMENT=$'&& (context.is_none() || context.is_some()) => {\n            Ok(())\n        }\n        NodeOutcome::Error(_) => Ok(()),\n        NodeOutcome::Success { .. } => Err(ApplyError::InvalidEventEmission),'
      SUITE="lifecycle_node_abi_prototype"
      GATE="event_emissions_that_replace_context_or_leave_the_main_port_are_refused"
      ;;
    fail-message-defaults-to-empty)
      NEEDLE='config.message.as_deref().unwrap_or(&config.code),'
      REPLACEMENT='config.message.as_deref().unwrap_or(""),'
      SUITE="reserved"
      GATE="respond_releases_and_continues_then_late_fail_leaves_caller_untouched"
      ;;
    *)
      echo "unknown mutant: $id" >&2
      return 2
      ;;
  esac
  TEST_ARGV=(
    cargo test --locked --offline -p wamn-runner
    --test "$SUITE" "$GATE" -- --exact
  )
}

sha256() {
  sha256sum "$1" | cut -d ' ' -f 1
}

assert_precondition() {
  local actual count
  actual="$(sha256 "$TARGET")"
  [[ "$actual" == "$EXPECTED_SHA" ]] || {
    echo "$TARGET hash mismatch: expected $EXPECTED_SHA, got $actual" >&2
    return 2
  }
  count="$(TARGET="$TARGET" NEEDLE="$NEEDLE" python3 -c \
    'import os, pathlib; print(pathlib.Path(os.environ["TARGET"]).read_text().count(os.environ["NEEDLE"]))')"
  [[ "$count" == "$EXPECTED_COUNT" ]] || {
    echo "$TARGET must contain mutation anchor $EXPECTED_COUNT time(s) (found $count)" >&2
    return 2
  }
}

replace_once() {
  TARGET="$TARGET" NEEDLE="$NEEDLE" REPLACEMENT="$REPLACEMENT" python3 -c \
    'import os, pathlib; p=pathlib.Path(os.environ["TARGET"]); s=p.read_text(); p.write_text(s.replace(os.environ["NEEDLE"], os.environ["REPLACEMENT"], 1))'
}

run_green() {
  local id="$1"
  load_mutation "$id"
  assert_precondition
  echo "GREEN campaign=$CAMPAIGN bead=$BEAD id=$id gate=$GATE target=$TARGET command=${TEST_ARGV[*]}"
  "${TEST_ARGV[@]}"
}

run_mutant() (
  local id="$1" backup_dir backup restored_sha mutant_sha exit_code
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
    [[ "$restored_sha" == "$EXPECTED_SHA" ]] || {
      echo "restore failed for $TARGET" >&2
      exit 3
    }
  }
  trap restore EXIT INT TERM
  replace_once
  mutant_sha="$(sha256 "$TARGET")"
  [[ "$mutant_sha" != "$EXPECTED_SHA" ]] || {
    echo "mutation $id did not change $TARGET" >&2
    exit 3
  }
  echo "MUTANT campaign=$CAMPAIGN bead=$BEAD id=$id gate=$GATE target=$TARGET baseline_sha256=$EXPECTED_SHA mutant_sha256=$mutant_sha command=${TEST_ARGV[*]}"
  set +e
  "${TEST_ARGV[@]}"
  exit_code=$?
  set -e
  [[ $exit_code -ne 0 ]] || {
    echo "SURVIVED id=$id gate=$GATE" >&2
    exit 1
  }
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
  list) mutation_ids ;;
  check) check_campaign ;;
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
  *) usage; exit 2 ;;
esac
