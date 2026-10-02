//! Live PostgreSQL 18 test of the statement check that generation runs as `wamn_app`.
//!
//! The test connects as the superuser of a test database on the test PostgreSQL
//! server. The database holds a platform-owned widget table with one private column.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};
use tokio_postgres::NoTls;
use wamn_schema_generator::{
    MaterializeMode, classify_statements_with_existing_grants,
    classify_statements_with_existing_grants_in_transaction, materialize_package_verified,
};

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
///
/// Generation already counts a whole-row function argument such as
/// `to_jsonb(widget)` as a read of every column. A whole-row cast is outside
/// that reading, so PostgreSQL is the check that refuses it.
const WHOLE_ROW_READ: &str =
    "SELECT widget::text AS image FROM widget AS widget ORDER BY widget.id ASC;\n";

fn generation_database() -> wamn_test_postgres::Database {
    let database = wamn_test_postgres::database();
    database.execute(&[
        "CREATE SCHEMA inventory; CREATE TABLE inventory.widget (id uuid CONSTRAINT widget_id_pkey PRIMARY KEY, secret text NOT NULL)",
        &format!("ALTER DATABASE {} SET search_path TO inventory, public", database.name()),
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN CREATE ROLE wamn_app NOLOGIN; END IF; END $$",
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
                "type": "projection", "lists": {"key_field": "id"}, "visibility": "public", "permission": "widget.list",
                "connection": "postgres", "input": {"fields": []},
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
        "type": "projection",
        "lists": {"key_field": "image"},
        "visibility": "public",
        "permission": "widget.load_image",
        "connection": "postgres",
        "input": {"fields": []},
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

#[tokio::test(flavor = "current_thread")]
async fn existing_grants_planner_refuses_the_intermediate_state_and_preserves_authority() {
    let _serialized = wamn_test_postgres::lock();
    let database = generation_database();
    let url = database.url().to_owned();
    let (mut client, connection) = tokio_postgres::connect(&url, NoTls)
        .await
        .expect("connect to the upgrade planning database");
    let connection = tokio::spawn(connection);
    client
        .batch_execute(
            "GRANT USAGE ON SCHEMA inventory, public TO wamn_app; \
             GRANT SELECT (id, secret), UPDATE (secret) ON inventory.widget TO wamn_app; \
             INSERT INTO inventory.widget (id, secret) \
               VALUES ('00000000-0000-4000-8000-000000000001', 'retained'); \
             ALTER TABLE inventory.widget ADD COLUMN note text; \
             CREATE TABLE public.public_only (id uuid); \
             GRANT SELECT ON public.public_only TO wamn_app;",
        )
        .await
        .expect("retain predecessor grants while adding a candidate column");
    let before = authority_snapshot(&url).await;
    let session = client
        .query_one(
            "SELECT current_user::text, current_setting('search_path')",
            &[],
        )
        .await
        .unwrap();
    let session: (String, String) = (session.get(0), session.get(1));
    let mut corpus = BTreeMap::from([
        (
            "predecessor/widget.list/whole_row.sql".to_owned(),
            b"SELECT to_jsonb(widget) FROM widget".to_vec(),
        ),
        (
            "predecessor/widget.update/write.sql".to_owned(),
            b"UPDATE widget SET secret = $1 WHERE id = $2".to_vec(),
        ),
        (
            "predecessor/public_fallback.sql".to_owned(),
            b"SELECT id FROM public_only".to_vec(),
        ),
    ]);
    let refusal = classify_statements_with_existing_grants(&mut client, &corpus, "inventory")
        .await
        .expect_err("old column grants cannot read the candidate whole row");
    let refusal = refusal.to_string();
    assert!(
        refusal.contains("predecessor/widget.list/whole_row.sql: 42501"),
        "{refusal}"
    );
    assert!(
        refusal.contains("predecessor/public_fallback.sql: 42P01"),
        "the runtime search path must not append public: {refusal}"
    );
    assert_eq!(authority_snapshot(&url).await, before);
    let after = client
        .query_one(
            "SELECT current_user::text, current_setting('search_path')",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(
        (after.get::<_, String>(0), after.get::<_, String>(1)),
        session
    );

    client
        .batch_execute("GRANT SELECT (note) ON inventory.widget TO wamn_app")
        .await
        .expect("supply the candidate grant that repairs whole-row access");
    corpus.remove("predecessor/public_fallback.sql");
    let candidate_authority = authority_snapshot(&url).await;
    let mut transaction = client.transaction().await.unwrap();
    transaction
        .batch_execute("UPDATE inventory.widget SET secret = 'caller write'")
        .await
        .unwrap();
    let verdicts = classify_statements_with_existing_grants_in_transaction(
        &mut transaction,
        &corpus,
        "inventory",
    )
    .await
    .expect("the same predecessor statements pass under candidate grants");
    assert_eq!(
        transaction
            .query_one("SELECT secret FROM inventory.widget", &[])
            .await
            .unwrap()
            .get::<_, String>(0),
        "caller write",
        "planning keeps the caller's surrounding transaction and changes"
    );
    transaction.rollback().await.unwrap();
    assert!(!verdicts.needs_transaction("predecessor/widget.list/whole_row.sql"));
    assert!(verdicts.needs_transaction("predecessor/widget.update/write.sql"));
    assert_eq!(authority_snapshot(&url).await, candidate_authority);
    let row = client
        .query_one(
            "SELECT secret, current_user::text, current_setting('search_path') \
               FROM inventory.widget",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(
        row.get::<_, String>(0),
        "retained",
        "EXPLAIN never executes writes"
    );
    assert_eq!((row.get::<_, String>(1), row.get::<_, String>(2)), session);
    drop(client);
    connection.await.unwrap().unwrap();
}
