//! Tests database shapes and author privileges in fresh PostgreSQL 18.
//!
//! It uses a superuser connection to a database on the PostgreSQL server of its
//! test process.

use sqlx::PgConnection as Client;
use sqlx::{Row as _, Statement as _};
use std::collections::BTreeMap;

use wamn_control_provision::identity_role::{
    IDENTITY_ISSUER_ROLE, IDENTITY_ISSUER_TABLES, grant_identity_issuer_surface_sql,
};
use wamn_control_provision::sql;
use wamn_environment_control::apply_package::{self, ApplyPackageRequest};
use wamn_postgres::schema::BareSchemaName;
use wamn_test_infrastructure::locked_database;

const SYSTEM_SCHEMA_SQL: &str = wamn_control_provision::SYSTEM_SCHEMA_SQL;
const CONTROL_PORTABLE_STORE_SQL: &str = wamn_control_provision::CONTROL_PORTABLE_STORE_SQL;
const APP_SCHEMA_SQL: &str = include_str!("../../../deploy/sql/app-schema.sql");
const CURRENT_DATABASE_PUBLIC_CONNECT_SQL: &str =
    include_str!("../../../test-support/fixtures/sql/current-database-public-connect.sql");
const AUTHOR_SQL_ROLES: [&str; 1] = ["wamn_app"];

#[derive(Debug, PartialEq, Eq)]
struct PortableFingerprint {
    columns: Vec<String>,
    constraints: Vec<String>,
}

async fn connect(url: &str) -> Client {
    let client = wamn_postgres::connect(url)
        .await
        .expect("connect to disposable PostgreSQL");

    client
}

async fn prepare_scratch_database(client: &mut Client) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "{CURRENT_DATABASE_PUBLIC_CONNECT_SQL} \
             DROP SCHEMA IF EXISTS audit_app CASCADE; \
             DROP SCHEMA IF EXISTS platform CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS registry CASCADE; \
             DROP SCHEMA IF EXISTS provisioning CASCADE; \
             DROP SCHEMA IF EXISTS identity CASCADE; \
             DO $$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') THEN \
                 CREATE ROLE wamn_system LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   INHERIT NOREPLICATION NOBYPASSRLS; \
               ELSE ALTER ROLE wamn_system LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   INHERIT NOREPLICATION NOBYPASSRLS; END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_app') THEN \
                 CREATE ROLE wamn_app LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   INHERIT NOREPLICATION NOBYPASSRLS; \
               ELSE ALTER ROLE wamn_app LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   INHERIT NOREPLICATION NOBYPASSRLS; END IF; \
                \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_run_projection_writer') THEN \
                 CREATE ROLE wamn_run_projection_writer NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; \
               ELSE ALTER ROLE wamn_run_projection_writer NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                   NOINHERIT NOREPLICATION NOBYPASSRLS; END IF; \
                \
             END $$; \
             DO $$ BEGIN \
               EXECUTE format('REVOKE TEMPORARY ON DATABASE %I FROM PUBLIC', current_database()); \
               EXECUTE format('REVOKE CONNECT ON DATABASE %I FROM wamn_run_projection_writer', current_database()); \
               EXECUTE format('REVOKE CONNECT ON DATABASE %I FROM wamn_app', current_database()); \
               EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_system', current_database()); \
             END $$;"
        ))).execute(&mut *client).await.map(drop)
        .expect("prepare disposable database roles");
}

async fn install_control_database(client: &mut Client) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (sql::ensure_db_owner_role_sql()).to_string(),
    ))
    .execute(&mut *client)
    .await
    .map(drop)
    .expect("ensure the database-owner role that the record history grants name");
    let system_install = format!(
        "SET ROLE wamn_system;\n{SYSTEM_SCHEMA_SQL}\n{CONTROL_PORTABLE_STORE_SQL}\nRESET ROLE;"
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe((&system_install).to_string()))
        .execute(&mut *client)
        .await
        .map(drop)
        .expect("install canonical control-plane schema");
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&grant_identity_issuer_surface_sql()).to_string(),
    ))
    .execute(&mut *client)
    .await
    .map(drop)
    .expect("install canonical identity issuer key-table authority");
    // Make sure of the identity issuer privileges, including SELECT and no TRUNCATE.
    for table in IDENTITY_ISSUER_TABLES {
        let relation = format!("identity.{table}");
        let privileges: bool = sqlx::query(
            "SELECT has_table_privilege($1, $2, 'SELECT') \
                    AND has_table_privilege($1, $2, 'INSERT') \
                    AND has_table_privilege($1, $2, 'UPDATE') \
                    AND has_table_privilege($1, $2, 'DELETE') \
                    AND NOT has_table_privilege($1, $2, 'TRUNCATE')",
        )
        .bind(&IDENTITY_ISSUER_ROLE)
        .bind(&relation)
        .fetch_one(&mut *client)
        .await
        .expect("read identity issuer key-table privileges")
        .get(0);
        assert!(
            privileges,
            "identity issuer privilege boundary drifted on {relation}"
        );
    }
}

async fn install_project_database(client: &mut Client, url: &str) {
    // Package DDL runs as the production database owner, not the administrator.
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (sql::ensure_db_owner_role_sql()).to_string(),
    ))
    .execute(&mut *client)
    .await
    .map(drop)
    .expect("ensure the canonical project database owner");
    let database: String = sqlx::query("SELECT current_database()")
        .fetch_one(&mut *client)
        .await
        .expect("read the disposable project database name")
        .get(0);
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&sql::set_database_owner_sql(&database)).to_string(),
    ))
    .execute(&mut *client)
    .await
    .map(drop)
    .expect("assign the canonical project database owner before package installation");
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             DROP SCHEMA IF EXISTS wamn_history CASCADE")
            .to_string(),
    ))
    .execute(&mut *client)
    .await
    .map(drop)
    .expect("remove same-qualified control portable schemas before project install");
    let schema = BareSchemaName::new("wamn_run").expect("canonical run schema name");
    wamn_environment_control::platform_migrations::install_project(client)
        .await
        .expect("install canonical project schemas through reconciler");
}

/// Portable relations that live only in the control plane.
/// `install_project_database` DROPs the `catalog` and `wamn_run` schemas
/// project-side and no project installer recreates these, so they have no
/// project fingerprint by design. Comparing them across planes is what
/// manufactured the seven-relation drift this list retires.
const CONTROL_ONLY_PORTABLE_RELATIONS: [&str; 1] = [
    // The projected environment identity is a CONTROL fact: the project plane
    // has no `registry.environment_projects` to project from, and its own catalog is
    // recreated per run (wamn-10yt.38).
    "wamn_authority.author_login_projects",
];

async fn portable_fingerprints(
    client: &mut Client,
    relations: &[&str],
) -> BTreeMap<String, PortableFingerprint> {
    let mut fingerprints = relations
        .iter()
        .copied()
        .map(|relation| {
            (
                relation.to_string(),
                PortableFingerprint {
                    columns: Vec::new(),
                    constraints: Vec::new(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    for row in sqlx::query(
        "SELECT n.nspname || '.' || c.relname, \
                    a.attnum::text || ':' || a.attname || ':' || \
                    pg_catalog.format_type(a.atttypid,a.atttypmod) || ':' || \
                    a.attnotnull::text || ':' || \
                    COALESCE(pg_get_expr(d.adbin,d.adrelid,true),'-') \
             FROM pg_catalog.pg_attribute a \
             JOIN pg_catalog.pg_class c ON c.oid=a.attrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
             LEFT JOIN pg_catalog.pg_attrdef d \
               ON d.adrelid=a.attrelid AND d.adnum=a.attnum \
             WHERE a.attnum > 0 AND NOT a.attisdropped \
               AND n.nspname || '.' || c.relname = ANY($1) \
             ORDER BY 1,a.attnum",
    )
    .bind(&relations)
    .fetch_all(&mut *client)
    .await
    .expect("read shared portable columns")
    {
        let relation: String = row.get(0);
        fingerprints
            .get_mut(&relation)
            .expect("shared relation")
            .columns
            .push(row.get(1));
    }
    for row in sqlx::query(
        "SELECT n.nspname || '.' || c.relname, \
                    con.contype::text || ':' || pg_get_constraintdef(con.oid,true) \
             FROM pg_catalog.pg_constraint con \
             JOIN pg_catalog.pg_class c ON c.oid=con.conrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
             WHERE n.nspname || '.' || c.relname = ANY($1) \
               AND con.contype <> 't' \
             ORDER BY 1,2",
    )
    .bind(&relations)
    .fetch_all(&mut *client)
    .await
    .expect("read shared portable constraints")
    {
        let relation: String = row.get(0);
        fingerprints
            .get_mut(&relation)
            .expect("shared relation")
            .constraints
            .push(row.get(1));
    }
    fingerprints
}

/// The one author-writable relation outside row security.
const WRITE_LOG: &str = "platform.write_log";

async fn assert_author_sql_boundaries(client: &mut Client) {
    sqlx::raw_sql("BEGIN READ ONLY")
        .execute(&mut *client)
        .await
        .map(drop)
        .expect("start read-only catalog snapshot");
    let read_only: bool = sqlx::query("SELECT current_setting('transaction_read_only') = 'on'")
        .fetch_one(&mut *client)
        .await
        .expect("verify read-only catalog snapshot")
        .get(0);
    assert!(read_only);

    let rows = sqlx::query(
        "WITH mutation_roles AS ( \
               SELECT c.oid AS relation, c.relowner AS role_oid \
               FROM pg_catalog.pg_class c WHERE c.relkind IN ('r','p') \
               UNION \
               SELECT c.oid, acl.grantee FROM pg_catalog.pg_class c \
               CROSS JOIN LATERAL pg_catalog.aclexplode( \
                   COALESCE(c.relacl, pg_catalog.acldefault('r', c.relowner))) acl \
               WHERE c.relkind IN ('r','p') \
                 AND acl.privilege_type IN ('INSERT','UPDATE','DELETE','TRUNCATE') \
               UNION \
               SELECT c.oid, acl.grantee FROM pg_catalog.pg_class c \
               JOIN pg_catalog.pg_attribute att ON att.attrelid=c.oid \
                 AND att.attnum > 0 AND NOT att.attisdropped AND att.attacl IS NOT NULL \
               CROSS JOIN LATERAL pg_catalog.aclexplode(att.attacl) acl \
               WHERE c.relkind IN ('r','p') AND acl.privilege_type IN ('INSERT','UPDATE') \
             ) \
             SELECT n.nspname || '.' || c.relname, granted_role.rolname, \
                    c.relrowsecurity AND c.relforcerowsecurity \
                    AND EXISTS (SELECT FROM pg_catalog.pg_policy p WHERE p.polrelid=c.oid) \
             FROM mutation_roles mutation \
             JOIN pg_catalog.pg_class c ON c.oid=mutation.relation \
             JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
             JOIN pg_catalog.pg_roles granted_role ON granted_role.oid=mutation.role_oid \
             WHERE granted_role.rolname::text = ANY($1) ORDER BY 1,2",
    )
    .bind(&AUTHOR_SQL_ROLES.as_slice())
    .fetch_all(&mut *client)
    .await
    .expect("read author SQL mutation privileges");
    for row in rows {
        let relation: String = row.get(0);
        let forced_row_security: bool = row.get(2);
        // The write log has no project column and no row policy: the database is
        // the project (docs/architecture/data-access.md#the-write-log). Its
        // grant is the claim insert and the result column alone.
        assert!(
            forced_row_security || relation == WRITE_LOG,
            "{relation} grants author SQL mutation without forced row security"
        );
    }
    sqlx::raw_sql("ROLLBACK")
        .execute(&mut *client)
        .await
        .map(drop)
        .expect("finish read-only catalog snapshot");
}

#[tokio::test]
async fn protected_relations_match_reconciled_postgres() {
    let url = locked_database::database(wamn_test_postgres::database);
    let mut client = connect(&url).await;
    let version: String = sqlx::query("SELECT current_setting('server_version_num')")
        .fetch_one(&mut client)
        .await
        .expect("read protected-relation probe server version")
        .get(0);
    let version: u32 = version.parse().expect("server_version_num is numeric");
    assert!(
        (180_000..190_000).contains(&version),
        "protected-relation derivation requires PostgreSQL 18, found {version}"
    );
    prepare_scratch_database(&mut client).await;
    install_control_database(&mut client).await;
    let control_only_fingerprints =
        portable_fingerprints(&mut client, &CONTROL_ONLY_PORTABLE_RELATIONS).await;
    assert!(
        control_only_fingerprints
            .values()
            .all(|fingerprint| !fingerprint.columns.is_empty()),
        "control-only portable relations must exist in the control plane: {control_only_fingerprints:?}"
    );
    assert_author_sql_boundaries(&mut client).await;
    install_project_database(&mut client, &url).await;
    let project_side_control_only =
        portable_fingerprints(&mut client, &CONTROL_ONLY_PORTABLE_RELATIONS).await;
    assert!(
        project_side_control_only
            .values()
            .all(|fingerprint| fingerprint.columns.is_empty()),
        "control-only portable relations must not be installed project-side: \
         {project_side_control_only:?}"
    );
    assert_author_sql_boundaries(&mut client).await;
}
