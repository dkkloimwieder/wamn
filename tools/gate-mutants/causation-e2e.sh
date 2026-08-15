#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-0h0g.11.9"
readonly OUTCOME="forward CDC causation and storage-owned event idempotency"
readonly CAMPAIGN="causation-e2e"
readonly BEAD="wamn-0h0g.11.9"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name the isolated debug target directory" >&2
  exit 2
fi

declare TARGET EXPECTED_SHA NEEDLE REPLACEMENT GATE
declare -a TEST_ARGV

mutation_ids() {
  printf '%s\n' \
    canonical-plan-accepts-wrong-artifact \
    source-event-accepts-wrong-message-id \
    stored-redelivery-skips-post-window
}

load_mutation() {
  local id="$1"
  case "$id" in
    canonical-plan-accepts-wrong-artifact)
      TARGET="crates/events/materializer/src/lib.rs"
      EXPECTED_SHA="079650a36ac047e9a1ad02754bd5009dd595dfa1f357d0e7f7e78f4ca6a663f2"
      NEEDLE='if plan.header.root_artifact_hash != artifact_hash {'
      REPLACEMENT='if plan.header.root_artifact_hash == artifact_hash {'
      GATE="execution_plan_tests::release_plan_requires_exact_hash_artifact_and_event_entry"
      TEST_ARGV=(cargo test --locked --offline -p wamn-materializer --lib "$GATE" -- --exact)
      ;;
    source-event-accepts-wrong-message-id)
      TARGET="crates/events/materializer/src/decide.rs"
      EXPECTED_SHA="34b5c1ec0828ba565d3ef59501206621df1a6242363af0cf6f7bd0422c911442"
      NEEDLE='[actual] if *actual == expected => Some(VerifiedSourceEventId(expected)),'
      REPLACEMENT='[actual] if *actual != expected => Some(VerifiedSourceEventId(expected)),'
      GATE="decide::tests::source_event_id_requires_one_exact_nats_message_id"
      TEST_ARGV=(cargo test --locked --offline -p wamn-materializer --lib "$GATE" -- --exact)
      ;;
    stored-redelivery-skips-post-window)
      TARGET="tests/integration/src/causation_e2e.rs"
      EXPECTED_SHA="c5d0c69c9f137d783b5e19be74534c4898126f31d407b4c8769c267019c064d3"
      NEEDLE='elapsed > Duration::from_secs(BROKER_DUP_WINDOW_SECS)'
      REPLACEMENT='elapsed < Duration::from_secs(BROKER_DUP_WINDOW_SECS)'
      GATE="causation_e2e::tests::stored_redelivery_contract_is_byte_and_sequence_exact"
      TEST_ARGV=(cargo test --locked --offline -p wamn-proof-integration --lib "$GATE" -- --exact)
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
  if [[ "${ALLOW_DIRTY_TARGET:-0}" == "1" ]]; then
    return
  fi
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
  MUTATION_TARGET="$TARGET" MUTATION_NEEDLE="$NEEDLE" python3 -c \
    'import os, pathlib, sys; data=pathlib.Path(os.environ["MUTATION_TARGET"]).read_text(); count=data.count(os.environ["MUTATION_NEEDLE"]); sys.exit(0 if count == 1 else 1)' || {
      echo "$TARGET must contain the mutation anchor exactly once" >&2
      return 2
    }
}

replace_once() {
  MUTATION_TARGET="$TARGET" MUTATION_NEEDLE="$NEEDLE" MUTATION_REPLACEMENT="$REPLACEMENT" python3 -c \
    'import os, pathlib; path=pathlib.Path(os.environ["MUTATION_TARGET"]); data=path.read_text(); path.write_text(data.replace(os.environ["MUTATION_NEEDLE"], os.environ["MUTATION_REPLACEMENT"], 1))'
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
