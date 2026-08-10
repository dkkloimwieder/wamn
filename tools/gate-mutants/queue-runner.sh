#!/usr/bin/env bash
set -euo pipefail

readonly CAMPAIGN="queue-runner"
readonly BEAD="wamn-2jdm.5.2"
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
    queue-stream-sequence-order \
    queue-blocking-dead-letter-policy \
    failover-lease-expiry-boundary \
    wakeproof-parked-only-actuation \
    capturebench-oversize-preview \
    runnerbench-dispatch-budget
}

load_mutation() {
  local id="$1"
  case "$id" in
    queue-stream-sequence-order)
      TARGET="crates/execution/run-state/src/queue/partition.rs"
      EXPECTED_SHA="0e0b3583c1527df430b02aff9fb93b8ab7c3c643e6a9a3b66cab46bf5aeb85fa"
      NEEDLE="(e.enqueued_at, e.stream_seq, e.run_id.as_str())"
      REPLACEMENT="(e.enqueued_at, 0, e.run_id.as_str())"
      GATE="guest_partition_loop_drives_each_key_in_stream_order"
      TEST_ARGV=(cargo test --locked -p wamn-run-state --test queue "$GATE" -- --exact)
      ;;
    queue-blocking-dead-letter-policy)
      TARGET="crates/execution/run-state/src/queue/claim.rs"
      EXPECTED_SHA="3d20e4939988915cc613c92652be5ca7200b153123e33d8616a9f0977b868fc7"
      NEEDLE="entry.partition_key.is_some() && entry.partition_policy == PartitionPolicy::Blocking"
      REPLACEMENT="entry.partition_key.is_some() || entry.partition_policy == PartitionPolicy::Blocking"
      GATE="dead_letters_on_terminal_is_blocking_partitioned_only"
      TEST_ARGV=(cargo test --locked -p wamn-run-state --test queue "$GATE" -- --exact)
      ;;
    failover-lease-expiry-boundary)
      TARGET="crates/execution/run-state/src/queue/lease.rs"
      EXPECTED_SHA="db379edecaae2d4379907806afd61f75a4bff5b13a4924c372bf83137eae76b1"
      NEEDLE="lease_expires_at.is_some_and(|t| t > now)"
      REPLACEMENT="lease_expires_at.is_some_and(|t| t >= now)"
      GATE="lease_liveness_and_renewal"
      TEST_ARGV=(cargo test --locked -p wamn-run-state --test queue "$GATE" -- --exact)
      ;;
    wakeproof-parked-only-actuation)
      TARGET="services/waker/src/lib.rs"
      EXPECTED_SHA="193bfef79c9ffde796d0554b0c95aeb2b2aa3a1dd7b6b35498e31778b284bf25"
      NEEDLE="if current_replicas == 0 {"
      REPLACEMENT="if current_replicas != 0 {"
      GATE="tests::decide_skips_an_already_awake_deployment"
      TEST_ARGV=(cargo test --locked -p wamn-waker --lib "$GATE" -- --exact)
      ;;
    capturebench-oversize-preview)
      TARGET="crates/execution/run-state/src/capture.rs"
      EXPECTED_SHA="c8a8189b4fc8db1b83cb227b9bd08c2434b3316c55f4bc8d16e62860a3a1bfca"
      NEEDLE="let oversized = raw.len() as u64 > policy.max_bytes;"
      REPLACEMENT="let oversized = (raw.len() as u64) < policy.max_bytes;"
      GATE="capture::tests::oversized_payload_is_stored_preview_only_in_any_mode"
      TEST_ARGV=(cargo test --locked -p wamn-run-state --lib "$GATE" -- --exact)
      ;;
    runnerbench-dispatch-budget)
      TARGET="crates/execution/flow-engine/src/engine.rs"
      EXPECTED_SHA="96fe41d4c484537d6738d441349aabb9f7a266252fa4f650abb003b31595a425"
      NEEDLE="if state.dispatched >= self.dispatch_budget {"
      REPLACEMENT="if state.dispatched > self.dispatch_budget {"
      GATE="a_runaway_cycle_fails_at_exactly_the_budget"
      TEST_ARGV=(cargo test --locked -p wamn-runner --test runner "$GATE" -- --exact)
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
