#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-0h0g.5.8"
readonly OUTCOME="dispatcher reconciliation stays tenant-scoped and read-only"
readonly CAMPAIGN="dispatcher-reconciliation-boundary"
readonly BEAD="wamn-0h0g.5.8"
readonly MUTANT="unscoped-literal-queue-scan"
readonly TARGET="services/dispatcher/src/lib.rs"
readonly EXPECTED_SHA="d52f20b6e77d89d53ffd4e0d905b9be22ee6b57ec25eb87d72ee418af0072408"
readonly NEEDLE='p.client.query(&parked_due_sql(batch), &[]).await?'
readonly REPLACEMENT='p.client.query("SELECT run_id FROM run_queue LIMIT 64", &[]).await?'
readonly GATE="dispatcher_reconciliation_is_tenant_scoped_and_read_only"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name the serialized debug target directory" >&2
  exit 2
fi
export CARGO_INCREMENTAL=0
export CARGO_BUILD_JOBS=2

readonly -a TEST_ARGV=(
  cargo test --locked --offline -p wamn-proof-conformance
  --test dispatcher_boundary "$GATE" -- --exact
)

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
  count="$(MUTATION_TARGET="$TARGET" MUTATION_NEEDLE="$NEEDLE" python3 -c \
    'import os, pathlib; print(pathlib.Path(os.environ["MUTATION_TARGET"]).read_text().count(os.environ["MUTATION_NEEDLE"]))')"
  [[ "$count" == "1" ]] || {
    echo "$TARGET must contain the mutation anchor once (found $count)" >&2
    return 2
  }
}

replace_once() {
  MUTATION_TARGET="$TARGET" MUTATION_NEEDLE="$NEEDLE" MUTATION_REPLACEMENT="$REPLACEMENT" python3 -c \
    'import os, pathlib; p=pathlib.Path(os.environ["MUTATION_TARGET"]); s=p.read_text(); p.write_text(s.replace(os.environ["MUTATION_NEEDLE"], os.environ["MUTATION_REPLACEMENT"], 1))'
}

run_green() {
  assert_precondition
  echo "GREEN campaign=$CAMPAIGN bead=$BEAD id=$MUTANT gate=$GATE target=$TARGET command=${TEST_ARGV[*]}"
  "${TEST_ARGV[@]}"
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
  replace_once
  mutant_sha="$(sha256 "$TARGET")"
  [[ "$mutant_sha" != "$EXPECTED_SHA" ]] || {
    echo "mutation $MUTANT did not change $TARGET" >&2
    exit 3
  }
  echo "MUTANT campaign=$CAMPAIGN bead=$BEAD id=$MUTANT gate=$GATE target=$TARGET baseline_sha256=$EXPECTED_SHA mutant_sha256=$mutant_sha command=${TEST_ARGV[*]}"
  set +e
  "${TEST_ARGV[@]}"
  exit_code=$?
  set -e
  [[ $exit_code -ne 0 ]] || {
    echo "SURVIVED id=$MUTANT gate=$GATE" >&2
    exit 1
  }
  echo "KILLED id=$MUTANT gate=$GATE exit_code=$exit_code"
)

usage() {
  echo "usage: $0 list | check | green | run" >&2
}

case "${1:-}" in
  list) printf '%s\n' "$MUTANT" ;;
  check)
    assert_precondition
    printf 'CHECKED id=%s gate=%s target=%s sha256=%s\n' \
      "$MUTANT" "$GATE" "$TARGET" "$EXPECTED_SHA"
    ;;
  green)
    [[ $# -eq 1 ]] || { usage; exit 2; }
    run_green
    ;;
  run)
    [[ $# -eq 1 ]] || { usage; exit 2; }
    run_mutant
    ;;
  *) usage; exit 2 ;;
esac
