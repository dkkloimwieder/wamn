//! Execute integration SQL under the generated application grants on PostgreSQL 18.
use serde_json::{Value, json};
use std::path::Path;
use tokio_postgres::NoTls;
use wamn_schema_generator::{
    DataAccessOverlay, DataAccessRelationFields, derive_effective_data_access,
    render_effective_data_access_sql,
};

#[tokio::test]
async fn settings_and_delivery_outcomes_use_only_application_grants() -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::database();
    let (mut client, connection) = tokio_postgres::connect(database.url(), NoTls).await?;
    let driver = tokio::spawn(connection);
    let package =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/platform/wamn_integration");
    client.batch_execute("DO $$ BEGIN CREATE ROLE wamn_app NOLOGIN; EXCEPTION WHEN duplicate_object THEN NULL; END $$").await?;
    client
        .batch_execute("CREATE SCHEMA wamn_integration")
        .await?;
    let migration = package.join("migrations/0001_initial.sql");
    let bytes = std::fs::read(&migration)?;
    wamn_schema_introspection::migration_policy::validate_migration_bytes_for_schemas(
        &migration,
        &bytes,
        &["wamn_integration"],
        &[],
    )?;
    client.batch_execute(std::str::from_utf8(&bytes)?).await?;
    wamn_schema_generator::receipt::check_current(&package)?;
    let overlay = DataAccessOverlay::from_slice(&std::fs::read(
        wamn_schema_generator::output_root(&package).join("platform-policy/data-access.json"),
    )?)?;
    let relations = overlay
        .relations()
        .iter()
        .map(|relation| {
            DataAccessRelationFields::new(
                relation.schema(),
                relation.table(),
                relation.all_fields().to_vec(),
            )
        })
        .collect::<Vec<_>>();
    client
        .batch_execute(&render_effective_data_access_sql(
            &derive_effective_data_access(&relations, &[overlay])?,
        )?)
        .await?;
    let write = std::fs::read_to_string(package.join("command/setting/write.sql"))?;
    let read = std::fs::read_to_string(package.join("command/setting/read.sql"))?;
    let append = std::fs::read_to_string(package.join("command/log/append.sql"))?;
    let transaction = client.transaction().await?;
    transaction
        .batch_execute("SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_integration, public")
        .await?;
    assert_eq!(
        transaction
            .query_one("SELECT current_user::text", &[])
            .await?
            .get::<_, String>(0),
        "wamn_app"
    );
    assert!(
        transaction
            .query_opt(&read, &[&"last_synced_at"])
            .await?
            .is_none()
    );
    for due in ["2026-10-21T12:00:00Z", "2026-10-21T12:01:00Z"] {
        transaction
            .query_one(&write, &[&"last_synced_at", &json!(due)])
            .await?;
        let row = transaction.query_one(&read, &[&"last_synced_at"]).await?;
        assert_eq!(row.get::<_, Value>(0), json!(due));
    }
    for expected in [1, 0] {
        let rows = transaction
            .query(
                &append,
                &[&"delivery-1", &"item-1", &"pushed", &None::<Value>],
            )
            .await?;
        assert_eq!(rows.len(), expected);
    }
    assert_eq!(
        transaction
            .query_one("SELECT count(record_id) FROM log", &[])
            .await?
            .get::<_, i64>(0),
        1
    );
    assert!(!transaction.query_one("SELECT has_column_privilege(current_user, 'wamn_integration.log', 'outcome', 'UPDATE')", &[]).await?.get::<_, bool>(0));
    transaction.commit().await?;
    let row = client
        .query_one(
            "SELECT recorded_at IS NOT NULL, detail IS NULL FROM wamn_integration.log",
            &[],
        )
        .await?;
    assert!(row.get::<_, bool>(0));
    assert!(row.get::<_, bool>(1));
    drop(client);
    driver.await??;
    Ok(())
}
