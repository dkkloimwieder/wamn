//! PostgreSQL 18 tests for the control portable package/release store.

use std::io::Write as _;
use std::process::{Command, Stdio};

const CURRENT_DATABASE_PUBLIC_CONNECT_SQL: &str =
    include_str!("../../../../test-support/fixtures/sql/current-database-public-connect.sql");
const STORE_SQL: &str = wamn_control_provision::CONTROL_PORTABLE_STORE_SQL;

fn psql(url: &str, script: &str) -> std::process::Output {
    let actor = wamn_control_provision::PlatformComponent::Provisioning.principal_id();
    let script =
        format!("SET app.user_id='{actor}'; SET app.operation='admin:provision-fixture'; {script}");
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

    script.push('\n');
    script.push_str(wamn_control_provision::sql::ensure_db_owner_role_sql());
    script.push_str(before_store);
    script.push_str(
        "DO $database$ BEGIN \
           EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_system', current_database()); \
         END $database$;\n\
         SET ROLE wamn_system;\n",
    );
    script.push_str(include_str!("../../../../deploy/sql/record-history.sql"));
    script.push_str(STORE_SQL);
    script.push_str("\nRESET ROLE;\n");
    psql_ok(url, "apply the control portable store", &script);
}

#[sqlx::test(migrations = false)]
async fn current_database_connect_posture_is_exactly_targeted(pool: sqlx::PgPool) {
    let test_database = wamn_test_postgres::url(&pool);
    let url = test_database.as_str().to_owned();
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

#[sqlx::test(migrations = false)]
async fn control_portable_store_enforces_the_current_record_on_postgres(pool: sqlx::PgPool) {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::url(&pool);
    let url = test_database.as_str().to_owned();
    reset_and_apply(&url, "");

    psql_ok(
        &url,
        "test the installed release record",
        r"
SET ROLE wamn_system;
SET app.project_env_key = 'org-a';
DO $shape$
DECLARE
  rls_gap text;
  public_acl text;
BEGIN
  SELECT string_agg(n.nspname || '.' || c.relname, ', ' ORDER BY 1) INTO rls_gap
    FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
   WHERE n.nspname IN ('catalog', 'wamn_run') AND c.relkind = 'r'
     AND NOT (c.relrowsecurity AND c.relforcerowsecurity);
  ASSERT rls_gap IS NULL, format('RLS base grants missing: %s', rls_gap);

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
