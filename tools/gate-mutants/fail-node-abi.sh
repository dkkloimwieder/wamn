#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-ayq7.25"
readonly OUTCOME="malformed fail results are refused before the lifecycle transaction"

readonly TARGET="components/execution/flowrunner/src/lib.rs"
readonly MUTATION="fail-result-bypasses-exact-terminal-validation"
readonly TEST="tests::malformed_fail_actions_are_refused_before_the_lifecycle_transaction"
readonly NEEDLE='validate_fail_outcome(dispatch, outcome).map_err(|error| error.to_string())?;'
readonly REPLACEMENT='if dispatch.node_type != "fail" { validate_fail_outcome(dispatch, outcome).map_err(|error| error.to_string())?; }'
readonly EXPECTED_SHA="1bc244bb02f9a872e2e9ba204972683a0cf521058a79d693f41279ece75cb2c4"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name a unique debug target directory" >&2
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
  count="$(env NEEDLE="$NEEDLE" python3 -c \
    'import os, pathlib; print(pathlib.Path("components/execution/flowrunner/src/lib.rs").read_text().count(os.environ["NEEDLE"]))')"
  [[ "$count" == 1 ]] || {
    echo "$TARGET must contain the mutation anchor exactly once" >&2
    exit 2
  }
  echo "CHECKED id=$MUTATION target=$TARGET sha256=$actual"
}

gate() {
  cargo test --locked --manifest-path components/Cargo.toml -p flowrunner "$TEST" -- --exact
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
  env NEEDLE="$NEEDLE" REPLACEMENT="$REPLACEMENT" python3 -c \
    'import os, pathlib; p=pathlib.Path("components/execution/flowrunner/src/lib.rs"); s=p.read_text(); p.write_text(s.replace(os.environ["NEEDLE"], os.environ["REPLACEMENT"], 1))'
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
