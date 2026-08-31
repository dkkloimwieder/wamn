//! PostgreSQL 18 proofs for the control portable package/release store.

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::sync::Mutex;

const CURRENT_DATABASE_PUBLIC_CONNECT_SQL: &str =
    include_str!("../../../../test-support/fixtures/sql/current-database-public-connect.sql");
const STORE_SQL: &str = wamn_control_provision::CONTROL_PORTABLE_STORE_SQL;

static STORE: Mutex<()> = Mutex::new(());

fn psql(url: &str, script: &str) -> std::process::Output {
    let mut child = Command::new("psql")
        .arg(url)
        .args([
            "-v",
            "ON_ERROR_STOP=1",
            "-v",
            "VERBOSITY=verbose",
            "-q",
            "-f",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn psql");
    child
        .stdin
        .take()
        .expect("psql stdin")
        .write_all(script.as_bytes())
        .expect("write psql script");
    child.wait_with_output().expect("wait for psql")
}

fn psql_ok(url: &str, stage: &str, script: &str) -> String {
    let output = psql(url, script);
    assert!(
        output.status.success(),
        "{stage} failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn as_role(url: &str, role: &str, password: &str) -> String {
    let mut parsed = url::Url::parse(url).expect("the control PG URL parses");
    parsed
        .set_username(role)
        .expect("a postgres URL carries a username");
    parsed
        .set_password(Some(password))
        .expect("a postgres URL carries a password");
    parsed.into()
}

fn database(url: &str) -> String {
    let parsed = url::Url::parse(url).expect("the control PG URL parses");
    let database = parsed.path().trim_start_matches('/').to_owned();
    assert!(!database.is_empty(), "the control PG URL names a database");
    database
}

fn reset_and_apply(url: &str, before_store: &str) {
    let mut script = String::from(CURRENT_DATABASE_PUBLIC_CONNECT_SQL);
    script.push_str(
        "DROP SCHEMA IF EXISTS catalog CASCADE;\n\
         DROP SCHEMA IF EXISTS wamn_run CASCADE;\n\
         DROP SCHEMA IF EXISTS wamn_authority CASCADE;\n\
         DO $roles$ BEGIN\n\
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN\n\
             CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS;\n\
           END IF;\n\
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_portable_probe') THEN\n\
             CREATE ROLE wamn_portable_probe NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS;\n\
           END IF;\n\
         END $roles$;\n",
    );
    script.push_str(&wamn_control_provision::sql::ensure_control_author_acl_role_sql());
    script.push('\n');
    script.push_str(before_store);
    script.push_str(
        "DO $database$ BEGIN \
           EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_system', current_database()); \
         END $database$;\n\
         SET ROLE wamn_system;\n",
    );
    script.push_str(STORE_SQL);
    script.push_str("\nRESET ROLE;\n");
    psql_ok(url, "apply the control portable store", &script);
}

fn render(statement: &wamn_schema_control::SqlStatement) -> String {
    let mut sql = statement.sql.clone();
    for (index, value) in statement.params.iter().enumerate().rev() {
        sql = sql.replace(&format!("${}", index + 1), &literal(value));
    }
    sql
}

fn literal(value: &wamn_schema_control::Value) -> String {
    match value {
        wamn_schema_control::Value::Text(text)
        | wamn_schema_control::Value::NullableText(Some(text)) => {
            format!("'{}'", text.replace('\'', "''"))
        }
        wamn_schema_control::Value::NullableText(None)
        | wamn_schema_control::Value::NullableInt(None) => "NULL".to_owned(),
        wamn_schema_control::Value::Int(value)
        | wamn_schema_control::Value::NullableInt(Some(value)) => value.to_string(),
        wamn_schema_control::Value::Bool(value) => value.to_string(),
    }
}

#[test]
#[ignore = "requires disposable PostgreSQL 18 URL in WAMN_CONTROL_PORTABLE_PG_URL"]
fn current_database_connect_posture_is_exactly_scoped() {
    let url = std::env::var("WAMN_CONTROL_PORTABLE_PG_URL")
        .expect("WAMN_CONTROL_PORTABLE_PG_URL names a disposable PostgreSQL 18 database");
    let sibling = format!("wamn_portable_sibling_{}", std::process::id());
    psql_ok(
        &url,
        "create sibling database",
        &format!(
            "DROP DATABASE IF EXISTS {sibling} WITH (FORCE); \
             CREATE DATABASE {sibling}; \
             GRANT CONNECT ON DATABASE {sibling} TO PUBLIC;"
        ),
    );
    psql_ok(
        &url,
        "apply current-database posture",
        &format!(
            "BEGIN; \
             DO $contaminate$ BEGIN \
               EXECUTE format('GRANT CONNECT ON DATABASE %I TO PUBLIC', current_database()); \
             END $contaminate$; \
             {CURRENT_DATABASE_PUBLIC_CONNECT_SQL} \
             {CURRENT_DATABASE_PUBLIC_CONNECT_SQL} \
             DO $proof$ BEGIN \
               ASSERT NOT EXISTS ( \
                 SELECT FROM pg_database database \
                 CROSS JOIN LATERAL aclexplode( \
                   COALESCE(database.datacl, acldefault('d', database.datdba))) acl \
                 WHERE database.datname = current_database() \
                   AND acl.grantee = 0 AND acl.privilege_type = 'CONNECT'); \
               ASSERT EXISTS ( \
                 SELECT FROM pg_database database \
                 CROSS JOIN LATERAL aclexplode( \
                   COALESCE(database.datacl, acldefault('d', database.datdba))) acl \
                 WHERE database.datname = '{sibling}' \
                   AND acl.grantee = 0 AND acl.privilege_type = 'CONNECT'); \
             END $proof$; \
             COMMIT;"
        ),
    );
    psql_ok(
        &url,
        "drop sibling database",
        &format!("DROP DATABASE IF EXISTS {sibling} WITH (FORCE);"),
    );
}

#[test]
fn control_portable_store_enforces_the_current_record_on_postgres() {
    let Ok(url) = std::env::var("WAMN_CONTROL_PORTABLE_PG_URL") else {
        eprintln!(
            "skipping control_portable_store_enforces_the_current_record_on_postgres \
             (set WAMN_CONTROL_PORTABLE_PG_URL)"
        );
        return;
    };
    let _serialized = STORE.lock().unwrap_or_else(|poison| poison.into_inner());
    reset_and_apply(&url, "");

    psql_ok(
        &url,
        "prove the installed package/release record",
        r#"
SET ROLE wamn_system;
SET app.tenant = 'tenant-a';
DO $shape$
DECLARE
  catalog_tables text[];
  run_tables text[];
  rls_gap text;
  public_acl text;
BEGIN
  SELECT array_agg(tablename ORDER BY tablename) INTO catalog_tables
    FROM pg_tables WHERE schemaname = 'catalog';
  ASSERT catalog_tables = ARRAY[
    'authoring_command_audit', 'component_library',
    'connection_requirements', 'deployment_attestations',
    'effective_release_heads', 'effective_release_packages',
    'effective_releases', 'package_migrations', 'packages'
  ]::text[], format('catalog inventory drifted: %s', catalog_tables);
  SELECT array_agg(tablename ORDER BY tablename) INTO run_tables
    FROM pg_tables WHERE schemaname = 'wamn_run';
  ASSERT run_tables = ARRAY['gate_reports']::text[];

  SELECT string_agg(n.nspname || '.' || c.relname, ', ' ORDER BY 1) INTO rls_gap
    FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
   WHERE n.nspname IN ('catalog', 'wamn_run') AND c.relkind = 'r'
     AND NOT (c.relrowsecurity AND c.relforcerowsecurity);
  ASSERT rls_gap IS NULL, format('RLS floor missing: %s', rls_gap);

  SELECT string_agg(n.nspname || '.' || c.relname || ':' || acl.privilege_type,
                    ', ' ORDER BY 1) INTO public_acl
    FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
    CROSS JOIN LATERAL aclexplode(c.relacl) acl
   WHERE n.nspname IN ('catalog', 'wamn_run') AND acl.grantee = 0;
  ASSERT public_acl IS NULL, format('PUBLIC table authority survived: %s', public_acl);

  ASSERT NOT EXISTS (
    SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
     WHERE n.nspname IN ('catalog', 'wamn_run') AND c.relkind = 'r'
       AND pg_get_userbyid(c.relowner) <> 'wamn_system'
  ), 'a portable-store relation has the wrong owner';
END
$shape$;

SELECT catalog.register_package(
  'tenant-a', 'receiving', '1.0.0', 'sha256:' || repeat('a', 64), NULL);
SELECT catalog.register_package(
  'tenant-a', 'receiving', '1.0.0', 'sha256:' || repeat('a', 64), NULL);
DO $package_conflict$ BEGIN
  BEGIN
    PERFORM catalog.register_package(
      'tenant-a', 'receiving', '1.0.0', 'sha256:' || repeat('b', 64), NULL);
    ASSERT false, 'same package coordinate accepted different bytes';
  EXCEPTION WHEN unique_violation THEN
    ASSERT SQLERRM LIKE 'package-coordinate-content-conflict:%';
  END;
END
$package_conflict$;

INSERT INTO catalog.package_migrations
  (tenant_id, package_id, package_version, ordinal, relative_path, sha256)
VALUES
  ('tenant-a', 'receiving', '1.0.0', 1, 'migrations/0001_initial.sql',
   'sha256:' || repeat('c', 64));
DO $immutable$ BEGIN
  BEGIN
    UPDATE catalog.package_migrations SET sha256 = 'sha256:' || repeat('d', 64);
    ASSERT false, 'the exact-byte ledger mutated';
  EXCEPTION WHEN SQLSTATE '55000' THEN NULL;
  END;
END
$immutable$;

SELECT catalog.project_effective_release_identity('tenant-a', 1, 'dev');
SELECT catalog.project_effective_release_identity('tenant-a', 1, 'dev');
INSERT INTO catalog.effective_release_packages
  (tenant_id, effective_release_id, package_id, package_version)
VALUES ('tenant-a', 1, 'receiving', '1.0.0');
INSERT INTO catalog.effective_release_heads
  (tenant_id, environment, effective_release_id)
VALUES ('tenant-a', 'dev', 1);

DO $projection_conflict$ BEGIN
  BEGIN
    PERFORM catalog.project_effective_release_identity('tenant-a', 1, 'prod');
    ASSERT false, 'the same effective release id accepted another environment';
  EXCEPTION WHEN unique_violation THEN
    ASSERT SQLERRM = 'effective-release-identity-projection-content-conflict';
  END;
END
$projection_conflict$;
RESET ROLE;
"#,
    );
}

#[test]
fn control_author_is_tenant_bound_and_exactly_scoped_on_postgres() {
    let Ok(url) = std::env::var("WAMN_CONTROL_PORTABLE_PG_URL") else {
        eprintln!(
            "skipping control_author_is_tenant_bound_and_exactly_scoped_on_postgres \
             (set WAMN_CONTROL_PORTABLE_PG_URL)"
        );
        return;
    };
    let _serialized = STORE.lock().unwrap_or_else(|poison| poison.into_inner());
    let database = database(&url);
    let author_a = wamn_control_provision::control_author_generation_role(
        "acme",
        "receiving",
        "dev",
        &database,
        wamn_control_provision::CredentialGeneration::A,
    );
    let author_b = wamn_control_provision::control_author_generation_role(
        "acme",
        "shipping",
        "dev",
        &database,
        wamn_control_provision::CredentialGeneration::B,
    );
    const PASSWORD: &str = "control-author-live-proof";
    let mut generations = String::new();
    for role in [&author_a, &author_b] {
        generations.push_str(
            &wamn_control_provision::sql::prepare_control_author_generation_sql(
                &database,
                role,
                PASSWORD,
                "2099-01-01T00:00:00Z",
            ),
        );
        generations.push('\n');
    }
    reset_and_apply(&url, &generations);

    psql_ok(
        &url,
        "seed two control tenants",
        &format!(
            r#"
SET ROLE wamn_system;
INSERT INTO wamn_authority.author_login_tenants
  (login_identity, tenant_id, org_id, project_id, environment)
VALUES ('{author_a}', 'tenant-a', 'acme', 'receiving', 'dev'),
       ('{author_b}', 'tenant-b', 'acme', 'shipping', 'dev');
DO $seed$ DECLARE tenant text; package text; release int; BEGIN
  FOR tenant, package, release IN
    SELECT * FROM (VALUES
      ('tenant-a', 'receiving', 1), ('tenant-b', 'shipping', 2)
    ) AS seed(tenant, package, release)
  LOOP
    PERFORM set_config('app.tenant', tenant, false);
    PERFORM catalog.register_package(
      tenant, package, '1.0.0', 'sha256:' || repeat('a', 64), NULL);
    PERFORM catalog.project_effective_release_identity(tenant, release, 'dev');
    INSERT INTO catalog.effective_release_packages
      (tenant_id, effective_release_id, package_id, package_version)
    VALUES (tenant, release, package, '1.0.0');
    INSERT INTO catalog.effective_release_heads
      (tenant_id, environment, effective_release_id)
    VALUES (tenant, 'dev', release);
    INSERT INTO catalog.component_library
      (tenant_id, package_id, package_version, component, interface_version,
       operation, component_digest, projection_hash, imports, imports_fingerprint,
       effects, input_ports, output_ports, parameters)
    VALUES
      (tenant, package, '1.0.0', 'worker', '0.1.0', 'run',
       'sha256:' || repeat('b', 64), 'sha256:' || repeat('c', 64), '[]',
       'sha256:' || repeat('d', 64), '[]', '[]', '[]', '[]');
    INSERT INTO catalog.connection_requirements
      (tenant_id, component_digest, store_alias, requirement_json, requirement_hash)
    VALUES
      (tenant, 'sha256:' || repeat('b', 64), 'db', '{{}}',
       'sha256:' || repeat('e', 64));
  END LOOP;
END
$seed$;
RESET ROLE;
"#
        ),
    );

    psql_ok(
        &as_role(&url, &author_a, PASSWORD),
        "prove tenant-a author authority",
        &format!(
            r#"
DO $identity$ BEGIN
  ASSERT session_user = '{author_a}';
  ASSERT current_user = session_user;
  ASSERT wamn_authority.session_author_tenant() = 'tenant-a';
  ASSERT pg_has_role(session_user, 'wamn_control_author', 'USAGE');
  ASSERT NOT pg_has_role(session_user, 'wamn_system', 'USAGE');
END
$identity$;

SET app.tenant = 'tenant-a';
DO $positive$ BEGIN
  ASSERT (SELECT count(*) FROM catalog.effective_releases) = 1;
  ASSERT (SELECT count(*) FROM catalog.effective_release_heads) = 1;
  ASSERT (SELECT count(*) FROM catalog.connection_requirements) = 1;
END
$positive$;
INSERT INTO catalog.authoring_command_audit
  (tenant_id, command_id, command_kind, principal_id, principal_kind,
   principal_subject, effective_role, org, project, environment, target_ref,
   request_hash, outcome_bytes)
VALUES
  ('tenant-a', 'command-1', 'gate', 'principal-1', 'human', 'someone',
   'project-author', 'acme', 'receiving', 'dev', 'receiving@1.0.0',
   'sha256:' || repeat('1', 64), '\x7b7d'::bytea);
INSERT INTO wamn_run.gate_reports
  (tenant_id, wiring_hash, passed, summary)
VALUES ('tenant-a', 'sha256:' || repeat('2', 64), true, '{{}}');

SET app.tenant = 'tenant-b';
DO $narrowed$ BEGIN
  ASSERT (SELECT count(*) FROM catalog.effective_releases) = 0;
  BEGIN
    INSERT INTO catalog.authoring_command_audit
      (tenant_id, command_id, command_kind, principal_id, principal_kind,
       principal_subject, effective_role, org, project, environment, target_ref,
       request_hash, outcome_bytes)
    VALUES
      ('tenant-b', 'forged', 'gate', 'principal-1', 'human', 'someone',
       'project-author', 'acme', 'shipping', 'dev', 'forged',
       'sha256:' || repeat('3', 64), '\x7b7d'::bytea);
    ASSERT false, 'app.tenant widened the author mapping';
  EXCEPTION WHEN insufficient_privilege THEN NULL;
  END;
END
$narrowed$;

SET app.tenant = 'tenant-a';
DO $denied$ BEGIN
  BEGIN PERFORM 1 FROM catalog.packages;
    ASSERT false, 'the author read package custody facts';
  EXCEPTION WHEN insufficient_privilege THEN NULL; END;
  BEGIN PERFORM 1 FROM catalog.deployment_attestations;
    ASSERT false, 'the author read deployment attestations';
  EXCEPTION WHEN insufficient_privilege THEN NULL; END;
  BEGIN UPDATE catalog.effective_release_heads SET effective_release_id = 2;
    ASSERT false, 'the author moved an effective release head';
  EXCEPTION WHEN insufficient_privilege THEN NULL; END;
  BEGIN PERFORM 1 FROM wamn_authority.author_login_tenants;
    ASSERT false, 'the author read its tenant mapping';
  EXCEPTION WHEN insufficient_privilege THEN NULL; END;
  BEGIN CREATE TABLE catalog.author_owned (id int);
    ASSERT false, 'the author created a catalog relation';
  EXCEPTION WHEN insufficient_privilege THEN NULL; END;
END
$denied$;
"#
        ),
    );

    psql_ok(
        &as_role(&url, &author_b, PASSWORD),
        "prove tenant-b author isolation",
        &format!(
            r#"
SET app.tenant = 'tenant-b';
DO $second$ BEGIN
  ASSERT session_user = '{author_b}';
  ASSERT wamn_authority.session_author_tenant() = 'tenant-b';
  ASSERT (SELECT count(*) FROM catalog.effective_releases) = 1;
  ASSERT (SELECT count(*) FROM catalog.authoring_command_audit) = 0;
  ASSERT (SELECT count(*) FROM wamn_run.gate_reports) = 0;
END
$second$;
"#
        ),
    );

    psql_ok(
        &url,
        "retire tenant-a author",
        &format!(
            "{}\n\
             DO $retired$ BEGIN \
               ASSERT NOT (SELECT rolcanlogin FROM pg_roles WHERE rolname = '{author_a}'); \
               ASSERT NOT has_database_privilege('{author_a}', current_database(), 'CONNECT'); \
               ASSERT NOT pg_has_role('{author_a}', 'wamn_control_author', 'USAGE'); \
             END $retired$;",
            wamn_control_provision::sql::retire_control_author_generation_sql(&database, &author_a)
        ),
    );
}

#[test]
fn deployment_attestation_rust_binding_holds_on_postgres() {
    let Ok(url) = std::env::var("WAMN_CONTROL_PORTABLE_PG_URL") else {
        eprintln!(
            "skipping deployment_attestation_rust_binding_holds_on_postgres \
             (set WAMN_CONTROL_PORTABLE_PG_URL)"
        );
        return;
    };
    let _serialized = STORE.lock().unwrap_or_else(|poison| poison.into_inner());
    reset_and_apply(&url, "");

    let hash = format!("sha256:{}", "a".repeat(64));
    let other_hash = format!("sha256:{}", "b".repeat(64));
    let identity = wamn_schema_control::attestation::EffectiveReleaseIdentity {
        tenant_id: "tenant-a",
        effective_release_id: 7,
        environment: "prod",
    };
    let attestation = wamn_schema_control::attestation::Attestation {
        tenant_id: "tenant-a",
        effective_release_id: 7,
        org_id: "acme",
        project_id: "billing",
        environment: "prod",
        deployed_manifest_hash: &hash,
        attested_at: "2026-08-15T12:00:00Z",
    };
    let conflicting = wamn_schema_control::attestation::Attestation {
        deployed_manifest_hash: &other_hash,
        ..attestation
    };
    let project = wamn_schema_control::attestation::project_effective_release_identity(&identity);
    let write = wamn_schema_control::attestation::register_attestation(&attestation);
    let conflicting_write = wamn_schema_control::attestation::register_attestation(&conflicting);

    let output = psql(
        &url,
        &format!(
            r#"
SET ROLE wamn_system;
SET app.tenant = 'tenant-a';
{project};
PREPARE wamn_rust_attestation AS {prepared};
DO $types$ DECLARE found text[]; BEGIN
  SELECT parameter_types::text[] INTO found
    FROM pg_prepared_statements WHERE name = 'wamn_rust_attestation';
  ASSERT found = ARRAY['text','integer','text','text','text','text','text']::text[],
    format('PostgreSQL types the Rust binding as %s', found);
END
$types$;
DEALLOCATE wamn_rust_attestation;

DO $binding$ DECLARE first_at timestamptz; BEGIN
  first_at := ({write});
  ASSERT first_at = ({write});
  ASSERT (SELECT count(*) FROM catalog.deployment_attestations) = 1;
  ASSERT EXISTS (
    SELECT 1 FROM catalog.deployment_attestations
     WHERE tenant_id = 'tenant-a'
       AND effective_release_id = 7
       AND org_id = 'acme'
       AND project_id = 'billing'
       AND environment = 'prod'
       AND deployed_manifest_hash = '{hash}'
       AND attested_at = first_at
  ), 'the Rust binding placed a value in the wrong column';
END
$binding$;

DO $refusal$ BEGIN
  BEGIN
    PERFORM ({conflicting});
    ASSERT false, 'a differing attestation was accepted';
  EXCEPTION WHEN unique_violation THEN
    RAISE NOTICE 'WAMN-RUST-ATTESTATION-REFUSAL % %', SQLSTATE, SQLERRM;
  END;
END
$refusal$;
RESET ROLE;
"#,
            project = render(&project),
            prepared = write.sql,
            write = render(&write),
            conflicting = render(&conflicting_write),
        ),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "deployment-attestation Rust binding proof failed:\n{stderr}"
    );
    let reported = stderr
        .lines()
        .find_map(|line| line.split_once("WAMN-RUST-ATTESTATION-REFUSAL "))
        .map(|(_, refusal)| refusal.trim().to_owned())
        .expect("the server reported the conflicting Rust-bound write");
    let (sqlstate, message) = reported
        .split_once(' ')
        .expect("the refusal carries SQLSTATE and message");
    assert_eq!(message, wamn_schema_control::attestation::CONTENT_CONFLICT);
    let error =
        wamn_schema_control::attestation::translate_failure(&conflicting, Some(sqlstate), message);
    assert_eq!(
        error.kind(),
        wamn_schema_control::attestation::AttestationErrorKind::ContentConflict
    );
    assert_eq!(error.coordinate(), "tenant-a/7 -> acme/billing/prod");
}
