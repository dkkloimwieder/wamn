//! Live test of `system/0008_environment_status.sql` (wamn-zua8.2): on a
//! control database installed before it, the migration gives
//! `registry.project_envs` the status column as a fresh install has it, and
//! the identity issuer and the control family the grants on it that their
//! prepares grant. The test holds
//! the process lock of its server, because the installers create cluster-wide
//! roles.

use tokio_postgres::{Client, NoTls};
use wamn_control::provision_system::{ProvisionSystemRequest, provision_system};
use wamn_control_provision::identity_issuer::grant_identity_issuer_surface_sql;
use wamn_control_provision::sql::grant_control_surface_sql;
use wamn_test_infrastructure::locked_database;

const MIGRATION: &str =
    include_str!("../../../../deploy/sql/migrations/system/0008_environment_status.sql");

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to the disposable database");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

/// The columns and constraints of `registry.project_envs`.
async fn definition(client: &Client) -> Vec<String> {
    client
        .query(
            "SELECT entry FROM ( \
               SELECT 'column ' || a.attname || ' ' || format_type(a.atttypid, a.atttypmod) \
                      || ' ' || a.attnotnull::text || ' ' \
                      || coalesce(pg_get_expr(d.adbin, d.adrelid), '') AS entry \
                 FROM pg_attribute a \
                 LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
                WHERE a.attrelid = 'registry.project_envs'::regclass \
                  AND a.attnum > 0 AND NOT a.attisdropped \
               UNION ALL \
               SELECT 'constraint ' || conname || ' ' || pg_get_constraintdef(oid) \
                 FROM pg_constraint WHERE conrelid = 'registry.project_envs'::regclass \
             ) q ORDER BY entry COLLATE \"C\"",
            &[],
        )
        .await
        .expect("read the definition of registry.project_envs")
        .iter()
        .map(|row| row.get(0))
        .collect()
}

/// The table and column grants of `role` on `registry.project_envs`.
async fn grants(client: &Client, role: &str) -> Vec<String> {
    client
        .query(
            "SELECT entry FROM ( \
               SELECT 'table ' || x.privilege_type AS entry \
                 FROM pg_class c, aclexplode(c.relacl) x, pg_roles r \
                WHERE c.oid = 'registry.project_envs'::regclass \
                  AND x.grantee = r.oid AND r.rolname = $1 \
               UNION ALL \
               SELECT 'column ' || a.attname || ' ' || x.privilege_type \
                 FROM pg_attribute a, aclexplode(a.attacl) x, pg_roles r \
                WHERE a.attrelid = 'registry.project_envs'::regclass \
                  AND x.grantee = r.oid AND r.rolname = $1 \
             ) q ORDER BY entry",
            &[&role],
        )
        .await
        .expect("read the grants on registry.project_envs")
        .iter()
        .map(|row| row.get(0))
        .collect()
}

#[tokio::test]
async fn the_migration_adds_the_status_column_as_a_fresh_install_has_it() {
    let url = locked_database::database(wamn_test_postgres::database);
    let client = connect(&url).await;
    client
        .batch_execute(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN \
               CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                 NOREPLICATION NOBYPASSRLS; \
             END IF; END $$;",
        )
        .await
        .expect("create the wamn_system role");
    provision_system(&ProvisionSystemRequest {
        system_database_url: url.to_string(),
        platform_domain: "wamn.example.test".to_owned(),
    })
    .await
    .expect("install the control store");
    client
        .batch_execute(&format!(
            "{} {}",
            grant_identity_issuer_surface_sql(),
            grant_control_surface_sql()
        ))
        .await
        .expect("prepare the identity issuer and control surfaces");
    let fresh = definition(&client).await;
    let issuer = grants(&client, "wamn_identity_issuer").await;
    let control = grants(&client, "wamn_control").await;
    assert!(
        issuer.iter().any(|entry| entry == "column status SELECT"),
        "the issuer prepare grants the status read: {issuer:?}"
    );
    assert_eq!(
        control,
        ["column status UPDATE", "table SELECT"],
        "the control prepare grants the status write and no other write"
    );
    assert!(
        fresh
            .iter()
            .any(|entry| entry == "column status text true 'active'::text"),
        "a fresh install has the status column: {fresh:?}"
    );

    client
        .batch_execute("ALTER TABLE registry.project_envs DROP COLUMN status")
        .await
        .expect("make the table as 0007 left it");
    client
        .batch_execute(&format!(
            "BEGIN; SET LOCAL ROLE wamn_system; {MIGRATION} COMMIT;"
        ))
        .await
        .expect("apply system/0008 as wamn_system");
    assert_eq!(
        definition(&client).await,
        fresh,
        "the migration adds the status column as a fresh install has it"
    );
    assert_eq!(
        grants(&client, "wamn_identity_issuer").await,
        issuer,
        "the migration grants the issuer the read that its prepare grants"
    );
    assert_eq!(
        grants(&client, "wamn_control").await,
        control,
        "the migration grants the control family the write that its prepare grants"
    );
}
