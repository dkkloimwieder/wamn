#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-0h0g.8.3"
readonly OUTCOME="capture remains immutable full-or-off with fail-closed storage and projection"

readonly CAMPAIGN="capture-mode"
readonly BEAD="wamn-0h0g.8.3"
readonly EXPECTED_PROFILE="debug"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name the shared debug target directory" >&2
  exit 2
fi
command -v perl >/dev/null || {
  echo "perl is required for byte-exact mutation replacement" >&2
  exit 2
}

declare TARGET EXPECTED_SHA NEEDLE REPLACEMENT GATE
declare -a TEST_ARGV

mutation_ids() {
  printf '%s\n' \
    default-draft-capture-off \
    default-run-column-full \
    permit-full-on-nondraft-run \
    allow-post-admission-mode-change \
    grant-author-capture-mode \
    capture-off-writes-node-io \
    retain-over-ceiling-output \
    omit-output-too-large-projection \
    skip-full-output-redaction \
    rederive-async-capture-off \
    retain-capture-off-error-detail
}

load_mutation() {
  local id="$1"
  case "$id" in
    default-draft-capture-off)
      TARGET="crates/authoring/model/src/lib.rs"
      EXPECTED_SHA="d60a5396b2f80aa4ff87656867bf263fff98e8d3c075ae3eb2ecc0f07d550990"
      NEEDLE='    #[default]
    Full,
    Off,'
      REPLACEMENT='    Full,
    #[default]
    Off,'
      GATE="draft_run_capture_defaults_to_full_and_accepts_only_full_or_off"
      TEST_ARGV=(cargo test --locked --offline -p wamn-authoring-model --test contract "$GATE" -- --exact)
      ;;
    default-run-column-full)
      TARGET="deploy/sql/run-state.sql"
      EXPECTED_SHA="ed139ec1c41a7dbcf9fc565ee74102d9573eac5f24d19eb51b353153972f381a"
      NEEDLE="capture_mode    text NOT NULL DEFAULT 'off'"
      REPLACEMENT="capture_mode    text NOT NULL DEFAULT 'full'"
      GATE="run_state_sql_matches_the_model"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --test store "$GATE" -- --exact)
      ;;
    permit-full-on-nondraft-run)
      TARGET="deploy/sql/run-state.sql"
      EXPECTED_SHA="ed139ec1c41a7dbcf9fc565ee74102d9573eac5f24d19eb51b353153972f381a"
      NEEDLE="capture_mode <> 'full' OR trigger_source IS NOT DISTINCT FROM 'scenario-draft'"
      REPLACEMENT="capture_mode <> 'full' OR true"
      GATE="run_state_sql_matches_the_model"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --test store "$GATE" -- --exact)
      ;;
    allow-post-admission-mode-change)
      TARGET="deploy/sql/run-state.sql"
      EXPECTED_SHA="ed139ec1c41a7dbcf9fc565ee74102d9573eac5f24d19eb51b353153972f381a"
      NEEDLE='       OR NEW.capture_mode IS DISTINCT FROM OLD.capture_mode THEN'
      REPLACEMENT='       OR false THEN'
      GATE="run_state_sql_matches_the_model"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --test store "$GATE" -- --exact)
      ;;
    grant-author-capture-mode)
      TARGET="deploy/sql/run-state.sql"
      EXPECTED_SHA="ed139ec1c41a7dbcf9fc565ee74102d9573eac5f24d19eb51b353153972f381a"
      NEEDLE='    fail_kind, fail_node, fail_reason, created_at, updated_at
), UPDATE ('
      REPLACEMENT='    fail_kind, fail_node, fail_reason, created_at, updated_at, capture_mode
), UPDATE ('
      GATE="run_state_sql_matches_the_model"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --test store "$GATE" -- --exact)
      ;;
    capture-off-writes-node-io)
      TARGET="crates/execution/run-state/src/capture.rs"
      EXPECTED_SHA="d737209d977e2e348d650c7473906b30cf42fa0a82bf82283229deae24e7f80f"
      NEEDLE='    if mode == CaptureMode::Off {'
      REPLACEMENT='    if false {'
      GATE="capture::tests::off_records_no_node_io_facts"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --lib "$GATE" -- --exact)
      ;;
    retain-over-ceiling-output)
      TARGET="crates/execution/run-state/src/capture.rs"
      EXPECTED_SHA="d737209d977e2e348d650c7473906b30cf42fa0a82bf82283229deae24e7f80f"
      NEEDLE='        output_json: (raw_output.len() <= OUTPUT_CAPTURE_CEILING_BYTES).then_some(scrubbed_output),'
      REPLACEMENT='        output_json: (raw_output.len() >= OUTPUT_CAPTURE_CEILING_BYTES).then_some(scrubbed_output),'
      GATE="capture::tests::over_ceiling_output_records_size_and_projects_typed_metadata"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --lib "$GATE" -- --exact)
      ;;
    omit-output-too-large-projection)
      TARGET="crates/execution/run-state/src/capture.rs"
      EXPECTED_SHA="d737209d977e2e348d650c7473906b30cf42fa0a82bf82283229deae24e7f80f"
      NEEDLE='        (None, Some(size)) => Some(NodeOutputProjection::OutputTooLarge(OutputTooLarge {
            kind: OutputTooLargeKind::OutputTooLarge,
            size,
            hash: payload_hash,
        })),'
      REPLACEMENT='        (None, Some(_)) => None,'
      GATE="capture::tests::over_ceiling_output_records_size_and_projects_typed_metadata"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --lib "$GATE" -- --exact)
      ;;
    skip-full-output-redaction)
      TARGET="crates/execution/run-state/src/capture.rs"
      EXPECTED_SHA="d737209d977e2e348d650c7473906b30cf42fa0a82bf82283229deae24e7f80f"
      NEEDLE='    scrub(&mut scrubbed_output);'
      REPLACEMENT='    let _ = &mut scrubbed_output;'
      GATE="capture::tests::full_scrubs_stored_input_and_output"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --lib "$GATE" -- --exact)
      ;;
    rederive-async-capture-off)
      TARGET="crates/execution/run-state/src/queue/sql.rs"
      EXPECTED_SHA="acfe9ad630d6e9bf7857408a9fa442451e284f99c1589145d04c5bd20d3e21ca"
      NEEDLE='                r.capture_mode AS capture_mode, \
'
      REPLACEMENT="                'off'::text AS capture_mode, \\
"
      GATE="combined_claim_and_checkpoint_builders_compose_the_split_statements"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --test queue "$GATE" -- --exact)
      ;;
    retain-capture-off-error-detail)
      TARGET="components/execution/flowrunner/src/lib.rs"
      EXPECTED_SHA="1bc244bb02f9a872e2e9ba204972683a0cf521058a79d693f41279ece75cb2c4"
      NEEDLE='    let detail = if capture_detail {
        wamn_run_state::capture::scrub(&mut detail);
        jsonb(&detail)
    } else {
        SqlValue::Null
    };'
      REPLACEMENT='    let detail = if capture_detail {
        wamn_run_state::capture::scrub(&mut detail);
        jsonb(&detail)
    } else {
        jsonb(&detail)
    };'
      GATE="tests::capture_off_error_binds_retain_only_the_typed_kind"
      TEST_ARGV=(cargo test --locked --offline --manifest-path components/Cargo.toml -p flowrunner "$GATE" -- --exact)
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
    $count += s/\Q$needle\E//g;
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
    $count += s/\Q$needle\E/$replacement/g;
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
  echo "GREEN campaign=$CAMPAIGN bead=$BEAD profile=$EXPECTED_PROFILE id=$id gate=$GATE target=$TARGET command=${TEST_ARGV[*]}"
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

  echo "MUTANT campaign=$CAMPAIGN id=$id gate=$GATE target=$TARGET baseline_sha256=$EXPECTED_SHA mutant_sha256=$mutant_sha command=${TEST_ARGV[*]}"
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
