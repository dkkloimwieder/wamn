#!/usr/bin/env bash
set -euo pipefail

readonly CAMPAIGN="m1-composition"
readonly BEAD="wamn-0h0g.11.18"
readonly TARGET="tests/integration/src/m1.rs"
readonly EXPECTED_SHA="b027f1e85c6911fd6a0c25a8e1904f52d6fc76d98d61dacef2f196a23c69f699"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name the isolated debug target directory" >&2
  exit 2
fi

declare NEEDLE REPLACEMENT GATE

mutation_ids() {
  printf '%s\n' invoke-check-9-twice wrap-check-9-error
}

load_mutation() {
  local id="$1"
  case "$id" in
    invoke-check-9-twice)
      NEEDLE='    check().await'
      REPLACEMENT=$'    check().await?;\n    check().await'
      GATE="m1::tests::composition_invokes_check_9_exactly_once"
      ;;
    wrap-check-9-error)
      NEEDLE='    check().await'
      REPLACEMENT='    check().await.map_err(|error| anyhow::anyhow!("M1 check 9 failed: {error:#}"))'
      GATE="m1::tests::composition_propagates_check_9_error_unchanged"
      ;;
    *)
      echo "unknown mutant: $id" >&2
      return 2
      ;;
  esac
}

sha256() {
  sha256sum "$TARGET" | cut -d ' ' -f 1
}

assert_precondition() {
  local actual count
  actual="$(sha256)"
  [[ "$actual" == "$EXPECTED_SHA" ]] || {
    echo "$TARGET hash mismatch: expected $EXPECTED_SHA, got $actual" >&2
    return 2
  }
  count="$(MUTATION_NEEDLE="$NEEDLE" perl -0ne \
    '$count += () = /\Q$ENV{MUTATION_NEEDLE}\E/g; END { print $count }' "$TARGET")"
  [[ "$count" == 1 ]] || {
    echo "$TARGET must contain the mutation anchor exactly once (found $count)" >&2
    return 2
  }
}

run_gate() {
  cargo test --locked --offline -p wamn-proof-integration --lib "$GATE" -- --exact
}

green() {
  local id="$1"
  load_mutation "$id"
  assert_precondition
  echo "GREEN campaign=$CAMPAIGN bead=$BEAD gate=$GATE target=$TARGET command=cargo test --locked --offline -p wamn-proof-integration --lib $GATE -- --exact"
  run_gate
}

run_mutant() (
  local id="$1"
  local backup_dir backup restored_sha mutant_sha exit_code
  load_mutation "$id"
  assert_precondition
  backup_dir="$(mktemp -d)"
  backup="$backup_dir/original"
  cp "$TARGET" "$backup"
  restore() {
    cp "$backup" "$TARGET"
    restored_sha="$(sha256)"
    rm -f "$backup"
    rmdir "$backup_dir"
    if [[ "$restored_sha" != "$EXPECTED_SHA" ]]; then
      echo "restore failed for $TARGET: expected $EXPECTED_SHA, got $restored_sha" >&2
      exit 3
    fi
    echo "RESTORED campaign=$CAMPAIGN target=$TARGET sha256=$restored_sha"
  }
  trap restore EXIT INT TERM

  MUTATION_NEEDLE="$NEEDLE" MUTATION_REPLACEMENT="$REPLACEMENT" perl -0pi -e \
    's/\Q$ENV{MUTATION_NEEDLE}\E/$ENV{MUTATION_REPLACEMENT}/' "$TARGET"
  mutant_sha="$(sha256)"
  [[ "$mutant_sha" != "$EXPECTED_SHA" ]] || {
    echo "mutation $id did not change $TARGET" >&2
    exit 3
  }

  echo "MUTANT campaign=$CAMPAIGN bead=$BEAD gate=$GATE target=$TARGET baseline_sha256=$EXPECTED_SHA mutant_sha256=$mutant_sha"
  set +e
  run_gate
  exit_code=$?
  set -e
  [[ $exit_code -ne 0 ]] || {
    echo "SURVIVED id=$id gate=$GATE" >&2
    exit 1
  }
  echo "KILLED id=$id gate=$GATE exit_code=$exit_code"
)

check_campaign() {
  local id
  while IFS= read -r id; do
    load_mutation "$id"
    assert_precondition
    echo "CHECKED id=$id gate=$GATE target=$TARGET sha256=$EXPECTED_SHA"
  done < <(mutation_ids)
}

usage() {
  echo "usage: $0 list | check | green MUTANT | green-all | run MUTANT | run-all" >&2
}

case "${1:-}" in
  list) mutation_ids ;;
  check) check_campaign ;;
  green)
    [[ $# -eq 2 ]] || { usage; exit 2; }
    green "$2"
    ;;
  green-all)
    [[ $# -eq 1 ]] || { usage; exit 2; }
    while IFS= read -r id; do green "$id"; done < <(mutation_ids)
    ;;
  run)
    [[ $# -eq 2 ]] || { usage; exit 2; }
    run_mutant "$2"
    ;;
  run-all)
    [[ $# -eq 1 ]] || { usage; exit 2; }
    while IFS= read -r id; do run_mutant "$id"; done < <(mutation_ids)
    ;;
  *) usage; exit 2 ;;
esac
