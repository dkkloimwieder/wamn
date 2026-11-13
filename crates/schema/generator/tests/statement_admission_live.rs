//! Generated CRUD returns its declared outcomes.

use tokio_postgres::NoTls;

#[tokio::test(flavor = "current_thread")]
async fn generated_crud_outcomes_hold_under_column_grants() -> anyhow::Result<()> {
    use std::time::SystemTime;

    use wamn_compiler::{
        AuthoredSql, GenerationInput, StatementTransactionality, compile_manifest, derive_manifest,
        generate,
    };

    let _serialized = wamn_test_postgres::lock();
    let database = wamn_test_postgres::database();
    let (client, connection) = tokio_postgres::connect(database.url(), NoTls).await?;
    let connection = tokio::spawn(connection);
    wamn_fixture_package::apply_migrations(&client).await?;
    let catalog = wamn_schema_introspection::postgres::read_catalog(
        &sqlx::PgPool::connect(database.url()).await?,
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
        &StatementTransactionality::from_paths(std::collections::BTreeMap::new()),
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
