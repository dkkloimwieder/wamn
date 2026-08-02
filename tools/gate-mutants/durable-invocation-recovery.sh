#!/usr/bin/env bash
set -euo pipefail

readonly CAMPAIGN="durable-invocation-recovery"
readonly BEAD="wamn-2jdm.5.1"
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
    invocation-generation-fence \
    admission-write-ahead-atomicity \
    cancellation-generation-fence \
    child-release-wake-fence \
    child-occurrence-identity \
    attempt-intent-before-renewal \
    persisted-version-recovery \
    flow-spec-http-classifier
}

load_mutation() {
  local id="$1"
  case "$id" in
    invocation-generation-fence)
      TARGET="crates/execution/run-state/src/queue/sql.rs"
      EXPECTED_SHA="ef9c1d0d1eda6a997f3de1fff5f148e11d77fc92908e2e198412c0ac9bdce693"
      NEEDLE="OR a.lease_generation <> \$3 THEN 'fence-lost'"
      REPLACEMENT="OR false THEN 'fence-lost'"
      GATE="invocationproof::tests::exact_driver_sql_has_one_locked_authority_and_no_available_scan"
      TEST_ARGV=(cargo test --locked -p wamn-proof-system --lib "$GATE" -- --exact)
      ;;
    admission-write-ahead-atomicity)
      TARGET="crates/execution/run-state/src/admission.rs"
      EXPECTED_SHA="0912c481f9577f256fc640dde5d37ff6e7ddb71239631f0e73309b27b6bb189f"
      NEEDLE="FROM created_run AS r JOIN classified AS c USING (tenant_id, run_id)"
      REPLACEMENT="FROM input AS r JOIN classified AS c USING (tenant_id, run_id)"
      GATE="admission::tests::admission_recipe_locks_then_mutates_in_one_transaction"
      TEST_ARGV=(cargo test --locked -p wamn-run-state --lib "$GATE" -- --exact)
      ;;
    cancellation-generation-fence)
      TARGET="crates/execution/run-state/src/cancellation.rs"
      EXPECTED_SHA="645d81d47a1508d1ca58da4538f4e11d2123503e20b6d7f356772afe8fca3601"
      NEEDLE="WHEN q.run_id IS NULL OR q.lease_generation <> i.expected_generation"
      REPLACEMENT="WHEN q.run_id IS NULL"
      GATE="cancellation::tests::request_is_generation_fenced_and_never_seizes"
      TEST_ARGV=(cargo test --locked -p wamn-run-state --lib "$GATE" -- --exact)
      ;;
    child-release-wake-fence)
      TARGET="crates/execution/run-state/src/child.rs"
      EXPECTED_SHA="9d4b5b2ce04ca6028b5b340b9cb5a73f1ad7bcadd19c2176bdf5a25af06d8083"
      NEEDLE="WHEN p.wait_generation IS DISTINCT FROM \$13::bigint"
      REPLACEMENT="WHEN false"
      GATE="child::tests::child_release_fences_wait_generation_and_wakes_atomically"
      TEST_ARGV=(cargo test --locked -p wamn-run-state --lib "$GATE" -- --exact)
      ;;
    child-occurrence-identity)
      TARGET="crates/execution/run-state/src/child.rs"
      EXPECTED_SHA="9d4b5b2ce04ca6028b5b340b9cb5a73f1ad7bcadd19c2176bdf5a25af06d8083"
      NEEDLE="AND c.parent_node_id = \$5 AND c.parent_occurrence = \$6 \\
              FOR UPDATE OF c"
      REPLACEMENT="AND c.parent_node_id = \$5 AND c.parent_occurrence = 0 \\
              FOR UPDATE OF c"
      GATE="child::tests::child_create_is_occurrence_keyed_fenced_and_atomic"
      TEST_ARGV=(cargo test --locked -p wamn-run-state --lib "$GATE" -- --exact)
      ;;
    attempt-intent-before-renewal)
      TARGET="crates/execution/run-state/src/transitions.rs"
      EXPECTED_SHA="3a9f9c2d48641220f413afaec0c5ee84677e65033faf173592a87df9b561bd1e"
      NEEDLE="inserted AS ( \\
             INSERT INTO node_runs"
      REPLACEMENT="intent_mutant AS ( \\
             INSERT INTO node_runs"
      GATE="transitions::tests::attempt_intent_precedes_dispatch_and_classifies_recovery"
      TEST_ARGV=(cargo test --locked -p wamn-run-state --lib "$GATE" -- --exact)
      ;;
    persisted-version-recovery)
      TARGET="crates/execution/run-state/src/queue/sql.rs"
      EXPECTED_SHA="ef9c1d0d1eda6a997f3de1fff5f148e11d77fc92908e2e198412c0ac9bdce693"
      NEEDLE="r.input_json::text, r.flow_version AS flow_version"
      REPLACEMENT="r.input_json::text, 4::int AS flow_version"
      GATE="combined_claim_and_checkpoint_builders_compose_the_split_statements"
      TEST_ARGV=(cargo test --locked -p wamn-run-state --test queue "$GATE" -- --exact)
      ;;
    flow-spec-http-classifier)
      TARGET="docs/execution/FLOW-SPEC.md"
      EXPECTED_SHA="30d6d5035b31af70ef6aed1db5e8f989af1949479177612674232876ce2ce268"
      NEEDLE="HTTP verbs do not imply any class: GET and HEAD do not authorize replay, and"
      REPLACEMENT='HTTP verbs classify recovery: GET/HEAD `replay` policy-gated, and'
      GATE="flow_spec_normative_policy_rejects_legacy_recovery_classifiers"
      TEST_ARGV=(cargo test --locked -p wamn-proof-conformance --test flow_spec_recovery_authority "$GATE" -- --exact)
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
    if [[ "${TEST_ARGV[0]}" != "cargo" || "${TEST_ARGV[1]}" != "test" ]]; then
      echo "$id does not use a fixed cargo test command" >&2
      return 2
    fi
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
