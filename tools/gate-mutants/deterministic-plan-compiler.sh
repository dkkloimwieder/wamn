#!/usr/bin/env bash
set -euo pipefail

readonly TARGET="services/scenario-worker/src/authoring.rs"
readonly EXPECTED_SHA="e6f790345348f4654d3d30233a90d755062db2b721b501022c0af48f45e53258"
readonly GATE="deterministic_plan_compiler"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name this lane's isolated debug target directory" >&2
  exit 2
fi

declare NEEDLE REPLACEMENT

mutation_ids() {
  printf '%s\n' \
    node-sorting-disabled \
    edge-ordinal-ignored \
    exact-byte-hash-bypassed
}

load_mutation() {
  local id="$1"
  case "$id" in
    node-sorting-disabled)
      NEEDLE='semantic_nodes.sort_by(|left, right| left.id.cmp(&right.id));'
      REPLACEMENT='semantic_nodes.reverse();'
      ;;
    edge-ordinal-ignored)
      NEEDLE='semantic_edges.sort_by(|left, right| {
        (
            left.from.as_str(),
            left.from_port.as_str(),
            left.ordinal.unwrap_or(0),
            left.to.as_str(),
            left.to_port.as_deref().unwrap_or(""),
        )
            .cmp(&(
                right.from.as_str(),
                right.from_port.as_str(),
                right.ordinal.unwrap_or(0),
                right.to.as_str(),
                right.to_port.as_deref().unwrap_or(""),
            ))
    });'
      REPLACEMENT='semantic_edges.sort_by(|left, right| {
        (
            left.from.as_str(),
            left.from_port.as_str(),
            left.to.as_str(),
            left.to_port.as_deref().unwrap_or(""),
        )
            .cmp(&(
                right.from.as_str(),
                right.from_port.as_str(),
                right.to.as_str(),
                right.to_port.as_deref().unwrap_or(""),
            ))
    });'
      ;;
    exact-byte-hash-bypassed)
      NEEDLE='let execution_bundle_hash = execution_bundle_hash(&exact_bytes);'
      REPLACEMENT='let execution_bundle_hash = execution_bundle_hash(
        &serde_json::to_vec_pretty(&plan).context("serialize mutant pretty execution bundle")?,
    );'
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

assert_precondition() {
  local actual
  actual="$(sha256 "$TARGET")"
  if [[ "$actual" != "$EXPECTED_SHA" ]]; then
    echo "$TARGET hash mismatch: expected $EXPECTED_SHA, got $actual" >&2
    return 2
  fi
  MUTANT_TARGET="$TARGET" MUTANT_NEEDLE="$NEEDLE" python3 -c \
    'import os, pathlib, sys; data=pathlib.Path(os.environ["MUTANT_TARGET"]).read_text(); count=data.count(os.environ["MUTANT_NEEDLE"]); sys.exit(0 if count == 1 else 1)' || {
      echo "$TARGET must contain the mutation anchor exactly once" >&2
      return 2
    }
}

replace_once() {
  MUTANT_TARGET="$TARGET" MUTANT_NEEDLE="$NEEDLE" MUTANT_REPLACEMENT="$REPLACEMENT" python3 -c \
    'import os, pathlib; path=pathlib.Path(os.environ["MUTANT_TARGET"]); data=path.read_text(); path.write_text(data.replace(os.environ["MUTANT_NEEDLE"], os.environ["MUTANT_REPLACEMENT"], 1))'
}

run_gate() {
  cargo test --locked --offline -p wamn-scenario-worker --lib "$GATE" -- --nocapture
}

run_green() {
  local id="$1"
  load_mutation "$id"
  assert_precondition
  echo "GREEN id=$id gate=$GATE target=$TARGET command=cargo test --locked --offline -p wamn-scenario-worker --lib $GATE -- --nocapture"
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
    restored_sha="$(sha256 "$TARGET")"
    rm -f "$backup"
    rmdir "$backup_dir"
    if [[ "$restored_sha" != "$EXPECTED_SHA" ]]; then
      echo "restore failed for $TARGET: expected $EXPECTED_SHA, got $restored_sha" >&2
      exit 3
    fi
    echo "RESTORED id=$id target=$TARGET sha256=$restored_sha"
  }
  trap restore EXIT INT TERM

  replace_once
  mutant_sha="$(sha256 "$TARGET")"
  if [[ "$mutant_sha" == "$EXPECTED_SHA" ]]; then
    echo "mutation $id did not change $TARGET" >&2
    exit 3
  fi

  echo "MUTANT id=$id gate=$GATE target=$TARGET baseline_sha256=$EXPECTED_SHA mutant_sha256=$mutant_sha"
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
