#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-0h0g.2.5"
readonly OUTCOME="HTTP effects require the exact current plan, source resolution, and attempt"

readonly CAMPAIGN="current-plan-effect-authority"
readonly BEAD="wamn-0h0g.2.5"

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
    connection-node-permission-bypass \
    connection-attempt-bypass \
    connection-root-plan-bypass \
    connection-resolution-bypass \
    claims-root-bundle-for-current-plan \
    claims-drop-source-resolution-match \
    claims-drop-attempt-occurrence-match \
    claims-invert-plan-node-permission
}

load_mutation() {
  local id="$1"
  case "$id" in
    connection-node-permission-bypass)
      TARGET="crates/platform/runtime/src/plugins/connection_http.rs"
      EXPECTED_SHA="fa581bea81e9afa26b167ad11efbd47bfa3e8fd0616750207d8745bf5289b2d7"
      NEEDLE='if !snapshot.node_permitted {'
      REPLACEMENT='if false {'
      GATE="plugins::connection_http::tests::refusal_precedence_is_explicit_and_typed"
      TEST_ARGV=(cargo test --locked -p wamn-runtime --lib "$GATE" -- --exact)
      ;;
    connection-attempt-bypass)
      TARGET="crates/platform/runtime/src/plugins/connection_http.rs"
      EXPECTED_SHA="fa581bea81e9afa26b167ad11efbd47bfa3e8fd0616750207d8745bf5289b2d7"
      NEEDLE='|| !snapshot.attempt_matches'
      REPLACEMENT='|| false'
      GATE="plugins::connection_http::tests::wrong_attempt_and_wrong_run_identity_fail_before_authorization"
      TEST_ARGV=(cargo test --locked -p wamn-runtime --lib "$GATE" -- --exact)
      ;;
    connection-root-plan-bypass)
      TARGET="crates/platform/runtime/src/plugins/connection_http.rs"
      EXPECTED_SHA="fa581bea81e9afa26b167ad11efbd47bfa3e8fd0616750207d8745bf5289b2d7"
      NEEDLE='|| !snapshot.root_plan_matches'
      REPLACEMENT='|| false'
      GATE="plugins::connection_http::tests::wrong_attempt_and_wrong_run_identity_fail_before_authorization"
      TEST_ARGV=(cargo test --locked -p wamn-runtime --lib "$GATE" -- --exact)
      ;;
    connection-resolution-bypass)
      TARGET="crates/platform/runtime/src/plugins/connection_http.rs"
      EXPECTED_SHA="fa581bea81e9afa26b167ad11efbd47bfa3e8fd0616750207d8745bf5289b2d7"
      NEEDLE='|| !snapshot.resolution_matches'
      REPLACEMENT='|| false'
      GATE="plugins::connection_http::tests::mismatched_source_or_revoked_draft_generation_is_denied_before_network_data"
      TEST_ARGV=(cargo test --locked -p wamn-runtime --lib "$GATE" -- --exact)
      ;;
    claims-root-bundle-for-current-plan)
      TARGET="crates/platform/runtime/src/plugins/wamn_postgres/claims.rs"
      EXPECTED_SHA="e67c2ee071aec3fbc7d69b84cfaace0fad52129674bfe944f1317cc75fd9da1b"
      NEEDLE='WHERE bundle.execution_bundle_hash = $3 \'
      REPLACEMENT='WHERE bundle.execution_bundle_hash = $2 \'
      GATE="plugins::wamn_postgres::claims::tests::live_effect_authority_uses_callee_plan_and_exact_attempt"
      TEST_ARGV=(cargo test --locked -p wamn-runtime --lib "$GATE" -- --ignored --exact --nocapture)
      ;;
    claims-drop-source-resolution-match)
      TARGET="crates/platform/runtime/src/plugins/wamn_postgres/claims.rs"
      EXPECTED_SHA="e67c2ee071aec3fbc7d69b84cfaace0fad52129674bfe944f1317cc75fd9da1b"
      NEEDLE='AND resolution.source_artifact_hash = $7 \'
      REPLACEMENT='AND true \'
      GATE="plugins::wamn_postgres::claims::tests::live_effect_authority_uses_callee_plan_and_exact_attempt"
      TEST_ARGV=(cargo test --locked -p wamn-runtime --lib "$GATE" -- --ignored --exact --nocapture)
      ;;
    claims-drop-attempt-occurrence-match)
      TARGET="crates/platform/runtime/src/plugins/wamn_postgres/claims.rs"
      EXPECTED_SHA="e67c2ee071aec3fbc7d69b84cfaace0fad52129674bfe944f1317cc75fd9da1b"
      NEEDLE='AND attempt.occurrence = $6 \'
      REPLACEMENT='AND true \'
      GATE="plugins::wamn_postgres::claims::tests::live_effect_authority_uses_callee_plan_and_exact_attempt"
      TEST_ARGV=(cargo test --locked -p wamn-runtime --lib "$GATE" -- --ignored --exact --nocapture)
      ;;
    claims-invert-plan-node-permission)
      TARGET="crates/platform/runtime/src/plugins/wamn_postgres/claims.rs"
      EXPECTED_SHA="e67c2ee071aec3fbc7d69b84cfaace0fad52129674bfe944f1317cc75fd9da1b"
      NEEDLE='COALESCE(plan_node.match_count = 1 AND plan_node.permitted, false)'
      REPLACEMENT='COALESCE(plan_node.match_count = 1 AND NOT plan_node.permitted, false)'
      GATE="plugins::wamn_postgres::claims::tests::live_effect_authority_uses_callee_plan_and_exact_attempt"
      TEST_ARGV=(cargo test --locked -p wamn-runtime --lib "$GATE" -- --ignored --exact --nocapture)
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
    ;;
  *)
    usage
    exit 2
    ;;
esac
