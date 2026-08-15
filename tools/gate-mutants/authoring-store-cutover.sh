#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-0h0g.7.3"
readonly OUTCOME="the retry ledger and retired validation identities cut over only while empty"
readonly TARGET="crates/schema/control/src/run_plane.rs"
readonly EXPECTED_SHA="7a55b4542841164741bee5b4ea345460752f5d6f2ea3393c6248bc89c032ee4c"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name the shared debug target directory" >&2
  exit 2
fi

declare NEEDLE REPLACEMENT GATE EXPECTED_COUNT

mutation_ids() {
  printf '%s\n' authoring-retry-ledger-populated-refusal-removed populated-validation-identity-rewritten
}

load_mutation() {
  case "$1" in
    populated-validation-identity-rewritten)
      NEEDLE="AND EXISTS (SELECT 1 FROM catalog.validated_flow_drafts)"
      REPLACEMENT="AND false"
      EXPECTED_COUNT=2
      GATE="run_plane::tests::retired_validation_dimension_is_empty_only_and_idempotent"
      ;;
    authoring-retry-ledger-populated-refusal-removed)
      NEEDLE='IF EXISTS (SELECT 1 FROM catalog.authoring_command_audit) \'
      REPLACEMENT='IF false \'
      EXPECTED_COUNT=1
      GATE="run_plane::tests::authoring_retry_ledger_cutover_is_empty_only_exact_and_idempotent"
      ;;
    *)
      echo "unknown mutant: $1" >&2
      return 2
      ;;
  esac
}

sha256() {
  sha256sum "$TARGET" | cut -d ' ' -f 1
}

check_one() {
  local actual count
  actual="$(sha256)"
  [[ "$actual" == "$EXPECTED_SHA" ]] || {
    echo "$TARGET hash mismatch: expected $EXPECTED_SHA, got $actual" >&2
    return 2
  }
  count="$(NEEDLE="$NEEDLE" perl -0ne \
    '$count += () = /\Q$ENV{NEEDLE}\E/g; END { print $count }' "$TARGET")"
  [[ "$count" == "$EXPECTED_COUNT" ]] || {
    echo "$TARGET must contain $EXPECTED_COUNT mutation anchor(s) (found $count)" >&2
    return 2
  }
}

gate() {
  CARGO_INCREMENTAL=0 cargo test --locked --offline -p wamn-schema-control \
    --lib "$GATE" -- --exact
}

run_one() (
  local id="$1" backup_dir backup exit_code restored
  load_mutation "$id"
  check_one
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
  NEEDLE="$NEEDLE" REPLACEMENT="$REPLACEMENT" perl -0pi -e \
    's/\Q$ENV{NEEDLE}\E/$ENV{REPLACEMENT}/' "$TARGET"
  set +e
  gate
  exit_code=$?
  set -e
  [[ $exit_code -ne 0 ]] || {
    echo "SURVIVED $id" >&2
    exit 1
  }
  echo "KILLED $id gate=$GATE exit_code=$exit_code"
)

case "${1:-}" in
  check)
    while IFS= read -r id; do load_mutation "$id"; check_one; done < <(mutation_ids)
    echo "authoring store cutover mutation anchors check clean"
    ;;
  green)
    while IFS= read -r id; do load_mutation "$id"; check_one; gate; done < <(mutation_ids)
    ;;
  run-all)
    while IFS= read -r id; do run_one "$id"; done < <(mutation_ids)
    ;;
  *)
    echo "usage: $0 check | green | run-all" >&2
    exit 2
    ;;
esac
