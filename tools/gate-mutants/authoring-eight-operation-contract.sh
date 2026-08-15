#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-0h0g.7.3"
readonly OUTCOME="the eight-operation wire cut retains its query bound and trace-only query seam"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name the shared debug target directory" >&2
  exit 2
fi

declare TARGET EXPECTED_SHA NEEDLE REPLACEMENT EXPECTED_COUNT GATE

mutation_ids() {
  printf '%s\n' query-id-bound-expanded query-trace-renamed query-ledger-seam-added \
    retry-lock-removed retry-principal-key-removed exact-retry-refused
}

load_mutation() {
  case "$1" in
    query-id-bound-expanded)
      TARGET="crates/authoring/model/src/lib.rs"
      EXPECTED_SHA="5de9aec398f7b4c4ee4ce1ce2501e0d2ceb657decfc1979ddf1a08d8d19e1536"
      NEEDLE="pub const MAX_QUERY_ID_BYTES: usize = 64;"
      REPLACEMENT="pub const MAX_QUERY_ID_BYTES: usize = 65;"
      EXPECTED_COUNT=1
      GATE="model"
      ;;
    query-trace-renamed)
      TARGET="services/scenario-worker/src/management.rs"
      EXPECTED_SHA="f5177de364f4bec29ca0bac9c8b862d348da11c3a90d7365744b49e37c4f7a36"
      NEEDLE='            "authoring_query",'
      REPLACEMENT='            "authoring_command",'
      EXPECTED_COUNT=1
      GATE="query-adapter"
      ;;
    query-ledger-seam-added)
      TARGET="services/scenario-worker/src/management.rs"
      EXPECTED_SHA="f5177de364f4bec29ca0bac9c8b862d348da11c3a90d7365744b49e37c4f7a36"
      NEEDLE='        async { Ok(empty(StatusCode::NOT_IMPLEMENTED)) }'
      REPLACEMENT='        async { /* record( */ Ok(empty(StatusCode::NOT_IMPLEMENTED)) }'
      EXPECTED_COUNT=1
      GATE="query-adapter"
      ;;
    retry-lock-removed)
      TARGET="services/scenario-worker/src/management.rs"
      EXPECTED_SHA="f5177de364f4bec29ca0bac9c8b862d348da11c3a90d7365744b49e37c4f7a36"
      NEEDLE='            LOCK_COMMAND_RETRY_SQL,'
      REPLACEMENT='            "SELECT 1",'
      EXPECTED_COUNT=1
      GATE="retry-order"
      ;;
    retry-principal-key-removed)
      TARGET="services/scenario-worker/src/management.rs"
      EXPECTED_SHA="f5177de364f4bec29ca0bac9c8b862d348da11c3a90d7365744b49e37c4f7a36"
      NEEDLE='    WHERE tenant_id = $1 AND principal_id = $2 AND command_id = $3'
      REPLACEMENT='    WHERE tenant_id = $1 AND principal_id = $1 AND command_id = $3'
      EXPECTED_COUNT=1
      GATE="retry-key"
      ;;
    exact-retry-refused)
      TARGET="services/scenario-worker/src/management.rs"
      EXPECTED_SHA="f5177de364f4bec29ca0bac9c8b862d348da11c3a90d7365744b49e37c4f7a36"
      NEEDLE='Some(existing) if existing.request_hash == request_hash => RetryDecision::Replay'
      REPLACEMENT='Some(existing) if existing.request_hash == request_hash => RetryDecision::Reuse'
      EXPECTED_COUNT=1
      GATE="retry-classifier"
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
  case "$GATE" in
    model)
      CARGO_INCREMENTAL=0 cargo test --locked --offline -p wamn-authoring-model \
        --test contract public_numeric_and_test_set_bounds_match_their_owners -- --exact
      ;;
    query-adapter)
      CARGO_INCREMENTAL=0 cargo test --locked --offline -p wamn-scenario-worker \
        --lib management::tests::query_adapter_is_trace_only_and_unmounted -- --exact
      ;;
    retry-order)
      CARGO_INCREMENTAL=0 cargo test --locked --offline -p wamn-scenario-worker \
        --lib management::tests::every_authored_mutation_and_completed_outcome_commit_atomically -- --exact
      ;;
    retry-key)
      CARGO_INCREMENTAL=0 cargo test --locked --offline -p wamn-scenario-worker \
        --lib management::tests::the_audit_statement_writes_every_attribution_column -- --exact
      ;;
    retry-classifier)
      CARGO_INCREMENTAL=0 cargo test --locked --offline -p wamn-scenario-worker \
        --lib management::tests::retry_classifier_is_exact_hash_or_reuse -- --exact
      ;;
  esac
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
    echo "authoring eight-operation mutation anchors check clean"
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
