#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-0h0g.7.1"
readonly OUTCOME="flow invocation replays one durable run and preserves derived-key and typed HTTP outcomes"
readonly CAMPAIGN="flow-invocation-replay"
readonly BEAD="wamn-0h0g.7.1"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name the serialized debug target directory" >&2
  exit 2
fi
command -v perl >/dev/null || {
  echo "perl is required for byte-exact mutation replacement" >&2
  exit 2
}
export CARGO_INCREMENTAL=0
export CARGO_BUILD_JOBS=2

declare TARGET EXPECTED_SHA NEEDLE REPLACEMENT GATE
declare -a TEST_ARGV

mutation_ids() {
  printf '%s\n' \
    in-flight-recovery-becomes-refusal \
    promotion-derived-key-recheck-bypass \
    visible-duplicate-winner-becomes-refusal \
    duplicate-winner-recovery-bypass \
    own-plan-derived-key-bypass \
    storage-client-key-restores-not-null \
    timeout-retry-literal-changed \
    effect-uncertain-status-changed
}

load_mutation() {
  local id="$1"
  case "$id" in
    in-flight-recovery-becomes-refusal)
      TARGET="crates/platform/runtime/src/flow_invocation.rs"
      EXPECTED_SHA="b2a7955ec4301689f4732e25c362f8f8d7caf2c6e9268187ad557ee25c1e239f"
      NEEDLE='        InvocationRecovery::InFlight { run_id } => Some(BeginResult::Admitted(Admitted { run_id })),'
      REPLACEMENT='        InvocationRecovery::InFlight { run_id: _ } => Some(rejected(409, "in-flight")),'
      GATE="flow_invocation::tests::in_flight_recovery_returns_the_same_run_without_admission"
      TEST_ARGV=(cargo test --locked -p wamn-runtime --lib "$GATE" -- --exact)
      ;;
    promotion-derived-key-recheck-bypass)
      TARGET="crates/platform/runtime/src/flow_invocation.rs"
      EXPECTED_SHA="b2a7955ec4301689f4732e25c362f8f8d7caf2c6e9268187ad557ee25c1e239f"
      NEEDLE='                            if next.idempotency_required
                                && admission.request.idempotency_key.is_none()
                            {'
      REPLACEMENT='                            if false {'
      GATE="flow_invocation::tests::promotion_cannot_add_a_key_requirement_after_a_keyless_preflight"
      TEST_ARGV=(cargo test --locked -p wamn-runtime --lib "$GATE" -- --exact)
      ;;
    visible-duplicate-winner-becomes-refusal)
      TARGET="crates/platform/runtime/src/flow_invocation.rs"
      EXPECTED_SHA="b2a7955ec4301689f4732e25c362f8f8d7caf2c6e9268187ad557ee25c1e239f"
      NEEDLE='                AdmissionResult::Duplicate {
                    run_id: Some(run_id),
                } => {
                    return Ok(BeginResult::Admitted(Admitted { run_id }));
                }'
      REPLACEMENT='                AdmissionResult::Duplicate {
                    run_id: Some(_),
                } => {
                    return Ok(rejected(409, "admission-retry"));
                }'
      GATE="flow_invocation::tests::admission_visible_duplicate_returns_the_winning_run"
      TEST_ARGV=(cargo test --locked -p wamn-runtime --lib "$GATE" -- --exact)
      ;;
    duplicate-winner-recovery-bypass)
      TARGET="crates/platform/runtime/src/flow_invocation.rs"
      EXPECTED_SHA="b2a7955ec4301689f4732e25c362f8f8d7caf2c6e9268187ad557ee25c1e239f"
      NEEDLE='                    return Ok(recovered_begin(recovery)
                        .unwrap_or_else(|| rejected(409, "admission-retry")));'
      REPLACEMENT='                    let _ = recovery;
                    return Ok(rejected(409, "admission-retry"));'
      GATE="flow_invocation::tests::concurrent_duplicate_recovers_the_winning_run_after_insert_visibility"
      TEST_ARGV=(cargo test --locked -p wamn-runtime --lib "$GATE" -- --exact)
      ;;
    own-plan-derived-key-bypass)
      TARGET="crates/execution/run-state/src/invocation.rs"
      EXPECTED_SHA="b079186ef68a5b2e5805517f190b864a237242a1acb1911dc98d01fe79177e3d"
      NEEDLE='        Ok(plan) => plan.requires_idempotency_key(),'
      REPLACEMENT='        Ok(_) => false,'
      GATE="invocation::tests::own_plan_requires_a_key_for_effectful_or_call_flow_only"
      TEST_ARGV=(cargo test --locked -p wamn-run-state --lib "$GATE" -- --exact)
      ;;
    storage-client-key-restores-not-null)
      TARGET="crates/schema/control/src/run_plane.rs"
      EXPECTED_SHA="a2bfb016786fd6d400da654584a9332c8162a0216c6ca049f507d822a8536417"
      NEEDLE='            alterations.push("ALTER COLUMN client_key_digest DROP NOT NULL".to_string());'
      REPLACEMENT='            alterations.push("ALTER COLUMN client_key_digest SET NOT NULL".to_string());'
      GATE="run_plane::tests::invocation_admission_retention_cutover_is_exact_and_idempotent"
      TEST_ARGV=(cargo test --locked -p wamn-schema-control --lib "$GATE" -- --exact)
      ;;
    timeout-retry-literal-changed)
      TARGET="components/ingress/flow-http/src/lib.rs"
      EXPECTED_SHA="d67afd2558e66133512021eda6fd676200982ddd13753a73d859cc52512fa452"
      NEEDLE='                retry: "same-idempotency-key",'
      REPLACEMENT='                retry: "new-idempotency-key",'
      GATE="wait_is_finite_and_disconnect_detaches_without_mutating_the_run"
      TEST_ARGV=(cargo test --locked --manifest-path components/Cargo.toml -p flow-http --test adversarial "$GATE" -- --exact)
      ;;
    effect-uncertain-status-changed)
      TARGET="crates/platform/runtime/src/flow_invocation.rs"
      EXPECTED_SHA="b2a7955ec4301689f4732e25c362f8f8d7caf2c6e9268187ad557ee25c1e239f"
      NEEDLE='const EFFECT_UNCERTAIN_HTTP_STATUS: u16 = 502;'
      REPLACEMENT='const EFFECT_UNCERTAIN_HTTP_STATUS: u16 = 500;'
      GATE="flow_invocation::tests::effect_uncertain_decodes_only_the_non_committal_stored_identity"
      TEST_ARGV=(cargo test --locked -p wamn-runtime --lib "$GATE" -- --exact)
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
  local actual count
  actual="$(sha256 "$TARGET")"
  if [[ "$actual" != "$EXPECTED_SHA" ]]; then
    echo "$TARGET hash mismatch: expected $EXPECTED_SHA, got $actual" >&2
    return 2
  fi
  count="$(TARGET="$TARGET" NEEDLE="$NEEDLE" perl -0ne '
    BEGIN { $needle = $ENV{NEEDLE}; $count = 0 }
    $count += () = /\Q$needle\E/g;
    END { print $count }
  ' "$TARGET")"
  if [[ "$count" != 1 ]]; then
    echo "$TARGET must contain the mutation anchor exactly once; found $count" >&2
    return 2
  fi
}

replace_once() {
  TARGET="$TARGET" NEEDLE="$NEEDLE" REPLACEMENT="$REPLACEMENT" perl -0pi -e '
    BEGIN {
      $needle = $ENV{NEEDLE};
      $replacement = $ENV{REPLACEMENT};
      $count = 0;
    }
    $count += s/\Q$needle\E/$replacement/;
    END { exit($count == 1 ? 0 : 1) }
  ' "$TARGET"
}

run_gate() {
  "${TEST_ARGV[@]}"
}

run_green() {
  local id="$1"
  load_mutation "$id"
  assert_precondition
  echo "GREEN campaign=$CAMPAIGN bead=$BEAD id=$id gate=$GATE target=$TARGET command=${TEST_ARGV[*]}"
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
  }
  trap restore EXIT INT TERM

  replace_once
  mutant_sha="$(sha256 "$TARGET")"
  if [[ "$mutant_sha" == "$EXPECTED_SHA" ]]; then
    echo "mutation $id did not change $TARGET" >&2
    exit 3
  fi

  echo "MUTANT campaign=$CAMPAIGN bead=$BEAD id=$id gate=$GATE target=$TARGET baseline_sha256=$EXPECTED_SHA mutant_sha256=$mutant_sha command=${TEST_ARGV[*]}"
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
    echo "flow invocation replay mutation campaign: 8/8 killed"
    ;;
  *)
    usage
    exit 2
    ;;
esac
