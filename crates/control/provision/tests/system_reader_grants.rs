//! Live test that the T1 control-database readers hold EXACTLY their own
//! grant set (`wamn-0h0g.12.116`).
//!
//! The test reads the SERVER'S OWN ANSWER, never the text of a statement:
//! `has_schema_privilege`, `has_table_privilege`, `pg_roles` attributes,
//! `aclexplode`, and the real SQLSTATE a refused statement raises. Pinning DDL
//! text would show nothing here — this crate emits the DDL, so a text
//! assertion would be checking a builder against itself, and it cannot tell a
//! declaration from a comment mentioning one.
//!
//! Every denial is measured from a session authenticated AS the reader
//! generation, and that session first asserts it is neither `rolsuper` nor
//! `rolbypassrls` — a superuser fixture would satisfy every probe below while
//! showing nothing.
//!
//! Each test takes its own test database on the test PostgreSQL server and holds
//! the process lock, because it drops and creates cluster-wide roles.

use std::fmt::Write as _;

use std::io::Write as _;
use std::process::{Command, Stdio};

use url::Url;

use wamn_control_provision::sql::{
    grant_identity_reader_surface_sql, grant_registry_reader_surface_sql,
    prepare_workload_generation_sql,
};
use wamn_control_provision::{
    CredentialGeneration, SYSTEM_SCHEMA_SQL, SystemReader, WorkloadRoleFamily,
    system_reader_generation_role,
};

const ORG: &str = "acme";
const PROJECT: &str = "widgets";
const ENV: &str = "dev";
const READER_PW: &str = "wamn_system_reader_pw";

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
    let mut url = Url::parse(admin_url).expect("the test database URL is a URL");
    url.set_username(role).expect("set the probe role");
    url.set_password(Some(password)).expect("set the password");
    url.into()
}

fn generation_role(
    reader: SystemReader,
    generation: CredentialGeneration,
    database: &str,
) -> String {
    system_reader_generation_role(reader, ORG, PROJECT, ENV, database, generation)
}

/// Drop every role this gate mints, so a leftover HEALTHY role cannot satisfy
/// the builder's `IF NOT EXISTS` and mask a mutated one.
///
/// Roles are CLUSTER-wide, so re-creating the schemas does not reach them.
/// `DROP OWNED BY` precedes `DROP ROLE` inside an existence check: a privilege
/// left behind in this database would otherwise refuse the drop outright.
fn reset_roles(admin_url: &str, database: &str) {
    let mut roles = Vec::new();
    for reader in SystemReader::ALL {
        roles.push(generation_role(reader, CredentialGeneration::A, database));
        roles.push(generation_role(reader, CredentialGeneration::B, database));
        roles.push(reader.family().acl_role().to_owned());
    }
    let mut script = String::new();
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
           current_database()); END $$;\n",
    );
    script.push_str(wamn_control_provision::sql::ensure_db_owner_role_sql());
    script.push_str("\nSET ROLE wamn_system;\n");
    script.push_str(SYSTEM_SCHEMA_SQL);
    script.push_str("\nRESET ROLE;\n");
    run_admin(admin_url, "apply the system schema", &script);
}

/// The whole ACL the named role holds in this database, as the server reports
/// it through `aclexplode` — relation, column, schema and database entries
/// alike, so a grant of any grain lands in the comparison.
fn role_grants_sql(role: &str) -> String {
    format!(
        "SELECT type || '|' || sch || '|' || obj || '|' || priv || '|' || grantable FROM ( \
           SELECT 'relation' AS type, n.nspname::text AS sch, c.relname::text AS obj, \
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
           SELECT 'routine', n.nspname::text, p.proname::text, x.privilege_type::text, x.is_grantable \
             FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace \
             CROSS JOIN LATERAL aclexplode(p.proacl) x WHERE x.grantee='{role}'::regrole \
           UNION ALL \
           SELECT 'database', '', d.datname::text, x.privilege_type::text, x.is_grantable \
             FROM pg_catalog.pg_database d CROSS JOIN LATERAL aclexplode(d.datacl) x \
            WHERE x.grantee = '{role}'::regrole \
         ) q ORDER BY 1;"
    )
}

fn load_role_grants(admin_url: &str, role: &str) -> Vec<String> {
    let (ok, out, err) = psql_tuples(admin_url, &role_grants_sql(role));
    assert!(ok, "read the direct grants for {role}:\n{err}");
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
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let admin_url = test_database.url().to_owned();
    let database = {
        let (ok, out, err) = psql_tuples(&admin_url, "SELECT current_database()");
        assert!(ok, "read the target database name:\n{err}");
        out.trim().to_owned()
    };

    reset_roles(&admin_url, &database);
    apply_system_schema(&admin_url);

    let role = generation_role(SystemReader::Registry, CredentialGeneration::A, &database);
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
        load_role_grants(&admin_url, stable),
        vec![
            "relation|registry|capture_gap|SELECT|false".to_owned(),
            "relation|registry|event_readers|SELECT|false".to_owned(),
            "schema|registry|registry|USAGE|false".to_owned(),
        ],
        "the stable registry-reader role's aclexplode grants are not exactly \
         USAGE on registry plus SELECT on registry.event_readers and registry.capture_gap"
    );
    // The generation itself holds only CONNECT, directly: its read authority is
    // inherited, so a direct table grant here would be a second, unmanaged path.
    assert_eq!(
        load_role_grants(&admin_url, &role),
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
        writeln!(
            probes,
            "  ASSERT NOT has_schema_privilege(r, '{schema}', 'USAGE'), \
               'the registry reader reaches the {schema} schema'; "
        )
        .expect("writing to a String cannot fail");
    }
    for relation in [
        "identity.password_logins",
        "identity.pats",
        "identity.principals",
        "identity.project_env_memberships",
        "identity.project_roles",
        "registry.orgs",
        "registry.projects",
        "registry.project_envs",
    ] {
        for privilege in ["SELECT", "INSERT", "UPDATE", "DELETE"] {
            writeln!(
                probes,
                "  ASSERT NOT has_table_privilege(r, '{relation}', '{privilege}'), \
                   'the registry reader holds {privilege} on {relation}'; "
            )
            .expect("writing to a String cannot fail");
        }
    }
    // …and on its OWN relation it holds the read and nothing else.
    for privilege in ["INSERT", "UPDATE", "DELETE", "TRUNCATE", "REFERENCES"] {
        writeln!(
            probes,
            "  ASSERT NOT has_table_privilege(r, 'registry.event_readers', '{privilege}'), \
               'the registry reader holds {privilege} on its own relation'; "
        )
        .expect("writing to a String cannot fail");
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
             'this session is superuser or bypasses RLS and establishes nothing'; \
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

/// THE FORGERY PRIMITIVE, CLOSED AND MEASURED (`wamn-0h0g.12.67`), and the
/// DISJOINTNESS checked live with BOTH readers provisioned at once.
///
/// `wamn-system-db` authenticated as `wamn_system`, the owner of `identity.pats`
/// and `identity.project_roles`, and `identity.*` has no row-level security. The
/// two statements below are the ones that primitive enabled: bind a token digest
/// to any principal, and self-grant a project role. Both are run FROM THE
/// READER'S OWN SESSION and must come back `42501`.
///
/// The live cluster role carries `SELECT, INSERT, UPDATE`; the grant drifted
/// SELECT-only -> +INSERT -> +INSERT,UPDATE three times out of band. This is the
/// live half of the guard that fails on the fourth.
#[test]
fn the_identity_reader_can_never_write_identity_and_neither_reader_reaches_the_other() {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::database();
    let admin_url = test_database.url().to_owned();
    let database = {
        let (ok, out, err) = psql_tuples(&admin_url, "SELECT current_database()");
        assert!(ok, "read the target database name:\n{err}");
        out.trim().to_owned()
    };

    reset_roles(&admin_url, &database);
    apply_system_schema(&admin_url);

    // BOTH readers, provisioned together: disjointness is only meaningful when
    // the two roles coexist, and "one role for both" is exactly the failure the
    // two families exist to prevent.
    let mut reader_url = Vec::new();
    for (reader, grant) in [
        (SystemReader::Identity, grant_identity_reader_surface_sql()),
        (SystemReader::Registry, grant_registry_reader_surface_sql()),
        (
            SystemReader::Connection,
            wamn_control_provision::sql::grant_connection_reader_surface_sql(),
        ),
    ] {
        let role = generation_role(reader, CredentialGeneration::A, &database);
        run_admin(&admin_url, "converge a system reader", &grant);
        // Replayed: the convergent batch must narrow-and-grant to the same state.
        run_admin(&admin_url, "replay a convergent grant", &grant);
        run_admin(
            &admin_url,
            "prepare a system-reader generation",
            &prepare_workload_generation_sql(
                reader.family(),
                &database,
                &role,
                READER_PW,
                "2099-01-01T00:00:00Z",
            ),
        );
        reader_url.push((reader, role_url(&admin_url, &role, READER_PW), role));
    }

    // --- the server's answer about the identity reader's whole ACL ----------
    assert_eq!(
        load_role_grants(&admin_url, WorkloadRoleFamily::IdentityReader.acl_role()),
        vec![
            "relation|identity|password_logins|SELECT|false".to_owned(),
            "relation|identity|pats|SELECT|false".to_owned(),
            "relation|identity|principals|SELECT|false".to_owned(),
            "relation|identity|project_env_memberships|SELECT|false".to_owned(),
            "relation|identity|project_roles|SELECT|false".to_owned(),
            "schema|identity|identity|USAGE|false".to_owned(),
        ],
        "the stable identity-reader role's aclexplode grants are not exactly \
         USAGE on identity plus SELECT on the five relations it reads"
    );
    // The registry reader is untouched by the identity reader's convergence —
    // neither batch may narrow or widen the other.
    assert_eq!(
        load_role_grants(&admin_url, WorkloadRoleFamily::RegistryReader.acl_role()),
        vec![
            "relation|registry|capture_gap|SELECT|false".to_owned(),
            "relation|registry|event_readers|SELECT|false".to_owned(),
            "schema|registry|registry|USAGE|false".to_owned(),
        ],
    );

    assert_eq!(
        load_role_grants(&admin_url, WorkloadRoleFamily::ConnectionReader.acl_role()),
        vec![
            "routine|registry|lookup_connection_binding_mode|EXECUTE|false".to_owned(),
            "routine|registry|lookup_connection_credential|EXECUTE|false".to_owned(),
            "routine|registry|lookup_connector_bindings|EXECUTE|false".to_owned(),
            "routine|registry|lookup_file_connection|EXECUTE|false".to_owned(),
            "routine|registry|lookup_file_generations|EXECUTE|false".to_owned(),
            "schema|registry|registry|USAGE|false".to_owned(),
        ],
        "the connection reader holds exactly its credential and connection read surface"
    );
    assert_eq!(
        reader_sqlstate(
            &reader_url[2].1,
            "SELECT connection_id FROM registry.project_connection"
        ),
        Some("42501".to_owned()),
        "the connection reader cannot directly read project enablement"
    );
    assert_eq!(
        reader_sqlstate(&reader_url[2].1, "DELETE FROM registry.project_connection"),
        Some("42501".to_owned()),
        "the connection reader cannot change project enablement"
    );

    let identity_role = &reader_url[0].2;
    let mut probes = format!(
        "DO $probe$ DECLARE r text := '{identity_role}'; BEGIN \
           ASSERT NOT (SELECT rolsuper OR rolbypassrls FROM pg_roles WHERE rolname = r), \
             'the probe role is superuser or bypasses RLS — that masks every denial below'; \n"
    );
    for relation in [
        "identity.password_logins",
        "identity.pats",
        "identity.principals",
        "identity.project_env_memberships",
        "identity.project_roles",
    ] {
        writeln!(
            probes,
            "  ASSERT has_table_privilege(r, '{relation}', 'SELECT'), \
               'the identity reader cannot read {relation}'; "
        )
        .expect("writing to a String cannot fail");
        // THE THREE-TIMES-DRIFT GUARD, as the server reports it.
        for privilege in ["INSERT", "UPDATE", "DELETE", "TRUNCATE", "REFERENCES"] {
            writeln!(
                probes,
                "  ASSERT NOT has_table_privilege(r, '{relation}', '{privilege}'), \
                   'the identity reader holds {privilege} on {relation}'; "
            )
            .expect("writing to a String cannot fail");
        }
    }
    for schema in ["registry", "provisioning"] {
        writeln!(
            probes,
            "  ASSERT NOT has_schema_privilege(r, '{schema}', 'USAGE'), \
               'the identity reader reaches the {schema} schema'; "
        )
        .expect("writing to a String cannot fail");
    }
    for relation in ["registry.event_readers", "registry.orgs"] {
        writeln!(
            probes,
            "  ASSERT NOT has_table_privilege(r, '{relation}', 'SELECT'), \
               'the identity reader can read {relation}'; "
        )
        .expect("writing to a String cannot fail");
    }
    probes.push_str("END $probe$;\n");
    run_admin(&admin_url, "the identity-reader privilege probes", &probes);

    // --- each reader's OWN session ------------------------------------------
    for (reader, url, _) in &reader_url {
        run_admin(
            url,
            "a reader session's own self-check",
            "DO $self$ BEGIN \
               ASSERT NOT (SELECT rolsuper OR rolbypassrls FROM pg_roles \
                            WHERE rolname = current_user), \
                 'this session is superuser or bypasses RLS and establishes nothing'; \
             END $self$;",
        );
        // Neither reader may reach the OTHER's plane, in either direction.
        let foreign = match reader {
            SystemReader::Identity => "SELECT publication FROM registry.event_readers",
            SystemReader::Registry | SystemReader::Connection => {
                "SELECT token_hash FROM identity.pats"
            }
        };
        assert_eq!(
            reader_sqlstate(url, foreign).as_deref(),
            Some("42501"),
            "the {reader} reached the other reader's plane"
        );
    }

    let identity_url = &reader_url[0].1;
    // The REAL consuming statements, executed by the REAL credential.
    for statement in [
        "SELECT p.id::text, p.type, p.subject, p.display_name, p.status, \
         identity.pats.token_hash, \
         (identity.pats.revoked_at IS NULL AND identity.pats.expires_at > now()) AS usable \
         FROM identity.pats JOIN identity.principals p \
           ON p.id = identity.pats.principal_id \
         WHERE identity.pats.token_prefix = '0123456789abcdef'",
        "SELECT role FROM identity.project_roles \
         WHERE principal_id = '00000000-0000-4000-8000-000000000000'::uuid \
           AND org = 'acme' AND project = 'widgets' ORDER BY role",
        "SELECT EXISTS (SELECT 1 FROM identity.project_env_memberships \
         WHERE principal_id = '00000000-0000-4000-8000-000000000000'::uuid \
           AND org = 'acme' AND project = 'widgets' AND env = 'dev')",
    ] {
        assert_eq!(
            reader_sqlstate(identity_url, statement),
            None,
            "the identity reader cannot run a statement it exists for"
        );
    }
    // …and the two forgeries that credential used to permit.
    for (statement, what) in [
        (
            "INSERT INTO identity.pats \
             (principal_id, token_prefix, token_hash, label, expires_at) \
             VALUES ('00000000-0000-4000-8000-000000000000'::uuid, '0123456789abcdef', \
                     repeat('a', 64), 'forged', now() + interval '1 day')",
            "mint a personal access token bound to any principal",
        ),
        (
            "UPDATE identity.pats SET token_hash = repeat('b', 64)",
            "re-point an existing token at a digest it controls",
        ),
        (
            "INSERT INTO identity.project_roles (principal_id, org, project, role) \
             VALUES ('00000000-0000-4000-8000-000000000000'::uuid, 'acme', 'widgets', \
                     'project-admin')",
            "self-grant a project role",
        ),
        (
            "UPDATE identity.project_roles SET role = 'project-admin'",
            "escalate an existing project role",
        ),
        (
            "INSERT INTO identity.project_env_memberships (principal_id, org, project, env) \
             VALUES ('00000000-0000-4000-8000-000000000000'::uuid, 'acme', 'widgets', 'dev')",
            "self-grant environment membership",
        ),
        (
            "UPDATE identity.project_env_memberships SET env = 'prod'",
            "move membership into another environment",
        ),
        (
            "DELETE FROM identity.project_env_memberships",
            "revoke environment memberships",
        ),
        (
            "INSERT INTO identity.principals (type, subject, display_name) \
             VALUES ('service', 'forged', 'forged')",
            "mint a principal",
        ),
    ] {
        assert_eq!(
            reader_sqlstate(identity_url, statement).as_deref(),
            Some("42501"),
            "the identity reader was able to {what}"
        );
    }
}

#[cfg(feature = "test-util")]
#[test]
fn credential_lookup_uses_the_login_scope_and_current_generation() {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_control_provision::test_database::system();
    let admin_url = test_database.url();
    let database = test_database.name();
    reset_roles(admin_url, database);
    run_admin(
        admin_url,
        "grant only the credential lookup",
        &wamn_control_provision::sql::grant_connection_reader_surface_sql(),
    );
    let mut setup = String::from("BEGIN; SET LOCAL ROLE wamn_system;");
    setup.push_str(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ));
    for (org, project) in [(ORG, PROJECT), (ORG, "unenabled"), ("other", PROJECT)] {
        setup.push_str(
            &wamn_control_provision::test_database::seed_org_project_env_sql(org, project, ENV),
        );
    }
    setup.push_str("SET LOCAL ROLE wamn_control;");
    for org in [ORG, "other"] {
        write!(setup, "SELECT registry.configure_connection('{org}','external_system.write', '{{\"name\":\"erp-system\",\"integration\":\"http\",\"parameters\":{{}},\"status\":\"active\"}}'); SELECT registry.configure_connection('{org}','connection.write', '{{\"name\":\"erp\",\"external_system\":\"erp-system\",\"integration\":\"http\",\"transport\":\"http\",\"exchange\":\"request\",\"parameters\":{{\"primary-authority\":\"https://example.invalid\",\"tls-verification\":\"verify-authority\"}},\"status\":\"active\"}}'); SELECT registry.configure_connection('{org}','project_connection.enable','{{\"project\":\"{PROJECT}\",\"connection\":\"erp\"}}'); SELECT registry.rotate_connection('{org}','erp','first'); SELECT registry.rotate_connection('{org}','erp','second');").unwrap();
    }
    setup.push_str("COMMIT;");
    run_admin(admin_url, "seed credential authority as control", &setup);
    let mapped = generation_role(SystemReader::Connection, CredentialGeneration::A, database);
    let unmapped = generation_role(SystemReader::Connection, CredentialGeneration::B, database);
    let unenabled = system_reader_generation_role(
        SystemReader::Connection,
        ORG,
        "unenabled",
        ENV,
        database,
        CredentialGeneration::A,
    );
    for role in [&mapped, &unmapped, &unenabled] {
        run_admin(
            admin_url,
            "mint a real reader login",
            &prepare_workload_generation_sql(
                WorkloadRoleFamily::ConnectionReader,
                database,
                role,
                READER_PW,
                "2099-01-01T00:00:00Z",
            ),
        );
    }
    run_admin(
        admin_url,
        "map only the authenticated reader logins",
        &format!(
            "BEGIN; SET LOCAL ROLE wamn_system; {} INSERT INTO wamn_authority.author_login_tenants(login_identity,env_key,org_id,project_id,environment) VALUES ('{mapped}','acme--widgets--dev','{ORG}','{PROJECT}','{ENV}'),('{unenabled}','acme--unenabled--dev','{ORG}','unenabled','{ENV}'); COMMIT;",
            wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning,
            ),
        ),
    );
    let reader = role_url(admin_url, &mapped, READER_PW);
    run_admin(
        &reader,
        "assert the login and role are unprivileged",
        "DO $$ BEGIN ASSERT NOT (SELECT rolsuper OR rolbypassrls FROM pg_roles WHERE rolname=current_user); END $$;",
    );
    for relation in [
        "identity.credential",
        "registry.connection",
        "registry.external_system",
        "registry.connection_generation",
        "registry.project_connection",
    ] {
        assert_eq!(
            reader_sqlstate(&reader, &format!("SELECT * FROM {relation}")),
            Some("42501".into()),
            "direct SELECT on {relation} refuses"
        );
    }
    let (ok, expected_system_id, error) = psql_tuples(
        admin_url,
        &format!(
            "SELECT id::text FROM registry.external_system WHERE org='{ORG}' AND name='erp-system'"
        ),
    );
    assert!(ok, "{error}");
    assert!(
        !expected_system_id.trim().is_empty(),
        "the seed has a system id"
    );
    let (ok, external_system, error) = psql_tuples(
        &reader,
        &format!(
            "SELECT external_system FROM registry.lookup_connection_credential('{ORG}','{PROJECT}','erp',NULL)"
        ),
    );
    assert!(ok, "{error}");
    assert_eq!(
        external_system.trim(),
        expected_system_id.trim(),
        "the scoped lookup returns the projected system id without a table grant"
    );
    for (generation, expected) in [("NULL", "second"), ("2", "first"), ("3", "second")] {
        let (ok, out, err) = psql_tuples(
            &reader,
            &format!(
                "SELECT secret FROM registry.lookup_connection_credential('{ORG}','{PROJECT}','erp',{generation})"
            ),
        );
        assert!(ok, "{err}");
        assert_eq!(out.trim(), expected);
    }
    for (url, org, project) in [
        (reader.clone(), "other", PROJECT),
        (reader.clone(), ORG, "unenabled"),
        (role_url(admin_url, &unmapped, READER_PW), ORG, PROJECT),
        (role_url(admin_url, &unenabled, READER_PW), ORG, "unenabled"),
    ] {
        for generation in ["NULL", "2"] {
            let (ok, out, err) = psql_tuples(
                &url,
                &format!(
                    "SET app.org='other'; SET app.project='{PROJECT}'; SELECT count(*) FROM registry.lookup_connection_credential('{org}','{project}','erp',{generation})"
                ),
            );
            assert!(ok, "{err}");
            assert_eq!(
                out.trim(),
                "0",
                "claims, requested scope and pinned generations cannot widen a login"
            );
        }
    }
    run_admin(
        admin_url,
        "disable the enabled connection as control",
        &format!(
            "SET ROLE wamn_control; SELECT registry.configure_connection('{ORG}','connection.write','{{\"name\":\"erp\",\"external_system\":\"erp-system\",\"integration\":\"http\",\"transport\":\"http\",\"exchange\":\"request\",\"parameters\":{{}},\"status\":\"disabled\"}}');"
        ),
    );
    let (ok, out, err) = psql_tuples(
        &reader,
        &format!(
            "SELECT count(*) FROM registry.lookup_connection_credential('{ORG}','{PROJECT}','erp',2)"
        ),
    );
    assert!(ok, "{err}");
    assert_eq!(
        out.trim(),
        "0",
        "disabled connections refuse pinned secrets and principals"
    );
}

#[cfg(feature = "test-util")]
#[test]
fn file_connection_lookup_scopes_bindings_and_pins_on_fresh_install() {
    let _serialized = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let admin = database.url();
    reset_roles(admin, database.name());
    run_admin(
        admin,
        "grant file lookup",
        &wamn_control_provision::sql::grant_connection_reader_surface_sql(),
    );
    let mut setup = String::from("BEGIN; SET LOCAL ROLE wamn_system;");
    setup.push_str(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ));
    for (org, project) in [(ORG, PROJECT), (ORG, "unenabled"), ("other", PROJECT)] {
        setup.push_str(
            &wamn_control_provision::test_database::seed_org_project_env_sql(org, project, ENV),
        );
    }
    write!(setup, "SET LOCAL ROLE wamn_control; SELECT registry.configure_connection('{ORG}','connection.write','{{\"name\":\"files\",\"integration\":\"gcs\",\"transport\":\"gcs\",\"exchange\":\"request\",\"parameters\":{{\"provider\":\"gcs\",\"container\":\"old-bucket\",\"prefix\":\"old/\"}},\"status\":\"active\"}}'); SELECT registry.configure_connection('{ORG}','project_connection.enable','{{\"project\":\"{PROJECT}\",\"connection\":\"files\"}}'); SELECT registry.configure_connection('{ORG}','connection.write','{{\"name\":\"other-files\",\"integration\":\"gcs\",\"transport\":\"gcs\",\"exchange\":\"request\",\"parameters\":{{\"provider\":\"gcs\",\"container\":\"other-bucket\",\"prefix\":\"other/\"}},\"status\":\"active\"}}'); SELECT registry.configure_connection('{ORG}','project_connection.enable','{{\"project\":\"{PROJECT}\",\"connection\":\"other-files\"}}'); SET LOCAL ROLE wamn_system; INSERT INTO registry.environment_connection_binding(org,project,env,application,alias,target_id,connection_id,generation,mode,exclusive) SELECT '{ORG}','{PROJECT}','{ENV}','app',CASE c.name WHEN 'files' THEN 'files' ELSE 'other' END,t.id,c.id,1,'write',false FROM registry.target t,registry.connection c WHERE t.name='cloud' AND c.org='{ORG}' AND c.name IN ('files','other-files'); COMMIT;").unwrap();
    run_admin(admin, "seed file binding with restricted owners", &setup);
    let mapped = generation_role(
        SystemReader::Connection,
        CredentialGeneration::A,
        database.name(),
    );
    let unmapped = generation_role(
        SystemReader::Connection,
        CredentialGeneration::B,
        database.name(),
    );
    for role in [&mapped, &unmapped] {
        run_admin(
            admin,
            "mint file reader",
            &prepare_workload_generation_sql(
                WorkloadRoleFamily::ConnectionReader,
                database.name(),
                role,
                READER_PW,
                "2099-01-01T00:00:00Z",
            ),
        );
    }
    let run_system = |what: &str, script: &str| {
        run_admin(
            admin,
            what,
            &format!(
                "BEGIN; SET LOCAL ROLE wamn_system; {} {script} COMMIT;",
                wamn_control_provision::bind_platform_principal_sql(
                    wamn_control_provision::PlatformComponent::Provisioning
                )
            ),
        )
    };
    run_system(
        "map the authenticated file reader",
        &format!(
            "INSERT INTO wamn_authority.author_login_tenants(login_identity,env_key,org_id,project_id,environment) VALUES ('{mapped}','acme--widgets--dev','{ORG}','{PROJECT}','{ENV}');"
        ),
    );
    let reader = role_url(admin, &mapped, READER_PW);
    run_admin(
        &reader,
        "assert reader attributes",
        "DO $$ BEGIN ASSERT NOT (SELECT rolsuper OR rolbypassrls FROM pg_roles WHERE rolname=current_user); END $$;",
    );
    for relation in [
        "registry.connection",
        "registry.connection_generation",
        "registry.environment_connection_binding",
        "identity.credential",
    ] {
        assert_eq!(
            reader_sqlstate(&reader, &format!("SELECT * FROM {relation}")),
            Some("42501".to_owned())
        );
    }
    let (ok, id, error) = psql_tuples(
        admin,
        &format!("SELECT id::text FROM registry.connection WHERE org='{ORG}' AND name='files'"),
    );
    assert!(ok, "{error}");
    let id = id.trim();
    assert!(!id.is_empty());
    let binding = format!("'{ORG}','{PROJECT}','{ENV}','app','files',NULL,NULL");
    let pin = format!("'{ORG}','{PROJECT}','{ENV}','app',NULL,'{id}',1");
    let binding_pin = format!("'{ORG}','{PROJECT}','{ENV}','app','files','{id}',1");
    assert_eq!(
        reader_sqlstate(&reader, "SET ROLE wamn_conn_reader"),
        Some("42501".to_owned()),
        "the reader inherits its grants but cannot assume the ACL role"
    );
    let lookup = |url: &str, arguments: &str, expected: &str| {
        let (ok, out, error) = psql_tuples(
            url,
            &format!(
                "SET app.org='forged'; SET app.project='forged'; SELECT coalesce(string_agg(concat_ws('|',connection_name,generation,parameters->>'prefix',mode),','),'none') FROM registry.lookup_file_connection({arguments})"
            ),
        );
        assert!(ok, "{arguments}: {error}");
        assert_eq!(out.trim(), expected, "{arguments}");
    };
    let definition = "SELECT pg_get_functiondef(p.oid), pg_get_userbyid(p.proowner), p.provolatile, p.prosecdef, p.proconfig::text FROM pg_proc p WHERE p.oid='registry.lookup_file_connection(text,text,text,text,text,text,bigint)'::regprocedure";
    let (ok, fresh, error) = psql_tuples(admin, definition);
    assert!(ok, "{error}");
    assert!(
        fresh.ends_with("wamn_system|s|t|{search_path=pg_catalog}\n"),
        "{fresh}"
    );
    lookup(&reader, &binding, "files|1|old/|write");
    lookup(&reader, &pin, "files|1|old/|write");
    lookup(&reader, &binding_pin, "files|1|old/|write");
    let (ok, columns, error) = psql_tuples(
        &reader,
        &format!(
            "SELECT array_agg(key ORDER BY key)::text FROM registry.lookup_file_connection({binding}) f CROSS JOIN LATERAL jsonb_object_keys(to_jsonb(f)) AS keys(key)"
        ),
    );
    assert!(ok, "{error}");
    assert_eq!(
        columns.trim(),
        "{connection_id,connection_name,generation,mode,parameters}",
        "the lookup returns coordinates without a secret"
    );
    let (ok, grants, error) = psql_tuples(
        admin,
        "SELECT string_agg(pg_get_userbyid(a.grantee),',' ORDER BY pg_get_userbyid(a.grantee)) FROM pg_proc p CROSS JOIN LATERAL aclexplode(p.proacl) a WHERE p.oid='registry.lookup_file_connection(text,text,text,text,text,text,bigint)'::regprocedure AND a.grantee<>p.proowner AND a.privilege_type='EXECUTE'",
    );
    assert!(ok, "{error}");
    assert_eq!(grants.trim(), "wamn_conn_reader");
    run_admin(
        admin,
        "move the active store as control",
        &format!(
            "BEGIN; SET LOCAL ROLE wamn_control; {} SELECT registry.configure_connection('{ORG}','connection.write','{{\"name\":\"files\",\"integration\":\"gcs\",\"transport\":\"gcs\",\"exchange\":\"request\",\"parameters\":{{\"provider\":\"gcs\",\"container\":\"new-bucket\",\"prefix\":\"new/\"}},\"status\":\"active\"}}'); COMMIT;",
            wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning
            )
        ),
    );
    lookup(&reader, &binding, "files|2|new/|write");
    lookup(&reader, &binding_pin, "files|1|old/|write");
    lookup(&reader, &pin, "files|1|old/|write");
    lookup(&role_url(admin, &unmapped, READER_PW), &binding, "none");
    lookup(&role_url(admin, &unmapped, READER_PW), &pin, "none");
    for arguments in [
        binding.replacen("'acme'", "'other'", 1),
        pin.replacen("'acme'", "'other'", 1),
        binding.replacen("'widgets'", "'unenabled'", 1),
        pin.replacen("'widgets'", "'unenabled'", 1),
        binding.replacen("'dev'", "'prod'", 1),
        pin.replacen("'dev'", "'prod'", 1),
        binding.replacen("'app'", "'another-app'", 1),
        format!("'{ORG}','{PROJECT}','{ENV}','app',NULL,'{id}',999"),
        format!("'{ORG}','{PROJECT}','{ENV}','app','other','{id}',1"),
        binding_pin.replacen("'app'", "'another-app'", 1),
        binding_pin.replace("'files'", "'missing'"),
        format!("'{ORG}','{PROJECT}','{ENV}','app','files','{id}',999"),
        format!("'{ORG}','{PROJECT}','{ENV}','app','files','{id}',NULL"),
        format!("'{ORG}','{PROJECT}','{ENV}','app',NULL,'{id}',NULL"),
        format!("'{ORG}','{PROJECT}','{ENV}','app',NULL,'not-a-uuid',1"),
    ] {
        lookup(&reader, &arguments, "none");
    }
    run_system(
        "downgrade the application's current binding",
        "UPDATE registry.environment_connection_binding SET mode='read';",
    );
    lookup(&reader, &pin, "files|1|old/|read");
    lookup(&reader, &binding_pin, "none");
    run_system(
        "add an independent write alias",
        "INSERT INTO registry.environment_connection_binding SELECT org,project,env,application,'writer',target_id,connection_id,generation,'write',false FROM registry.environment_connection_binding WHERE alias='files';",
    );
    lookup(&reader, &pin, "files|1|old/|write");
    lookup(&reader, &binding, "files|2|new/|read");
    lookup(&reader, &binding_pin, "none");
    lookup(
        &reader,
        &binding_pin.replace("'files'", "'writer'"),
        "files|1|old/|write",
    );
    run_system(
        "remove application bindings",
        "DELETE FROM registry.environment_connection_binding;",
    );
    lookup(&reader, &binding, "none");
    lookup(&reader, &pin, "files|1|old/|read");
    lookup(&reader, &binding_pin, "none");
    run_system(
        "remove project enablement",
        "DELETE FROM registry.project_connection;",
    );
    lookup(&reader, &pin, "none");
    run_system(
        "reenable the project and disable the connection",
        &format!(
            "INSERT INTO registry.project_connection VALUES ('{ORG}','{PROJECT}','{id}'); UPDATE registry.connection SET status='disabled' WHERE id='{id}';"
        ),
    );
    lookup(&reader, &pin, "none");
}
