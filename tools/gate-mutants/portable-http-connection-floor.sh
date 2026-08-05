#!/usr/bin/env bash
set -euo pipefail

readonly CAMPAIGN="portable-http-connection-floor"
readonly BEAD="wamn-ko5r.8"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name the shared debug target directory" >&2
  exit 2
fi

declare TARGET EXPECTED_SHA NEEDLE REPLACEMENT GATE
declare -a TEST_ARGV

mutation_ids() {
  printf '%s\n' \
    node-authorization-bypass \
    wrong-attempt-attribution \
    wrong-run-attribution \
    base-path-escape \
    bare-portable-spelling \
    send-without-durable-intent
}

load_mutation() {
  local id="$1"
  case "$id" in
    node-authorization-bypass)
      TARGET="crates/platform/runtime/src/plugins/connection_http.rs"
      EXPECTED_SHA="2d162602606ef17392a69b5ab6c3ab8fbb8f27555592542601ef2a0b0dda46dd"
      NEEDLE='if snapshot.node_connection.as_deref() != Some(requirement_name) || !snapshot.node_permitted {'
      REPLACEMENT='if snapshot.node_connection.as_deref() != Some(requirement_name) {'
      GATE="plugins::connection_http::tests::refusal_precedence_is_explicit_and_typed"
      TEST_ARGV=(cargo test --locked -p wamn-runtime --lib "$GATE" -- --exact)
      ;;
    wrong-attempt-attribution)
      TARGET="crates/platform/runtime/src/plugins/connection_http.rs"
      EXPECTED_SHA="2d162602606ef17392a69b5ab6c3ab8fbb8f27555592542601ef2a0b0dda46dd"
      NEEDLE='        || !snapshot.attempt_matches'
      REPLACEMENT=''
      GATE="plugins::connection_http::tests::wrong_attempt_and_wrong_run_identity_fail_before_authorization"
      TEST_ARGV=(cargo test --locked -p wamn-runtime --lib "$GATE" -- --exact)
      ;;
    wrong-run-attribution)
      TARGET="crates/platform/runtime/src/plugins/connection_http.rs"
      EXPECTED_SHA="2d162602606ef17392a69b5ab6c3ab8fbb8f27555592542601ef2a0b0dda46dd"
      NEEDLE='        || snapshot.admitted_artifact_digest.as_deref() != Some(context.artifact_digest.as_str())'
      REPLACEMENT=''
      GATE="plugins::connection_http::tests::wrong_attempt_and_wrong_run_identity_fail_before_authorization"
      TEST_ARGV=(cargo test --locked -p wamn-runtime --lib "$GATE" -- --exact)
      ;;
    base-path-escape)
      TARGET="crates/platform/runtime/src/connection_authority.rs"
      EXPECTED_SHA="4c7d9cafea7941b241720c9c8fc1d627e676ab48c7014678858b37b2d66dc50e"
      NEEDLE='    validate_target_url(connection, &target)?;'
      REPLACEMENT='    // mutant: base containment validation skipped'
      GATE="connection_authority::tests::resolved_target_outside_base_is_refused"
      TEST_ARGV=(cargo test --locked -p wamn-runtime --lib "$GATE" -- --exact)
      ;;
    bare-portable-spelling)
      TARGET="crates/node/manifest/src/http_operation_fingerprint.rs"
      EXPECTED_SHA="34f3b03db0cdb0d943e2be5429b6269faf5935c891ea53f05ddb3e670167ce9d"
      NEEDLE="    let Some(canonical) = portable.strip_prefix('/') else {"
      REPLACEMENT="    let Some(canonical) = Some(portable.strip_prefix('/').unwrap_or(portable)) else {"
      GATE="endpoint_and_environment_injections_fail_closed"
      TEST_ARGV=(cargo test --locked -p wamn-node-manifest --test http_operation_fingerprint "$GATE" -- --exact)
      ;;
    send-without-durable-intent)
      TARGET="crates/platform/runtime/src/plugins/connection_http.rs"
      EXPECTED_SHA="2d162602606ef17392a69b5ab6c3ab8fbb8f27555592542601ef2a0b0dda46dd"
      NEEDLE='    if !snapshot.attempt_recorded {'
      REPLACEMENT='    if false && !snapshot.attempt_recorded {'
      GATE="plugins::connection_http::tests::durable_intent_is_required_before_the_wire_path"
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
  [[ "$actual" == "$EXPECTED_SHA" ]] || {
    echo "$TARGET hash mismatch: expected $EXPECTED_SHA, got $actual" >&2
    return 2
  }
  count="$(TARGET="$TARGET" NEEDLE="$NEEDLE" python3 -c \
    'import os, pathlib; print(pathlib.Path(os.environ["TARGET"]).read_text().count(os.environ["NEEDLE"]))')"
  [[ "$count" == 1 ]] || {
    echo "$TARGET must contain the mutation anchor exactly once (found $count)" >&2
    return 2
  }
}

replace_once() {
  TARGET="$TARGET" NEEDLE="$NEEDLE" REPLACEMENT="$REPLACEMENT" python3 -c \
    'import os, pathlib; p=pathlib.Path(os.environ["TARGET"]); s=p.read_text(); p.write_text(s.replace(os.environ["NEEDLE"], os.environ["REPLACEMENT"], 1))'
}

run_green() {
  local id="$1"
  load_mutation "$id"
  assert_precondition
  echo "GREEN campaign=$CAMPAIGN bead=$BEAD id=$id gate=$GATE target=$TARGET command=${TEST_ARGV[*]}"
  "${TEST_ARGV[@]}"
}

run_mutant() (
  local id="$1" backup_dir backup restored_sha mutant_sha exit_code
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
    [[ "$restored_sha" == "$EXPECTED_SHA" ]] || {
      echo "restore failed for $TARGET" >&2
      exit 3
    }
  }
  trap restore EXIT INT TERM
  replace_once
  mutant_sha="$(sha256 "$TARGET")"
  [[ "$mutant_sha" != "$EXPECTED_SHA" ]] || {
    echo "mutation $id did not change $TARGET" >&2
    exit 3
  }
  echo "MUTANT campaign=$CAMPAIGN bead=$BEAD id=$id gate=$GATE target=$TARGET baseline_sha256=$EXPECTED_SHA mutant_sha256=$mutant_sha command=${TEST_ARGV[*]}"
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
    printf 'CHECKED id=%s gate=%s target=%s sha256=%s\n' \
      "$id" "$GATE" "$TARGET" "$EXPECTED_SHA"
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
  *) usage; exit 2 ;;
esac
