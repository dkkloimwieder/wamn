#!/usr/bin/env bash
set -euo pipefail

readonly TARGET="services/scenario-worker/src/lib.rs"
readonly EXPECTED_SHA="05005e75b808ddfba15a2004d79c9bb107eba79e28d9203abe13e2ee5d820972"
readonly NEEDLE='let schema = template.replace("{ordinal}", &ordinal.to_string());'
readonly REPLACEMENT='let schema = template.replace("{ordinal}", "0");'
readonly GATE="tests::root_runs_in_one_invocation_have_distinct_schema_and_run_identity"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name the loop wave debug target directory" >&2
  exit 2
fi

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
  cargo test --locked -p wamn-scenario-worker --lib "$GATE" -- --exact
}

run_mutant() (
  local backup_dir backup restored_sha mutant_sha exit_code
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
  mutant_sha="$(sha256 "$TARGET")"
  [[ "$mutant_sha" != "$EXPECTED_SHA" ]] || {
    echo "mutation did not change $TARGET" >&2
    exit 3
  }

  echo "MUTANT id=root-run-schema-collapse gate=$GATE target=$TARGET baseline_sha256=$EXPECTED_SHA mutant_sha256=$mutant_sha"
  set +e
  run_gate
  exit_code=$?
  set -e
  [[ $exit_code -ne 0 ]] || {
    echo "SURVIVED root-run-schema-collapse" >&2
    exit 1
  }
  echo "KILLED root-run-schema-collapse gate=$GATE exit_code=$exit_code"
)

case "${1:-}" in
  check) assert_precondition ;;
  green) assert_precondition; run_gate ;;
  run) run_mutant ;;
  *) echo "usage: $0 check | green | run" >&2; exit 2 ;;
esac
