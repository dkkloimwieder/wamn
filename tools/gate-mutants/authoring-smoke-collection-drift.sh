#!/usr/bin/env bash
# The S0 authoring smoke must send the CHECKED-IN collection's request, not a
# hand-rolled duplicate of it. This mutant makes the script write one field the
# collection owns — `flow-id`, which is not a declared per-run substitution — and
# requires the drift check to refuse to send anything.
#
# Network-free: the gate is the smoke script's static `--check` half.
set -euo pipefail

readonly OWNER="bd:wamn-jvzx.4"
readonly OUTCOME="the authenticated smoke refuses hand-written request-collection drift"

readonly TARGET="clients/authoring-client/scripts/smoke.mjs"
readonly EXPECTED_SHA="6a39f78bdea5a28866fa7829adce94f79b79d668469d3282f10cbea891dacb4d"
readonly NEEDLE='    writePath(document, path, values[field]);'
readonly REPLACEMENT='    writePath(document, path, values[field]);
    writePath(document, ["body", "command", "input", "flow-id"], "hand-rolled-flow");'
readonly GATE="collection-derivation"

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

run_gate() {
  node "$TARGET" --check
}

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
  cat "$output"
  [[ $exit_code -ne 0 ]] || { echo "SURVIVED hand-rolled-request" >&2; exit 1; }
  grep -q "check=$GATE" "$output" || {
    echo "gate failed for the wrong reason: $GATE was not the failing check" >&2
    exit 1
  }
  echo "KILLED hand-rolled-request gate=$GATE exit_code=$exit_code"
)

case "${1:-}" in
  check) assert_precondition ;;
  green) assert_precondition; run_gate ;;
  run) run_mutant ;;
  *) echo "usage: $0 check | green | run" >&2; exit 2 ;;
esac
