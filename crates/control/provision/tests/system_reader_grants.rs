//! Live proof that the T1 control-database readers hold EXACTLY their own
//! grant set (`wamn-0h0g.12.116`).
//!
//! The proof is the SERVER'S OWN ANSWER, never the text of a statement:
//! `has_schema_privilege`, `has_table_privilege`, `pg_roles` attributes,
//! `aclexplode`, and the real SQLSTATE a refused statement raises. Pinning DDL
//! text would prove nothing here — this crate emits the DDL, so a text
//! assertion would be checking a builder against itself, and it cannot tell a
//! declaration from a comment mentioning one.
//!
//! Every denial is measured from a session authenticated AS the reader
//! generation, and that session first asserts it is neither `rolsuper` nor
//! `rolbypassrls` — a superuser fixture would satisfy every probe below while
//! proving nothing.
//!
//! Set `WAMN_REGISTRY_PG_URL` to a throwaway superuser URL to arm it (the same
//! knob `control_storage.rs` uses); it prints a skip line and returns when unset.

use std::io::Write as _;
use std::process::{Command, Stdio};

use url::Url;

use wamn_control_provision::sql::{
    grant_registry_reader_surface_sql, prepare_workload_generation_sql,
};
use wamn_control_provision::{
    CredentialGeneration, SYSTEM_SCHEMA_SQL, SystemReader, WorkloadRoleFamily,
    system_reader_generation_role,
};

const ORG: &str = "acme";
const PROJECT: &str = "receiving";
const ENV: &str = "dev";
const READER_PW: &str = "wamn_registry_reader_pw";

/// One psql run: `(succeeded, stdout, stderr)`.
///
/// `VERBOSITY=verbose` is what carries the real SQLSTATE back into Rust, so a
/// refusal can be asserted on `42501` rather than on an error string.
fn psql(url: &str, script: &str) -> (bool, String, String) {
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

fn run_admin(url: &str, what: &str, script: &str) -> String {
    let (ok, out, err) = psql(url, script);
    assert!(ok, "{what} failed:\nstdout:\n{out}\nstderr:\n{err}");
    out
}

/// The admin URL with its userinfo replaced by one role's login.
fn role_url(admin_url: &str, role: &str, password: &str) -> String {
    let mut url = Url::parse(admin_url).expect("WAMN_REGISTRY_PG_URL is a URL");
    url.set_username(role).expect("set the probe role");
    url.set_password(Some(password)).expect("set the password");
    url.into()
}

fn generation_role(generation: CredentialGeneration, database: &str) -> String {
    system_reader_generation_role(
        SystemReader::Registry,
        ORG,
        PROJECT,
        ENV,
        database,
        generation,
    )
}

/// Drop every role this gate mints, so a leftover HEALTHY role cannot satisfy
/// the builder's `IF NOT EXISTS` and mask a mutated one.
///
/// Roles are CLUSTER-wide, so re-creating the schemas does not reach them.
/// `DROP OWNED BY` precedes `DROP ROLE` inside an existence check: a privilege
/// left behind in this database would otherwise refuse the drop outright.
fn reset_roles(admin_url: &str, database: &str) {
    let roles = [
        generation_role(CredentialGeneration::A, database),
        generation_role(CredentialGeneration::B, database),
        WorkloadRoleFamily::RegistryReader.acl_role().to_owned(),
    ];
    let mut script = String::new();
    for role in roles {
        script.push_str(&format!(
            "DO $reset$ BEGIN \
               IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{role}') THEN \
                 EXECUTE 'DROP OWNED BY \"{role}\"'; \
                 EXECUTE 'DROP ROLE \"{role}\"'; \
               END IF; \
             END $reset$;\n"
        ));
    }
    run_admin(admin_url, "reset leftover reader roles", &script);
}

/// Apply `deploy/sql/system-schema.sql` under its real `wamn_system` owner.
fn apply_system_schema(admin_url: &str) {
    let mut script = String::from(
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') THEN \
           CREATE ROLE wamn_system LOGIN PASSWORD 'wamn_system' NOSUPERUSER; END IF; END $$;\n\
         DROP SCHEMA IF EXISTS registry CASCADE;\n\
         DROP SCHEMA IF EXISTS provisioning CASCADE;\n\
         DROP SCHEMA IF EXISTS identity CASCADE;\n\
         DO $$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_system', \
           current_database()); END $$;\n\
         SET ROLE wamn_system;\n",
    );
    script.push_str(SYSTEM_SCHEMA_SQL);
    script.push_str("\nRESET ROLE;\n");
    run_admin(admin_url, "apply the system schema", &script);
}

/// The whole ACL the named role holds in this database, as the server reports
/// it through `aclexplode` — relation, column, schema and database entries
/// alike, so a grant of any grain lands in the comparison.
fn acl_inventory_sql(role: &str) -> String {
    format!(
        "SELECT kind || '|' || sch || '|' || obj || '|' || priv || '|' || grantable FROM ( \
           SELECT 'relation' AS kind, n.nspname::text AS sch, c.relname::text AS obj, \
                  x.privilege_type::text AS priv, x.is_grantable AS grantable \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             CROSS JOIN LATERAL aclexplode(c.relacl) x \
            WHERE x.grantee = '{role}'::regrole \
           UNION ALL \
           SELECT 'column', n.nspname::text, c.relname || '.' || a.attname, \
                  x.privilege_type::text, x.is_grantable \
             FROM pg_catalog.pg_attribute a \
             JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             CROSS JOIN LATERAL aclexplode(a.attacl) x \
            WHERE x.grantee = '{role}'::regrole \
           UNION ALL \
           SELECT 'schema', n.nspname::text, n.nspname::text, \
                  x.privilege_type::text, x.is_grantable \
             FROM pg_catalog.pg_namespace n CROSS JOIN LATERAL aclexplode(n.nspacl) x \
            WHERE x.grantee = '{role}'::regrole \
           UNION ALL \
           SELECT 'database', '', d.datname::text, x.privilege_type::text, x.is_grantable \
             FROM pg_catalog.pg_database d CROSS JOIN LATERAL aclexplode(d.datacl) x \
            WHERE x.grantee = '{role}'::regrole \
         ) q ORDER BY 1;"
    )
}

fn inventory(admin_url: &str, role: &str) -> Vec<String> {
    let (ok, out, err) = psql_tuples(admin_url, &acl_inventory_sql(role));
    assert!(ok, "read the ACL inventory for {role}:\n{err}");
    out.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

fn psql_tuples(url: &str, query: &str) -> (bool, String, String) {
    let child = Command::new("psql")
        .arg(url)
        .args(["-v", "ON_ERROR_STOP=1", "-tAq", "-c", query])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn psql");
    let output = child.wait_with_output().expect("wait for psql");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// Run one statement in a session authenticated AS the reader and return the
/// SQLSTATE it raised, or `None` when it succeeded.
fn reader_sqlstate(reader_url: &str, statement: &str) -> Option<String> {
    let (ok, _, err) = psql(reader_url, statement);
    if ok {
        return None;
    }
    // `VERBOSITY=verbose` prints `ERROR:  42501: permission denied for …`. A
    // SQLSTATE is five uppercase alphanumerics carrying at least one digit,
    // which is what separates it from the `ERROR:` label beside it.
    let state = err
        .split_whitespace()
        .filter_map(|token| token.strip_suffix(':'))
        .find(|code| {
            code.len() == 5
                && code
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                && code.chars().any(|c| c.is_ascii_digit())
        })
        .map(str::to_owned);
    Some(state.unwrap_or_else(|| panic!("no SQLSTATE in psql stderr:\n{err}")))
}

#[test]
fn the_registry_reader_holds_one_select_and_is_refused_everywhere_else() {
    let Ok(admin_url) = std::env::var("WAMN_REGISTRY_PG_URL") else {
        eprintln!(
            "skipping the_registry_reader_holds_one_select_and_is_refused_everywhere_else \
             (set WAMN_REGISTRY_PG_URL to run)"
        );
        return;
    };
    let database = {
        let (ok, out, err) = psql_tuples(&admin_url, "SELECT current_database()");
        assert!(ok, "read the target database name:\n{err}");
        out.trim().to_owned()
    };

    reset_roles(&admin_url, &database);
    apply_system_schema(&admin_url);

    let role = generation_role(CredentialGeneration::A, &database);
    let stable = WorkloadRoleFamily::RegistryReader.acl_role();
    // The convergent grant batch, applied TWICE: a replay against an already
    // correct database must be a no-op, not a widening or an error.
    let grant = grant_registry_reader_surface_sql();
    run_admin(&admin_url, "converge the registry reader", &grant);
    run_admin(&admin_url, "replay the convergent grant", &grant);
    run_admin(
        &admin_url,
        "prepare the registry-reader generation",
        &prepare_workload_generation_sql(
            WorkloadRoleFamily::RegistryReader,
            &database,
            &role,
            READER_PW,
            "2099-01-01T00:00:00Z",
        ),
    );

    // --- the server's answer about the STABLE role's whole ACL --------------
    assert_eq!(
        inventory(&admin_url, stable),
        vec![
            "relation|registry|event_readers|SELECT|false".to_owned(),
            "schema|registry|registry|USAGE|false".to_owned(),
        ],
        "the stable registry-reader role's aclexplode inventory is not exactly \
         USAGE on registry plus SELECT on registry.event_readers"
    );
    // The generation itself holds only CONNECT, directly: its read authority is
    // inherited, so a direct table grant here would be a second, unmanaged path.
    assert_eq!(
        inventory(&admin_url, &role),
        vec![format!("database||{database}|CONNECT|false")],
        "the registry-reader generation carries a direct grant beyond CONNECT"
    );

    // --- the server's answer about the effective privilege ------------------
    let mut probes = format!(
        "DO $probe$ DECLARE r text := '{role}'; BEGIN \
           ASSERT NOT (SELECT rolsuper OR rolbypassrls FROM pg_roles WHERE rolname = r), \
             'the probe role is superuser or bypasses RLS — that masks every denial below'; \
           ASSERT (SELECT rolcanlogin FROM pg_roles WHERE rolname = r), \
             'the prepared generation must be able to authenticate'; \
           ASSERT has_schema_privilege(r, 'registry', 'USAGE'), \
             'the registry reader cannot reach its own schema'; \
           ASSERT has_table_privilege(r, 'registry.event_readers', 'SELECT'), \
             'the registry reader cannot read its one relation'; \n"
    );
    // Every OTHER system-plane schema, and the sensitive identity relations, are
    // denied — this is the disjointness the two families exist to keep.
    for schema in ["identity", "provisioning"] {
        probes.push_str(&format!(
            "  ASSERT NOT has_schema_privilege(r, '{schema}', 'USAGE'), \
               'the registry reader reaches the {schema} schema'; \n"
        ));
    }
    for relation in [
        "identity.pats",
        "identity.principals",
        "identity.project_roles",
        "provisioning.sagas",
        "registry.orgs",
        "registry.projects",
        "registry.project_envs",
    ] {
        for privilege in ["SELECT", "INSERT", "UPDATE", "DELETE"] {
            probes.push_str(&format!(
                "  ASSERT NOT has_table_privilege(r, '{relation}', '{privilege}'), \
                   'the registry reader holds {privilege} on {relation}'; \n"
            ));
        }
    }
    // …and on its OWN relation it holds the read and nothing else.
    for privilege in ["INSERT", "UPDATE", "DELETE", "TRUNCATE", "REFERENCES"] {
        probes.push_str(&format!(
            "  ASSERT NOT has_table_privilege(r, 'registry.event_readers', '{privilege}'), \
               'the registry reader holds {privilege} on its own relation'; \n"
        ));
    }
    probes.push_str("END $probe$;\n");
    run_admin(&admin_url, "the effective-privilege probes", &probes);

    // --- the reader's OWN session -------------------------------------------
    let reader_url = role_url(&admin_url, &role, READER_PW);
    run_admin(
        &reader_url,
        "the reader session's own self-check",
        "DO $self$ BEGIN \
           ASSERT NOT (SELECT rolsuper OR rolbypassrls FROM pg_roles \
                        WHERE rolname = current_user), \
             'this session is superuser or bypasses RLS and proves nothing'; \
         END $self$;",
    );
    // The REAL consuming statement, executed by the REAL credential.
    assert_eq!(
        reader_sqlstate(
            &reader_url,
            &format!(
                "PREPARE probe (text,text,text) AS {select}; \
                 EXECUTE probe('{ORG}','{PROJECT}','{ENV}'); DEALLOCATE probe;",
                select = wamn_control_registry::sql::select_event_reader_sql(),
            ),
        ),
        None,
        "the registry reader cannot run the statement it exists for"
    );
    for (statement, what) in [
        (
            "SELECT token_hash FROM identity.pats".to_owned(),
            "read the token digests it must never see",
        ),
        (
            "SELECT role FROM identity.project_roles".to_owned(),
            "read project role grants",
        ),
        (
            "INSERT INTO identity.project_roles (principal_id, org, project, role) \
             VALUES (gen_random_uuid(), 'x', 'y', 'admin')"
                .to_owned(),
            "self-grant a project role",
        ),
        (
            "SELECT id FROM registry.orgs".to_owned(),
            "read a registry relation outside its grant",
        ),
        (
            "UPDATE registry.event_readers SET enabled = false".to_owned(),
            "write its own relation",
        ),
        (
            "INSERT INTO registry.event_readers (org, project, env, publication, slot, \
             stream, replication_secret_name) VALUES ('a','b','c','d','e','f','g')"
                .to_owned(),
            "insert into its own relation",
        ),
    ] {
        assert_eq!(
            reader_sqlstate(&reader_url, &statement).as_deref(),
            Some("42501"),
            "the registry reader was able to {what}"
        );
    }
}
