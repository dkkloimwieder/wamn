//! Live test of `system/0017_environment_apply.sql` (wamn-snz0.1): on a
//! control database installed before it, the migration gives
//! `registry.env_policies` and `registry.project_envs` the columns of
//! `env apply` as a fresh install has them. The test holds the process lock of
//! its server, because the installer creates cluster-wide roles.

use tokio_postgres::{Client, NoTls};
use wamn_control::provision_system::{ProvisionSystemRequest, provision_system};
use wamn_test_infrastructure::locked_database;

const MIGRATION: &str =
    include_str!("../../../../deploy/sql/migrations/system/0017_environment_apply.sql");

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to the disposable database");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

/// The columns and constraints of the two tables.
async fn definition(client: &Client) -> Vec<String> {
    client
        .query(
            "SELECT entry FROM ( \
               SELECT a.attrelid::regclass::text || ' column ' || a.attname || ' ' \
                      || format_type(a.atttypid, a.atttypmod) \
                      || ' ' || a.attnotnull::text || ' ' \
                      || coalesce(pg_get_expr(d.adbin, d.adrelid), '') AS entry \
                 FROM pg_attribute a \
                 LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
                WHERE a.attrelid IN ('registry.env_policies'::regclass, \
                                     'registry.project_envs'::regclass) \
                  AND a.attnum > 0 AND NOT a.attisdropped \
               UNION ALL \
               SELECT conrelid::regclass::text || ' constraint ' || conname || ' ' \
                      || pg_get_constraintdef(oid) \
                 FROM pg_constraint \
                WHERE conrelid IN ('registry.env_policies'::regclass, \
                                   'registry.project_envs'::regclass) \
             ) q ORDER BY entry COLLATE \"C\"",
            &[],
        )
        .await
        .expect("read the definition of the two tables")
        .iter()
        .map(|row| row.get(0))
        .collect()
}

#[tokio::test]
async fn the_migration_adds_the_apply_columns_as_a_fresh_install_has_them() {
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
    let fresh = definition(&client).await;
    for column in [
        "registry.env_policies column readiness_budget_seconds integer true 600",
        "registry.env_policies column drain_bound_seconds integer true 300",
        "registry.env_policies column approval_required boolean true false",
        "registry.project_envs column route_host text false ",
    ] {
        assert!(
            fresh.iter().any(|entry| entry == column),
            "{column}: {fresh:?}"
        );
    }

    client
        .batch_execute(
            "ALTER TABLE registry.env_policies DROP COLUMN readiness_budget_seconds, \
               DROP COLUMN drain_bound_seconds, DROP COLUMN approval_required; \
             ALTER TABLE registry.project_envs DROP COLUMN route_host",
        )
        .await
        .expect("make the tables as 0016 left them");
    client
        .batch_execute(&format!(
            "BEGIN; SET LOCAL ROLE wamn_system; {MIGRATION} COMMIT;"
        ))
        .await
        .expect("apply system/0017 as wamn_system");
    assert_eq!(
        definition(&client).await,
        fresh,
        "the migration adds the columns as a fresh install has them"
    );
}
