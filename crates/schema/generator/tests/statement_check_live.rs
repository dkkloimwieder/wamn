//! Live PostgreSQL 18 test of the statement check that generation runs as `wamn_app`.
//!
//! `WAMN_SCHEMA_INTROSPECTION_PG_URL` names a disposable PostgreSQL 18 database
//! through a superuser connection. The database holds the migrations of the
//! `whole_row` fixture in the schema `whole_row_probe`. The owned test runner
//! creates that database on a fresh server:
//!
//! ```bash
//! cargo build --locked --offline -p wamn-test-infrastructure --bin wamn-test-postgres
//! target/debug/wamn-test-postgres --database whole_row_probe --schema whole_row_probe \
//!   --migration-dir crates/schema/generator/tests/fixtures/whole_row/migrations \
//!   --url-env WAMN_SCHEMA_INTROSPECTION_PG_URL -- \
//!   cargo test --locked --offline -p wamn-schema-generator --test statement_check_live \
//!   -- --ignored --test-threads=1
//! ```

use std::path::Path;

use tokio_postgres::NoTls;
use wamn_schema_generator::{MaterializeMode, materialize_package_verified};

/// The roles and privileges that the statement check must leave unchanged.
const AUTHORITY_SNAPSHOT_SQL: &str = "SELECT \
    EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_app')::text, \
    (SELECT nspacl::text FROM pg_catalog.pg_namespace WHERE nspname = 'whole_row_probe'), \
    (SELECT string_agg(relname || '=' || coalesce(relacl::text, ''), ',' ORDER BY relname) \
       FROM pg_catalog.pg_class WHERE relnamespace = 'whole_row_probe'::regnamespace), \
    (SELECT string_agg(c.relname || '.' || a.attname || '=' || a.attacl::text, ',' \
                       ORDER BY c.relname, a.attname) \
       FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
      WHERE c.relnamespace = 'whole_row_probe'::regnamespace AND a.attacl IS NOT NULL)";

async fn authority_snapshot(url: &str) -> Vec<Option<String>> {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to the generation database");
    let connection = tokio::spawn(connection);
    let row = client
        .query_one(AUTHORITY_SNAPSHOT_SQL, &[])
        .await
        .expect("read the roles and privileges");
    drop(client);
    connection
        .await
        .expect("join the connection task")
        .expect("drive the connection");
    (0..row.len()).map(|index| row.get(index)).collect()
}

/// Every whole-row form that the lexer reads too few columns from fails at
/// generate time with its source path. The statement that names its columns
/// passes, and the generation database keeps its roles and privileges.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires WAMN_SCHEMA_INTROSPECTION_PG_URL and disposable PostgreSQL 18"]
async fn generation_refuses_whole_row_references_as_the_application_role() {
    let url = std::env::var("WAMN_SCHEMA_INTROSPECTION_PG_URL").expect(
        "WAMN_SCHEMA_INTROSPECTION_PG_URL must name the migrated whole_row fixture database",
    );
    let package = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/whole_row");
    let before = authority_snapshot(&url).await;

    let refusal = materialize_package_verified(MaterializeMode::Check, &url, &package)
        .await
        .expect_err("the whole-row references must fail the statement check");

    assert_eq!(
        refusal.to_string(),
        "PostgreSQL refused these statements as wamn_app under the grants that the package declaration derives:\n\
         command/insert_item.sql: 42501 permission denied for table item\n\
         query/item_field.sql: 42501 permission denied for table item\n\
         query/item_image.sql: 42501 permission denied for table item\n\
         query/line_item.sql: 42501 permission denied for table line"
    );
    assert_eq!(
        authority_snapshot(&url).await,
        before,
        "the statement check must roll back its role, revocations, and grants"
    );
}
