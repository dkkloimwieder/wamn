#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-99wl"
readonly OUTCOME="admission rejects malformed trusted invocation context"

readonly TARGET="crates/execution/run-state/src/admission.rs"
readonly EXPECTED_SHA="54a41d363738302c590c31db3dcc0a5cdfadb2a1fdf082a95985d594fa763266"
readonly NEEDLE='OR jsonb_typeof(i.invocation_context) IS DISTINCT FROM '\''object'\'' \'
readonly REPLACEMENT='OR false \'
readonly GATE="admission::tests::admission_persists_the_versioned_release_artifact_principal"

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
  cargo test --locked -p wamn-run-state --lib "$GATE" -- --exact
}

run_mutant() (
  local backup_dir backup restored_sha exit_code
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
  MUTANT_TARGET="$TARGET" MUTANT_NEEDLE="$NEEDLE" MUTANT_REPLACEMENT="$REPLACEMENT" python3 -c \
    'import os, pathlib; p=pathlib.Path(os.environ["MUTANT_TARGET"]); d=p.read_text(); p.write_text(d.replace(os.environ["MUTANT_NEEDLE"], os.environ["MUTANT_REPLACEMENT"], 1))'
  set +e
  run_gate
  exit_code=$?
  set -e
  [[ $exit_code -ne 0 ]] || { echo "SURVIVED shape-bypass" >&2; exit 1; }
  echo "KILLED shape-bypass gate=$GATE exit_code=$exit_code"
)

case "${1:-}" in
  check) assert_precondition ;;
  green) assert_precondition; run_gate ;;
  run) run_mutant ;;
  *) echo "usage: $0 check | green | run" >&2; exit 2 ;;
esac
