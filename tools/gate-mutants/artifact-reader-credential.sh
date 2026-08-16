#!/usr/bin/env bash
set -euo pipefail

readonly OWNER="bd:wamn-0h0g.9.10"
readonly OUTCOME="the control artifact reader preserves tenant, credential, rotation, and revocation boundaries"
readonly CAMPAIGN="artifact-reader-credential"
readonly BEAD="wamn-0h0g.9.10"
readonly EXPECTED_PROFILE="debug"
readonly LIVE_PROOF="services/ctl/tests/artifact_reader_generation_live.rs"
readonly LIVE_PROOF_SHA="8be3c99b8c5466b565774d028178fda8353468327f3759b6e11185ee2945311e"

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
  echo "CARGO_TARGET_DIR must name the dedicated debug target directory" >&2
  exit 2
fi

declare TARGET EXPECTED_SHA NEEDLE REPLACEMENT GATE
declare -a TEST_ARGV

mutation_ids() {
  printf '%s\n' \
    stable-role-login \
    generation-set-option \
    generation-wrong-parent \
    grant-table-select \
    grant-created-at-select \
    make-policy-permissive \
    drop-restrictive-policy \
    bind-policy-to-app-tenant \
    skip-public-schema-floor \
    skip-rls-enforcement-probe \
    skip-public-authority-probe \
    skip-public-parameter-authority \
    allow-published-secret-data \
    emit-secret-before-verification \
    skip-publish-rollback \
    accept-any-replacement-session \
    ignore-published-secret-retirement \
    skip-old-authority-revoke \
    require-old-generation-unexpired \
    delete-secret-before-database-revoke \
    relax-generation-marker \
    relax-credential-scope \
    expose-credential-url-debug \
    add-secret-key
}

set_live_gate() {
  GATE="artifact_reader_generation_lifecycle_is_exact_and_fail_closed"
  TEST_ARGV=(
    cargo test --locked --offline -p wamn-ctl
    --test artifact_reader_generation_live "$GATE"
    -- --ignored --exact --nocapture
  )
}

set_sql_gate() {
  GATE="sql::tests::$1"
  TEST_ARGV=(cargo test --locked --offline -p wamn-control-provision --lib "$GATE" -- --exact)
}

set_artifact_gate() {
  GATE="artifact_reader::tests::$1"
  TEST_ARGV=(cargo test --locked --offline -p wamn-control-provision --lib "$GATE" -- --exact)
}

set_secret_gate() {
  GATE="secret::tests::$1"
  TEST_ARGV=(cargo test --locked --offline -p wamn-control-provision --lib "$GATE" -- --exact)
}

set_publish_order_gate() {
  GATE="artifact-reader-publish-secret-after-verification"
  TEST_ARGV=(
    python3 -c
    'from pathlib import Path
s=Path("services/ctl/src/provision_project_env.rs").read_text()
b=s[s.index("async fn publish_artifact_reader_generation("):s.index("async fn rollback_artifact_reader_generation(")]
assert b.count("write_secret_json(")==1
w=b.index("write_secret_json(")
assert b.index("verify_artifact_reader_public_access_floor(") < w
assert b.index("authenticate_artifact_reader(") < w
assert b.index("validate_artifact_reader_generation_state(") < w
assert b.index("verify_artifact_reader_tenant_role(") < w
assert b.index("verify_role_acl_inventory(") < w
assert b.index("validate_artifact_reader_credential(") < w'
  )
}

set_revoke_order_gate() {
  GATE="artifact-reader-secret-delete-after-database-revoke"
  TEST_ARGV=(
    python3 -c
    'from pathlib import Path
s=Path("services/ctl/src/provision_project_env.rs").read_text()
b=s[s.index("async fn revoke_artifact_reader_credential("):s.index("const PAT_TTL")]
assert b.count("std::fs::remove_file(secret_path)")==1
d=b.index("std::fs::remove_file(secret_path)")
assert b.index(".commit()") < d
assert b.index("terminate_artifact_reader_sessions(") < d
assert b.index("verify_artifact_reader_effective_authority(") < d'
  )
}

load_mutation() {
  local id="$1"
  case "$id" in
    stable-role-login)
      TARGET="crates/control/provision/src/sql.rs"
      EXPECTED_SHA="f3bda841b71c686184767d8944f02a64e10465bcc107d85653bb4d7dc4078aa3"
      NEEDLE='CREATE ROLE {role_ident} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS; \'
      REPLACEMENT='CREATE ROLE {role_ident} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS; \'
      set_sql_gate artifact_reader_stable_install_is_exactly_tenant_scoped_read_only
      ;;
    generation-set-option)
      TARGET="crates/control/provision/src/sql.rs"
      EXPECTED_SHA="f3bda841b71c686184767d8944f02a64e10465bcc107d85653bb4d7dc4078aa3"
      NEEDLE='WITH ADMIN FALSE, INHERIT TRUE, SET FALSE; \'
      REPLACEMENT='WITH ADMIN FALSE, INHERIT TRUE, SET TRUE; \'
      set_sql_gate artifact_reader_generation_prepare_and_retire_preserve_exact_order
      ;;
    generation-wrong-parent)
      TARGET="crates/control/provision/src/sql.rs"
      EXPECTED_SHA="f3bda841b71c686184767d8944f02a64e10465bcc107d85653bb4d7dc4078aa3"
      NEEDLE='    let stable_ident = quote_ident(stable_role);'
      REPLACEMENT='    let stable_ident = quote_ident("wamn_app");'
      set_sql_gate artifact_reader_generation_prepare_and_retire_preserve_exact_order
      ;;
    grant-table-select)
      TARGET="crates/control/provision/src/sql.rs"
      EXPECTED_SHA="f3bda841b71c686184767d8944f02a64e10465bcc107d85653bb4d7dc4078aa3"
      NEEDLE='GRANT SELECT (tenant_id, execution_bundle_hash, format_version, \
           exact_bytes, byte_length) ON TABLE catalog.execution_bundles TO {role_ident}; \'
      REPLACEMENT='GRANT SELECT ON TABLE catalog.execution_bundles TO {role_ident}; \'
      set_sql_gate artifact_reader_stable_install_is_exactly_tenant_scoped_read_only
      ;;
    grant-created-at-select)
      TARGET="crates/control/provision/src/sql.rs"
      EXPECTED_SHA="f3bda841b71c686184767d8944f02a64e10465bcc107d85653bb4d7dc4078aa3"
      NEEDLE='GRANT SELECT (tenant_id, execution_bundle_hash, format_version, \
           exact_bytes, byte_length) ON TABLE catalog.execution_bundles TO {role_ident}; \'
      REPLACEMENT='GRANT SELECT (tenant_id, execution_bundle_hash, format_version, \
           exact_bytes, byte_length, created_at) ON TABLE catalog.execution_bundles TO {role_ident}; \'
      set_sql_gate artifact_reader_stable_install_is_exactly_tenant_scoped_read_only
      ;;
    make-policy-permissive)
      TARGET="crates/control/provision/src/sql.rs"
      EXPECTED_SHA="f3bda841b71c686184767d8944f02a64e10465bcc107d85653bb4d7dc4078aa3"
      NEEDLE='AS RESTRICTIVE FOR SELECT TO {role_ident} \'
      REPLACEMENT='AS PERMISSIVE FOR SELECT TO {role_ident} \'
      set_sql_gate artifact_reader_stable_install_is_exactly_tenant_scoped_read_only
      ;;
    drop-restrictive-policy)
      TARGET="crates/control/provision/src/sql.rs"
      EXPECTED_SHA="f3bda841b71c686184767d8944f02a64e10465bcc107d85653bb4d7dc4078aa3"
      NEEDLE='IF NOT EXISTS (SELECT FROM pg_policy \
             WHERE polrelid = '\''catalog.execution_bundles'\''::regclass \
               AND polname = {policy_lit}) THEN \'
      REPLACEMENT='IF false THEN \'
      set_live_gate
      ;;
    bind-policy-to-app-tenant)
      TARGET="crates/control/provision/src/sql.rs"
      EXPECTED_SHA="f3bda841b71c686184767d8944f02a64e10465bcc107d85653bb4d7dc4078aa3"
      NEEDLE='               USING (tenant_id = {tenant}); \'
      REPLACEMENT='               USING (tenant_id = NULLIF(current_setting('\''app.tenant'\'', true), '\'''\'')); \'
      set_sql_gate artifact_reader_stable_install_is_exactly_tenant_scoped_read_only
      ;;
    skip-public-schema-floor)
      TARGET="crates/control/provision/src/sql.rs"
      EXPECTED_SHA="f3bda841b71c686184767d8944f02a64e10465bcc107d85653bb4d7dc4078aa3"
      NEEDLE='pub fn artifact_reader_revoke_public_schema_floor_sql() -> &'\''static str {
    "REVOKE ALL ON SCHEMA public FROM PUBLIC;"
}'
      REPLACEMENT='pub fn artifact_reader_revoke_public_schema_floor_sql() -> &'\''static str {
    "SELECT 1;"
}'
      set_live_gate
      ;;
    skip-rls-enforcement-probe)
      TARGET="services/ctl/src/provision_project_env.rs"
      EXPECTED_SHA="e654286724bbf03e40d08e0691d6fb0249082af4069fd599c025ba4ff0527cfe"
      NEEDLE='        rls.get::<_, bool>(0) && rls.get::<_, bool>(1),'
      REPLACEMENT='        true,'
      set_live_gate
      ;;
    skip-public-authority-probe)
      TARGET="services/ctl/src/provision_project_env.rs"
      EXPECTED_SHA="e654286724bbf03e40d08e0691d6fb0249082af4069fd599c025ba4ff0527cfe"
      NEEDLE='        public.is_empty(),'
      REPLACEMENT='        true,'
      set_live_gate
      ;;
    skip-public-parameter-authority)
      TARGET="crates/control/provision/src/sql.rs"
      EXPECTED_SHA="f3bda841b71c686184767d8944f02a64e10465bcc107d85653bb4d7dc4078aa3"
      NEEDLE='       SELECT '\''parameter'\'', '\'''\''::text, p.parname::text, x.privilege_type::text \
         FROM pg_parameter_acl p CROSS JOIN LATERAL aclexplode(p.paracl) x \
        WHERE x.grantee=0 \
       UNION ALL \
       SELECT '\''default-acl'\'''
      REPLACEMENT='       SELECT '\''parameter'\'', '\'''\''::text, p.parname::text, x.privilege_type::text \
         FROM pg_parameter_acl p CROSS JOIN LATERAL aclexplode(p.paracl) x \
        WHERE false \
       UNION ALL \
       SELECT '\''default-acl'\'''
      set_live_gate
      ;;
    allow-published-secret-data)
      TARGET="services/ctl/src/provision_project_env.rs"
      EXPECTED_SHA="e654286724bbf03e40d08e0691d6fb0249082af4069fd599c025ba4ff0527cfe"
      NEEDLE='        manifest.get("data").is_none(),'
      REPLACEMENT='        true,'
      set_live_gate
      ;;
    emit-secret-before-verification)
      TARGET="services/ctl/src/provision_project_env.rs"
      EXPECTED_SHA="e654286724bbf03e40d08e0691d6fb0249082af4069fd599c025ba4ff0527cfe"
      NEEDLE=') -> anyhow::Result<()> {
    verify_artifact_reader_public_access_floor(admin).await?;'
      REPLACEMENT=') -> anyhow::Result<()> {
    let premature_url = artifact_reader_url(admin_url, role, password, &scope.database)?;
    let premature_credential =
        artifact_reader_credential(scope, credential_id, generation, validity, &premature_url);
    let premature_secret =
        render_artifact_reader_secret_manifest(&args.namespace, &premature_credential);
    write_secret_json(secret_path, &premature_secret)
        .context("mutant emitted artifact-reader Secret before verification")?;
    verify_artifact_reader_public_access_floor(admin).await?;'
      set_publish_order_gate
      ;;
    skip-publish-rollback)
      TARGET="services/ctl/src/provision_project_env.rs"
      EXPECTED_SHA="e654286724bbf03e40d08e0691d6fb0249082af4069fd599c025ba4ff0527cfe"
      NEEDLE='    if let Err(error) = publish_result {
        let rollback = rollback_artifact_reader_generation('
      REPLACEMENT='    if let Err(error) = publish_result {
        return Err(error);
        #[allow(unreachable_code)]
        let rollback = rollback_artifact_reader_generation('
      set_live_gate
      ;;
    accept-any-replacement-session)
      TARGET="crates/control/provision/src/sql.rs"
      EXPECTED_SHA="f3bda841b71c686184767d8944f02a64e10465bcc107d85653bb4d7dc4078aa3"
      NEEDLE='pub fn artifact_reader_replacement_use_sql() -> &'\''static str {
    "SELECT count(*)::bigint FROM pg_catalog.pg_stat_activity \
      WHERE usename=$1 AND datname=$2 AND application_name=$3"
}'
      REPLACEMENT='pub fn artifact_reader_replacement_use_sql() -> &'\''static str {
    "SELECT count(*)::bigint FROM pg_catalog.pg_stat_activity \
      WHERE datname=$2 AND $1::text IS NOT NULL AND $3::text IS NOT NULL"
}'
      set_live_gate
      ;;
    ignore-published-secret-retirement)
      TARGET="services/ctl/src/provision_project_env.rs"
      EXPECTED_SHA="e654286724bbf03e40d08e0691d6fb0249082af4069fd599c025ba4ff0527cfe"
      NEEDLE='    anyhow::ensure!(
        published.generation() == generation.other() && published.role() == replacement_role,
        "normal retirement must preserve the generation named by the published artifact-reader Secret"
    );'
      REPLACEMENT='    anyhow::ensure!(
        true,
        "normal retirement must preserve the generation named by the published artifact-reader Secret"
    );'
      set_live_gate
      ;;
    skip-old-authority-revoke)
      TARGET="crates/control/provision/src/sql.rs"
      EXPECTED_SHA="f3bda841b71c686184767d8944f02a64e10465bcc107d85653bb4d7dc4078aa3"
      NEEDLE='"REVOKE {stable_role} FROM {generation_role}; \'
      REPLACEMENT='"SELECT 1; \'
      set_live_gate
      ;;
    require-old-generation-unexpired)
      TARGET="services/ctl/src/provision_project_env.rs"
      EXPECTED_SHA="e654286724bbf03e40d08e0691d6fb0249082af4069fd599c025ba4ff0527cfe"
      NEEDLE='    validate_artifact_reader_active_generation_state(
        &old,
        scope,
        generation,
        &stable_role,
        false,
        Utc::now(),
    )?;'
      REPLACEMENT='    validate_artifact_reader_active_generation_state(
        &old,
        scope,
        generation,
        &stable_role,
        true,
        Utc::now(),
    )?;'
      set_live_gate
      ;;
    delete-secret-before-database-revoke)
      TARGET="services/ctl/src/provision_project_env.rs"
      EXPECTED_SHA="e654286724bbf03e40d08e0691d6fb0249082af4069fd599c025ba4ff0527cfe"
      NEEDLE=') -> anyhow::Result<()> {
    let tenant_scope = scope.tenant_scope();
    let stable_role = artifact_reader_tenant_role(&scope.tenant_id, &scope.database);
    let roles = [CredentialGeneration::A, CredentialGeneration::B].map(|generation| {'
      REPLACEMENT=') -> anyhow::Result<()> {
    let _ = std::fs::remove_file(secret_path);
    let tenant_scope = scope.tenant_scope();
    let stable_role = artifact_reader_tenant_role(&scope.tenant_id, &scope.database);
    let roles = [CredentialGeneration::A, CredentialGeneration::B].map(|generation| {'
      set_revoke_order_gate
      ;;
    relax-generation-marker)
      TARGET="crates/control/provision/src/artifact_reader.rs"
      EXPECTED_SHA="30e32b4e104d846e227d38561178f748b6cce1926d8c626dfc4ffe568ff6deff"
      NEEDLE='    if actual != expected_marker {
        return Err(ArtifactReaderCredentialError::new(
            ArtifactReaderCredentialErrorKind::Marker,
            "artifact-reader generation role marker does not match its scoped role",'
      REPLACEMENT='    if false {
        return Err(ArtifactReaderCredentialError::new(
            ArtifactReaderCredentialErrorKind::Marker,
            "artifact-reader generation role marker does not match its scoped role",'
      set_artifact_gate role_markers_are_closed_and_scope_bound
      ;;
    relax-credential-scope)
      TARGET="crates/control/provision/src/artifact_reader.rs"
      EXPECTED_SHA="30e32b4e104d846e227d38561178f748b6cce1926d8c626dfc4ffe568ff6deff"
      NEEDLE='    if credential.tenant_id != expected.tenant_id
        || credential.org != expected.org
        || credential.project != expected.project
        || credential.environment != expected.environment
        || credential.database != expected.database'
      REPLACEMENT='    if false'
      set_artifact_gate credential_rejects_unknown_fields_and_scope_drift
      ;;
    expose-credential-url-debug)
      TARGET="crates/control/provision/src/artifact_reader.rs"
      EXPECTED_SHA="30e32b4e104d846e227d38561178f748b6cce1926d8c626dfc4ffe568ff6deff"
      NEEDLE='            .field("url", &"<redacted>")'
      REPLACEMENT='            .field("url", &self.url)'
      set_artifact_gate credential_round_trips_and_redacts_secret_material
      ;;
    add-secret-key)
      TARGET="crates/control/provision/src/secret.rs"
      EXPECTED_SHA="ea6778e09d14241e398dc394fd063ed4a324d6a9a6ab86e252fbd4746d0005d6"
      NEEDLE='            (ARTIFACT_READER_CREDENTIAL_KEY): serde_json::to_string(credential)
                .expect("artifact-reader credential serializes"),'
      REPLACEMENT='            (ARTIFACT_READER_CREDENTIAL_KEY): serde_json::to_string(credential)
                .expect("artifact-reader credential serializes"),
            "extra.json": "mutant",'
      set_secret_gate artifact_reader_secret_and_document_are_fixed_mount_exact
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
  local actual count proof_sha
  proof_sha="$(sha256 "$LIVE_PROOF")"
  if [[ "$proof_sha" != "$LIVE_PROOF_SHA" ]]; then
    echo "$LIVE_PROOF hash mismatch: expected $LIVE_PROOF_SHA, got $proof_sha" >&2
    return 2
  fi
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
