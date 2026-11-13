//! Live PostgreSQL 18 test of the statement make sure that generation runs as `wamn_app`.
//!
//! The test connects as the superuser of a test database on the test PostgreSQL
//! server. The database holds a platform-owned widget table with one private column.

use sqlx::{Connection as _, Row as _, Statement as _};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};
use wamn_compiler::{
    classify_statements_with_existing_grants_in_transaction, materialize_package_verified,
};

/// The roles and privileges that the statement test must leave unchanged.
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
/// that reading, so PostgreSQL is the test that refuses it.
const WHOLE_ROW_READ: &str =
    "SELECT widget::text AS image FROM widget AS widget ORDER BY widget.id ASC;\n";

fn generation_database() -> wamn_test_postgres::Database {
    let mut database = wamn_test_postgres::database();
    database.execute(&[
        "CREATE SCHEMA inventory; CREATE TABLE inventory.widget (id uuid CONSTRAINT widget_id_pkey PRIMARY KEY, secret text NOT NULL)",
        &format!("ALTER DATABASE {} SET search_path TO inventory, public", database.name()),
        "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN CREATE ROLE wamn_app NOLOGIN NOINHERIT; END IF; END $$",
    ]).expect("prepare the platform generation database");
    database
}

async fn authority_snapshot(url: &str) -> Vec<Option<String>> {
    let mut client = wamn_postgres::connect(url)
        .await
        .expect("connect to the generation database");

    let row = sqlx::query(AUTHORITY_SNAPSHOT_SQL)
        .fetch_one(&mut client)
        .await
        .expect("read the roles and privileges");
    drop(client);
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
            "wamn-statement-admission-platform-{}",
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
    let mut database = generation_database();
    let url = database.url().to_owned();
    let copy = PlatformPackage::new();
    let before = authority_snapshot(&url).await;

    materialize_package_verified(&url, &copy.package)
        .await
        .expect("the platform fixture passes the statement admission test");

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
        .expect_err("the whole-row read must fail the statement admission test");

    assert_eq!(
        refusal.to_string(),
        "PostgreSQL refused these statements as wamn_app under the grants that the package declaration derives:\n\
         query/load_widget_image.sql: 42501 permission denied for table widget"
    );
    assert_eq!(
        authority_snapshot(&url).await,
        before,
        "the statement admission test must roll back its role, revocations, and grants"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn existing_grants_planner_refuses_the_intermediate_state_and_preserves_authority() {
    let _serialized = wamn_test_postgres::lock();
    let mut database = generation_database();
    let url = database.url().to_owned();
    let mut client = wamn_postgres::connect(&url)
        .await
        .expect("connect to the upgrade planning database");

    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("GRANT USAGE ON SCHEMA inventory, public TO wamn_app; \
             GRANT SELECT (id, secret), UPDATE (secret) ON inventory.widget TO wamn_app; \
             INSERT INTO inventory.widget (id, secret) \
               VALUES ('00000000-0000-4000-8000-000000000001', 'retained'); \
             ALTER TABLE inventory.widget ADD COLUMN note text; \
             CREATE TABLE public.public_only (id uuid); \
             GRANT SELECT ON public.public_only TO wamn_app;")
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .expect("retain predecessor grants while adding a candidate column");
    let before = authority_snapshot(&url).await;
    let session = sqlx::query("SELECT current_user::text, current_setting('search_path')")
        .fetch_one(&mut client)
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
    let mut transaction = client.begin().await.unwrap();
    classify_statements_with_existing_grants_in_transaction(&mut transaction, &corpus, "inventory")
        .await
        .expect_err("old column grants cannot read the candidate whole row");
    transaction.rollback().await.unwrap();
    assert_eq!(authority_snapshot(&url).await, before);
    let after = sqlx::query("SELECT current_user::text, current_setting('search_path')")
        .fetch_one(&mut client)
        .await
        .unwrap();
    assert_eq!(
        (after.get::<String, _>(0), after.get::<String, _>(1)),
        session
    );

    sqlx::raw_sql("GRANT SELECT (note) ON inventory.widget TO wamn_app")
        .execute(&mut client)
        .await
        .map(drop)
        .expect("supply the candidate grant that repairs whole-row access");
    corpus.remove("predecessor/public_fallback.sql");
    let candidate_authority = authority_snapshot(&url).await;
    let mut transaction = client.begin().await.unwrap();
    sqlx::raw_sql("UPDATE inventory.widget SET secret = 'caller write'")
        .execute(&mut *transaction)
        .await
        .map(drop)
        .unwrap();
    let verdicts = classify_statements_with_existing_grants_in_transaction(
        &mut transaction,
        &corpus,
        "inventory",
    )
    .await
    .expect("the same predecessor statements pass under candidate grants");
    assert_eq!(
        sqlx::query("SELECT secret FROM inventory.widget")
            .fetch_one(&mut *transaction)
            .await
            .unwrap()
            .get::<String, _>(0),
        "caller write",
        "planning keeps the caller's surrounding transaction and changes"
    );
    transaction.rollback().await.unwrap();
    assert!(!verdicts.needs_transaction("predecessor/widget.list/whole_row.sql"));
    assert!(verdicts.needs_transaction("predecessor/widget.update/write.sql"));
    assert_eq!(authority_snapshot(&url).await, candidate_authority);
    let row = sqlx::query(
        "SELECT secret, current_user::text, current_setting('search_path') \
               FROM inventory.widget",
    )
    .fetch_one(&mut client)
    .await
    .unwrap();
    assert_eq!(
        row.get::<String, _>(0),
        "retained",
        "EXPLAIN never executes writes"
    );
    assert_eq!((row.get::<String, _>(1), row.get::<String, _>(2)), session);
    drop(client);
}

#[tokio::test(flavor = "current_thread")]
async fn generated_crud_outcomes_hold_under_column_grants() -> anyhow::Result<()> {
    type Timestamp = sqlx::types::chrono::DateTime<sqlx::types::chrono::Utc>;

    use wamn_compiler::{
        AuthoredSql, GenerationInput, StatementTransactionality, compile_manifest, derive_manifest,
        generate,
    };

    let _serialized = wamn_test_postgres::lock();
    let mut database = wamn_test_postgres::database();
    let mut client = wamn_postgres::connect(database.url()).await?;

    wamn_fixture_package::apply_migrations(&mut client).await?;
    let catalog = wamn_schema_introspection::postgres::read_catalog(
        &mut client,
        &[wamn_fixture_package::SCHEMA],
    )
    .await?;
    let manifest = compile_manifest(&wamn_fixture_package::package_root())?;
    let sources = wamn_fixture_package::authored_sql();
    let sources = sources
        .iter()
        .map(|(path, bytes)| AuthoredSql::new(path, bytes))
        .collect::<Vec<_>>();
    let manifest = derive_manifest(&catalog, &manifest, &sources, &[])?;
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
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') \
               THEN CREATE ROLE wamn_app NOLOGIN NOINHERIT; END IF; END $$; \
             ALTER TABLE inventory.counter ADD COLUMN private_note text DEFAULT 'untouched'; \
             GRANT USAGE ON SCHEMA inventory TO wamn_app; \
             GRANT SELECT (id, label, edit_version, created_at), UPDATE (label, edit_version), \
               DELETE ON inventory.counter TO wamn_app; \
             SET search_path = inventory;")
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)?;
    let id: i64 = sqlx::query("INSERT INTO counter (label) VALUES ('initial') RETURNING id")
        .fetch_one(&mut client)
        .await?
        .get(0);
    sqlx::raw_sql("SET ROLE wamn_app")
        .execute(&mut client)
        .await
        .map(drop)?;
    let privileges = sqlx::query(
        "SELECT has_table_privilege(current_user, 'inventory.counter', 'SELECT'), \
             has_column_privilege(current_user, 'inventory.counter', 'private_note', 'SELECT')",
    )
    .fetch_one(&mut client)
    .await?;
    assert!(!privileges.get::<bool, _>(0));
    assert!(!privileges.get::<bool, _>(1));

    let initial = sqlx::query(sqlx::AssertSqlSafe((get).to_string()))
        .bind(&id)
        .fetch_one(&mut client)
        .await?;
    assert_eq!(initial.get::<i64, _>("id"), id);
    assert_eq!(initial.get::<String, _>("label"), "initial");
    assert_eq!(initial.get::<i32, _>("edit_version"), 1);
    let created_at: Timestamp = initial.get("created_at");
    let changed = sqlx::query(sqlx::AssertSqlSafe((update).to_string()))
        .bind(&id)
        .bind(&1_i32)
        .bind(&true)
        .bind(&"changed")
        .fetch_one(&mut client)
        .await?;
    assert_eq!(changed.get::<String, _>("outcome"), "updated");
    assert_eq!(changed.get::<Option<i64>, _>("id"), Some(id));
    assert_eq!(changed.get::<Option<i32>, _>("edit_version"), Some(2));
    assert_eq!(
        changed.get::<Option<String>, _>("label").as_deref(),
        Some("changed")
    );
    assert_eq!(
        changed.get::<Option<Timestamp>, _>("created_at"),
        Some(created_at)
    );

    // The fixture calls its row_version edit_version, including the observed value.
    for (expected, present, value, outcome, version) in [
        (1_i32, true, "stale", "concurrency_conflict", Some(2_i32)),
        (2, true, "changed", "updated", Some(2)),
        (2, false, "ignored", "updated", Some(2)),
        (1, true, "changed", "concurrency_conflict", Some(2)),
    ] {
        let row = sqlx::query(sqlx::AssertSqlSafe((update).to_string()))
            .bind(&id)
            .bind(&expected)
            .bind(&present)
            .bind(&value)
            .fetch_one(&mut client)
            .await?;
        assert_eq!(row.get::<String, _>("outcome"), outcome);
        assert_eq!(row.get::<Option<i32>, _>("observed_edit_version"), version);
        if outcome == "updated" {
            assert_eq!(row.get::<Option<i32>, _>("edit_version"), Some(2));
            assert_eq!(row.get::<Option<i64>, _>("id"), Some(id));
            assert_eq!(
                row.get::<Option<String>, _>("label").as_deref(),
                Some("changed")
            );
            assert_eq!(
                row.get::<Option<Timestamp>, _>("created_at"),
                Some(created_at)
            );
        } else {
            assert_eq!(row.get::<Option<i64>, _>("id"), None);
            assert_eq!(row.get::<Option<String>, _>("label"), None);
            assert_eq!(row.get::<Option<i32>, _>("edit_version"), None);
            assert_eq!(row.get::<Option<Timestamp>, _>("created_at"), None);
        }
        let stored = sqlx::query(sqlx::AssertSqlSafe((get).to_string()))
            .bind(&id)
            .fetch_one(&mut client)
            .await?;
        assert_eq!(stored.get::<String, _>("label"), "changed");
        assert_eq!(stored.get::<i32, _>("edit_version"), 2);
        assert_eq!(stored.get::<Timestamp, _>("created_at"), created_at);
    }

    let stale = sqlx::query(sqlx::AssertSqlSafe((delete).to_string()))
        .bind(&id)
        .bind(&1_i32)
        .fetch_one(&mut client)
        .await?;
    assert_eq!(stale.get::<String, _>("outcome"), "concurrency_conflict");
    assert_eq!(
        stale.get::<Option<i32>, _>("observed_edit_version"),
        Some(2)
    );
    assert!(
        sqlx::query(sqlx::AssertSqlSafe((get).to_string()))
            .bind(&id)
            .fetch_optional(&mut client)
            .await?
            .is_some()
    );
    sqlx::raw_sql("RESET ROLE")
        .execute(&mut client)
        .await
        .map(drop)?;
    assert_eq!(
        sqlx::query("SELECT private_note FROM counter WHERE id = $1")
            .bind(&id)
            .fetch_one(&mut client)
            .await?
            .get::<String, _>(0),
        "untouched"
    );
    sqlx::raw_sql("SET ROLE wamn_app")
        .execute(&mut client)
        .await
        .map(drop)?;

    let deleted = sqlx::query(sqlx::AssertSqlSafe((delete).to_string()))
        .bind(&id)
        .bind(&2_i32)
        .fetch_one(&mut client)
        .await?;
    assert_eq!(deleted.get::<String, _>("outcome"), "deleted");
    assert_eq!(
        deleted.get::<Option<i32>, _>("observed_edit_version"),
        Some(2)
    );
    assert!(
        sqlx::query(sqlx::AssertSqlSafe((get).to_string()))
            .bind(&id)
            .fetch_optional(&mut client)
            .await?
            .is_none()
    );
    let missing = sqlx::query(sqlx::AssertSqlSafe((update).to_string()))
        .bind(&id)
        .bind(&2_i32)
        .bind(&true)
        .bind(&"missing")
        .fetch_one(&mut client)
        .await?;
    assert_eq!(missing.get::<String, _>("outcome"), "not_found");
    assert_eq!(missing.get::<Option<i32>, _>("observed_edit_version"), None);
    assert_eq!(missing.get::<Option<i64>, _>("id"), None);
    assert_eq!(missing.get::<Option<String>, _>("label"), None);
    assert_eq!(missing.get::<Option<i32>, _>("edit_version"), None);
    assert_eq!(missing.get::<Option<Timestamp>, _>("created_at"), None);
    let missing = sqlx::query(sqlx::AssertSqlSafe((delete).to_string()))
        .bind(&id)
        .bind(&2_i32)
        .fetch_one(&mut client)
        .await?;
    assert_eq!(missing.get::<String, _>("outcome"), "not_found");
    assert_eq!(missing.get::<Option<i32>, _>("observed_edit_version"), None);

    drop(client);
    Ok(())
}
