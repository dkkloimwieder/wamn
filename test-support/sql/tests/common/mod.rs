//! The refusals that an installed catalog implies.

use std::borrow::Cow;

use anyhow::{Result, bail, ensure};
use postgres_protocol::escape::escape_identifier;
use sqlx::{Connection as _, PgConnection};
use url::Url;
use wamn_test_infrastructure::containers::Postgres;
use wamn_test_infrastructure::database::{self, execute};

/// Schemas that PostgreSQL owns.
const USER_SCHEMA: &str = "n.nspname <> ALL (ARRAY['pg_catalog', 'information_schema']) \
     AND n.nspname !~ '^pg_(toast|temp)'";

/// The functions of the user schemas, other than extension members. The extension
/// check covers those. The support functions of a user aggregate are user functions.
const SCHEMA_FUNCTION: &str = "USER_SCHEMA AND NOT EXISTS (
    SELECT FROM pg_catalog.pg_depend d
     WHERE d.classid = 'pg_catalog.pg_proc'::regclass AND d.objid = p.oid AND d.deptype = 'e')";

/// A value built for the pseudo type `t`, or NULL for a pseudo type with no value.
const PSEUDO_VALUE: &str = "CASE t.typname
    WHEN 'record' THEN 'ROW(NULL::integer)'
    WHEN 'any' THEN 'NULL::integer'
    WHEN 'anyelement' THEN 'NULL::integer'
    WHEN 'anynonarray' THEN 'NULL::integer'
    WHEN 'anycompatible' THEN 'NULL::integer'
    WHEN 'anycompatiblenonarray' THEN 'NULL::integer'
    WHEN 'anyarray' THEN 'NULL::integer[]'
    WHEN 'anycompatiblearray' THEN 'NULL::integer[]'
    WHEN 'anyrange' THEN 'NULL::int4range'
    WHEN 'anycompatiblerange' THEN 'NULL::int4range'
    WHEN 'anymultirange' THEN 'NULL::int4multirange'
    WHEN 'anycompatiblemultirange' THEN 'NULL::int4multirange'
  END";

/// Every action outside the effective privileges of `$1`, from the catalog of the current
/// database: column reads and writes, row deletes, truncation, sequence use, function,
/// aggregate and procedure calls, object creation, and role changes.
const REFUSED_ACTIONS_SQL: &str = r"
WITH relation AS (
    SELECT c.oid, c.relkind, format('%I.%I', n.nspname, c.relname) AS name
      FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
     WHERE USER_SCHEMA AND c.relkind IN ('r', 'p', 'v', 'm', 'f', 'S')
), column_privilege AS (
    SELECT r.oid, r.name, a.attname, p.privilege
      FROM relation r
      JOIN pg_catalog.pg_attribute a ON a.attrelid = r.oid AND a.attnum > 0 AND NOT a.attisdropped
     CROSS JOIN (VALUES ('SELECT'), ('INSERT'), ('UPDATE')) AS p(privilege)
     WHERE r.relkind IN ('r', 'p') OR (r.relkind IN ('v', 'm', 'f') AND p.privilege = 'SELECT')
), routine AS (
    SELECT p.oid, p.prokind, n.nspname, p.proname,
           coalesce((SELECT string_agg(
                         CASE WHEN p.provariadic <> 0 AND a.ordinality = p.pronargs
                              THEN 'VARIADIC ' ELSE '' END
                         || '(SELECT ' || CASE WHEN t.typtype <> 'p'
                                               THEN 'NULL::' || pg_catalog.format_type(a.type, NULL)
                                               ELSE PSEUDO_VALUE END || ')',
                         ', ' ORDER BY a.ordinality)
                       FROM unnest(p.proargtypes::oid[]) WITH ORDINALITY AS a(type, ordinality)
                       JOIN pg_catalog.pg_type t ON t.oid = a.type),
                    '') AS arguments
      FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
     WHERE SCHEMA_FUNCTION
       -- A trigger function cannot be called: the function grant check covers it.
       AND p.prorettype <> ALL (ARRAY['trigger'::regtype, 'event_trigger'::regtype])
       -- Each value is a subquery, because the planner folds a strict call on a NULL
       -- constant without the EXECUTE check. A pseudo type with no value, such as
       -- internal, is checked in the catalog only.
       AND NOT EXISTS (SELECT FROM unnest(p.proargtypes::oid[]) AS a(type)
                         JOIN pg_catalog.pg_type t ON t.oid = a.type
                        WHERE t.typtype = 'p' AND PSEUDO_VALUE IS NULL)
)
SELECT CASE privilege
         WHEN 'SELECT' THEN format('SELECT %I FROM %s WHERE false', attname, name)
         WHEN 'INSERT' THEN format('INSERT INTO %s (%I) VALUES (DEFAULT)', name, attname)
         ELSE format('UPDATE %s SET %I = DEFAULT WHERE false', name, attname)
       END
  FROM column_privilege WHERE NOT has_column_privilege($1::name, oid, attname, privilege)
UNION ALL
SELECT format('DELETE FROM %s WHERE false', name) FROM relation
 WHERE relkind IN ('r', 'p') AND NOT has_table_privilege($1::name, oid, 'DELETE')
UNION ALL
SELECT format('TRUNCATE %s', name) FROM relation
 WHERE relkind IN ('r', 'p') AND NOT has_table_privilege($1::name, oid, 'TRUNCATE')
UNION ALL
SELECT format('CREATE TRIGGER wamn_grant_probe BEFORE UPDATE ON %s FOR EACH ROW '
              'EXECUTE FUNCTION pg_catalog.suppress_redundant_updates_trigger()', name)
  FROM relation WHERE relkind IN ('r', 'p') AND NOT has_table_privilege($1::name, oid, 'TRIGGER')
UNION ALL
SELECT format('SELECT nextval(%L::regclass)', name) FROM relation
 -- CASE orders the checks: has_sequence_privilege refuses a relation of another kind.
 WHERE CASE WHEN relkind = 'S' THEN NOT has_sequence_privilege($1::name, oid, 'USAGE')
                                     AND NOT has_sequence_privilege($1::name, oid, 'UPDATE') END
UNION ALL
SELECT format('SELECT last_value FROM %s', name) FROM relation
 WHERE CASE WHEN relkind = 'S' THEN NOT has_sequence_privilege($1::name, oid, 'SELECT') END
UNION ALL
SELECT format(CASE prokind WHEN 'p' THEN 'CALL %I.%I(%s)'
                              WHEN 'w' THEN 'SELECT %I.%I(%s) OVER ()'
                              ELSE 'SELECT %I.%I(%s)' END,
              nspname, proname, arguments)
  FROM routine WHERE NOT has_function_privilege($1::name, oid, 'EXECUTE')
UNION ALL
SELECT format('CREATE TABLE %I.wamn_grant_probe ()', n.nspname) FROM pg_catalog.pg_namespace n
 WHERE USER_SCHEMA AND NOT has_schema_privilege($1::name, n.oid, 'CREATE')
UNION ALL
SELECT 'CREATE SCHEMA wamn_grant_probe'
 WHERE NOT has_database_privilege($1::name, current_database(), 'CREATE')
UNION ALL
SELECT 'CREATE TEMPORARY TABLE wamn_grant_probe ()'
 WHERE NOT has_database_privilege($1::name, current_database(), 'TEMPORARY')
";

/// The tables that `$1` may reference in a foreign key, other than its own.
const REFERENCES_SQL: &str = "
SELECT c.oid::regclass::text FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
 WHERE USER_SCHEMA AND c.relkind IN ('r', 'p') AND NOT pg_has_role($1::name, c.relowner, 'USAGE')
   AND (has_table_privilege($1::name, c.oid, 'REFERENCES')
        OR has_any_column_privilege($1::name, c.oid, 'REFERENCES'))";

/// The functions, other than its own, that `$1` executes without a grant to a role it uses,
/// and the trigger functions that it executes at all.
const FUNCTION_GRANT_SQL: &str = "
SELECT p.oid::regprocedure::text FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
 WHERE SCHEMA_FUNCTION AND NOT pg_has_role($1::name, p.proowner, 'USAGE')
   AND has_function_privilege($1::name, p.oid, 'EXECUTE')
   AND (p.prorettype = ANY (ARRAY['trigger'::regtype, 'event_trigger'::regtype])
        OR NOT EXISTS (SELECT FROM aclexplode(coalesce(p.proacl, acldefault('f', p.proowner))) a
                        WHERE a.privilege_type = 'EXECUTE' AND a.grantee <> 0
                          AND pg_has_role($1::name, a.grantee, 'USAGE')))";

/// Role changes the session cannot make. PostgreSQL checks SET ROLE against the
/// session login, so only a login actor runs them.
const REFUSED_ROLE_SQL: &str = r"
SELECT format('SET ROLE %I', rolname) FROM pg_catalog.pg_roles
 WHERE rolname <> $1::name AND NOT pg_has_role($1::name, oid, 'SET')
";

/// Log in as `login`, and check that the server sees that login.
pub async fn log_in(url: &Url) -> Result<PgConnection> {
    let mut connection = wamn_postgres::connect(url.as_str()).await?;
    let current: String = sqlx::query_scalar("SELECT current_user::text")
        .fetch_one(&mut connection)
        .await?;
    ensure!(current == url.username(), "logged in as {current}");
    Ok(connection)
}

/// The SQLSTATE of a database error.
pub fn sqlstate(error: &sqlx::Error) -> Option<Cow<'_, str>> {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
}

/// Run each action as `actor` in a rolled-back transaction, and check that the server refuses
/// each one with SQLSTATE 42501. Return the number of actions.
pub async fn assert_refused(actor: &mut PgConnection, label: &str, actions: &[String]) -> Result<usize> {
    let mut failures = Vec::new();
    for action in actions {
        let mut transaction = actor.begin().await?;
        let result = sqlx::raw_sql(sqlx::AssertSqlSafe(action.as_str()))
            .execute(&mut *transaction)
            .await;
        transaction.rollback().await?;
        match result {
            Ok(_) => failures.push(format!("{action}: accepted")),
            Err(error) if sqlstate(&error).as_deref() == Some("42501") => {}
            Err(error) => failures.push(format!("{action}: {error}")),
        }
    }
    ensure!(
        failures.is_empty(),
        "{label} was not refused:\n{}",
        failures.join("\n")
    );
    Ok(actions.len())
}

/// The actions outside the effective privileges of `role` in the database of `admin`.
///
/// The set is empty for a role that owns every object, such as the system migration login.
pub async fn refused_actions(admin: &mut PgConnection, role: &str) -> Result<Vec<String>> {
    let sql = REFUSED_ACTIONS_SQL
        .replace("SCHEMA_FUNCTION", SCHEMA_FUNCTION)
        .replace("PSEUDO_VALUE", PSEUDO_VALUE)
        .replace("USER_SCHEMA", USER_SCHEMA);
    let actions: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(role)
        .fetch_all(&mut *admin)
        .await?;
    Ok(actions)
}

/// Check the refusals that the catalog alone shows for `role`. It may reference no table
/// that it does not own, it executes no trigger function, and it executes no other
/// function without a grant to a role that it uses.
pub async fn assert_catalog_refusals(admin: &mut PgConnection, role: &str) -> Result<()> {
    let references: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(
        REFERENCES_SQL.replace("USER_SCHEMA", USER_SCHEMA),
    ))
    .bind(role)
    .fetch_all(&mut *admin)
    .await?;
    ensure!(references.is_empty(), "{role} may reference {references:?}");
    let sql = FUNCTION_GRANT_SQL
        .replace("SCHEMA_FUNCTION", SCHEMA_FUNCTION)
        .replace("USER_SCHEMA", USER_SCHEMA);
    let functions: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(role)
        .fetch_all(&mut *admin)
        .await?;
    ensure!(
        functions.is_empty(),
        "{role} executes without a grant to it: {functions:?}"
    );
    Ok(())
}

/// The installed extensions are the server's own, read from template1, and the `declared` ones.
pub async fn assert_extensions(
    postgres: &Postgres,
    admin: &mut PgConnection,
    declared: &[String],
) -> Result<()> {
    let extensions = "SELECT extname::text FROM pg_catalog.pg_extension ORDER BY 1";
    let mut server =
        wamn_postgres::connect(database::database_admin_url(postgres, "template1")?.as_str())
            .await?;
    let mut expected: Vec<String> = sqlx::query_scalar(extensions)
        .fetch_all(&mut server)
        .await?;
    expected.extend_from_slice(declared);
    expected.sort();
    let installed: Vec<String> = sqlx::query_scalar(extensions)
        .fetch_all(&mut *admin)
        .await?;
    ensure!(
        installed == expected,
        "installed extensions {installed:?}, expected {expected:?}"
    );
    Ok(())
}

/// Check every refusal of a login: the catalog-derived actions, role changes,
/// self-elevation and databases it may not connect to. Return the number checked.
pub async fn assert_login_refusals(
    admin: &mut PgConnection,
    postgres: &Postgres,
    login: &str,
    password: &str,
    actor: &mut PgConnection,
    contract: &[String],
) -> Result<usize> {
    let mut actions = refused_actions(admin, login).await?;
    let roles: Vec<String> = sqlx::query_scalar(REFUSED_ROLE_SQL)
        .bind(login)
        .fetch_all(&mut *admin)
        .await?;
    actions.extend(roles);
    for attribute in ["SUPERUSER", "BYPASSRLS", "CREATEROLE", "CREATEDB", "REPLICATION"] {
        actions.push(format!("ALTER ROLE {} {attribute}", escape_identifier(login)));
    }
    actions.extend_from_slice(contract);
    let mut checked = assert_refused(actor, login, &actions).await?;
    assert_catalog_refusals(admin, login).await?;

    let databases: Vec<String> = sqlx::query_scalar(
        "SELECT datname::text FROM pg_catalog.pg_database \
          WHERE datallowconn AND NOT has_database_privilege($1::name, oid, 'CONNECT')",
    )
    .bind(login)
    .fetch_all(&mut *admin)
    .await?;
    for database in databases {
        let url = postgres.database_url(&database, login, password)?;
        match wamn_postgres::connect(url.as_str()).await {
            Ok(_) => bail!("{login} connected to {database}"),
            Err(error) => {
                let code = error.downcast_ref::<sqlx::Error>().and_then(sqlstate);
                ensure!(
                    code.as_deref() == Some("42501"),
                    "{login} connecting to {database}: {error:#}"
                );
            }
        }
        checked += 1;
    }
    Ok(checked)
}

/// The role holds no elevated attribute and reaches no elevated role.
pub async fn assert_unelevated(admin: &mut PgConnection, role: &str, replication: bool) -> Result<()> {
    let (elevated, replicates, reaches): (bool, bool, bool) = sqlx::query_as(
        "SELECT rolsuper OR rolbypassrls OR rolcreatedb OR rolcreaterole, rolreplication, \
                EXISTS (SELECT FROM pg_catalog.pg_roles r \
                         WHERE r.oid <> l.oid \
                           AND (r.rolsuper OR r.rolbypassrls OR r.rolcreatedb OR r.rolcreaterole) \
                           AND pg_has_role(l.oid, r.oid, 'MEMBER')) \
           FROM pg_catalog.pg_roles l WHERE rolname = $1::name",
    )
    .bind(role)
    .fetch_one(&mut *admin)
    .await?;
    ensure!(!elevated, "{role} holds an elevated attribute");
    ensure!(replicates == replication, "{role} replication is {replicates}");
    ensure!(!reaches, "{role} is a member of an elevated role");
    Ok(())
}

/// The role can pass on no privilege, through itself, its groups or PUBLIC.
pub async fn assert_no_grant_options(admin: &mut PgConnection, role: &str) -> Result<()> {
    let grantable: Vec<String> = sqlx::query_scalar(
        "WITH privilege AS (
           SELECT c.oid::regclass::text AS object, a.* FROM pg_catalog.pg_class c CROSS JOIN LATERAL aclexplode(c.relacl) a
           UNION ALL SELECT c.attrelid::regclass::text, a.* FROM pg_catalog.pg_attribute c CROSS JOIN LATERAL aclexplode(c.attacl) a
           UNION ALL SELECT n.nspname::text, a.* FROM pg_catalog.pg_namespace n CROSS JOIN LATERAL aclexplode(n.nspacl) a
           UNION ALL SELECT p.oid::regprocedure::text, a.* FROM pg_catalog.pg_proc p CROSS JOIN LATERAL aclexplode(p.proacl) a
           UNION ALL SELECT d.datname::text, a.* FROM pg_catalog.pg_database d CROSS JOIN LATERAL aclexplode(d.datacl) a)
         SELECT DISTINCT object FROM privilege
          WHERE is_grantable AND (grantee = 0 OR pg_has_role($1::name, grantee, 'USAGE'))",
    )
    .bind(role)
    .fetch_all(&mut *admin)
    .await?;
    ensure!(grantable.is_empty(), "{role} can pass on privileges: {grantable:?}");
    Ok(())
}

/// The relations that `role` can write, other than its own transaction identity, outside `schema`.
pub async fn writes_outside(admin: &mut PgConnection, role: &str, schema: Option<&str>) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT c.oid::regclass::text FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
          WHERE {USER_SCHEMA} AND n.nspname IS DISTINCT FROM $2 AND c.relkind IN ('r', 'p', 'v', 'm', 'f')
            AND (has_table_privilege($1::name, c.oid, 'UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER')
                 OR has_any_column_privilege($1::name, c.oid, 'UPDATE, REFERENCES')
                 OR (c.oid <> 'platform.tx_identity'::regclass
                     AND (has_table_privilege($1::name, c.oid, 'INSERT')
                          OR has_any_column_privilege($1::name, c.oid, 'INSERT'))))"
    )))
    .bind(role)
    .bind(schema)
    .fetch_all(&mut *admin)
    .await?)
}

/// The logins mapped to platform principals that `role` reaches by membership.
pub async fn mapped_logins_reached(admin: &mut PgConnection, role: &str) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        "SELECT target.rolname::text FROM pg_catalog.pg_roles target
           JOIN platform.login_principal mapped ON mapped.login = target.rolname
          WHERE target.rolname <> $1::name AND pg_has_role($1::name, target.oid, 'MEMBER')",
    )
    .bind(role)
    .fetch_all(&mut *admin)
    .await?)
}

/// Every login mapped to a principal is restricted and maps to a platform component.
pub async fn assert_mapped_logins_are_platform_components(admin: &mut PgConnection) -> Result<()> {
    let rows: Vec<(String, wamn_identity_protocol::PrincipalId, bool)> = sqlx::query_as(
        "SELECT p.login::text, p.principal_id,
                NOT (r.rolsuper OR r.rolbypassrls OR r.rolcreatedb OR r.rolcreaterole OR r.rolreplication)
           FROM platform.login_principal p JOIN pg_catalog.pg_roles r ON r.rolname = p.login",
    )
    .fetch_all(&mut *admin)
    .await?;
    ensure!(!rows.is_empty(), "the install maps no login to a principal");
    for (login, principal, restricted) in rows {
        ensure!(restricted, "mapped login {login} can bypass database privileges");
        ensure!(
            wamn_identity_protocol::PlatformComponent::ALL
                .into_iter()
                .any(|component| component.principal_id() == principal),
            "{login} maps to an unknown principal {principal}"
        );
    }
    Ok(())
}

/// A role with only PUBLIC access executes no security definer.
pub async fn assert_public_executes_no_definer(admin: &mut PgConnection) -> Result<usize> {
    const PUBLIC_ONLY: &str = "wamn_public_probe";
    execute(admin, &format!("CREATE ROLE {PUBLIC_ONLY} NOLOGIN")).await?;
    let executable: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT p.oid::regprocedure::text FROM pg_catalog.pg_proc p
           JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
          WHERE {USER_SCHEMA} AND p.prosecdef AND has_function_privilege($1::name, p.oid, 'EXECUTE')"
    )))
    .bind(PUBLIC_ONLY)
    .fetch_all(&mut *admin)
    .await?;
    ensure!(executable.is_empty(), "PUBLIC executes security definers: {executable:?}");
    let actions = refused_actions(admin, PUBLIC_ONLY).await?;
    execute(admin, &format!("SET ROLE {PUBLIC_ONLY}")).await?;
    let checked = assert_refused(admin, PUBLIC_ONLY, &actions).await;
    execute(admin, "RESET ROLE").await?;
    checked
}
