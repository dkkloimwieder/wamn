//! Live PostgreSQL 18 test of the statement check that generation runs as `wamn_app`.
//!
//! The test connects as the superuser of a test database on the test PostgreSQL
//! server. The database holds the Receiving migrations in the schema
//! `receiving` and the history table of each logged relation.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tokio_postgres::NoTls;
use wamn_schema_generator::{MaterializeMode, PackageManifest, materialize_package_verified};

const RECORD_HISTORY_SQL: &str = include_str!("../../../../deploy/sql/record-history.sql");
const RECORD_HISTORY_APP_GRANTS_SQL: &str =
    include_str!("../../../../deploy/sql/record-history-app-grants.sql");

/// The roles and privileges that the statement check must leave unchanged.
const AUTHORITY_SNAPSHOT_SQL: &str = "SELECT \
    EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_app')::text, \
    (SELECT nspacl::text FROM pg_catalog.pg_namespace WHERE nspname = 'receiving'), \
    (SELECT string_agg(relname || '=' || coalesce(relacl::text, ''), ',' ORDER BY relname) \
       FROM pg_catalog.pg_class WHERE relnamespace = 'receiving'::regnamespace), \
    (SELECT string_agg(c.relname || '.' || a.attname || '=' || a.attacl::text, ',' \
                       ORDER BY c.relname, a.attname) \
       FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
      WHERE c.relnamespace = 'receiving'::regnamespace AND a.attacl IS NOT NULL)";

/// The whole-row read that the test adds to the copy of Receiving. Receiving
/// grants `wamn_app` only `receipt_line.id`, and the lexer attributes only `id`.
const WHOLE_ROW_READ: &str = "SELECT\n    to_jsonb(receipt_line)::text AS image\n\
                              FROM receipt_line AS receipt_line\n\
                              ORDER BY receipt_line.id ASC;\n";

/// Create the Receiving generation database: the Receiving migrations in the
/// schema `receiving`, then the history table of each logged relation.
///
/// `wamn_app` exists first, because `record-history-app-grants.sql` grants the
/// history read functions to it.
fn receiving_generation_database() -> wamn_test_postgres::Database {
    let receiving = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/wamn_receiving");
    let database = wamn_test_postgres::database();
    let mut batches = vec![
        "CREATE SCHEMA receiving".to_owned(),
        format!(
            "ALTER DATABASE {} SET search_path TO receiving, public",
            database.name()
        ),
    ];
    let mut migrations = fs::read_dir(receiving.join("migrations"))
        .expect("read the Receiving migrations")
        .map(|entry| entry.expect("read a migration entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "sql"))
        .collect::<Vec<_>>();
    migrations.sort();
    for migration in migrations {
        batches.push(fs::read_to_string(migration).expect("read a Receiving migration"));
    }
    batches.extend([
        "CREATE ROLE wamn_app NOLOGIN".to_owned(),
        RECORD_HISTORY_SQL.to_owned(),
        RECORD_HISTORY_APP_GRANTS_SQL.to_owned(),
    ]);
    let manifest = PackageManifest::from_slice(
        &fs::read(receiving.join("wamn.json")).expect("read the Receiving manifest"),
    )
    .expect("parse the Receiving manifest");
    for model in manifest
        .models
        .values()
        .filter(|model| model.owner == manifest.package.id && model.log_retention().is_some())
    {
        batches.push(format!(
            "SELECT wamn_history.create_history_table('{}', '{}', false)",
            model.schema, model.table
        ));
    }
    database
        .execute(&batches.iter().map(String::as_str).collect::<Vec<_>>())
        .expect("prepare the Receiving generation database");
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

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create the copy directory");
    for entry in fs::read_dir(from).expect("read the source directory") {
        let source = entry.expect("read a source entry").path();
        let target = to.join(source.file_name().expect("an entry has a name"));
        if source.is_dir() {
            copy_tree(&source, &target);
        } else {
            fs::copy(&source, &target).expect("copy a source file");
        }
    }
}

/// A copy of `apps/wamn_receiving` in a temporary Cargo workspace.
///
/// The generated operator crate names its workspace by relative path. The copy
/// therefore keeps the `apps/wamn_receiving` layout below a manifest with the
/// repository workspace settings and the two Receiving UI crates as members.
struct ReceivingCopy {
    scratch: PathBuf,
    package: PathBuf,
}

impl ReceivingCopy {
    fn new() -> Self {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let scratch = std::env::temp_dir().join(format!(
            "wamn-statement-check-receiving-{}",
            std::process::id()
        ));
        if scratch.exists() {
            fs::remove_dir_all(&scratch).expect("remove a stale Receiving copy");
        }
        let package = scratch.join("apps/wamn_receiving");
        copy_tree(&repository.join("apps/wamn_receiving"), &package);

        let mut manifest: toml::Table = fs::read_to_string(repository.join("Cargo.toml"))
            .expect("read the repository workspace manifest")
            .parse()
            .expect("parse the repository workspace manifest");
        let workspace = manifest
            .get_mut("workspace")
            .and_then(toml::Value::as_table_mut)
            .expect("the repository manifest declares a workspace");
        workspace.insert(
            "members".to_owned(),
            toml::Value::Array(vec![
                "apps/wamn_receiving/ui".into(),
                "apps/wamn_receiving/generated/receiving-tui".into(),
            ]),
        );
        workspace.remove("default-members");
        fs::write(scratch.join("Cargo.toml"), manifest.to_string())
            .expect("write the copy workspace manifest");
        Self { scratch, package }
    }
}

impl Drop for ReceivingCopy {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.scratch);
    }
}

/// The unchanged copy of Receiving passes the check. A whole-row read that the
/// lexer reads too few columns from then fails at generate time with its source
/// path, and the generation database keeps its roles and privileges.
#[tokio::test(flavor = "current_thread")]
async fn generation_refuses_whole_row_references_as_the_application_role() {
    // The setup creates the cluster-wide wamn_app and wamn_db_owner roles.
    let _serialized = wamn_test_postgres::lock();
    let database = receiving_generation_database();
    let url = database.url().to_owned();
    let copy = ReceivingCopy::new();
    let before = authority_snapshot(&url).await;

    materialize_package_verified(MaterializeMode::Check, &url, &copy.package)
        .await
        .expect("the unchanged copy of Receiving passes the statement check");

    fs::write(
        copy.package.join("query/load_receipt_line_image.sql"),
        WHOLE_ROW_READ,
    )
    .expect("write the whole-row read");
    let manifest_path = copy.package.join("wamn.json");
    let mut manifest: Value =
        serde_json::from_slice(&fs::read(&manifest_path).expect("read the Receiving manifest"))
            .expect("parse the Receiving manifest");
    manifest["custom_operations"]["receiving.load_receipt_line_image"] = json!({
        "kind": "projection",
        "visibility": "public",
        "permission": "receiving.load_receipt_line_image",
        "connection": "postgres",
        "input": {"fields": [{"path": "request_id", "type": "text", "nullable": false}]},
        "result": {
            "class": "bounded_list",
            "fields": [{"path": "image", "type": "text", "nullable": false}]
        },
        "errors": ["invalid_input", "retry", "timeout", "permission_denied", "internal_error"],

        "constraint_errors": {},
        "relations": [{
            "schema": "receiving",
            "table": "receipt_line",
            "select_fields": ["id"],
            "insert_fields": [],
            "update_fields": [],
            "lock": false,
            "constraints": []
        }],
        "statements": {
            "load_receipt_line_image": {
                "path": "query/load_receipt_line_image.sql",
                "fetch": "bounded_list",
                "parameters": [],
                "row": [{"name": "image", "type": "text", "nullable": false}]
            }
        }
    });
    fs::write(
        &manifest_path,
        serde_json::to_vec(&manifest).expect("serialize the Receiving manifest"),
    )
    .expect("write the Receiving manifest");

    let refusal = materialize_package_verified(MaterializeMode::Check, &url, &copy.package)
        .await
        .expect_err("the whole-row read must fail the statement check");

    assert_eq!(
        refusal.to_string(),
        "PostgreSQL refused these statements as wamn_app under the grants that the package declaration derives:\n\
         query/load_receipt_line_image.sql: 42501 permission denied for table receipt_line"
    );
    assert_eq!(
        authority_snapshot(&url).await,
        before,
        "the statement check must roll back its role, revocations, and grants"
    );
}
