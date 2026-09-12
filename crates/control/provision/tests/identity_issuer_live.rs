//! Execute the identity issuer grants on a disposable PostgreSQL 18 database.
//!
//! Set WAMN_IDENTITY_ISSUER_PG_URL and WAMN_IDENTITY_ISSUER_ALLOW_SCHEMA_RESET=1.
//! The URL must name wamn_system on a disposable cluster. This ignored test
//! resets its system schemas and closes the cluster PUBLIC CONNECT floor.

use std::io::Write as _;
use std::process::{Command, Stdio};

use url::Url;
use wamn_control_provision::identity_issuer::{
    IDENTITY_ISSUER_ROLE, identity_issuer_generation_role, parse_identity_issuer_url,
    prepare_identity_issuer_generation_sql, retire_identity_issuer_generation_sql,
};
use wamn_control_provision::sql::{
    prepare_workload_generation_sql, revoke_public_connect_floor_sql,
    role_database_grants_sql, terminate_workload_generation_sessions_sql,
};
use wamn_control_provision::{
    CredentialGeneration, SYSTEM_SCHEMA_SQL, SystemReader, WorkloadRoleFamily,
    system_reader_generation_role,
};

const ISSUER: &str = "https://identity-issuer-test.wamn-system.svc";
const PASSWORD: &str = "identity-issuer-test-password";

fn psql(url: &str, script: &str) -> (bool, String, String) {
    let mut child = Command::new("psql")
        .arg(url)
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
    assert!(ok, "identity issuer SQL failed: {error}");
    output
}

fn denied(url: &str, script: &str) {
    let (ok, _, error) = psql(url, script);
    assert!(
        !ok && error.contains("42501"),
        "expected insufficient_privilege: {error}"
    );
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
        "DROP SCHEMA IF EXISTS identity CASCADE; DROP SCHEMA IF EXISTS registry CASCADE; DROP SCHEMA IF EXISTS provisioning CASCADE;",
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

#[test]
#[ignore = "requires an explicitly armed disposable PostgreSQL 18 cluster"]
fn scoped_issuer_grants_and_generation_retirement_execute_on_postgres() {
    let admin =
        std::env::var("WAMN_IDENTITY_ISSUER_PG_URL").expect("set WAMN_IDENTITY_ISSUER_PG_URL");
    assert_eq!(
        std::env::var("WAMN_IDENTITY_ISSUER_ALLOW_SCHEMA_RESET").as_deref(),
        Ok("1"),
        "set WAMN_IDENTITY_ISSUER_ALLOW_SCHEMA_RESET=1 only for a disposable cluster"
    );
    assert_eq!(
        Url::parse(&admin).expect("administrator URL").path(),
        "/wamn_system"
    );
    assert_eq!(
        run(
            &admin,
            "SELECT current_database() = 'wamn_system' AND current_setting('server_version_num')::int >= 180000 AND current_setting('server_version_num')::int < 190000 AND rolsuper FROM pg_roles WHERE rolname = current_user"
        ),
        "t"
    );
    let a = identity_issuer_generation_role(ISSUER, CredentialGeneration::A).unwrap();
    let b = identity_issuer_generation_role(ISSUER, CredentialGeneration::B).unwrap();
    let reader = system_reader_generation_role(
        SystemReader::Identity,
        "acme",
        "receiving",
        "dev",
        "wamn_system",
        CredentialGeneration::A,
    );
    let roles = [
        a.clone(),
        b.clone(),
        reader.clone(),
        IDENTITY_ISSUER_ROLE.to_owned(),
        "wamn_identity_reader".to_owned(),
    ];
    reset(&admin, &roles);
    let outcome = std::panic::catch_unwind(|| {
        run(
            &admin,
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN CREATE ROLE wamn_system NOLOGIN; END IF; END $$; GRANT CREATE ON DATABASE wamn_system TO wamn_system;",
        );
        run(
            &admin,
            &format!("SET ROLE wamn_system; {SYSTEM_SCHEMA_SQL} RESET ROLE;"),
        );
        run(&admin, revoke_public_connect_floor_sql());
        run(
            &admin,
            "REVOKE TEMPORARY ON DATABASE wamn_system FROM PUBLIC;",
        );
        let expires = run(
            &admin,
            "SELECT to_char(clock_timestamp() + interval '1 day', 'YYYY-MM-DD\"T\"HH24:MI:SSOF')",
        );
        for generation in [CredentialGeneration::A, CredentialGeneration::B] {
            run(
                &admin,
                &prepare_identity_issuer_generation_sql(ISSUER, generation, PASSWORD, &expires)
                    .unwrap(),
            );
        }
        run(
            &admin,
            &prepare_workload_generation_sql(
                WorkloadRoleFamily::IdentityReader,
                "wamn_system",
                &reader,
                PASSWORD,
                &expires,
            ),
        );
        let a_url = login_url(&admin, &a);
        let b_url = login_url(&admin, &b);
        assert_eq!(parse_identity_issuer_url(&a_url, ISSUER).unwrap().role(), a);
        assert_eq!(
            run(
                &a_url,
                "SELECT NOT rolsuper AND NOT rolcreaterole AND NOT rolcreatedb AND NOT rolreplication AND NOT rolbypassrls FROM pg_roles WHERE rolname = current_user"
            ),
            "t"
        );
        assert_eq!(
            run(
                &admin,
                "SELECT NOT rolcanlogin AND NOT rolinherit AND rolpassword IS NULL FROM pg_authid WHERE rolname = 'wamn_identity_issuer'"
            ),
            "t"
        );
        let acl = run(
            &admin,
            &format!(
                "PREPARE issuer_acl(text) AS {}; EXECUTE issuer_acl('wamn_identity_issuer');",
                role_database_grants_sql()
            ),
        );
        assert_eq!(
            acl,
            [
                "column|identity|pats.created_at|SELECT|f",
                "column|identity|pats.expires_at|INSERT|f",
                "column|identity|pats.expires_at|SELECT|f",
                "column|identity|pats.id|SELECT|f",
                "column|identity|pats.label|INSERT|f",
                "column|identity|pats.label|SELECT|f",
                "column|identity|pats.principal_id|INSERT|f",
                "column|identity|pats.principal_id|SELECT|f",
                "column|identity|pats.revoked_at|SELECT|f",
                "column|identity|pats.token_hash|INSERT|f",
                "column|identity|pats.token_hash|SELECT|f",
                "column|identity|pats.token_prefix|INSERT|f",
                "column|identity|pats.token_prefix|SELECT|f",
                "column|identity|principals.display_name|SELECT|f",
                "column|identity|principals.id|SELECT|f",
                "column|identity|principals.kind|SELECT|f",
                "column|identity|principals.status|SELECT|f",
                "column|identity|principals.subject|SELECT|f",
                "column|identity|project_env_memberships.env|SELECT|f",
                "column|identity|project_env_memberships.org|SELECT|f",
                "column|identity|project_env_memberships.principal_id|SELECT|f",
                "column|identity|project_env_memberships.project|SELECT|f",
                "column|registry|project_envs.env|SELECT|f",
                "column|registry|project_envs.instance_suffix|SELECT|f",
                "column|registry|project_envs.org|SELECT|f",
                "column|registry|project_envs.project|SELECT|f",
                "relation|identity|session_keys|DELETE|f",
                "relation|identity|session_keys|INSERT|f",
                "relation|identity|session_keys|SELECT|f",
                "relation|identity|session_keys|UPDATE|f",
                "relation|identity|session_signing_state|DELETE|f",
                "relation|identity|session_signing_state|INSERT|f",
                "relation|identity|session_signing_state|SELECT|f",
                "relation|identity|session_signing_state|UPDATE|f",
                "schema|identity|identity|USAGE|f",
                "schema|registry|registry|USAGE|f",
            ]
            .join("\n")
        );
        let actual = run(
            &a_url,
            "SELECT c.relname FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = 'identity' AND c.relkind = 'r' \
               AND has_table_privilege(current_user, c.oid, 'SELECT') ORDER BY c.relname",
        );
        assert_eq!(actual, "session_keys\nsession_signing_state");
        for url in [&a_url, &b_url] {
            // Execute each fresh-read shape as the actual scoped issuer login.
            run(
                url,
                "SELECT p.id::text, p.kind, p.subject, p.display_name, p.status, t.token_hash, \
                (t.revoked_at IS NULL AND t.expires_at > now()) AS usable \
                FROM identity.pats t JOIN identity.principals p ON p.id = t.principal_id \
                WHERE t.token_prefix = 'absent-fixture-token';",
            );
            assert_eq!(
                run(
                    url,
                    "SELECT EXISTS(SELECT 1 FROM identity.project_env_memberships \
                WHERE principal_id = '00000000-0000-0000-0000-000000000001' \
                  AND org = 'acme' AND project = 'receiving' AND env = 'dev');"
                ),
                "f"
            );
            run(
                url,
                "SELECT org, project, env, instance_suffix FROM registry.project_envs \
                WHERE org = 'acme' AND project = 'receiving' AND env = 'dev';",
            );
            for statement in [
                "SELECT secret_name FROM registry.project_envs",
                "SELECT * FROM identity.project_roles",
                "UPDATE identity.principals SET status = 'disabled'",
                "UPDATE identity.pats SET revoked_at = now()",
                "DELETE FROM identity.pats",
                "INSERT INTO identity.pats (id) VALUES (DEFAULT)",
                "INSERT INTO identity.pats (created_at) VALUES (DEFAULT)",
                "INSERT INTO identity.pats (revoked_at) VALUES (NULL)",
                "INSERT INTO identity.principals (kind, subject, display_name) VALUES ('human', 'escape', 'Escape')",
                "INSERT INTO identity.project_env_memberships (principal_id, org, project, env) VALUES ('00000000-0000-0000-0000-000000000001', 'acme', 'receiving', 'dev')",
                "DELETE FROM identity.project_env_memberships",
                "UPDATE registry.project_envs SET instance_suffix = 'z9z9z9z9'",
            ] {
                denied(url, statement);
            }
        }
        run(
            &a_url,
            &format!(
                "INSERT INTO identity.session_keys (issuer, kid, public_key, private_pkcs8) VALUES \
             ('{ISSUER}', '00000000-0000-0000-0000-000000000001', decode(repeat('00',32),'hex'), decode('01','hex')); \
             INSERT INTO identity.session_signing_state (issuer, active_kid) VALUES \
             ('{ISSUER}', '00000000-0000-0000-0000-000000000001'); \
             UPDATE identity.session_keys SET private_pkcs8 = decode('02','hex') WHERE issuer = '{ISSUER}'; \
             UPDATE identity.session_signing_state SET active_kid = NULL WHERE issuer = '{ISSUER}';"
            ),
        );
        assert_eq!(
            run(&b_url, "SELECT count(*) FROM identity.session_keys"),
            "1"
        );
        for statement in [
            "SELECT * FROM identity.principals",
            "SELECT * FROM registry.orgs",
            "CREATE ROLE identity_escape",
            "CREATE SCHEMA identity_escape",
            "CREATE TABLE identity.identity_escape (n int)",
            "CREATE TEMP TABLE identity_escape (n int)",
            "SET ROLE wamn_system",
            "SET ROLE wamn_identity_issuer",
            "TRUNCATE identity.session_keys CASCADE",
            "ALTER TABLE identity.session_keys ADD COLUMN escape text",
        ] {
            denied(&a_url, statement);
        }
        let reader_url = login_url(&admin, &reader);
        for statement in [
            "SELECT private_pkcs8 FROM identity.session_keys",
            "SELECT * FROM identity.session_signing_state",
        ] {
            denied(&reader_url, statement);
        }
        let mut other_database = Url::parse(&a_url).unwrap();
        other_database.set_path("/postgres");
        let (ok, _, error) = psql(other_database.as_str(), "SELECT 1");
        assert!(
            !ok && error.contains("permission denied for database"),
            "cross-database login was not refused: {error}"
        );
        run(
            &b_url,
            &format!(
                "DELETE FROM identity.session_signing_state WHERE issuer = '{ISSUER}'; DELETE FROM identity.session_keys WHERE issuer = '{ISSUER}';"
            ),
        );
        run(
            &admin,
            &retire_identity_issuer_generation_sql(ISSUER, CredentialGeneration::A).unwrap(),
        );
        run(&admin, &terminate_workload_generation_sessions_sql(&a));
        assert!(
            !psql(&a_url, "SELECT 1").0,
            "retired generation must not authenticate"
        );
        assert_eq!(
            run(&b_url, "SELECT count(*) FROM identity.session_keys"),
            "0"
        );
        assert_eq!(
            run(
                &admin,
                &format!(
                    "SELECT NOT rolcanlogin AND rolpassword IS NULL AND NOT EXISTS (SELECT FROM pg_auth_members WHERE member = r.oid) AND NOT has_database_privilege(r.oid, 'wamn_system', 'CONNECT') FROM pg_authid r WHERE rolname = '{a}'"
                )
            ),
            "t"
        );
    });
    reset(&admin, &roles);
    if let Err(error) = outcome {
        std::panic::resume_unwind(error);
    }
}
