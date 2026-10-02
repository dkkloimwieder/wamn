//! Live test of `system/0012_saga_steps.sql` (wamn-zua8.3): on a control
//! database installed before it, the migration gives `provisioning.sagas` and
//! `provisioning.saga_steps` the definition a fresh install has, and the
//! control family the grants on them that its prepare grants. The test holds
//! the process lock of its server, because the installers create cluster-wide
//! roles.

use tokio_postgres::{Client, NoTls};
use wamn_control::provision_system::{ProvisionSystemRequest, provision_system};
use wamn_control_provision::sql::grant_control_surface_sql;
use wamn_test_infrastructure::locked_database;

const MIGRATION: &str =
    include_str!("../../../../deploy/sql/migrations/system/0012_saga_steps.sql");

/// The two saga tables as system migration 0011 left them.
const BEFORE_0012: &str = "\
    DROP TABLE provisioning.saga_steps; \
    ALTER TABLE provisioning.sagas \
        DROP CONSTRAINT sagas_create_environment_input, \
        DROP COLUMN org, \
        DROP COLUMN input, \
        DROP CONSTRAINT sagas_type_check, \
        ADD CONSTRAINT sagas_type_check \
            CHECK (type IN ('provision-org', 'provision-project-env')), \
        DROP CONSTRAINT sagas_status_check, \
        ADD CONSTRAINT sagas_status_check \
            CHECK (status IN ('pending', 'running', 'completed', 'failed', \
                              'compensating', 'compensated')); \
    REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA provisioning FROM wamn_control; \
    REVOKE ALL PRIVILEGES ON SCHEMA provisioning FROM wamn_control;";

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to the disposable database");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

/// The columns and constraints of the two saga tables.
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
                WHERE a.attrelid IN ('provisioning.sagas'::regclass, \
                                     'provisioning.saga_steps'::regclass) \
                  AND a.attnum > 0 AND NOT a.attisdropped \
               UNION ALL \
               SELECT conrelid::regclass::text || ' constraint ' || conname || ' ' \
                      || pg_get_constraintdef(oid) \
                 FROM pg_constraint \
                WHERE conrelid IN ('provisioning.sagas'::regclass, \
                                   'provisioning.saga_steps'::regclass) \
             ) q ORDER BY entry COLLATE \"C\"",
            &[],
        )
        .await
        .expect("read the definition of the saga tables")
        .iter()
        .map(|row| row.get(0))
        .collect()
}

/// The schema and table grants of the control family in `provisioning`.
async fn control_grants(client: &Client) -> Vec<String> {
    client
        .query(
            "SELECT entry FROM ( \
               SELECT 'schema ' || x.privilege_type AS entry \
                 FROM pg_namespace n, aclexplode(n.nspacl) x, pg_roles r \
                WHERE n.nspname = 'provisioning' \
                  AND x.grantee = r.oid AND r.rolname = 'wamn_control' \
               UNION ALL \
               SELECT c.oid::regclass::text || ' ' || x.privilege_type \
                 FROM pg_class c, aclexplode(c.relacl) x, pg_roles r \
                WHERE c.relnamespace = 'provisioning'::regnamespace \
                  AND x.grantee = r.oid AND r.rolname = 'wamn_control' \
             ) q ORDER BY entry COLLATE \"C\"",
            &[],
        )
        .await
        .expect("read the control grants in provisioning")
        .iter()
        .map(|row| row.get(0))
        .collect()
}

#[tokio::test]
async fn the_migration_adds_the_saga_steps_as_a_fresh_install_has_them() {
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
        .batch_execute(&grant_control_surface_sql())
        .await
        .expect("prepare the control surface");
    let fresh = definition(&client).await;
    let control = control_grants(&client).await;
    assert_eq!(
        control,
        [
            "provisioning.saga_steps INSERT",
            "provisioning.saga_steps SELECT",
            "provisioning.sagas INSERT",
            "provisioning.sagas SELECT",
            "schema USAGE",
        ],
        "the control prepare grants the saga writes and reads and no update"
    );

    client
        .batch_execute(BEFORE_0012)
        .await
        .expect("make the tables as 0011 left them");
    client
        .batch_execute(&format!(
            "BEGIN; SET LOCAL ROLE wamn_system; {MIGRATION} COMMIT;"
        ))
        .await
        .expect("apply system/0012 as wamn_system");
    assert_eq!(
        definition(&client).await,
        fresh,
        "the migration gives the saga tables the definition of a fresh install"
    );
    assert_eq!(
        control_grants(&client).await,
        control,
        "the migration grants the control family what its prepare grants"
    );
}
