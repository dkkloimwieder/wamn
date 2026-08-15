#!/usr/bin/env bash
set -euo pipefail

readonly CAMPAIGN="profile-selectors"
readonly BEAD="wamn-0h0g.10.3"
readonly EXPECTED_PROFILE="debug"
readonly TARGET="architecture/workspace-tiers.json"
readonly EXPECTED_SHA="a415079ed615b2e40e8436260d7b20b98b28af7973dc40e31e6e65025f8efad1"
readonly GATE="profile_contract_matches_locked_metadata"
readonly -a TEST_ARGV=(
  cargo test --locked --offline -p wamn-proof-conformance
  --test profile_selectors "$GATE" -- --exact
)

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name the isolated debug target directory" >&2
  exit 2
fi

declare NEEDLE REPLACEMENT

mutation_ids() {
  printf '%s\n' duplicate-m2-addition widen-component-m1
}

load_mutation() {
  local -r id="$1"
  case "$id" in
    duplicate-m2-addition)
      NEEDLE='      "m2_additions": [
        "wamn-dispatcher",
        "wamn-waker"
      ],'
      REPLACEMENT='      "m2_additions": [
        "wamn-dispatcher",
        "wamn-dispatcher"
      ],'
      ;;
    widen-component-m1)
      NEEDLE='      "m1_inventory_tier": "product_components",'
      REPLACEMENT='      "m1_inventory_tier": "full_ci",'
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
  local actual
  actual="$(sha256)"
  [[ "$actual" == "$EXPECTED_SHA" ]] || {
    echo "$TARGET hash mismatch: expected $EXPECTED_SHA, got $actual" >&2
    return 2
  }
  NEEDLE="$NEEDLE" perl -0ne \
    '$count += () = /\Q$ENV{NEEDLE}\E/g; END { exit($count == 1 ? 0 : 1) }' \
    "$TARGET" || {
      echo "$TARGET must contain the mutation anchor exactly once" >&2
      return 2
    }
}

run_green() {
  local -r id="$1"
  load_mutation "$id"
  assert_precondition
  echo "GREEN campaign=$CAMPAIGN bead=$BEAD id=$id profile=$EXPECTED_PROFILE gate=$GATE target=$TARGET command=${TEST_ARGV[*]}"
  "${TEST_ARGV[@]}"
}

run_mutant() (
  local -r id="$1"
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
    echo "RESTORED campaign=$CAMPAIGN id=$id target=$TARGET sha256=$restored_sha"
  }
  trap restore EXIT INT TERM

  NEEDLE="$NEEDLE" REPLACEMENT="$REPLACEMENT" perl -0pi -e \
    's/\Q$ENV{NEEDLE}\E/$ENV{REPLACEMENT}/' "$TARGET"
  mutant_sha="$(sha256)"
  [[ "$mutant_sha" != "$EXPECTED_SHA" ]] || {
    echo "mutation $id did not change $TARGET" >&2
    exit 3
  }

  echo "MUTANT campaign=$CAMPAIGN bead=$BEAD id=$id profile=$EXPECTED_PROFILE gate=$GATE target=$TARGET baseline_sha256=$EXPECTED_SHA mutant_sha256=$mutant_sha command=${TEST_ARGV[*]}"
  set +e
  "${TEST_ARGV[@]}"
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
    while IFS= read -r id; do run_green "$id"; done < <(mutation_ids)
    ;;
  run)
    [[ $# -eq 2 ]] || { usage; exit 2; }
    run_mutant "$2"
    ;;
  run-all)
    [[ $# -eq 1 ]] || { usage; exit 2; }
    while IFS= read -r id; do run_mutant "$id"; done < <(mutation_ids)
    ;;
  *)
    usage
    exit 2
    ;;
esac
