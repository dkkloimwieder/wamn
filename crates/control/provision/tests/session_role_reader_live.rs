//! Exact session role-reader ACLs and A/B retirement on disposable PostgreSQL 18.
//!
//! Arm WAMN_SESSION_ROLE_READER_PG_URL naming wamn_session_role_reader_proof and
//! WAMN_SESSION_ROLE_READER_ALLOW_SCHEMA_RESET=1. Use a fresh owned cluster: this
//! proof resets its four project schemas and named fixture roles, and revokes
//! the cluster's PUBLIC CONNECT floor. It never connects to a deployed database.

use std::io::Write as _;
use std::process::{Command, Stdio};

use url::Url;
use wamn_control_provision::session_role_reader::parse_session_role_reader_url;
use wamn_control_provision::sql;
use wamn_control_provision::workload_role::{WorkloadRoleScope, workload_generation_role};
use wamn_control_provision::{CredentialGeneration, WorkloadRoleFamily};

const DATABASE: &str = "wamn_session_role_reader_proof";
const PASSWORD: &str = "session-role-reader-fixture-password";
const FAMILY: WorkloadRoleFamily = WorkloadRoleFamily::SessionRoleReader;
const PRINCIPAL: &str = "00000000-0000-0000-0000-000000000001";
const READ_ROLES: &str = "SELECT r.role_name FROM app_system.users u \
    JOIN app_system.user_roles r ON r.tenant_id = u.tenant_id AND r.user_id = u.id \
    WHERE u.tenant_id = 'fixture-a' AND u.id = '00000000-0000-0000-0000-000000000001' \
      AND u.status = 'active' ORDER BY r.role_name";

fn psql(url: &str, script: &str) -> (bool, String, String) {
    let mut public_url = Url::parse(url).ok().expect("fixture URL");
    // Use the existing URL decoder without turning literal user-info '+' or
    // '&' bytes into form separators. The credential stays out of argv.
    let encoded = format!(
        "password={}",
        public_url
            .password()
            .unwrap_or_default()
            .replace('+', "%2B")
            .replace('&', "%26")
    );
    let (_, password) = url::form_urlencoded::parse(encoded.as_bytes())
        .next()
        .expect("fixture password field");
    public_url.set_password(None).expect("fixture URL password");
    let mut child = Command::new("psql")
        .arg(public_url.as_str())
        .env("PGPASSWORD", password.as_ref())
        .env("PGCONNECT_TIMEOUT", "5")
        .env(
            "PGOPTIONS",
            "-c statement_timeout=10000 -c lock_timeout=5000",
        )
        .args([
            "-X",
            "-A",
            "-t",
            "-q",
            "-v",
            "ON_ERROR_STOP=1",
            "-v",
            "VERBOSITY=verbose",
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
        .expect("write psql input");
    let output = child.wait_with_output().expect("wait for psql");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn run(url: &str, script: &str) -> String {
    let (ok, output, error) = psql(url, script);
    assert!(ok, "session role-reader SQL failed: {error}");
    output
}

fn denied(url: &str, script: &str) {
    let (ok, _, error) = psql(url, script);
    assert!(
        !ok && error.contains("42501"),
        "expected insufficient_privilege for {script}: {error}"
    );
}

fn role(generation: CredentialGeneration) -> String {
    workload_generation_role(
        FAMILY,
        WorkloadRoleScope::ProjectEnvironment {
            org: "acme",
            project: "receiving",
            environment: "dev",
            database: DATABASE,
        },
        generation,
    )
    .expect("project-environment family")
}

fn login_url(admin: &str, role: &str) -> String {
    let mut url = Url::parse(admin).expect("armed administrator URL");
    url.set_username(role).expect("set role");
    url.set_password(Some(PASSWORD))
        .expect("set fixture password");
    url.into()
}

fn reset(admin: &str, roles: &[String]) {
    run(
        admin,
        "DROP SCHEMA IF EXISTS wamn_run CASCADE; DROP SCHEMA IF EXISTS catalog CASCADE; \
        DROP SCHEMA IF EXISTS app_system CASCADE; DROP SCHEMA IF EXISTS wamn_authority CASCADE;",
    );
    for role in roles {
        run(
            admin,
            &format!(
                "DO $$ BEGIN IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{role}') THEN \
            DROP OWNED BY \"{role}\"; DROP ROLE \"{role}\"; END IF; END $$;"
            ),
        );
    }
}

fn assert_surface(admin: &str) {
    let actual = run(
        admin,
        &format!(
            "PREPARE reader_acl(text) AS {}; EXECUTE reader_acl('wamn_session_role_reader');",
            sql::role_database_grants_sql()
        ),
    );
    assert_eq!(
        actual,
        [
            "column|app_system|user_roles.role_name|SELECT|f",
            "column|app_system|user_roles.tenant_id|SELECT|f",
            "column|app_system|user_roles.user_id|SELECT|f",
            "column|app_system|users.id|SELECT|f",
            "column|app_system|users.status|SELECT|f",
            "column|app_system|users.tenant_id|SELECT|f",
            "schema|app_system|app_system|USAGE|f",
        ]
        .join("\n"),
        "dedicated reader must hold only the six approved SELECT columns"
    );
}

#[test]
#[ignore = "requires an explicitly armed disposable PostgreSQL 18 cluster"]
fn dedicated_session_reader_columns_and_generations_execute_on_postgres() {
    let admin = std::env::var("WAMN_SESSION_ROLE_READER_PG_URL")
        .expect("set WAMN_SESSION_ROLE_READER_PG_URL");
    assert_eq!(
        std::env::var("WAMN_SESSION_ROLE_READER_ALLOW_SCHEMA_RESET").as_deref(),
        Ok("1"),
        "arm only a disposable cluster"
    );
    assert_eq!(
        Url::parse(&admin).expect("administrator URL").path(),
        format!("/{DATABASE}")
    );
    assert_eq!(
        run(
            &admin,
            "SELECT current_database() = 'wamn_session_role_reader_proof' \
        AND current_setting('server_version_num')::int >= 180000 \
        AND current_setting('server_version_num')::int < 190000 AND rolsuper \
        FROM pg_roles WHERE rolname = current_user"
        ),
        "t"
    );
    let a = role(CredentialGeneration::A);
    let b = role(CredentialGeneration::B);
    let mut roles = vec![a.clone(), b.clone(), FAMILY.acl_role().to_owned()];
    roles.extend(
        [
            "wamn_app",
            "wamn_scenario_author",
            "wamn_control_author",
            "wamn_effect_writer",
            "wamn_run_retention",
            "wamn_platform",
        ]
        .map(str::to_owned),
    );
    reset(&admin, &roles);
    let outcome = std::panic::catch_unwind(|| {
        run(
            &admin,
            "CREATE ROLE wamn_app NOLOGIN; CREATE ROLE wamn_scenario_author NOLOGIN; \
            CREATE ROLE wamn_control_author NOLOGIN; CREATE ROLE wamn_effect_writer NOLOGIN;",
        );
        for artifact in [
            wamn_catalog::CATALOG_SCHEMA_SQL,
            include_str!("../../../../deploy/sql/run-state.sql"),
            include_str!("../../../../deploy/sql/run-queue.sql"),
            include_str!("../../../../deploy/sql/app-schema.sql"),
        ] {
            run(&admin, artifact);
        }
        run(&admin, sql::revoke_public_connect_floor_sql());
        run(
            &admin,
            &format!(
                "REVOKE TEMPORARY ON DATABASE {DATABASE} FROM PUBLIC; \
            INSERT INTO app_system.users (tenant_id,id,email) VALUES \
              ('fixture-a','{PRINCIPAL}','a@fixture.invalid'), ('fixture-b','{PRINCIPAL}','b@fixture.invalid'); \
            INSERT INTO app_system.roles (tenant_id,name) VALUES ('fixture-a','receiver'), ('fixture-b','outsider'); \
            INSERT INTO app_system.user_roles (tenant_id,user_id,role_name) VALUES \
              ('fixture-a','{PRINCIPAL}','receiver'), ('fixture-b','{PRINCIPAL}','outsider');"
            ),
        );
        for generation in [CredentialGeneration::A, CredentialGeneration::B] {
            run(
                &admin,
                &sql::prepare_workload_generation_sql(
                    FAMILY,
                    DATABASE,
                    &role(generation),
                    PASSWORD,
                    "2099-01-01T00:00:00Z",
                ),
            );
        }
        assert_surface(&admin);
        let a_url = login_url(&admin, &a);
        let b_url = login_url(&admin, &b);
        for (url, generation) in [
            (&a_url, CredentialGeneration::A),
            (&b_url, CredentialGeneration::B),
        ] {
            assert_eq!(
                parse_session_role_reader_url(url, "acme", "receiving", "dev", DATABASE)
                    .unwrap()
                    .generation(),
                generation
            );
            assert_eq!(
                run(
                    url,
                    "SELECT NOT rolsuper AND NOT rolcreatedb AND NOT rolcreaterole \
                AND NOT rolreplication AND NOT rolbypassrls FROM pg_roles WHERE rolname = current_user"
                ),
                "t"
            );
            assert_eq!(
                run(
                    url,
                    "SELECT pg_has_role(current_user, 'wamn_platform', 'USAGE')"
                ),
                "t"
            );
            // Resolve function names as the fixture owner. The reader cannot
            // resolve objects in a schema on which it has no USAGE.
            assert_eq!(
                run(
                    &admin,
                    &format!(
                        "SELECT NOT has_function_privilege('{role}', 'wamn_authority.tenant_key(text)', 'EXECUTE') \
                         AND NOT has_function_privilege('{role}', 'wamn_authority.current_tenant_key()', 'EXECUTE')",
                        role = role(generation),
                    ),
                ),
                "t"
            );
            assert_eq!(
                run(url, READ_ROLES),
                "receiver",
                "dedicated login must read the requested tenant's active-user roles"
            );
            // The approved platform arm spans tenants; the consumer predicate
            // above, not a new RLS model, selects one tenant.
            assert_eq!(
                run(
                    url,
                    "SELECT tenant_id FROM app_system.users ORDER BY tenant_id"
                ),
                "fixture-a\nfixture-b"
            );
            for statement in [
                "SELECT email FROM app_system.users",
                "SELECT granted_at FROM app_system.user_roles",
                "SELECT * FROM app_system.permissions",
                "SELECT * FROM app_system.roles",
                "SELECT * FROM catalog.packages",
                "SELECT * FROM wamn_run.runs",
                "INSERT INTO app_system.users (tenant_id,id,status) VALUES ('fixture-a','00000000-0000-0000-0000-000000000002','active')",
                "UPDATE app_system.users SET status = 'disabled'",
                "DELETE FROM app_system.user_roles",
                "TRUNCATE app_system.users CASCADE",
                "SELECT wamn_authority.tenant_key('fixture-a')",
                "SELECT wamn_run.require_executor_platform_authority()",
                "SET ROLE wamn_platform",
                "SET ROLE wamn_session_role_reader",
            ] {
                denied(url, statement);
            }
        }
        run(
            &admin,
            "UPDATE app_system.users SET status = 'disabled' WHERE tenant_id = 'fixture-a'",
        );
        assert_eq!(
            run(&a_url, READ_ROLES),
            "",
            "disabled application user contributes no session roles"
        );
        run(
            &admin,
            "UPDATE app_system.users SET status = 'active' WHERE tenant_id = 'fixture-a'",
        );
        assert_eq!(run(&b_url, READ_ROLES), "receiver");
        // Reapply after deliberately widening both column and table ACLs.
        run(
            &admin,
            "GRANT SELECT(email), UPDATE(status) ON app_system.users TO wamn_session_role_reader; \
            GRANT SELECT ON app_system.permissions TO wamn_session_role_reader; \
            GRANT USAGE ON SCHEMA catalog, wamn_run, wamn_authority TO wamn_session_role_reader; \
            GRANT SELECT ON catalog.packages, wamn_run.runs TO wamn_session_role_reader; \
            GRANT EXECUTE ON FUNCTION wamn_authority.tenant_key(text) TO wamn_session_role_reader;",
        );
        run(&admin, &sql::grant_session_role_reader_surface_sql());
        assert_surface(&admin);
        denied(&a_url, "SELECT email FROM app_system.users");
        denied(&b_url, "UPDATE app_system.users SET status = 'disabled'");
        assert_eq!(run(&a_url, READ_ROLES), "receiver");
        let mut other = Url::parse(&a_url).unwrap();
        other.set_path("/postgres");
        let (ok, _, error) = psql(other.as_str(), "SELECT 1");
        assert!(
            !ok && error.contains("permission denied for database"),
            "cross-database CONNECT must be refused"
        );
        run(
            &admin,
            &sql::retire_workload_generation_sql(FAMILY, DATABASE, &a),
        );
        run(&admin, &sql::terminate_workload_generation_sessions_sql(&a));
        assert!(
            !psql(&a_url, "SELECT 1").0,
            "retired A must not authenticate"
        );
        assert_eq!(
            run(&b_url, READ_ROLES),
            "receiver",
            "B remains usable after A retirement"
        );
    });
    reset(&admin, &roles);
    if let Err(error) = outcome {
        std::panic::resume_unwind(error);
    }
}
