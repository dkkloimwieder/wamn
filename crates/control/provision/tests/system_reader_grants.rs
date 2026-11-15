//! Reader credentials permit their consuming queries and refuse foreign writes.
//!
//! The test reads the SERVER'S OWN ANSWER, never the text of a statement:
//! `has_schema_privilege`, `has_table_privilege`, `pg_roles` attributes,
//! `aclexplode`, and the real SQLSTATE a refused statement raises. Pinning DDL
//! text would show nothing here — this crate emits the DDL, so a text
//! assertion would be testing a builder against itself, and it cannot tell a
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

use wamn_control_provision::sql::grant_identity_reader_surface_sql;
use wamn_control_provision::{
    SYSTEM_SCHEMA_SQL, SystemReader, WorkloadRoleFamily, system_reader_login,
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

fn generation_role(reader: SystemReader, database: &str) -> String {
    system_reader_login(reader, ORG, PROJECT, ENV, database)
}

/// Drop every role this acceptance test mints, so a leftover HEALTHY role cannot satisfy
/// the builder's `IF NOT EXISTS` and mask a mutated one.
///
/// Roles are CLUSTER-wide, so re-creating the schemas does not reach them.
/// `DROP OWNED BY` precedes `DROP ROLE` inside an existence test: a privilege
/// left behind in this database would otherwise refuse the drop outright.
fn reset_roles(admin_url: &str, database: &str) {
    let mut roles = Vec::new();
    for reader in SystemReader::ALL {
        roles.push(generation_role(reader, database));
        roles.push(generation_role(reader, database));
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

/// Read whether any privilege permits delegation to another role.
const ROLE_GRANTS_SQL: &str = "SELECT grantable FROM ( \
           SELECT 'relation' AS type, n.nspname::text AS sch, c.relname::text AS obj, \
                  x.privilege_type::text AS priv, x.is_grantable AS grantable \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             CROSS JOIN LATERAL aclexplode(c.relacl) x \
            WHERE x.grantee = $1::text::regrole \
           UNION ALL \
           SELECT 'column', n.nspname::text, c.relname || '.' || a.attname, \
                  x.privilege_type::text, x.is_grantable \
             FROM pg_catalog.pg_attribute a \
             JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             CROSS JOIN LATERAL aclexplode(a.attacl) x \
            WHERE x.grantee = $1::text::regrole \
           UNION ALL \
           SELECT 'schema', n.nspname::text, n.nspname::text, \
                  x.privilege_type::text, x.is_grantable \
             FROM pg_catalog.pg_namespace n CROSS JOIN LATERAL aclexplode(n.nspacl) x \
            WHERE x.grantee = $1::text::regrole \
           UNION ALL \
           SELECT 'routine', n.nspname::text, p.proname::text, x.privilege_type::text, x.is_grantable \
             FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace \
             CROSS JOIN LATERAL aclexplode(p.proacl) x WHERE x.grantee=$1::text::regrole \
           UNION ALL \
           SELECT 'database', '', d.datname::text, x.privilege_type::text, x.is_grantable \
             FROM pg_catalog.pg_database d CROSS JOIN LATERAL aclexplode(d.datacl) x \
            WHERE x.grantee = $1::text::regrole \
         ) q ORDER BY 1;";

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

#[sqlx::test(migrations = false)]
async fn the_identity_reader_can_never_write_identity_and_neither_reader_reaches_the_other(
    pool: sqlx::PgPool,
) {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::url(&pool);
    let admin_url = test_database.as_str().to_owned();
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
        (
            SystemReader::Connection,
            wamn_control_provision::sql::grant_connection_reader_surface_sql(),
        ),
    ] {
        let role = generation_role(reader, &database);
        run_admin(&admin_url, "converge a system reader", &grant);
        // Replayed: the convergent batch must narrow-and-grant to the same state.
        run_admin(&admin_url, "replay a convergent grant", &grant);
        run_admin(
            &admin_url,
            "prepare a system-reader generation",
            &wamn_control_provision::test_database::login_sql(
                reader.family(),
                &database,
                &role,
                READER_PW,
            ),
        );
        reader_url.push((reader, role_url(&admin_url, &role, READER_PW), role));
    }

    // No reader can delegate its privileges to another role.
    for (reader, _, _) in &reader_url {
        let grants: Vec<bool> = sqlx::query_scalar(ROLE_GRANTS_SQL)
            .bind(reader.family().acl_role())
            .fetch_all(&pool)
            .await
            .unwrap();
        assert!(grants.into_iter().all(|grantable| !grantable));
    }
    assert_eq!(
        reader_sqlstate(&reader_url[1].1, "SELECT id FROM registry.connection"),
        Some("42501".to_owned()),
        "the connection reader cannot directly read connection rows"
    );
    assert_eq!(
        reader_sqlstate(&reader_url[1].1, "DELETE FROM registry.connection"),
        Some("42501".to_owned()),
        "the connection reader cannot change connection rows"
    );

    let identity_url = &reader_url[0].1;
    let (ok, _, error) = psql_tuples(
        identity_url,
        "SELECT principal_id, org, status FROM identity.org_memberships",
    );
    assert!(
        ok,
        "the identity reader can read active membership: {error}"
    );
    for denied in [
        "SELECT updated_by FROM identity.org_memberships",
        "SELECT status FROM identity.org_memberships FOR UPDATE",
        "UPDATE identity.org_memberships SET status='active'",
    ] {
        assert_eq!(
            reader_sqlstate(identity_url, denied),
            Some("42501".to_owned()),
            "{denied}"
        );
    }

    // Enumerate installed objects. Effective privileges include inherited,
    // PUBLIC and column grants, even when they cannot be passed to another role.
    for (reader, _, role) in &reader_url {
        let tables: Vec<(String, String, String, bool)> = sqlx::query_as(
            "SELECT n.nspname,c.relname,p.privilege_type,
                    has_table_privilege($1,c.oid,p.privilege_type)
             FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
             CROSS JOIN LATERAL aclexplode(acldefault('r',c.relowner)) p
             WHERE n.nspname NOT LIKE 'pg_%' AND n.nspname<>'information_schema'
               AND c.relkind IN ('r','p','v','m','f')",
        )
        .bind(role)
        .fetch_all(&pool)
        .await
        .unwrap();
        for (schema, relation, privilege, permitted) in tables {
            let expected = schema == "identity"
                && match reader {
                    SystemReader::Identity => {
                        privilege == "SELECT"
                            && wamn_control_provision::sql::IDENTITY_READER_RELATIONS
                                .contains(&relation.as_str())
                    }
                    SystemReader::Connection => {
                        matches!(relation.as_str(), "oauth_token" | "oauth_authorization")
                            && matches!(
                                privilege.as_str(),
                                "SELECT" | "INSERT" | "UPDATE" | "DELETE"
                            )
                    }
                };
            assert_eq!(
                permitted, expected,
                "{reader}: {privilege} on {schema}.{relation}"
            );
        }
        let columns: Vec<(String, String, String, String, bool)> = sqlx::query_as(
            "SELECT n.nspname,c.relname,a.attname,p.privilege,
                    has_column_privilege($1,c.oid,a.attnum,p.privilege)
             FROM pg_attribute a JOIN pg_class c ON c.oid=a.attrelid
             JOIN pg_namespace n ON n.oid=c.relnamespace
             CROSS JOIN unnest(ARRAY['SELECT','INSERT','UPDATE','REFERENCES']) p(privilege)
             WHERE a.attnum>0 AND NOT a.attisdropped
               AND n.nspname NOT LIKE 'pg_%' AND n.nspname<>'information_schema'
               AND c.relkind IN ('r','p','v','m','f')",
        )
        .bind(role)
        .fetch_all(&pool)
        .await
        .unwrap();
        for (schema, relation, column, privilege, permitted) in columns {
            let expected = schema == "identity"
                && match reader {
                    SystemReader::Identity => {
                        privilege == "SELECT"
                            && (wamn_control_provision::sql::IDENTITY_READER_RELATIONS
                                .contains(&relation.as_str())
                                || wamn_control_provision::sql::IDENTITY_READER_COLUMNS
                                    .iter()
                                    .any(|(table, columns)| {
                                        *table == relation && columns.contains(&column.as_str())
                                    }))
                    }
                    SystemReader::Connection => {
                        matches!(relation.as_str(), "oauth_token" | "oauth_authorization")
                            && matches!(privilege.as_str(), "SELECT" | "INSERT" | "UPDATE")
                    }
                };
            assert_eq!(
                permitted, expected,
                "{reader}: {privilege} on {schema}.{relation}.{column}"
            );
        }
        let sequences: Vec<(String, String, bool)> = sqlx::query_as(
            "SELECT n.nspname,c.relname,has_sequence_privilege($1,c.oid,'USAGE,SELECT,UPDATE')
             FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace
             WHERE c.relkind='S' AND n.nspname NOT LIKE 'pg_%'",
        )
        .bind(role)
        .fetch_all(&pool)
        .await
        .unwrap();
        for (schema, sequence, permitted) in sequences {
            assert!(!permitted, "{reader} reaches sequence {schema}.{sequence}");
        }
        let schemas: Vec<(String, bool, bool)> = sqlx::query_as(
            "SELECT nspname,has_schema_privilege($1,oid,'CREATE'),has_schema_privilege($1,oid,'USAGE')
             FROM pg_namespace WHERE nspname NOT LIKE 'pg_%' AND nspname<>'information_schema'"
        ).bind(role).fetch_all(&pool).await.unwrap();
        for (schema, create, usage) in schemas {
            assert!(!create, "{reader} can create objects in {schema}");
            assert_eq!(
                usage,
                schema == "public"
                    || schema == "identity"
                    || (*reader == SystemReader::Connection && schema == "registry"),
                "{reader} schema access: {schema}"
            );
        }
    }

    // --- each reader's OWN session ------------------------------------------
    for (reader, url, _) in &reader_url {
        run_admin(
            url,
            "a reader session's own self-test",
            "DO $self$ BEGIN \
               ASSERT NOT (SELECT rolsuper OR rolbypassrls FROM pg_roles \
                            WHERE rolname = current_user), \
                 'this session is superuser or bypasses RLS and establishes nothing'; \
             END $self$;",
        );
        // Neither reader may reach the OTHER's plane, in either direction.
        let foreign = match reader {
            SystemReader::Identity => "SELECT publication FROM registry.event_readers",
            SystemReader::Connection => "SELECT token_hash FROM identity.pats",
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

#[sqlx::test(migrations = false)]
async fn the_connection_reader_cannot_read_or_change_credential_metadata_directly(
    pool: sqlx::PgPool,
) {
    let _serialized = wamn_test_postgres::lock();
    let test_database = wamn_test_postgres::url(&pool);
    let admin = test_database.as_str();
    reset_roles(admin, test_database.path().trim_start_matches('/'));
    apply_system_schema(admin);
    run_admin(
        admin,
        "grant the connection reader surface",
        &wamn_control_provision::sql::grant_connection_reader_surface_sql(),
    );
    let role = generation_role(
        SystemReader::Connection,
        test_database.path().trim_start_matches('/'),
    );
    run_admin(
        admin,
        "create an authenticated reader",
        &wamn_control_provision::test_database::login_sql(
            WorkloadRoleFamily::ConnectionReader,
            test_database.path().trim_start_matches('/'),
            &role,
            READER_PW,
        ),
    );
    let reader = role_url(admin, &role, READER_PW);
    run_admin(
        &reader,
        "assert that the reader cannot bypass permissions",
        "DO $$ BEGIN ASSERT NOT (SELECT rolsuper OR rolbypassrls FROM pg_roles WHERE rolname=current_user); END $$;",
    );
    for (relation, column) in [
        ("identity.credential", "version"),
        ("registry.external_system", "name"),
        ("registry.integration", "name"),
        ("registry.connection", "status"),
        ("registry.connection_generation", "generation"),
    ] {
        for statement in [
            format!("SELECT * FROM {relation}"),
            format!("UPDATE {relation} SET {column}={column}"),
            format!("DELETE FROM {relation}"),
        ] {
            assert_eq!(
                reader_sqlstate(&reader, &statement),
                Some("42501".to_owned()),
                "the reader cannot access {relation} directly"
            );
        }
    }
}
