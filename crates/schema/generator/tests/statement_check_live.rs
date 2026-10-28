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
        // The package lies in its own scratch directory, beside the
        // `target/wamn` directory that holds its build output.
        let scratch = std::env::temp_dir().join(format!(
            "wamn-statement-check-platform-{}",
            std::process::id()
        ));
        if scratch.exists() {
            fs::remove_dir_all(&scratch).expect("remove stale test package");
        }
        let package = scratch.join("statement_fixture");
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
                "server_owned_fields": [], "audit_log": {"columns": [], "retention": "none"}, "operations": {}
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
            "connections": {}, "components": {"fixture": {"connections": ["postgres"]}}
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
        if let Some(scratch) = self.package.parent() {
            let _ = fs::remove_dir_all(scratch);
        }
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

    materialize_package_verified(&url, &copy.package)
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

    let refusal = materialize_package_verified(&url, &copy.package)
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
    let mut transaction = client.transaction().await.unwrap();
    let refusal = classify_statements_with_existing_grants_in_transaction(
        &mut transaction,
        &corpus,
        "inventory",
    )
    .await
    .expect_err("old column grants cannot read the candidate whole row");
    transaction.rollback().await.unwrap();
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

#[tokio::test(flavor = "current_thread")]
async fn generated_crud_outcomes_hold_under_column_grants() -> anyhow::Result<()> {
    use std::time::SystemTime;

    use wamn_schema_generator::{
        AuthoredSql, GenerationInput, StatementTransactionality, compile_manifest, derive_manifest,
        generate,
    };

    let _serialized = wamn_test_postgres::lock();
    let database = wamn_test_postgres::database();
    let (client, connection) = tokio_postgres::connect(database.url(), NoTls).await?;
    let connection = tokio::spawn(connection);
    wamn_fixture_package::apply_migrations(&client).await?;
    let catalog =
        wamn_schema_introspection::postgres::read_catalog(&client, &[wamn_fixture_package::SCHEMA])
            .await?;
    let manifest = compile_manifest(&wamn_fixture_package::package_root())?;
    let sources = wamn_fixture_package::authored_sql();
    let sources = sources
        .iter()
        .map(|(path, bytes)| AuthoredSql::new(path, bytes))
        .collect::<Vec<_>>();
    let manifest = derive_manifest(&catalog, &manifest, &sources)?;
    let package = generate(&GenerationInput::new(
        &catalog,
        &manifest,
        &sources,
        &StatementTransactionality::default(),
    ))?;
    let sql = |action| -> anyhow::Result<&str> {
        Ok(std::str::from_utf8(
            package
                .file(&format!("generated/sql/counter/{action}.sql"))
                .expect("the fixture declares the counter operation")
                .bytes(),
        )?)
    };
    let get = sql("get")?;
    let update = sql("update")?;
    let delete = sql("delete")?;

    // Generate before adding the private column to retain the predecessor's grants.
    client
        .batch_execute(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') \
               THEN CREATE ROLE wamn_app NOLOGIN; END IF; END $$; \
             ALTER TABLE inventory.counter ADD COLUMN private_note text DEFAULT 'untouched'; \
             GRANT USAGE ON SCHEMA inventory TO wamn_app; \
             GRANT SELECT (id, label, edit_version, created_at), UPDATE (label, edit_version), \
               DELETE ON inventory.counter TO wamn_app; \
             SET search_path = inventory;",
        )
        .await?;
    let id: i64 = client
        .query_one(
            "INSERT INTO counter (label) VALUES ('initial') RETURNING id",
            &[],
        )
        .await?
        .get(0);
    client.batch_execute("SET ROLE wamn_app").await?;
    let privileges = client
        .query_one(
            "SELECT has_table_privilege(current_user, 'inventory.counter', 'SELECT'), \
             has_column_privilege(current_user, 'inventory.counter', 'private_note', 'SELECT')",
            &[],
        )
        .await?;
    assert!(!privileges.get::<_, bool>(0));
    assert!(!privileges.get::<_, bool>(1));

    let initial = client.query_one(get, &[&id]).await?;
    assert_eq!(initial.get::<_, i64>("id"), id);
    assert_eq!(initial.get::<_, String>("label"), "initial");
    assert_eq!(initial.get::<_, i32>("edit_version"), 1);
    let created_at: SystemTime = initial.get("created_at");
    let changed = client
        .query_one(update, &[&id, &1_i32, &true, &"changed"])
        .await?;
    assert_eq!(changed.get::<_, String>("outcome"), "updated");
    assert_eq!(changed.get::<_, Option<i64>>("id"), Some(id));
    assert_eq!(changed.get::<_, Option<i32>>("edit_version"), Some(2));
    assert_eq!(
        changed.get::<_, Option<String>>("label").as_deref(),
        Some("changed")
    );
    assert_eq!(
        changed.get::<_, Option<SystemTime>>("created_at"),
        Some(created_at)
    );

    // The fixture calls its row_version edit_version, including the observed value.
    for (expected, present, value, outcome, version) in [
        (1_i32, true, "stale", "concurrency_conflict", Some(2_i32)),
        (2, true, "changed", "updated", Some(2)),
        (2, false, "ignored", "updated", Some(2)),
        (1, true, "changed", "concurrency_conflict", Some(2)),
    ] {
        let row = client
            .query_one(update, &[&id, &expected, &present, &value])
            .await?;
        assert_eq!(row.get::<_, String>("outcome"), outcome);
        assert_eq!(row.get::<_, Option<i32>>("observed_edit_version"), version);
        if outcome == "updated" {
            assert_eq!(row.get::<_, Option<i32>>("edit_version"), Some(2));
            assert_eq!(row.get::<_, Option<i64>>("id"), Some(id));
            assert_eq!(
                row.get::<_, Option<String>>("label").as_deref(),
                Some("changed")
            );
            assert_eq!(
                row.get::<_, Option<SystemTime>>("created_at"),
                Some(created_at)
            );
        } else {
            assert_eq!(row.get::<_, Option<i64>>("id"), None);
            assert_eq!(row.get::<_, Option<String>>("label"), None);
            assert_eq!(row.get::<_, Option<i32>>("edit_version"), None);
            assert_eq!(row.get::<_, Option<SystemTime>>("created_at"), None);
        }
        let stored = client.query_one(get, &[&id]).await?;
        assert_eq!(stored.get::<_, String>("label"), "changed");
        assert_eq!(stored.get::<_, i32>("edit_version"), 2);
        assert_eq!(stored.get::<_, SystemTime>("created_at"), created_at);
    }

    let stale = client.query_one(delete, &[&id, &1_i32]).await?;
    assert_eq!(stale.get::<_, String>("outcome"), "concurrency_conflict");
    assert_eq!(
        stale.get::<_, Option<i32>>("observed_edit_version"),
        Some(2)
    );
    assert!(client.query_opt(get, &[&id]).await?.is_some());
    client.batch_execute("RESET ROLE").await?;
    assert_eq!(
        client
            .query_one("SELECT private_note FROM counter WHERE id = $1", &[&id])
            .await?
            .get::<_, String>(0),
        "untouched"
    );
    client.batch_execute("SET ROLE wamn_app").await?;

    let deleted = client.query_one(delete, &[&id, &2_i32]).await?;
    assert_eq!(deleted.get::<_, String>("outcome"), "deleted");
    assert_eq!(
        deleted.get::<_, Option<i32>>("observed_edit_version"),
        Some(2)
    );
    assert!(client.query_opt(get, &[&id]).await?.is_none());
    let missing = client
        .query_one(update, &[&id, &2_i32, &true, &"missing"])
        .await?;
    assert_eq!(missing.get::<_, String>("outcome"), "not_found");
    assert_eq!(missing.get::<_, Option<i32>>("observed_edit_version"), None);
    assert_eq!(missing.get::<_, Option<i64>>("id"), None);
    assert_eq!(missing.get::<_, Option<String>>("label"), None);
    assert_eq!(missing.get::<_, Option<i32>>("edit_version"), None);
    assert_eq!(missing.get::<_, Option<SystemTime>>("created_at"), None);
    let missing = client.query_one(delete, &[&id, &2_i32]).await?;
    assert_eq!(missing.get::<_, String>("outcome"), "not_found");
    assert_eq!(missing.get::<_, Option<i32>>("observed_edit_version"), None);

    drop(client);
    connection.await??;
    Ok(())
}
