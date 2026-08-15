#!/usr/bin/env bash
# A command the surface has not mounted answers a bare `501`, and the CLI must
# report that as its own status — never as a completed command. This mutant makes
# the client read a `501` as a completion and requires the typed-answer gate to
# fail, because a green cycle over unmounted handlers is exactly the false
# evidence this bead must not produce.
#
# Network-free: the gate is the package harness's static half.
set -euo pipefail

readonly OWNER="bd:wamn-ftfc.14"
readonly OUTCOME="an unmounted authoring command cannot be reported as successful"

readonly TARGET="clients/authoring-client/src/cli/cli.ts"
readonly EXPECTED_SHA="67a6fd1ac9b88ed0628be30a704ecb48f8eb82074a207843827d58b2fd0c4da2"
readonly NEEDLE='        return { ...base, status: "unmounted", "elapsed-ms": elapsed(), "http-status": 501 };'
readonly REPLACEMENT='        return { ...base, status: "completed", "elapsed-ms": elapsed(), "http-status": 501 };'
readonly GATE="an unmounted command is its own answer and never a success"

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
  [[ "$(python3 -c 'import pathlib, sys; print(pathlib.Path(sys.argv[1]).read_text().count(sys.argv[2]))' "$TARGET" "$NEEDLE")" == 2 ]] || {
    echo "$TARGET must contain the command/query mutation anchor exactly twice" >&2
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
  [[ $exit_code -ne 0 ]] || { echo "SURVIVED unmounted-reads-as-completed" >&2; exit 1; }
  grep -q "not ok .*$GATE" "$output" || {
    echo "gate failed for the wrong reason: $GATE was not a failing check" >&2
    exit 1
  }
  echo "KILLED unmounted-reads-as-completed gate=$GATE exit_code=$exit_code"
)

case "${1:-}" in
  check) assert_precondition ;;
  green) assert_precondition; run_gate ;;
  run) run_mutant ;;
  *) echo "usage: $0 check | green | run" >&2; exit 2 ;;
esac
