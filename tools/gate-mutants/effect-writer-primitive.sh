#!/usr/bin/env bash
set -euo pipefail

readonly CAMPAIGN="effect-writer-primitive"
readonly BEAD="wamn-0h0g.4.9"
readonly EXPECTED_PROFILE="debug"

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
    restore-attempt-key-column \
    bypass-populated-ledger-preflight \
    allow-login-stable-writer-role \
    grant-attempt-insert-to-app \
    accept-divergent-attempt-retry \
    return-existing-dispatch-as-permit \
    accept-divergent-outcome-retry \
    drop-tenant-binding \
    hard-code-writer-schema \
    omit-pg-temp-search-path-sentinel \
    accept-noncanonical-host-schema \
    add-schema-to-credential-document \
    omit-secret-validity-metadata \
    bypass-credential-scope \
    bypass-credential-expiry \
    make-stable-acl-role-login \
    grant-generation-wrong-membership \
    ignore-public-connect-floor \
    retain-public-temporary \
    hard-code-stable-ledger-schema \
    skip-unpublished-generation-abort \
    skip-old-generation-retirement \
    activate-production-effect-call
}

load_mutation() {
  local id="$1"
  case "$id" in
    restore-attempt-key-column)
      TARGET="deploy/sql/run-state.sql"
      EXPECTED_SHA="8009b85401a5e3cfda2d7a5b0444fe84f67b9c9772153abdb4cd8cd2c01f764c"
      NEEDLE='    attempt_input_ref text NOT NULL,'
      REPLACEMENT='    attempt_input_ref text NOT NULL,
    attempt_key text,'
      GATE="transitions::tests::effect_attempt_schema_has_one_identity_and_no_successor_shape"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --lib "$GATE" -- --exact)
      ;;
    bypass-populated-ledger-preflight)
      TARGET="crates/schema/control/src/run_plane.rs"
      EXPECTED_SHA="fca678cc9fbc70a6f515e708a22bab6ace4a6ad1adf0e9e32195c02dfeeb4c7c"
      NEEDLE='DO $retire$
BEGIN
    IF {populated} THEN'
      REPLACEMENT='DO $retire$
BEGIN
    IF false THEN'
      GATE="run_plane_reconcile_live"
      TEST_ARGV=(cargo test --locked --offline -p wamn-ctl --test run_plane_live "$GATE" -- --exact --nocapture)
      ;;
    allow-login-stable-writer-role)
      TARGET="crates/schema/control/src/run_plane.rs"
      EXPECTED_SHA="fca678cc9fbc70a6f515e708a22bab6ace4a6ad1adf0e9e32195c02dfeeb4c7c"
      NEEDLE="WHERE rolname = 'wamn_effect_writer' AND NOT rolcanlogin \\"
      REPLACEMENT="WHERE rolname = 'wamn_effect_writer' \\"
      GATE="run_plane_reconcile_live"
      TEST_ARGV=(cargo test --locked --offline -p wamn-ctl --test run_plane_live "$GATE" -- --exact --nocapture)
      ;;
    grant-attempt-insert-to-app)
      TARGET="deploy/sql/run-state.sql"
      EXPECTED_SHA="8009b85401a5e3cfda2d7a5b0444fe84f67b9c9772153abdb4cd8cd2c01f764c"
      NEEDLE='GRANT SELECT, INSERT ON wamn_run.effect_attempts TO wamn_effect_writer;'
      REPLACEMENT='GRANT SELECT, INSERT ON wamn_run.effect_attempts TO wamn_app;'
      GATE="run_plane::tests::effect_writer_surface_uses_acl_not_insert_authorization_triggers"
      TEST_ARGV=(cargo test --locked --offline -p wamn-schema-control --lib "$GATE" -- --exact)
      ;;
    accept-divergent-attempt-retry)
      TARGET="crates/execution/run-state/src/effect_writer.rs"
      EXPECTED_SHA="7ba49a9cbf6c4f1b00ccb320d45d3c794ca676c8c0ed7275efd6ff87eff08bde"
      NEEDLE='           $19::text::timestamptz, $20::text)"#,'
      REPLACEMENT='           $19::text::timestamptz,
           CASE WHEN $20::text IS NULL THEN attempt_input_ref ELSE attempt_input_ref END)"#,'
      GATE="native_effect_writer_live"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --features native --test effect_writer_live "$GATE" -- --ignored --exact --nocapture)
      ;;
    return-existing-dispatch-as-permit)
      TARGET="crates/execution/run-state/src/effect_writer.rs"
      EXPECTED_SHA="7ba49a9cbf6c4f1b00ccb320d45d3c794ca676c8c0ed7275efd6ff87eff08bde"
      NEEDLE='ON CONFLICT DO NOTHING
RETURNING attempt_id::text, dispatched_at::text'
      REPLACEMENT='ON CONFLICT (tenant_id, run_id, frame_id, local_node_id, occurrence)
    DO UPDATE SET dispatched_at = effect_attempt_dispatches.dispatched_at
RETURNING attempt_id::text, dispatched_at::text'
      GATE="effect_writer::tests::dispatch_permit_is_only_a_returned_new_coordinate_row"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --features native --lib "$GATE" -- --exact)
      ;;
    accept-divergent-outcome-retry)
      TARGET="crates/execution/run-state/src/effect_writer.rs"
      EXPECTED_SHA="7ba49a9cbf6c4f1b00ccb320d45d3c794ca676c8c0ed7275efd6ff87eff08bde"
      NEEDLE='   AND outcome.outcome_status IS NOT DISTINCT FROM $3::text'
      REPLACEMENT='   AND true'
      GATE="effect_writer::tests::outcome_builder_accepts_only_an_identical_retry"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --features native --lib "$GATE" -- --exact)
      ;;
    drop-tenant-binding)
      TARGET="crates/execution/run-state/src/effect_writer.rs"
      EXPECTED_SHA="7ba49a9cbf6c4f1b00ccb320d45d3c794ca676c8c0ed7275efd6ff87eff08bde"
      NEEDLE="SELECT pg_catalog.set_config('app.tenant', \$1::text, true),"
      REPLACEMENT="SELECT pg_catalog.set_config('app.tenant_mutant', \$1::text, true),"
      GATE="effect_writer::tests::writer_statements_use_only_the_host_bound_search_path"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --features native --lib "$GATE" -- --exact)
      ;;
    hard-code-writer-schema)
      TARGET="crates/execution/run-state/src/effect_writer.rs"
      EXPECTED_SHA="7ba49a9cbf6c4f1b00ccb320d45d3c794ca676c8c0ed7275efd6ff87eff08bde"
      NEEDLE='r#"INSERT INTO effect_attempts'
      REPLACEMENT='r#"INSERT INTO wamn_run.effect_attempts'
      GATE="effect_writer::tests::writer_statements_use_only_the_host_bound_search_path"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --features native --lib "$GATE" -- --exact)
      ;;
    omit-pg-temp-search-path-sentinel)
      TARGET="crates/execution/run-state/src/effect_writer.rs"
      EXPECTED_SHA="7ba49a9cbf6c4f1b00ccb320d45d3c794ca676c8c0ed7275efd6ff87eff08bde"
      NEEDLE="pg_catalog.quote_ident(\$2::text) || ', pg_catalog, pg_temp', true)\","
      REPLACEMENT="pg_catalog.quote_ident(\$2::text) || ', pg_catalog', true)\","
      GATE="effect_writer::tests::writer_statements_use_only_the_host_bound_search_path"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --features native --lib "$GATE" -- --exact)
      ;;
    accept-noncanonical-host-schema)
      TARGET="crates/execution/run-state/src/effect_writer.rs"
      EXPECTED_SHA="7ba49a9cbf6c4f1b00ccb320d45d3c794ca676c8c0ed7275efd6ff87eff08bde"
      NEEDLE='        && bytes
            .iter()
            .all(|byte| matches!(byte, b'"'"'A'"'"'..=b'"'"'Z'"'"' | b'"'"'a'"'"'..=b'"'"'z'"'"' | b'"'"'0'"'"'..=b'"'"'9'"'"' | b'"'"'_'"'"'))'
      REPLACEMENT='        && true'
      GATE="effect_writer::tests::host_schema_identity_is_canonical_before_it_is_bound"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --features native --lib "$GATE" -- --exact)
      ;;
    add-schema-to-credential-document)
      TARGET="crates/control/provision/src/secret.rs"
      EXPECTED_SHA="9a65bc324564e5362229c43531d2f6595fc994f2734bf4812aede589a8d99887"
      NEEDLE='            (EFFECT_WRITER_CREDENTIAL_KEY): serde_json::to_string(credential)
                .expect("effect-writer credential serializes"),'
      REPLACEMENT='            (EFFECT_WRITER_CREDENTIAL_KEY): ({
                let mut document = serde_json::to_value(credential)
                    .expect("effect-writer credential serializes");
                document.as_object_mut()
                    .expect("credential object")
                    .insert(
                        "schema".to_string(),
                        Value::String("wamn_run".to_string()),
                    );
                serde_json::to_string(&document)
                    .expect("effect-writer credential serializes")
            }),'
      GATE="secret::tests::effect_writer_secret_and_document_are_fixed_mount_exact"
      TEST_ARGV=(cargo test --locked --offline -p wamn-control-provision --lib "$GATE" -- --exact)
      ;;
    omit-secret-validity-metadata)
      TARGET="crates/control/provision/src/secret.rs"
      EXPECTED_SHA="9a65bc324564e5362229c43531d2f6595fc994f2734bf4812aede589a8d99887"
      NEEDLE='        (
            "wamn.io/not-before".to_string(),
            Value::String(field("not-before").to_string()),
        ),'
      REPLACEMENT=''
      GATE="secret::tests::effect_writer_secret_and_document_are_fixed_mount_exact"
      TEST_ARGV=(cargo test --locked --offline -p wamn-control-provision --lib "$GATE" -- --exact)
      ;;
    bypass-credential-scope)
      TARGET="crates/execution/run-state/src/effect_writer_credential.rs"
      EXPECTED_SHA="9e20c4bdc7542cf82805f4783c29a5aa2179db169c2ebd5ed418cbfd88888c0e"
      NEEDLE='    if credential.org != expected.org'
      REPLACEMENT='    if false'
      GATE="effect_writer_credential::tests::mismatched_scope_and_expired_window_refuse"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --features effect-writer-credential --lib "$GATE" -- --exact)
      ;;
    bypass-credential-expiry)
      TARGET="crates/execution/run-state/src/effect_writer_credential.rs"
      EXPECTED_SHA="9e20c4bdc7542cf82805f4783c29a5aa2179db169c2ebd5ed418cbfd88888c0e"
      NEEDLE='    if now >= expires_at {'
      REPLACEMENT='    if false {'
      GATE="effect_writer_credential::tests::mismatched_scope_and_expired_window_refuse"
      TEST_ARGV=(cargo test --locked --offline -p wamn-run-state --features effect-writer-credential --lib "$GATE" -- --exact)
      ;;
    make-stable-acl-role-login)
      TARGET="crates/control/provision/src/sql.rs"
      EXPECTED_SHA="6a99148caf523cd0235eed4832591c26699ae6fa0a94fe18faa62279784dac08"
      NEEDLE='             CREATE ROLE {role} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \'
      REPLACEMENT='             CREATE ROLE {role} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \'
      GATE="sql::tests::effect_writer_acl_role_is_stable_nologin_and_owns_no_grants_here"
      TEST_ARGV=(cargo test --locked --offline -p wamn-control-provision --lib "$GATE" -- --exact)
      ;;
    grant-generation-wrong-membership)
      TARGET="crates/control/provision/src/sql.rs"
      EXPECTED_SHA="6a99148caf523cd0235eed4832591c26699ae6fa0a94fe18faa62279784dac08"
      NEEDLE='        expires_at = quote_literal(expires_at),
        acl_role = quote_ident(EFFECT_WRITER_ROLE),'
      REPLACEMENT='        expires_at = quote_literal(expires_at),
        acl_role = quote_ident(APP_ROLE),'
      GATE="sql::tests::generation_prepare_has_only_login_membership_and_project_connect"
      TEST_ARGV=(cargo test --locked --offline -p wamn-control-provision --lib "$GATE" -- --exact)
      ;;
    ignore-public-connect-floor)
      TARGET="crates/control/provision/src/sql.rs"
      EXPECTED_SHA="6a99148caf523cd0235eed4832591c26699ae6fa0a94fe18faa62279784dac08"
      NEEDLE="WHERE acl.grantee = 0 AND acl.privilege_type = 'CONNECT')"
      REPLACEMENT='WHERE false)'
      GATE="sql::tests::generation_cluster_probes_are_read_only_and_scope_locked"
      TEST_ARGV=(cargo test --locked --offline -p wamn-control-provision --lib "$GATE" -- --exact)
      ;;
    retain-public-temporary)
      TARGET="crates/control/provision/src/sql.rs"
      EXPECTED_SHA="6a99148caf523cd0235eed4832591c26699ae6fa0a94fe18faa62279784dac08"
      NEEDLE='REVOKE CONNECT, TEMPORARY ON DATABASE {db} FROM PUBLIC; \
         GRANT CONNECT ON DATABASE {db} TO {role};'
      REPLACEMENT='REVOKE CONNECT ON DATABASE {db} FROM PUBLIC; \
         GRANT CONNECT ON DATABASE {db} TO {role};'
      GATE="sql::tests::grant_connect_on_database_targets_an_arbitrary_db_name"
      TEST_ARGV=(cargo test --locked --offline -p wamn-control-provision --lib "$GATE" -- --exact)
      ;;
    hard-code-stable-ledger-schema)
      TARGET="services/ctl/src/provision_project_env.rs"
      EXPECTED_SHA="d3e46bb185c03dfd490fbbdcfa056f79bc6b97fb0484dafae10be32572f374eb"
      NEEDLE='    for (schema, actual) in by_schema {'
      REPLACEMENT='    for (schema, actual) in by_schema {
        anyhow::ensure!(schema == "wamn_run", "mutant hard-coded writer schema");'
      GATE="provision_project_env::tests::stable_acl_inventory_accepts_only_a_complete_host_schema_ledger_set"
      TEST_ARGV=(cargo test --locked --offline -p wamn-ctl --lib "$GATE" -- --exact)
      ;;
    skip-unpublished-generation-abort)
      TARGET="deploy/mvp/bootstrap.sh"
      EXPECTED_SHA="517c2497e155e3f5dbfa4303ad2491369d6cedd9aed0c9cd7a4dd531de91bf2c"
      NEEDLE='abort_prepared_effect_writer_then_fail() {
    local generation=$1 message=$2
    effect_writer_prepared_generation=$generation
    if ! abort_prepared_effect_writer "$generation"; then'
      REPLACEMENT='abort_prepared_effect_writer_then_fail() {
    local generation=$1 message=$2
    effect_writer_prepared_generation=$generation
    if false; then'
      GATE="deploy/mvp/tests/bootstrap.sh"
      TEST_ARGV=(bash deploy/mvp/tests/bootstrap.sh)
      ;;
    skip-old-generation-retirement)
      TARGET="deploy/mvp/bootstrap.sh"
      EXPECTED_SHA="517c2497e155e3f5dbfa4303ad2491369d6cedd9aed0c9cd7a4dd531de91bf2c"
      NEEDLE='        if [[ $old_login == t ]]; then
            "$ctl_bin" provision-project-env \'
      REPLACEMENT='        if false; then
            "$ctl_bin" provision-project-env \'
      GATE="deploy/mvp/tests/bootstrap.sh"
      TEST_ARGV=(bash deploy/mvp/tests/bootstrap.sh)
      ;;
    activate-production-effect-call)
      TARGET="crates/execution/host/src/effect_writer.rs"
      EXPECTED_SHA="819277627e994e172d2bd4ec395500ce82e9ead0df458685d4d1dbd040ec06f2"
      NEEDLE='    Ok(Some(client))'
      REPLACEMENT='    if false {
        let _ = client
            .acquire_dispatch(wamn_run_state::EffectAttemptId { attempt_id: "" })
            .await?;
    }
    Ok(Some(client))'
      GATE="effect_writer::tests::retained_private_client_has_no_effect_operation_caller"
      TEST_ARGV=(cargo test --locked --offline -p wamn-execution-host --lib "$GATE" -- --exact)
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
  count="$(TARGET="$TARGET" NEEDLE="$NEEDLE" python3 -c \
    'import os, pathlib; print(pathlib.Path(os.environ["TARGET"]).read_text().count(os.environ["NEEDLE"]))')"
  if [[ "$count" != 1 ]]; then
    echo "$TARGET must contain the mutation anchor exactly once (found $count)" >&2
    return 2
  fi
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
  echo "GREEN campaign=$CAMPAIGN bead=$BEAD profile=$EXPECTED_PROFILE id=$id gate=$GATE target=$TARGET command=${TEST_ARGV[*]}"
  run_gate
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
