#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-0h0g.5.14"
readonly OUTCOME="control artifact reads are credential-bound, tenant-scoped, bounded, and cache only fully verified plans"
readonly CAMPAIGN="control-artifact-reader"
readonly BEAD="wamn-0h0g.5.14"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name the serialized debug target directory" >&2
  exit 2
fi
export CARGO_INCREMENTAL=0
export CARGO_BUILD_JOBS=2

declare TARGET EXPECTED_SHA NEEDLE REPLACEMENT EXPECTED_COUNT GATE PACKAGE FEATURES
declare -a TEST_ARGV

mutation_ids() {
  printf '%s\n' \
    credential-validation-bypass \
    application-name-drift \
    tenant-scope-bypass \
    malformed-request-bypass \
    fetch-on-cache-hit \
    row-tenant-bypass \
    unrequested-row-bypass \
    duplicate-row-bypass \
    format-version-bypass \
    byte-length-bypass \
    digest-binding-bypass \
    malformed-plan-bypass \
    missing-kind-collapse \
    timeout-kind-collapse \
    unavailable-kind-collapse \
    retry-disabled \
    retry-timeout \
    retry-cap-widened \
    tenant-query-bypass \
    projection-widened \
    timeout-bounds-widened
}

load_mutation() {
  local id="$1"
  TARGET="crates/platform/runtime/src/plugins/control_artifact_reader.rs"
  EXPECTED_SHA="b8ccb76cfede49b253355e78f6c5b580c11647cb2cfceebfc403bd57b72acc5b"
  PACKAGE="wamn-runtime"
  FEATURES=""
  EXPECTED_COUNT=1
  case "$id" in
    credential-validation-bypass)
      TARGET="crates/control/provision/src/artifact_reader.rs"
      EXPECTED_SHA="cb357c412965cf57934703e5294412baf45113e4828f2aad35839982872e1043"
      PACKAGE="wamn-control-provision"
      FEATURES="postgres-config"
      NEEDLE='    validate_artifact_reader_credential(&credential, expected, expected_endpoint, now)?;'
      REPLACEMENT='    let _ = (expected, expected_endpoint, now);'
      GATE="artifact_reader::tests::native_connection_handoff_validates_first_and_keeps_url_opaque"
      ;;
    application-name-drift)
      TARGET="crates/control/provision/src/artifact_reader.rs"
      EXPECTED_SHA="cb357c412965cf57934703e5294412baf45113e4828f2aad35839982872e1043"
      PACKAGE="wamn-control-provision"
      FEATURES="postgres-config"
      NEEDLE='    config.application_name(ARTIFACT_READER_APPLICATION_NAME);'
      REPLACEMENT='    config.application_name("wrong-artifact-reader");'
      GATE="artifact_reader::tests::native_connection_handoff_validates_first_and_keeps_url_opaque"
      ;;
    tenant-scope-bypass)
      NEEDLE='        if tenant_id != self.tenant_id.as_ref() {'
      REPLACEMENT='        if false {'
      GATE="plugins::control_artifact_reader::tests::tenant_mismatch_refuses_before_store_or_cache_access"
      ;;
    malformed-request-bypass)
      NEEDLE='            .any(|hash| !valid_execution_bundle_hash(hash))'
      REPLACEMENT='            .any(|_| false)'
      GATE="plugins::control_artifact_reader::tests::malformed_requested_hash_refuses_before_store_access"
      ;;
    fetch-on-cache-hit)
      NEEDLE='        if !missing.is_empty() {'
      REPLACEMENT='        if true {'
      GATE="plugins::control_artifact_reader::tests::first_read_fills_and_second_read_performs_no_store_call"
      ;;
    row-tenant-bypass)
      NEEDLE='        if row.tenant_id != tenant_id'
      REPLACEMENT='        if false'
      GATE="plugins::control_artifact_reader::tests::format_length_and_coordinate_drift_are_malformed_before_fill"
      ;;
    unrequested-row-bypass)
      NEEDLE='            || !requested.contains(row.execution_bundle_hash.as_str())'
      REPLACEMENT='            || false'
      GATE="plugins::control_artifact_reader::tests::unrequested_and_duplicate_rows_are_refused_before_fill"
      ;;
    duplicate-row-bypass)
      NEEDLE='            || verified.contains_key(&row.execution_bundle_hash)'
      REPLACEMENT='            || false'
      GATE="plugins::control_artifact_reader::tests::unrequested_and_duplicate_rows_are_refused_before_fill"
      ;;
    format-version-bypass)
      NEEDLE='        if row.format_version != EXECUTION_BUNDLE_FORMAT_VERSION {'
      REPLACEMENT='        if false {'
      GATE="plugins::control_artifact_reader::tests::format_length_and_coordinate_drift_are_malformed_before_fill"
      ;;
    byte-length-bypass)
      NEEDLE='        if usize::try_from(row.byte_length).ok() != Some(row.exact_bytes.len()) {'
      REPLACEMENT='        if false {'
      GATE="plugins::control_artifact_reader::tests::format_length_and_coordinate_drift_are_malformed_before_fill"
      ;;
    digest-binding-bypass)
      NEEDLE='wamn_catalog::read_execution_plan(&row.execution_bundle_hash, &row.exact_bytes)'
      REPLACEMENT='wamn_catalog::read_execution_plan(&wamn_catalog::execution_bundle_hash(&row.exact_bytes), &row.exact_bytes)'
      GATE="plugins::control_artifact_reader::tests::malformed_and_hash_mismatch_never_enter_the_cache"
      ;;
    malformed-plan-bypass)
      NEEDLE=$'            Err(error) => {\n                return Err(ControlArtifactReaderError::with_source(\n                    ControlArtifactReaderErrorKind::Malformed,\n                    "control artifact bytes are not a valid execution plan",\n                    error,\n                ));\n            }'
      REPLACEMENT='            Err(_) => {}'
      GATE="plugins::control_artifact_reader::tests::malformed_and_hash_mismatch_never_enter_the_cache"
      ;;
    missing-kind-collapse)
      NEEDLE='    if verified.len() != requested.len() {'
      REPLACEMENT='    if false {'
      GATE="plugins::control_artifact_reader::tests::missing_timeout_and_unavailable_remain_distinct"
      ;;
    timeout-kind-collapse)
      NEEDLE='        ArtifactSourceErrorKind::Timeout => ControlArtifactReaderErrorKind::Timeout,'
      REPLACEMENT='        ArtifactSourceErrorKind::Timeout => ControlArtifactReaderErrorKind::Unavailable,'
      GATE="plugins::control_artifact_reader::tests::missing_timeout_and_unavailable_remain_distinct"
      ;;
    unavailable-kind-collapse)
      NEEDLE='        ArtifactSourceErrorKind::Unavailable => ControlArtifactReaderErrorKind::Unavailable,'
      REPLACEMENT='        ArtifactSourceErrorKind::Unavailable => ControlArtifactReaderErrorKind::Timeout,'
      GATE="plugins::control_artifact_reader::tests::missing_timeout_and_unavailable_remain_distinct"
      ;;
    retry-disabled)
      NEEDLE='                        if error.kind == ArtifactSourceErrorKind::Unavailable'
      REPLACEMENT='                        if false'
      GATE="plugins::control_artifact_reader::tests::unavailable_reads_retry_once_while_timeouts_do_not_retry"
      ;;
    retry-timeout)
      NEEDLE='                        if error.kind == ArtifactSourceErrorKind::Unavailable'
      REPLACEMENT='                        if true'
      GATE="plugins::control_artifact_reader::tests::unavailable_reads_retry_once_while_timeouts_do_not_retry"
      ;;
    retry-cap-widened)
      NEEDLE='const CONTROL_ARTIFACT_MAX_ATTEMPTS: usize = 2;'
      REPLACEMENT='const CONTROL_ARTIFACT_MAX_ATTEMPTS: usize = 3;'
      GATE="plugins::control_artifact_reader::tests::unavailable_reads_retry_once_while_timeouts_do_not_retry"
      ;;
    tenant-query-bypass)
      NEEDLE=' WHERE tenant_id = $1 \'
      REPLACEMENT=' WHERE TRUE \'
      GATE="plugins::control_artifact_reader::tests::query_is_exactly_tenant_scoped_and_projects_only_authorized_columns"
      ;;
    projection-widened)
      NEEDLE='SELECT tenant_id, execution_bundle_hash, format_version, exact_bytes, byte_length \'
      REPLACEMENT='SELECT tenant_id, execution_bundle_hash, format_version, exact_bytes, byte_length, created_at \'
      GATE="plugins::control_artifact_reader::tests::query_is_exactly_tenant_scoped_and_projects_only_authorized_columns"
      ;;
    timeout-bounds-widened)
      NEEDLE=$'const CONTROL_ARTIFACT_POOL_WAIT: Duration = Duration::from_secs(2);\nconst CONTROL_ARTIFACT_QUERY_TIMEOUT: Duration = Duration::from_secs(5);\nconst CONTROL_ARTIFACT_CANCEL_TIMEOUT: Duration = Duration::from_secs(2);\nconst CONTROL_ARTIFACT_STATEMENT_TIMEOUT_MS: u32 = 5_000;'
      REPLACEMENT=$'const CONTROL_ARTIFACT_POOL_WAIT: Duration = Duration::from_secs(30);\nconst CONTROL_ARTIFACT_QUERY_TIMEOUT: Duration = Duration::from_secs(30);\nconst CONTROL_ARTIFACT_CANCEL_TIMEOUT: Duration = Duration::from_secs(30);\nconst CONTROL_ARTIFACT_STATEMENT_TIMEOUT_MS: u32 = 30_000;'
      GATE="plugins::control_artifact_reader::tests::query_is_exactly_tenant_scoped_and_projects_only_authorized_columns"
      ;;
    *)
      echo "unknown mutant: $id" >&2
      return 2
      ;;
  esac
  TEST_ARGV=(cargo test --locked --offline -p "$PACKAGE")
  if [[ -n "$FEATURES" ]]; then
    TEST_ARGV+=(--features "$FEATURES")
  fi
  TEST_ARGV+=(--lib "$GATE" -- --exact)
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
  [[ "$count" == "$EXPECTED_COUNT" ]] || {
    echo "$TARGET must contain mutation anchor $EXPECTED_COUNT time(s) (found $count)" >&2
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
