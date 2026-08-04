#!/usr/bin/env bash
set -euo pipefail

readonly TARGET="crates/execution/standard-nodes/src/timeshift.rs"
readonly MUTATION="time-shift-drops-rfc3339-input"
readonly TEST="time_shift_computes_an_iso_cutoff"
readonly NEEDLE='Value::String(raw) => DateTime::parse_from_rfc3339(&raw).map_err(|error| {'
readonly REPLACEMENT='Value::Number(raw) => DateTime::parse_from_rfc3339(&raw.to_string()).map_err(|error| {'
readonly EXPECTED_SHA="b4befbfcaa16db2804f552fb067ad1b58d01dc6c2e1ce24ab234a5fe869d7fe0"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name the shared debug target directory" >&2
  exit 2
fi

sha256() {
  sha256sum "$TARGET" | cut -d ' ' -f 1
}

check() {
  local actual count
  actual="$(sha256)"
  [[ "$actual" == "$EXPECTED_SHA" ]] || {
    echo "$TARGET hash mismatch: expected $EXPECTED_SHA, got $actual" >&2
    exit 2
  }
  count="$(env TARGET="$TARGET" NEEDLE="$NEEDLE" python3 -c \
    'import os, pathlib; print(pathlib.Path(os.environ["TARGET"]).read_text().count(os.environ["NEEDLE"]))')"
  [[ "$count" == 1 ]] || {
    echo "$TARGET must contain the mutation anchor exactly once" >&2
    exit 2
  }
  echo "CHECKED id=$MUTATION target=$TARGET sha256=$actual"
}

gate() {
  cargo test --locked -p wamn-standard-nodes --test nodes "$TEST" -- --exact
}

run_mutant() (
  local backup_dir backup restored mutant_exit
  check
  backup_dir="$(mktemp -d)"
  backup="$backup_dir/original"
  cp "$TARGET" "$backup"
  restore() {
    cp "$backup" "$TARGET"
    restored="$(sha256)"
    rm -f "$backup"
    rmdir "$backup_dir"
    [[ "$restored" == "$EXPECTED_SHA" ]] || {
      echo "restore failed for $TARGET" >&2
      exit 3
    }
  }
  trap restore EXIT INT TERM
  env TARGET="$TARGET" NEEDLE="$NEEDLE" REPLACEMENT="$REPLACEMENT" python3 -c \
    'import os, pathlib; p=pathlib.Path(os.environ["TARGET"]); s=p.read_text(); p.write_text(s.replace(os.environ["NEEDLE"], os.environ["REPLACEMENT"], 1))'
  set +e
  gate
  mutant_exit=$?
  set -e
  [[ $mutant_exit -ne 0 ]] || {
    echo "SURVIVED id=$MUTATION" >&2
    exit 1
  }
  echo "KILLED id=$MUTATION exit_code=$mutant_exit"
)

case "${1:-}" in
  check) check ;;
  green) check; gate ;;
  run) run_mutant ;;
  *) echo "usage: $0 check | green | run" >&2; exit 2 ;;
esac
