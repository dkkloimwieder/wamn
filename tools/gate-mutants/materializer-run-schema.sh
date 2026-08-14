#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-l5i9.72"
readonly OUTCOME="materializer writes remain confined to the validated run schema"

readonly CAMPAIGN="materializer-run-schema"
readonly BEAD="wamn-l5i9.72"
readonly EXPECTED_PROFILE="debug"

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
    materializer-default-bypasses-canonical-schema \
    custom-schema-leaves-admission-boundaries-canonical \
    invalid-schema-bypasses-identifier-validation
}

load_mutation() {
  local id="$1"
  case "$id" in
    materializer-default-bypasses-canonical-schema)
      TARGET="components/execution/materializer/src/main.rs"
      EXPECTED_SHA="8c6d659e073f5862744b0ae2333976c14cc1a861c1a6b1a95b1b951c16598cdb"
      NEEDLE='env_or("WAMN_MAT_RUN_SCHEMA", "wamn_run")'
      REPLACEMENT='env_or("WAMN_MAT_RUN_SCHEMA", "mutant_run")'
      GATE="materializer::tests::admission_scopes_dedup_and_records_registration_evidence"
      TEST_ARGV=(cargo test --locked -p wamn-proof-integration --lib "$GATE" -- --exact)
      ;;
    custom-schema-leaves-admission-boundaries-canonical)
      TARGET="crates/execution/run-state/src/admission.rs"
      EXPECTED_SHA="54a41d363738302c590c31db3dcc0a5cdfadb2a1fdf082a95985d594fa763266"
      NEEDLE='admit: canonical.admit.replace("wamn_run.", &qualifier),'
      REPLACEMENT='admit: canonical.admit,'
      GATE="admission::tests::custom_admission_schema_qualifies_every_run_state_reference"
      TEST_ARGV=(cargo test --locked -p wamn-run-state --lib "$GATE" -- --exact)
      ;;
    invalid-schema-bypasses-identifier-validation)
      TARGET="crates/execution/run-state/src/admission.rs"
      EXPECTED_SHA="54a41d363738302c590c31db3dcc0a5cdfadb2a1fdf082a95985d594fa763266"
      NEEDLE='Identifier::new(value).map(Self)'
      REPLACEMENT='Identifier::new("mutant_run").map(Self)'
      GATE="admission::tests::run_state_schema_rejects_invalid_postgresql_identifiers"
      TEST_ARGV=(cargo test --locked -p wamn-run-state --lib "$GATE" -- --exact)
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
    while IFS= read -r id; do
      run_green "$id"
    done < <(mutation_ids)
    ;;
  run)
    [[ $# -eq 2 ]] || { usage; exit 2; }
    run_mutant "$2"
    ;;
  run-all)
    [[ $# -eq 1 ]] || { usage; exit 2; }
    while IFS= read -r id; do
      run_mutant "$id"
    done < <(mutation_ids)
    ;;
  *)
    usage
    exit 2
    ;;
esac
