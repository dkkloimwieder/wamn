//! Disposable-PG18 proof for generated Receiving data authority.

mod support;

use std::path::{Path, PathBuf};

use tokio_postgres::{Client, NoTls};
use url::Url;
use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, sql, workload_generation_role,
};
use wamn_ctl::apply_package::{self, ApplyPackageArgs};
use wamn_ctl::reconcile_package_data_access::{self, ReconcilePackageDataAccessArgs};

const CATALOG_SCHEMA: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
const APP_SCHEMA: &str = include_str!("../../../deploy/sql/app-schema.sql");
const TENANT: &str = "package-data-access-live";
const PASSWORD: &str = "package-data-access-live-password";

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to disposable PostgreSQL");
    tokio::spawn(connection);
    client
}

fn package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/receiving")
}

fn generation_url(admin_url: &str, role: &str) -> String {
    let mut url = Url::parse(admin_url).expect("parse disposable PostgreSQL URL");
    url.set_username(role).expect("set generation role");
    url.set_password(Some(PASSWORD))
        .expect("set generation password");
    url.into()
}

fn reconcile_args(url: &str) -> ReconcilePackageDataAccessArgs {
    ReconcilePackageDataAccessArgs {
        package: package_root(),
        database_url: url.to_owned(),
        tenant: TENANT.to_owned(),
    }
}

async fn acl_identity(client: &Client) -> Vec<String> {
    client
        .query(
            "SELECT identity FROM ( \
               SELECT 'schema:' || namespace.nspname || ':' || namespace.xmin::text AS identity \
                 FROM pg_catalog.pg_namespace AS namespace \
                WHERE namespace.nspname = 'receiving' \
               UNION ALL \
               SELECT 'table:' || relation.relname || ':' || relation.xmin::text \
                 FROM pg_catalog.pg_class AS relation \
                 JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
                WHERE namespace.nspname = 'receiving' AND relation.relname IN ( \
                    'item', 'location', 'purchase_order', 'purchase_order_line', \
                    'record_receipt_command', 'receipt', 'receipt_line') \
               UNION ALL \
               SELECT 'column:' || relation.relname || ':' || attribute.attname || ':' || attribute.xmin::text \
                 FROM pg_catalog.pg_attribute AS attribute \
                 JOIN pg_catalog.pg_class AS relation ON relation.oid = attribute.attrelid \
                 JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
                WHERE namespace.nspname = 'receiving' AND relation.relname IN ( \
                    'item', 'location', 'purchase_order', 'purchase_order_line', \
                    'record_receipt_command', 'receipt', 'receipt_line') \
                  AND attribute.attnum > 0 AND NOT attribute.attisdropped \
             ) AS observed ORDER BY identity COLLATE \"C\"",
            &[],
        )
        .await
        .expect("read ACL-bearing catalog identities")
        .into_iter()
        .map(|row| row.get(0))
        .collect()
}

#[tokio::test]
#[ignore = "requires disposable PG18 named by WAMN_CTL_PG_URL"]
async fn generated_overlay_reconciles_a_real_app_generation_exactly_and_replays_noop() {
    let Some(url) = support::LockedUrl::optional() else {
        eprintln!("skipping package_data_access_live; WAMN_CTL_PG_URL is unset");
        return;
    };
    let admin = connect(&url).await;
    let database: String = admin
        .query_one("SELECT current_database()::text", &[])
        .await
        .expect("read disposable database")
        .get(0);
    let generation = workload_generation_role(
        WorkloadRoleFamily::App,
        WorkloadRoleScope::Tenant {
            tenant: TENANT,
            database: &database,
        },
        CredentialGeneration::A,
    )
    .expect("App generation accepts tenant scope");
    admin
        .batch_execute(&format!(
            "DROP SCHEMA IF EXISTS receiving CASCADE; \
             DROP SCHEMA IF EXISTS app_system CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DO $reset$ BEGIN \
               IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{generation}') THEN \
                 EXECUTE format('DROP OWNED BY %I', '{generation}'); \
                 EXECUTE format('DROP ROLE %I', '{generation}'); \
               END IF; \
               IF EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                 EXECUTE 'DROP OWNED BY wamn_app'; \
                 EXECUTE 'DROP ROLE wamn_app'; \
               END IF; \
               CREATE ROLE wamn_app NOLOGIN; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN \
                 CREATE ROLE wamn_scenario_author NOLOGIN; \
               END IF; \
             END $reset$;"
        ))
        .await
        .expect("reset package data-access fixture");
    admin
        .batch_execute(CATALOG_SCHEMA)
        .await
        .expect("install package catalog");
    admin
        .batch_execute(APP_SCHEMA)
        .await
        .expect("install application authorization floor");
    apply_package::run(ApplyPackageArgs {
        package: package_root(),
        database_url: url.to_string(),
        tenant: TENANT.to_owned(),
    })
    .await
    .expect("apply Receiving package before policy");
    admin
        .batch_execute(&sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::App,
            &database,
            &generation,
            PASSWORD,
            "2099-01-01T00:00:00Z",
        ))
        .await
        .expect("prepare production App generation");
    admin
        .batch_execute(
            "GRANT DELETE ON TABLE receiving.purchase_order TO wamn_app; \
             GRANT UPDATE (location_code) ON TABLE receiving.location TO wamn_app; \
             GRANT SELECT (id) ON TABLE receiving.item TO wamn_app; \
             GRANT SELECT (item_number) ON TABLE receiving.item TO PUBLIC;",
        )
        .await
        .expect("seed direct ACL residue");

    let first_effect =
        reconcile_package_data_access::reconcile_package_data_access(reconcile_args(&url))
            .await
            .expect("apply generated data-access evidence");
    assert!(
        !first_effect.is_noop(),
        "residual ACL did not require repair"
    );
    let guest = connect(&generation_url(&url, &generation)).await;
    guest
        .query("SELECT id FROM receiving.location", &[])
        .await
        .expect("generated SELECT field is reachable through a real App generation");
    guest
        .execute(
            "UPDATE receiving.purchase_order \
                SET supplier_id = supplier_id, row_version = row_version \
              WHERE false",
            &[],
        )
        .await
        .expect("generated writable and revision fields are reachable");
    guest
        .query("SELECT id FROM receiving.location FOR KEY SHARE", &[])
        .await
        .expect("generated lock carrier permits the declared row lock");
    let delete = guest
        .execute("DELETE FROM receiving.purchase_order WHERE false", &[])
        .await
        .expect_err("residual table DELETE survived reconciliation");
    assert_eq!(
        delete.as_db_error().map(|error| error.code().code()),
        Some("42501")
    );
    let undeclared_column = guest
        .query("SELECT location_code FROM receiving.location", &[])
        .await
        .expect_err("undeclared location column remained readable");
    assert_eq!(
        undeclared_column
            .as_db_error()
            .map(|error| error.code().code()),
        Some("42501")
    );
    let unconsumed_relation = guest
        .query("SELECT id FROM receiving.item", &[])
        .await
        .expect_err("residual authority survived on an unconsumed package relation");
    assert_eq!(
        unconsumed_relation
            .as_db_error()
            .map(|error| error.code().code()),
        Some("42501")
    );

    let first = acl_identity(&admin).await;
    let again = reconcile_package_data_access::reconcile_package_data_access(reconcile_args(&url))
        .await
        .expect("replay generated data-access evidence");
    assert!(again.is_noop(), "replay did not report convergence");
    assert_eq!(
        acl_identity(&admin).await,
        first,
        "replay rewrote ACL state"
    );
}
