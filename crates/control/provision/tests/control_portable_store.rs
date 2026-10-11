//! PostgreSQL 18 tests for the control portable package/release store.

use std::io::Write as _;
use std::process::{Command, Stdio};

const CURRENT_DATABASE_PUBLIC_CONNECT_SQL: &str =
    include_str!("../../../../test-support/fixtures/sql/current-database-public-connect.sql");
const STORE_SQL: &str = wamn_control_provision::CONTROL_PORTABLE_STORE_SQL;

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

#[test]
fn current_database_connect_posture_is_exactly_scoped() {
    let test_database = wamn_test_postgres::database();
    let url = test_database.url().to_owned();
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
             DO $test$ BEGIN \
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
             END $test$; \
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
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let url = test_database.url().to_owned();
    reset_and_apply(&url, "");

    psql_ok(
        &url,
        "check the installed release record",
        r"
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
    'authoring_command_audit', 'tenant_environments'
  ]::text[], format('catalog table list drifted: %s', catalog_tables);
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

RESET ROLE;
",
    );
}

#[test]
fn control_author_is_tenant_bound_and_exactly_scoped_on_postgres() {
    const PASSWORD: &str = "control-author-live-test";

    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let url = test_database.url().to_owned();
    let database = database(&url);
    let author_a = wamn_control_provision::control_author_generation_role(
        "acme",
        "widgets",
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
            r"
SET ROLE wamn_system;
INSERT INTO wamn_authority.author_login_tenants
  (login_identity, tenant_id, org_id, project_id, environment)
VALUES ('{author_a}', 'tenant-a', 'acme', 'widgets', 'dev'),
       ('{author_b}', 'tenant-b', 'acme', 'shipping', 'dev');
RESET ROLE;
"
        ),
    );

    psql_ok(
        &as_role(&url, &author_a, PASSWORD),
        "check tenant-a author authority",
        &format!(
            r"
DO $identity$ BEGIN
  ASSERT session_user = '{author_a}';
  ASSERT current_user = session_user;
  ASSERT wamn_authority.session_author_tenant() = 'tenant-a';
  ASSERT pg_has_role(session_user, 'wamn_control_author', 'USAGE');
  ASSERT NOT pg_has_role(session_user, 'wamn_system', 'USAGE');
END
$identity$;

SET app.tenant = 'tenant-a';
INSERT INTO catalog.authoring_command_audit
  (tenant_id, command_id, command_type, principal_id, principal_type,
   principal_subject, effective_role, org, project, environment, target_ref,
   request_hash, outcome_bytes)
VALUES
  ('tenant-a', 'command-1', 'gate', 'principal-1', 'user', 'someone',
   'project-author', 'acme', 'widgets', 'dev', 'widgets@1.0.0',
   'sha256:' || repeat('1', 64), '\x7b7d'::bytea);
INSERT INTO wamn_run.gate_reports
  (tenant_id, wiring_hash, passed, summary)
VALUES ('tenant-a', 'sha256:' || repeat('2', 64), true, '{{}}');

SET app.tenant = 'tenant-b';
DO $narrowed$ BEGIN
  ASSERT (SELECT count(*) FROM catalog.authoring_command_audit) = 0;
  BEGIN
    INSERT INTO catalog.authoring_command_audit
      (tenant_id, command_id, command_type, principal_id, principal_type,
       principal_subject, effective_role, org, project, environment, target_ref,
       request_hash, outcome_bytes)
    VALUES
      ('tenant-b', 'forged', 'gate', 'principal-1', 'user', 'someone',
       'project-author', 'acme', 'shipping', 'dev', 'forged',
       'sha256:' || repeat('3', 64), '\x7b7d'::bytea);
    ASSERT false, 'app.tenant widened the author mapping';
  EXCEPTION WHEN insufficient_privilege THEN NULL;
  END;
END
$narrowed$;

SET app.tenant = 'tenant-a';
DO $denied$ BEGIN
  BEGIN PERFORM 1 FROM wamn_authority.author_login_tenants;
    ASSERT false, 'the author read its tenant mapping';
  EXCEPTION WHEN insufficient_privilege THEN NULL; END;
  BEGIN CREATE TABLE catalog.author_owned (id int);
    ASSERT false, 'the author created a catalog relation';
  EXCEPTION WHEN insufficient_privilege THEN NULL; END;
END
$denied$;
"
        ),
    );

    psql_ok(
        &as_role(&url, &author_b, PASSWORD),
        "check tenant-b author isolation",
        &format!(
            r"
SET app.tenant = 'tenant-b';
DO $second$ BEGIN
  ASSERT session_user = '{author_b}';
  ASSERT wamn_authority.session_author_tenant() = 'tenant-b';
  ASSERT (SELECT count(*) FROM catalog.authoring_command_audit) = 0;
  ASSERT (SELECT count(*) FROM wamn_run.gate_reports) = 0;
END
$second$;
"
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
fn gate_reports_refuse_update_and_delete() {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let url = test_database.url().to_owned();
    reset_and_apply(&url, "");
    psql_ok(
        &url,
        "gate report immutability",
        r#"
SET ROLE wamn_system;
SET app.tenant = 'tenant-a';
INSERT INTO wamn_run.gate_reports (tenant_id, wiring_hash, passed, summary)
VALUES ('tenant-a', 'sha256:' || repeat('a', 64), true, '{"cases": 1}');
DO $immutable$
DECLARE
  original wamn_run.gate_reports%ROWTYPE;
BEGIN
  ASSERT current_user = 'wamn_system';
  SELECT * INTO STRICT original FROM wamn_run.gate_reports
   WHERE tenant_id = 'tenant-a' AND wiring_hash = 'sha256:' || repeat('a', 64);
  BEGIN
    UPDATE wamn_run.gate_reports SET passed = false, summary = '{"cases": 0}'
     WHERE tenant_id = original.tenant_id AND wiring_hash = original.wiring_hash;
    ASSERT false, 'a gate report allowed UPDATE';
  EXCEPTION WHEN SQLSTATE '55000' THEN
    ASSERT SQLERRM = 'wamn_run.gate_reports is immutable';
  END;
  BEGIN
    DELETE FROM wamn_run.gate_reports
     WHERE tenant_id = original.tenant_id AND wiring_hash = original.wiring_hash;
    ASSERT false, 'a gate report allowed DELETE';
  EXCEPTION WHEN SQLSTATE '55000' THEN
    ASSERT SQLERRM = 'wamn_run.gate_reports is immutable';
  END;
  ASSERT EXISTS (
    SELECT FROM wamn_run.gate_reports AS report WHERE report = original
  ), 'the gate report changed after refused writes';
END
$immutable$;
RESET ROLE;
"#,
    );
}
