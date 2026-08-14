#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-0h0g.4.1"
readonly OUTCOME="one fenced host transaction classifies, resolves, and grants the global FIFO root claim"
readonly CAMPAIGN="global-fifo-claim"
readonly BEAD="wamn-0h0g.4.1"
readonly EXPECTED_PROFILE="debug"

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

declare TARGET EXPECTED_SHA NEEDLE REPLACEMENT GATE EXPECTED_COUNT
declare -a TEST_ARGV

mutation_ids() {
  printf '%s\n' \
    fifo-run-id-before-stream-seq \
    concurrent-claimer-uses-nowait \
    effect-attempt-classified-pre-effect \
    claimant-skips-effect-intent-fence \
    writer-skips-runnable-recheck \
    pre-effect-keeps-node-projections \
    effect-uncertain-overwrites-released-caller \
    resolution-map-materialization-bypassed \
    candidate-override-ignored \
    resolution-refusal-overwrites-released-caller \
    resolution-refusal-releases-callerless \
    janitor-reaps-effect-attempt \
    janitor-hash-diverges-from-jcs \
    exhausted-janitor-overwrites-released-caller \
    active-lease-cutover-proceeds \
    unobservable-lease-cutover-proceeds \
    dead-letter-cutover-proceeds \
    persisted-authored-ordering-cutover-proceeds
}

load_mutation() {
  local id="$1"
  case "$id" in
    fifo-run-id-before-stream-seq)
      TARGET="crates/execution/run-state/src/queue/sql.rs"
      EXPECTED_SHA="0fc07c601f2c2d7e31ead289ce2c14c6c24c23d1e4c7d5a4e0917620e398a737"
      NEEDLE='ORDER BY q.available_at, q.stream_seq, q.run_id'
      REPLACEMENT='ORDER BY q.available_at, q.run_id, q.stream_seq'
      EXPECTED_COUNT=3
      GATE="global_fifo_uses_available_stream_run_tie_break"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --test queue "$GATE" -- --exact)
      ;;
    concurrent-claimer-uses-nowait)
      TARGET="crates/execution/run-state/src/queue/sql.rs"
      EXPECTED_SHA="0fc07c601f2c2d7e31ead289ce2c14c6c24c23d1e4c7d5a4e0917620e398a737"
      NEEDLE='              FOR UPDATE OF selected_run, q SKIP LOCKED \
              LIMIT 1 \
         ) \
         SELECT candidate.tenant_id'
      REPLACEMENT='              FOR UPDATE OF selected_run, q NOWAIT \
              LIMIT 1 \
         ) \
         SELECT candidate.tenant_id'
      EXPECTED_COUNT=1
      GATE="production_claim_live"
      TEST_ARGV=(cargo test --locked --offline -p wamn-runtime --test production_claim_live "$GATE" -- --ignored --exact --nocapture --test-threads=1)
      ;;
    effect-attempt-classified-pre-effect)
      TARGET="crates/execution/run-state/src/queue/claim.rs"
      EXPECTED_SHA="a5159d8f9a6e34fd98b7a2cdd11ce373189845784f3a0d6a2b6c2028f996a8a0"
      NEEDLE='pub fn classify_production_claim(
    had_prior_lease: bool,
    has_effect_attempt: bool,
) -> ProductionClaimClass {
    if has_effect_attempt {'
      REPLACEMENT='pub fn classify_production_claim(
    had_prior_lease: bool,
    has_effect_attempt: bool,
) -> ProductionClaimClass {
    if false {'
      EXPECTED_COUNT=1
      GATE="reclaim_classifier_has_exact_three_actions"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --test queue "$GATE" -- --exact)
      ;;
    claimant-skips-effect-intent-fence)
      TARGET="crates/platform/runtime/src/plugins/wamn_postgres/production_claim.rs"
      EXPECTED_SHA="86ded382030e8d4ecc3792f19ec934f56a441fb9335449cdaf6681c805f23d38"
      NEEDLE='    let sql = serialize_effect_intent_sql();'
      REPLACEMENT='    let sql = "SELECT true".to_string();'
      EXPECTED_COUNT=1
      GATE="plugins::wamn_postgres::production_claim::tests::claim_and_reaper_fence_before_fresh_effect_snapshot"
      TEST_ARGV=(cargo test --locked --offline -p wamn-runtime --lib "$GATE" -- --exact)
      ;;
    writer-skips-runnable-recheck)
      TARGET="crates/execution/run-state/src/effect_writer.rs"
      EXPECTED_SHA="c71950c56ce4461eaa10ac501de8a668e21ed2afd713ce22c331afd77e815640"
      NEEDLE='            .query_one(effect_run_is_runnable().text(), &[&attempt.run_id])'
      REPLACEMENT='            .query_one("SELECT true", &[])'
      EXPECTED_COUNT=1
      GATE="effect_writer::tests::attempt_writer_rechecks_a_live_running_queue_row_after_the_fence"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --features native --lib "$GATE" -- --exact)
      ;;
    pre-effect-keeps-node-projections)
      TARGET="crates/execution/run-state/src/queue/sql.rs"
      EXPECTED_SHA="0fc07c601f2c2d7e31ead289ce2c14c6c24c23d1e4c7d5a4e0917620e398a737"
      NEEDLE=$'         DELETE FROM node_runs \\\n          WHERE'
      REPLACEMENT=$'         DELETE FROM node_runs \\\n          WHERE false AND'
      EXPECTED_COUNT=1
      GATE="production_claim_live"
      TEST_ARGV=(cargo test --locked --offline -p wamn-runtime --test production_claim_live "$GATE" -- --ignored --exact --nocapture --test-threads=1)
      ;;
    effect-uncertain-overwrites-released-caller)
      TARGET="crates/execution/run-state/src/queue/sql.rs"
      EXPECTED_SHA="0fc07c601f2c2d7e31ead289ce2c14c6c24c23d1e4c7d5a4e0917620e398a737"
      NEEDLE='        uncertain = RunStatus::EffectUncertain.as_sql(),
        unreleased_attached = "r.trigger_source IN ('\''http'\'','\''internal'\'','\''studio'\'') \
                               AND r.caller_released_at IS NULL",'
      REPLACEMENT='        uncertain = RunStatus::EffectUncertain.as_sql(),
        unreleased_attached = "r.trigger_source IN ('\''http'\'','\''internal'\'','\''studio'\'')",'
      EXPECTED_COUNT=1
      GATE="production_claim_live"
      TEST_ARGV=(cargo test --locked --offline -p wamn-runtime --test production_claim_live "$GATE" -- --ignored --exact --nocapture --test-threads=1)
      ;;
    resolution-map-materialization-bypassed)
      TARGET="crates/platform/runtime/src/plugins/wamn_postgres/production_claim.rs"
      EXPECTED_SHA="86ded382030e8d4ecc3792f19ec934f56a441fb9335449cdaf6681c805f23d38"
      NEEDLE='    let materialize_sql = materialize_run_flow_resolutions_sql();'
      REPLACEMENT=$'    let materialize_sql = "SELECT \'resolved\'::text, NULL::text WHERE $1::text IS NOT NULL AND $2::text IS NOT NULL".to_string();'
      EXPECTED_COUNT=1
      GATE="production_claim_live"
      TEST_ARGV=(cargo test --locked --offline -p wamn-runtime --test production_claim_live "$GATE" -- --ignored --exact --nocapture --test-threads=1)
      ;;
    candidate-override-ignored)
      TARGET="crates/platform/runtime/src/plugins/wamn_postgres/production_claim.rs"
      EXPECTED_SHA="86ded382030e8d4ecc3792f19ec934f56a441fb9335449cdaf6681c805f23d38"
      NEEDLE='        CandidateOverrideLoad::Ready(candidate) => candidate,'
      REPLACEMENT='        CandidateOverrideLoad::Ready(candidate) => {
            let _ = candidate;
            None
        },'
      EXPECTED_COUNT=1
      GATE="production_claim_live"
      TEST_ARGV=(cargo test --locked --offline -p wamn-runtime --test production_claim_live "$GATE" -- --ignored --exact --nocapture --test-threads=1)
      ;;
    resolution-refusal-overwrites-released-caller)
      TARGET="crates/execution/run-state/src/queue/sql.rs"
      EXPECTED_SHA="0fc07c601f2c2d7e31ead289ce2c14c6c24c23d1e4c7d5a4e0917620e398a737"
      NEEDLE='        failed = RunStatus::Failed.as_sql(),
        unreleased_attached = "r.trigger_source IN ('\''http'\'','\''internal'\'','\''studio'\'') \
                               AND r.caller_released_at IS NULL",'
      REPLACEMENT='        failed = RunStatus::Failed.as_sql(),
        unreleased_attached = "r.trigger_source IN ('\''http'\'','\''internal'\'','\''studio'\'')",'
      EXPECTED_COUNT=1
      GATE="production_claim_live"
      TEST_ARGV=(cargo test --locked --offline -p wamn-runtime --test production_claim_live "$GATE" -- --ignored --exact --nocapture --test-threads=1)
      ;;
    resolution-refusal-releases-callerless)
      TARGET="crates/execution/run-state/src/queue/sql.rs"
      EXPECTED_SHA="0fc07c601f2c2d7e31ead289ce2c14c6c24c23d1e4c7d5a4e0917620e398a737"
      NEEDLE='        failed = RunStatus::Failed.as_sql(),
        unreleased_attached = "r.trigger_source IN ('\''http'\'','\''internal'\'','\''studio'\'') \
                               AND r.caller_released_at IS NULL",'
      REPLACEMENT='        failed = RunStatus::Failed.as_sql(),
        unreleased_attached = "(r.trigger_source IS NULL \
                                OR r.trigger_source IN ('\''http'\'','\''internal'\'','\''studio'\'')) \
                               AND r.caller_released_at IS NULL",'
      EXPECTED_COUNT=1
      GATE="production_claim_live"
      TEST_ARGV=(cargo test --locked --offline -p wamn-runtime --test production_claim_live "$GATE" -- --ignored --exact --nocapture --test-threads=1)
      ;;
    janitor-reaps-effect-attempt)
      TARGET="crates/execution/run-state/src/queue/janitor.rs"
      EXPECTED_SHA="c50416b5686056c9d3ceef7ad0d49bfc78a385fae9281d2aeada57cfde3aa470"
      NEEDLE='    if has_effect_attempt {
        return JanitorVerdict::EffectAttempt;
    }'
      REPLACEMENT='    if false {
        return JanitorVerdict::EffectAttempt;
    }'
      EXPECTED_COUNT=1
      GATE="janitor_excludes_effect_attempts"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --test queue "$GATE" -- --exact)
      ;;
    janitor-hash-diverges-from-jcs)
      TARGET="crates/platform/runtime/src/plugins/wamn_postgres/production_claim.rs"
      EXPECTED_SHA="86ded382030e8d4ecc3792f19ec934f56a441fb9335449cdaf6681c805f23d38"
      NEEDLE='    let body_hash = wamn_flow::canonical_json_sha256(&body);'
      REPLACEMENT='    let body_hash = wamn_flow::canonical_json_sha256(&serde_json::json!({"mutant": &body}));'
      EXPECTED_COUNT=1
      GATE="plugins::wamn_postgres::production_claim::tests::janitor_failure_body_uses_host_jcs_not_database_json_text"
      TEST_ARGV=(cargo test --locked --offline -p wamn-runtime --lib "$GATE" -- --exact)
      ;;
    exhausted-janitor-overwrites-released-caller)
      TARGET="crates/execution/run-state/src/queue/sql.rs"
      EXPECTED_SHA="0fc07c601f2c2d7e31ead289ce2c14c6c24c23d1e4c7d5a4e0917620e398a737"
      NEEDLE='        infra = RunStatus::InfrastructureFailure.as_sql(),
        dispatched = RunStatus::Dispatched.as_sql(),
        running = RunStatus::Running.as_sql(),
        unreleased_attached = "r.trigger_source IN ('\''http'\'','\''internal'\'','\''studio'\'') \
                               AND r.caller_released_at IS NULL",'
      REPLACEMENT='        infra = RunStatus::InfrastructureFailure.as_sql(),
        dispatched = RunStatus::Dispatched.as_sql(),
        running = RunStatus::Running.as_sql(),
        unreleased_attached = "r.trigger_source IN ('\''http'\'','\''internal'\'','\''studio'\'')",'
      EXPECTED_COUNT=1
      GATE="production_claim_live"
      TEST_ARGV=(cargo test --locked --offline -p wamn-runtime --test production_claim_live "$GATE" -- --ignored --exact --nocapture --test-threads=1)
      ;;
    active-lease-cutover-proceeds)
      TARGET="crates/schema/control/src/run_plane.rs"
      EXPECTED_SHA="25f00a4ba18ef08cd05d0b761378a7423801046aa3cf9f70ef772480dce54ae0"
      NEEDLE='    if !active_lease_checks.is_empty() {'
      REPLACEMENT='    if false {'
      EXPECTED_COUNT=1
      GATE="run_plane::tests::legacy_partition_plane_plans_one_leading_cutover"
      TEST_ARGV=(cargo test --locked --offline -p wamn-schema-control --lib "$GATE" -- --exact)
      ;;
    unobservable-lease-cutover-proceeds)
      TARGET="crates/schema/control/src/run_plane.rs"
      EXPECTED_SHA="25f00a4ba18ef08cd05d0b761378a7423801046aa3cf9f70ef772480dce54ae0"
      NEEDLE='               IF EXISTS (SELECT 1 FROM {target}.run_queue) \
               THEN RAISE EXCEPTION USING ERRCODE = '\''55000'\'','
      REPLACEMENT='               IF false \
               THEN RAISE EXCEPTION USING ERRCODE = '\''55000'\'','
      EXPECTED_COUNT=1
      GATE="run_plane_reconcile_live"
      TEST_ARGV=(cargo test --locked --offline -p wamn-ctl --test run_plane_live "$GATE" -- --exact --nocapture --test-threads=1)
      ;;
    dead-letter-cutover-proceeds)
      TARGET="crates/schema/control/src/run_plane.rs"
      EXPECTED_SHA="25f00a4ba18ef08cd05d0b761378a7423801046aa3cf9f70ef772480dce54ae0"
      NEEDLE='               IF EXISTS (SELECT 1 FROM {target}.run_dead_letters) \
               THEN RAISE EXCEPTION USING ERRCODE = '\''55000'\'','
      REPLACEMENT='               IF false \
               THEN RAISE EXCEPTION USING ERRCODE = '\''55000'\'','
      EXPECTED_COUNT=1
      GATE="run_plane::tests::legacy_partition_plane_plans_one_leading_cutover"
      TEST_ARGV=(cargo test --locked --offline -p wamn-schema-control --lib "$GATE" -- --exact)
      ;;
    persisted-authored-ordering-cutover-proceeds)
      TARGET="crates/schema/control/src/run_plane.rs"
      EXPECTED_SHA="25f00a4ba18ef08cd05d0b761378a7423801046aa3cf9f70ef772480dce54ae0"
      NEEDLE='               IF EXISTS (SELECT 1 FROM {target}.flows \
                           WHERE graph_json ? '\''ordering'\'' \
                              OR graph_json ? '\''partition-policy'\'') \'
      REPLACEMENT='               IF false \'
      EXPECTED_COUNT=1
      GATE="run_plane_reconcile_live"
      TEST_ARGV=(cargo test --locked --offline -p wamn-ctl --test run_plane_live "$GATE" -- --exact --nocapture --test-threads=1)
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
  if [[ "$count" != "$EXPECTED_COUNT" ]]; then
    echo "$TARGET must contain $EXPECTED_COUNT mutation anchor(s); found $count" >&2
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

assert_gate_environment() {
  if [[ "$GATE" == "production_claim_live" \
      && -z "${WAMN_PRODUCTION_CLAIM_PG_URL:-}" ]]; then
    echo "WAMN_PRODUCTION_CLAIM_PG_URL must name a disposable PostgreSQL 18 database" >&2
    return 2
  fi
  if [[ "$GATE" == "run_plane_reconcile_live" \
      && -z "${WAMN_CTL_PG_URL:-}" ]]; then
    echo "WAMN_CTL_PG_URL must name a disposable PostgreSQL 18 database" >&2
    return 2
  fi
}

run_gate() {
  "${TEST_ARGV[@]}"
}

run_green() {
  local id="$1"
  load_mutation "$id"
  assert_precondition
  assert_gate_environment
  echo "GREEN campaign=$CAMPAIGN bead=$BEAD profile=$EXPECTED_PROFILE id=$id gate=$GATE target=$TARGET command=${TEST_ARGV[*]}"
  run_gate
}

run_mutant() (
  local id="$1" backup_dir backup restored_sha mutant_sha exit_code
  load_mutation "$id"
  assert_precondition
  assert_gate_environment

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

  echo "MUTANT campaign=$CAMPAIGN bead=$BEAD profile=$EXPECTED_PROFILE id=$id gate=$GATE target=$TARGET baseline_sha256=$EXPECTED_SHA mutant_sha256=$mutant_sha command=${TEST_ARGV[*]}"
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
