#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-0h0g.3.5"
readonly OUTCOME="trusted frame facts are complete, attributed, ordered, captured, and fail-closed"
readonly CAMPAIGN="frame-node-facts"
readonly BEAD="wamn-0h0g.3.5"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name the serialized debug target directory" >&2
  exit 2
fi
export CARGO_INCREMENTAL=0
export CARGO_BUILD_JOBS=2

declare TARGET EXPECTED_SHA NEEDLE REPLACEMENT EXPECTED_COUNT GATE
declare -a TEST_ARGV

mutation_ids() {
  printf '%s\n' \
    accepted-sequence-stall \
    source-map-fallback \
    capture-forced-off \
    sink-fail-open \
    callee-attribution-drop \
    retry-attempt-duplication
}

load_mutation() {
  local id="$1"
  TARGET="components/execution/flowrunner/src/frames.rs"
  EXPECTED_SHA="03eae52f098dfa0b12d50dc129d259222db254e8b58b0b2e44ea225c52e77309"
  EXPECTED_COUNT=1
  case "$id" in
    accepted-sequence-stall)
      NEEDLE='self.next_fact_sequence = next_sequence;'
      REPLACEMENT='self.next_fact_sequence = sequence;'
      GATE="frames::tests::sibling_calls_push_execute_pop_with_monotonic_trusted_frame_facts"
      ;;
    source-map-fallback)
      NEEDLE='source.source_node_id.clone(),'
      REPLACEMENT='plan_node.local_node_id.to_string(),'
      GATE="frames::tests::source_map_drives_author_selector_and_missing_mapping_never_falls_back"
      ;;
    capture-forced-off)
      NEEDLE='let capture = derive_capture(self.capture_mode, &output, input);'
      REPLACEMENT='let capture = derive_capture(CaptureMode::Off, &output, input);'
      GATE="frames::tests::capture_full_scrubs_and_bounds_while_off_records_zero_node_io"
      ;;
    sink-fail-open)
      NEEDLE=$'fact_sink\n            .emit(fact)\n            .map_err(|source| FrameExecutionError::fact_sink_refused(frame_id, node_id, source))?;'
      REPLACEMENT='let _ = fact_sink.emit(fact);'
      GATE="frames::tests::fact_sink_refusal_fails_closed_without_allocating_or_running_a_successor"
      ;;
    callee-attribution-drop)
      NEEDLE='frame.call_site_id().cloned(),'
      REPLACEMENT='None,'
      GATE="frames::tests::sibling_calls_push_execute_pop_with_monotonic_trusted_frame_facts"
      ;;
    retry-attempt-duplication)
      NEEDLE='if is_retry {'
      REPLACEMENT='if false {'
      GATE="frames::tests::retry_attempts_emit_only_the_completed_occurrence_fact"
      ;;
    *)
      echo "unknown mutant: $id" >&2
      return 2
      ;;
  esac
  TEST_ARGV=(
    cargo test --locked --offline --manifest-path components/Cargo.toml
    -p flowrunner "$GATE" -- --exact
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
