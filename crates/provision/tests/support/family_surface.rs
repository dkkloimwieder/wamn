use std::fmt::Write as _;

use std::io::Write as _;
use std::process::{Command, Stdio};

use url::Url;

use wamn_control_provision::WorkloadRoleFamily;
use wamn_control_provision::sql;
use wamn_control_provision::workload_role::{WorkloadRoleIdentity, workload_login};

pub const CATALOG_SCHEMA: &str = wamn_catalog::CATALOG_SCHEMA_SQL;
pub const RUN_STATE: &str = include_str!("../../../../deploy/sql/run-state.sql");
pub const RUN_QUEUE: &str = include_str!("../../../../deploy/sql/run-queue.sql");
pub const APP_SCHEMA: &str = include_str!("../../../../deploy/sql/app-schema.sql");

pub const ORG: &str = "acme";
pub const PROJECT: &str = "billing";
pub const ENV: &str = "dev";
pub const GENERATION_PW: &str = "wamn_family_surface_pw";

/// One psql run: `(succeeded, stdout, stderr)`.
///
/// `VERBOSITY=verbose` is what carries the real SQLSTATE back into Rust, so a
/// refusal is asserted on `42501` rather than on an error string.
pub fn psql(url: &str, script: &str) -> (bool, String, String) {
    let mut child = Command::new("psql")
        .arg(url)
        .args([
            "-v",
            "ON_ERROR_STOP=1",
            "-v",
            "VERBOSITY=verbose",
            "-Atq",
            "-f",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn psql (is it installed?)");
    child
        .stdin
        .take()
        .expect("psql stdin")
        .write_all(script.as_bytes())
        .expect("write the script to psql");
    let output = child.wait_with_output().expect("wait for psql");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

pub fn run_admin(url: &str, what: &str, script: &str) -> String {
    let (ok, out, err) = psql(url, script);
    assert!(ok, "{what} failed:\nstdout:\n{out}\nstderr:\n{err}");
    out
}

pub fn query(url: &str, sql: &str) -> String {
    let output = Command::new("psql")
        .arg(url)
        .args(["-v", "ON_ERROR_STOP=1", "-tAq", "-c", sql])
        .output()
        .expect("spawn psql");
    assert!(
        output.status.success(),
        "query failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// The SQLSTATE one statement raised as the given login, or `None` on success.
pub fn sqlstate(url: &str, statement: &str) -> Option<String> {
    let (ok, _, err) = psql(url, statement);
    if ok {
        return None;
    }
    // `VERBOSITY=verbose` prints `ERROR:  42501: permission denied for …`. A
    // SQLSTATE is five uppercase alphanumerics carrying at least one digit,
    // which is what separates it from the `ERROR:` label beside it.
    err.split_whitespace()
        .filter_map(|token| token.strip_suffix(':'))
        .find(|code| {
            code.len() == 5
                && code
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                && code.chars().any(|c| c.is_ascii_digit())
        })
        .map(str::to_owned)
        .or_else(|| panic!("no SQLSTATE in psql stderr:\n{err}"))
}

pub fn generation(family: WorkloadRoleFamily, database: &str) -> wamn_identity_protocol::LoginName {
    workload_login(
        family,
        WorkloadRoleIdentity::ProjectEnvironment {
            org: ORG,
            project: PROJECT,
            environment: &ENV.parse().unwrap(),
            database,
        },
    )
    .expect("these families take a project-environment target")
}

/// The admin URL with its userinfo replaced by one role's login.
pub fn role_url(
    admin_url: &str,
    role: &wamn_identity_protocol::LoginName,
    password: &str,
) -> String {
    let mut url = Url::parse(admin_url).expect("the test database URL is a URL");
    url.set_username(role.as_str()).expect("set the probe role");
    url.set_password(Some(password)).expect("set the password");
    url.into()
}

/// Rebuild the two schemas and every cluster-global role they and the families
/// name, so nothing this acceptance test measures survives from a previous run.
///
/// `DROP OWNED BY` before `DROP ROLE`, inside an existence test: a leftover
/// HEALTHY role satisfies `ensure_workload_acl_role_sql`'s `IF NOT EXISTS` and
/// would mask a mutated builder — which is the exact failure the mutant arms of
/// this bead depend on being impossible.
pub fn reset(admin_url: &str, database: &str) {
    let mut roles = vec![
        generation(WorkloadRoleFamily::ExecutorPlatform, database).to_string(),
        generation(WorkloadRoleFamily::HttpAdmitter, database).to_string(),
        WorkloadRoleFamily::ExecutorPlatform.acl_role().to_owned(),
        WorkloadRoleFamily::HttpAdmitter.acl_role().to_owned(),
    ];
    // The roles the two artifacts GRANT to or CREATE. A `GRANT … TO` a missing
    // role fails the whole apply, so the ones the files do not create themselves
    // are re-minted below.
    roles.extend(["wamn_app", "wamn_run_retention", "wamn_platform"].map(str::to_owned));
    let mut script = String::from(
        "DROP SCHEMA IF EXISTS wamn_run CASCADE;\n\
         DROP SCHEMA IF EXISTS catalog CASCADE;\n\
         DROP SCHEMA IF EXISTS platform CASCADE;\n\
         DROP SCHEMA IF EXISTS wamn_authority CASCADE;\n",
    );
    for role in roles {
        writeln!(
            script,
            "DO $reset$ BEGIN \
               IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{role}') THEN \
                 EXECUTE 'DROP OWNED BY \"{role}\"'; \
                 EXECUTE 'DROP ROLE \"{role}\"'; \
               END IF; \
             END $reset$;"
        )
        .expect("writing to a String cannot fail");
    }
    script.push_str(&sql::ensure_app_acl_role_sql());
    run_admin(admin_url, "reset the family-surface fixture", &script);
    for artifact in [CATALOG_SCHEMA, RUN_STATE, RUN_QUEUE, APP_SCHEMA] {
        run_admin(admin_url, "apply a run-plane artifact", artifact);
    }
}

/// The whole ACL the named role holds in this database, as the server reports
/// it — relation, column, schema and ROUTINE entries alike, so a grant of any
/// grain lands in the comparison.
pub async fn load_role_grants(
    admin_url: &str,
    role: &str,
) -> Vec<(String, String, String, String)> {
    let mut connection = wamn_postgres::connect(admin_url).await.unwrap();
    sqlx::query_as(
        "SELECT type, sch, obj, priv FROM ( \
               SELECT 'relation' AS type, n.nspname::text AS sch, c.relname::text AS obj, \
                      x.privilege_type::text AS priv \
                 FROM pg_catalog.pg_class c \
                 JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                 CROSS JOIN LATERAL aclexplode(c.relacl) x \
                WHERE x.grantee = $1::text::regrole \
               UNION ALL \
               SELECT 'column', n.nspname::text, c.relname || '.' || a.attname, \
                      x.privilege_type::text \
                 FROM pg_catalog.pg_attribute a \
                 JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
                 JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
                 CROSS JOIN LATERAL aclexplode(a.attacl) x \
                WHERE x.grantee = $1::text::regrole \
               UNION ALL \
               SELECT 'schema', n.nspname::text, n.nspname::text, x.privilege_type::text \
                 FROM pg_catalog.pg_namespace n CROSS JOIN LATERAL aclexplode(n.nspacl) x \
                WHERE x.grantee = $1::text::regrole \
               UNION ALL \
               SELECT 'routine', n.nspname::text, p.proname::text, x.privilege_type::text \
                 FROM pg_catalog.pg_proc p \
                 JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
                 CROSS JOIN LATERAL aclexplode(p.proacl) x \
                WHERE x.grantee = $1::text::regrole \
               UNION ALL \
               SELECT 'database', '', d.datname::text, x.privilege_type::text \
                 FROM pg_catalog.pg_database d CROSS JOIN LATERAL aclexplode(d.datacl) x \
                WHERE x.grantee = $1::text::regrole \
             ) q ORDER BY 1, 2, 3, 4",
    )
    .bind(role)
    .fetch_all(&mut connection)
    .await
    .unwrap()
}

/// The host establishes caller identity before a login reads product rows.
pub fn bound_query(url: &str, statement: &str) -> String {
    query(
        url,
        &format!(
            "BEGIN; {} {statement}; COMMIT;",
            wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Executor
            )
        ),
    )
}
