//! Live PostgreSQL 18 test of the statement check that generation runs as `wamn_app`.
//!
//! The test connects as the superuser of a test database on the test PostgreSQL
//! server. The database holds a platform-owned widget table with one private column.

use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};
use tokio_postgres::NoTls;
use wamn_schema_generator::{MaterializeMode, materialize_package_verified};

/// The roles and privileges that the statement check must leave unchanged.
const AUTHORITY_SNAPSHOT_SQL: &str = "SELECT \
    EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_app')::text, \
    (SELECT nspacl::text FROM pg_catalog.pg_namespace WHERE nspname = 'inventory'), \
    (SELECT string_agg(relname || '=' || coalesce(relacl::text, ''), ',' ORDER BY relname) \
       FROM pg_catalog.pg_class WHERE relnamespace = 'inventory'::regnamespace), \
    (SELECT string_agg(c.relname || '.' || a.attname || '=' || a.attacl::text, ',' \
                       ORDER BY c.relname, a.attname) \
       FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
      WHERE c.relnamespace = 'inventory'::regnamespace AND a.attacl IS NOT NULL)";

/// The declared SQL reads only id. The whole-row replacement also reads secret.
const WHOLE_ROW_READ: &str =
    "SELECT to_jsonb(widget)::text AS image FROM widget AS widget ORDER BY widget.id ASC;\n";

fn generation_database() -> wamn_test_postgres::Database {
    let database = wamn_test_postgres::database();
    database.execute(&[
        "CREATE SCHEMA inventory; CREATE TABLE inventory.widget (id uuid CONSTRAINT widget_id_pkey PRIMARY KEY, secret text NOT NULL)",
        &format!("ALTER DATABASE {} SET search_path TO inventory, public", database.name()),
        "CREATE ROLE wamn_app NOLOGIN",
    ]).expect("prepare the platform generation database");
    database
}

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

struct PlatformPackage {
    package: PathBuf,
}

impl PlatformPackage {
    fn new() -> Self {
        let package = std::env::temp_dir().join(format!(
            "wamn-statement-check-platform-{}",
            std::process::id()
        ));
        if package.exists() {
            fs::remove_dir_all(&package).expect("remove stale test package");
        }
        fs::create_dir_all(package.join("query")).unwrap();
        fs::write(
            package.join("Cargo.toml"),
            "[workspace]\nmembers = [\"generated/*\"]\n",
        )
        .unwrap();
        fs::write(
            package.join("query/widget.sql"),
            "SELECT id FROM widget ORDER BY id;\n",
        )
        .unwrap();
        let manifest = json!({
            "package": {"id": "statement_fixture", "version": "1.0.0"},
            "required_platform_policy_contract": {"id": "statement_access", "state": "unsatisfied"},
            "models": {"widget": {
                "schema": "inventory", "table": "widget", "owner": "statement_fixture",
                "server_owned_fields": ["id"], "audit_log": {"columns": [], "retention": "none"}, "operations": {}
            }},
            "custom_operations": {"widget.list": {
                "kind": "projection", "visibility": "public", "permission": "widget.list",
                "connection": "postgres", "input": {"fields": [{"path": "request_id", "type": "text", "nullable": false}]},
                "result": {"class": "bounded_list", "fields": [{"path": "id", "type": "uuid", "nullable": false}]},
                "errors": ["invalid_input", "retry", "timeout", "permission_denied", "internal_error"],
                "constraint_errors": {},
                "relations": [{"schema": "inventory", "table": "widget", "select_fields": ["id"], "insert_fields": [], "update_fields": [], "lock": false, "constraints": []}],
                "statements": {"list": {"path": "query/widget.sql", "fetch": "bounded_list", "parameters": [], "row": [{"name": "id", "type": "uuid", "nullable": false}]}}
            }},
            "connections": ["postgres"], "components": {"fixture": {"connections": ["postgres"]}}
        });
        fs::write(
            package.join("wamn.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        Self { package }
    }
}

impl Drop for PlatformPackage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.package);
    }
}

/// A valid declaration passes. A whole-row read then fails under the application
/// role, and generation leaves database authority unchanged.
#[tokio::test(flavor = "current_thread")]
async fn generation_refuses_whole_row_references_as_the_application_role() {
    // The setup creates the cluster-wide wamn_app and wamn_db_owner roles.
    let _serialized = wamn_test_postgres::lock();
    let database = generation_database();
    let url = database.url().to_owned();
    let copy = PlatformPackage::new();
    let before = authority_snapshot(&url).await;

    materialize_package_verified(MaterializeMode::Write, &url, &copy.package)
        .await
        .expect("the platform fixture passes the statement check");

    fs::write(
        copy.package.join("query/load_widget_image.sql"),
        WHOLE_ROW_READ,
    )
    .expect("write the whole-row read");
    let manifest_path = copy.package.join("wamn.json");
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("read the platform manifest"))
            .expect("parse the platform manifest");
    manifest["custom_operations"]["widget.load_image"] = json!({
        "kind": "projection",
        "visibility": "public",
        "permission": "widget.load_image",
        "connection": "postgres",
        "input": {"fields": [{"path": "request_id", "type": "text", "nullable": false}]},
        "result": {
            "class": "bounded_list",
            "fields": [{"path": "image", "type": "text", "nullable": false}]
        },
        "errors": ["invalid_input", "retry", "timeout", "permission_denied", "internal_error"],

        "constraint_errors": {},
        "relations": [{
            "schema": "inventory",
            "table": "widget",
            "select_fields": ["id"],
            "insert_fields": [],
            "update_fields": [],
            "lock": false,
            "constraints": []
        }],
        "statements": {
            "load_widget_image": {
                "path": "query/load_widget_image.sql",
                "fetch": "bounded_list",
                "parameters": [],
                "row": [{"name": "image", "type": "text", "nullable": false}]
            }
        }
    });
    fs::write(
        &manifest_path,
        serde_json::to_vec(&manifest).expect("serialize the platform manifest"),
    )
    .expect("write the platform manifest");

    let refusal = materialize_package_verified(MaterializeMode::Check, &url, &copy.package)
        .await
        .expect_err("the whole-row read must fail the statement check");

    assert_eq!(
        refusal.to_string(),
        "PostgreSQL refused these statements as wamn_app under the grants that the package declaration derives:\n\
         query/load_widget_image.sql: 42501 permission denied for table widget"
    );
    assert_eq!(
        authority_snapshot(&url).await,
        before,
        "the statement check must roll back its role, revocations, and grants"
    );
}
