#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-ec7j"
readonly OUTCOME="the exact admitted run causation survives the reader pipeline"

readonly CAMPAIGN="causation-e2e"
readonly BEAD="wamn-ec7j"

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
    invocation-drops-sink-write \
    reader-requests-r1 \
    readerbench-drops-exact-causation \
    reader-process-ignores-replica-argument
}

load_mutation() {
  local id="$1"
  case "$id" in
    invocation-drops-sink-write)
      TARGET="tests/integration/src/causation_e2e.rs"
      EXPECTED_SHA="cd38af0bd2d320b036adba7e1d035343c9dd0cf4731152244be81ed8902a5d3e"
      NEEDLE='{ "id": "write", "type": "pg-write" }'
      REPLACEMENT='{ "id": "write", "type": "transform" }'
      GATE="causation_e2e::tests::invocation_fixture_drives_one_gate_scoped_pg_write"
      TEST_ARGV=(cargo test --locked -p wamn-proof-integration --lib "$GATE" -- --exact)
      ;;
    reader-requests-r1)
      TARGET="tests/integration/src/causation_e2e.rs"
      EXPECTED_SHA="cd38af0bd2d320b036adba7e1d035343c9dd0cf4731152244be81ed8902a5d3e"
      NEEDLE='stream_replicas: 3,'
      REPLACEMENT='stream_replicas: 1,'
      GATE="causation_e2e::tests::proof_arguments_require_r3_and_the_exact_run_id"
      TEST_ARGV=(cargo test --locked -p wamn-proof-integration --lib "$GATE" -- --exact)
      ;;
    readerbench-drops-exact-causation)
      TARGET="tests/integration/src/causation_e2e.rs"
      EXPECTED_SHA="cd38af0bd2d320b036adba7e1d035343c9dd0cf4731152244be81ed8902a5d3e"
      NEEDLE='expect_causation_run: Some(run_id.into()),'
      REPLACEMENT='expect_causation_run: None,'
      GATE="causation_e2e::tests::proof_arguments_require_r3_and_the_exact_run_id"
      TEST_ARGV=(cargo test --locked -p wamn-proof-integration --lib "$GATE" -- --exact)
      ;;
    reader-process-ignores-replica-argument)
      TARGET="tests/integration/src/cdc_reader_process.rs"
      EXPECTED_SHA="fb907b28f99b6615abec9bc13f0fbf3ee000c06ec56f5372a1ea7c1fedcbe261"
      NEEDLE='.arg(args.stream_replicas.to_string())'
      REPLACEMENT='.arg("1")'
      GATE="cdc_reader_process::tests::reader_command_preserves_the_proof_runtime_contract"
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
