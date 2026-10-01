//! Live test of `system/0007_org_administration.sql` (wamn-a40n.3): on a
//! control database installed before it, the migration creates the org tables
//! as a fresh install has them, and gives the control family and the identity
//! issuer the surfaces that provisioning grants. The test holds the process
//! lock of its server, because the installers create cluster-wide roles.

use tokio_postgres::{Client, NoTls};
use wamn_control::provision_system::{ProvisionSystemRequest, provision_system};
use wamn_control_provision::identity_issuer::grant_identity_issuer_surface_sql;
use wamn_control_provision::sql::{ensure_workload_acl_role_sql, grant_control_surface_sql};
use wamn_control_provision::workload_role::WorkloadRoleFamily;
use wamn_test_infrastructure::locked_database;

const MIGRATION: &str =
    include_str!("../../../../deploy/sql/migrations/system/0007_org_administration.sql");

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to the disposable database");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

async fn column(client: &Client, statement: &str) -> Vec<String> {
    client
        .query(statement, &[])
        .await
        .expect(statement)
        .iter()
        .map(|row| row.get(0))
        .collect()
}

/// The columns, constraints and triggers of the two org tables.
async fn org_tables(client: &Client) -> Vec<String> {
    column(
        client,
        "SELECT entry FROM ( \
           SELECT 'column ' || c.oid::regclass::text || '.' || a.attname || ' ' \
                  || format_type(a.atttypid, a.atttypmod) || ' ' || a.attnotnull::text \
                  || ' ' || coalesce(pg_get_expr(d.adbin, d.adrelid), '') AS entry \
             FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid \
             LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
            WHERE c.oid IN ('identity.org_memberships'::regclass, 'identity.org_roles'::regclass) \
              AND a.attnum > 0 AND NOT a.attisdropped \
           UNION ALL \
           SELECT 'constraint ' || conrelid::regclass::text || ' ' || conname || ' ' \
                  || pg_get_constraintdef(oid) \
             FROM pg_constraint \
            WHERE conrelid IN ('identity.org_memberships'::regclass, 'identity.org_roles'::regclass) \
           UNION ALL \
           SELECT 'trigger ' || tgrelid::regclass::text || ' ' || pg_get_triggerdef(oid) \
             FROM pg_trigger \
            WHERE tgrelid IN ('identity.org_memberships'::regclass, 'identity.org_roles'::regclass) \
              AND NOT tgisinternal \
         ) q ORDER BY entry COLLATE \"C\"",
    )
    .await
}

/// Every privilege that names `role` in this database: on schemas, relations
/// and columns.
async fn surface(client: &Client, role: &str) -> Vec<String> {
    column(
        client,
        &format!(
            "WITH grantee AS (SELECT oid FROM pg_roles WHERE rolname = '{role}') \
             SELECT entry FROM ( \
               SELECT 'schema ' || n.nspname || ' ' || x.privilege_type AS entry \
                 FROM pg_namespace n, aclexplode(n.nspacl) x, grantee g WHERE x.grantee = g.oid \
               UNION ALL \
               SELECT 'table ' || c.oid::regclass::text || ' ' || x.privilege_type \
                 FROM pg_class c, aclexplode(c.relacl) x, grantee g WHERE x.grantee = g.oid \
               UNION ALL \
               SELECT 'column ' || c.oid::regclass::text || '.' || a.attname || ' ' || x.privilege_type \
                 FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid, \
                      aclexplode(a.attacl) x, grantee g WHERE x.grantee = g.oid \
             ) q ORDER BY entry COLLATE \"C\""
        ),
    )
    .await
}

#[tokio::test]
async fn the_migration_creates_the_org_tables_and_grants_the_provisioned_surfaces() {
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
    let fresh = org_tables(&client).await;

    // The database as 0006 left it: no org tables, the control family's
    // reads of issue 2, and an issuer that neither reads org roles nor
    // inserts a principal.
    client
        .batch_execute(&format!(
            "{control} {issuer} \
             DROP TABLE identity.org_roles, identity.org_memberships; \
             REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA identity, provisioning, registry \
               FROM wamn_control; \
             REVOKE ALL PRIVILEGES ON SCHEMA identity, provisioning, registry FROM wamn_control; \
             GRANT USAGE ON SCHEMA identity TO wamn_control; \
             GRANT SELECT ON identity.principals, identity.project_roles, identity.password_logins \
               TO wamn_control; \
             REVOKE INSERT (type, subject, email, display_name) ON identity.principals \
               FROM wamn_identity_issuer;",
            control = ensure_workload_acl_role_sql(WorkloadRoleFamily::Control),
            issuer = grant_identity_issuer_surface_sql(),
        ))
        .await
        .expect("make the database as 0006 left it");

    client
        .batch_execute(&format!(
            "BEGIN; SET LOCAL ROLE wamn_system; {MIGRATION} COMMIT;"
        ))
        .await
        .expect("apply system/0007 as wamn_system");
    assert_eq!(
        org_tables(&client).await,
        fresh,
        "the migration creates the org tables as a fresh install has them"
    );
    let control = surface(&client, "wamn_control").await;
    let issuer = surface(&client, "wamn_identity_issuer").await;
    for expected in [
        "table identity.org_roles INSERT",
        "table identity.project_env_memberships DELETE",
        "table registry.projects SELECT",
    ] {
        assert!(
            control.iter().any(|entry| entry == expected),
            "the migration grants the control family {expected}: {control:#?}"
        );
    }
    for expected in [
        "column identity.org_roles.role SELECT",
        "column identity.principals.email INSERT",
    ] {
        assert!(
            issuer.iter().any(|entry| entry == expected),
            "the migration grants the identity issuer {expected}: {issuer:#?}"
        );
    }

    client
        .batch_execute(&format!(
            "{} {}",
            grant_control_surface_sql(),
            grant_identity_issuer_surface_sql()
        ))
        .await
        .expect("apply the provisioned surfaces");
    assert_eq!(
        surface(&client, "wamn_control").await,
        control,
        "the migration and provisioning grant the control family one surface"
    );
    assert_eq!(
        surface(&client, "wamn_identity_issuer").await,
        issuer,
        "the migration and provisioning grant the identity issuer one surface"
    );
}
