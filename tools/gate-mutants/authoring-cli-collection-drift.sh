#!/usr/bin/env bash
# The headless CLI must send the CHECKED-IN collection's documents, not a
# hand-rolled duplicate of them. This mutant makes `save-flow-draft` silently
# drop the caller's `provenance` claim — an optional field, so it type-checks —
# and requires the request-shape drift gate to refuse the client before anything
# is sent.
#
# A field RENAME is deliberately not the mutation here: the generated schema
# types make a misspelled field a `tsc` error (TS2322, "Property '\"draft-id\"'
# is missing"), so it never reaches a gate. Dropping an OPTIONAL field is the
# divergence the type system cannot see, which is exactly what the collection
# comparison is for.
#
# Network-free: the gate is the package harness's static half.
set -euo pipefail

readonly OWNER="bd:wamn-ftfc.14"
readonly OUTCOME="the generated authoring collection detects a silently dropped optional field"

readonly TARGET="clients/authoring-client/src/cli/cli.ts"
readonly EXPECTED_SHA="67a6fd1ac9b88ed0628be30a704ecb48f8eb82074a207843827d58b2fd0c4da2"
readonly NEEDLE='    ...(options.provenance === undefined ? {} : { provenance: options.provenance }),'
readonly REPLACEMENT='    ...(options.provenance === undefined ? {} : {}),'
readonly GATE="every CLI request has the shape of its checked-in collection section"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

sha256() {
  sha256sum "$1" | cut -d ' ' -f 1
}

assert_precondition() {
  local actual
  actual="$(sha256 "$TARGET")"
  [[ "$actual" == "$EXPECTED_SHA" ]] || {
    echo "$TARGET hash mismatch: expected $EXPECTED_SHA, got $actual" >&2
    exit 2
  }
  [[ "$(python3 -c 'import pathlib, sys; print(pathlib.Path(sys.argv[1]).read_text().count(sys.argv[2]))' "$TARGET" "$NEEDLE")" == 1 ]] || {
    echo "$TARGET must contain the mutation anchor exactly once" >&2
    exit 2
  }
}

run_gate() (
  cd clients/authoring-client
  node scripts/test.mjs
)

run_mutant() (
  local backup_dir backup restored_sha exit_code output
  assert_precondition
  backup_dir="$(mktemp -d)"
  backup="$backup_dir/original"
  cp "$TARGET" "$backup"
  restore() {
    cp "$backup" "$TARGET"
    restored_sha="$(sha256 "$TARGET")"
    rm -f "$backup"
    rm -f "$backup_dir/gate.log"
    rmdir "$backup_dir"
    [[ "$restored_sha" == "$EXPECTED_SHA" ]] || {
      echo "restore failed for $TARGET" >&2
      exit 3
    }
  }
  trap restore EXIT INT TERM
  MUTANT_TARGET="$TARGET" MUTANT_NEEDLE="$NEEDLE" MUTANT_REPLACEMENT="$REPLACEMENT" python3 -c \
    'import os, pathlib; p=pathlib.Path(os.environ["MUTANT_TARGET"]); d=p.read_text(); p.write_text(d.replace(os.environ["MUTANT_NEEDLE"], os.environ["MUTANT_REPLACEMENT"], 1))'
  output="$backup_dir/gate.log"
  set +e
  run_gate >"$output" 2>&1
  exit_code=$?
  set -e
  tail -40 "$output"
  [[ $exit_code -ne 0 ]] || { echo "SURVIVED hand-rolled-cli-request" >&2; exit 1; }
  grep -q "not ok .*$GATE" "$output" || {
    echo "gate failed for the wrong reason: $GATE was not a failing check" >&2
    exit 1
  }
  echo "KILLED hand-rolled-cli-request gate=$GATE exit_code=$exit_code"
)

case "${1:-}" in
  check) assert_precondition ;;
  green) assert_precondition; run_gate ;;
  run) run_mutant ;;
  *) echo "usage: $0 check | green | run" >&2; exit 2 ;;
esac
