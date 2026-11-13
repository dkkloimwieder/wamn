//! Execute integration SQL under the generated application grants on PostgreSQL 18.
use serde_json::{Value, json};
use std::path::Path;
use wamn_compiler::{
    DataAccessOverlay, DataAccessRelationFields, derive_effective_data_access,
    render_effective_data_access_sql,
};

use sqlx::{Connection as _, Row as _};
#[tokio::test]
async fn settings_and_delivery_outcomes_use_only_application_grants() -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::database();
    let mut client = wamn_postgres::connect(database.url()).await?;
    let package =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/platform/wamn_integration");
    sqlx::raw_sql("DO $$ BEGIN CREATE ROLE wamn_app NOLOGIN NOINHERIT; EXCEPTION WHEN duplicate_object THEN NULL; END $$").execute(&mut client).await.map(drop)?;
    sqlx::raw_sql("CREATE SCHEMA wamn_integration")
        .execute(&mut client)
        .await
        .map(drop)?;
    let migration = package.join("migrations/0001_initial.sql");
    let bytes = std::fs::read(&migration)?;
    wamn_schema_introspection::migration_policy::validate_migration_bytes_for_schemas(
        &migration,
        &bytes,
        &["wamn_integration"],
        &[],
    )?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (std::str::from_utf8(&bytes)?).to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)?;
    wamn_compiler::build_record::require_current(&package)?;
    let overlay = DataAccessOverlay::from_slice(&std::fs::read(
        wamn_compiler::output_root(&package).join("platform-policy/data-access.json"),
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
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&render_effective_data_access_sql(&derive_effective_data_access(
            &relations,
            &[overlay],
        )?)?)
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)?;
    let write = std::fs::read_to_string(package.join("command/setting/write.sql"))?;
    let read = std::fs::read_to_string(package.join("command/setting/read.sql"))?;
    let append = std::fs::read_to_string(package.join("command/log/append.sql"))?;
    let mut transaction = client.begin().await?;
    sqlx::raw_sql("SET LOCAL ROLE wamn_app; SET LOCAL search_path TO wamn_integration, public")
        .execute(&mut *transaction)
        .await
        .map(drop)?;
    assert_eq!(
        sqlx::query("SELECT current_user::text")
            .fetch_one(&mut *transaction)
            .await?
            .get::<String, _>(0),
        "wamn_app"
    );
    assert!(
        sqlx::query(sqlx::AssertSqlSafe((&read).to_string()))
            .bind(&"last_synced_at")
            .fetch_optional(&mut *transaction)
            .await?
            .is_none()
    );
    for due in ["2026-10-21T12:00:00Z", "2026-10-21T12:01:00Z"] {
        sqlx::query(sqlx::AssertSqlSafe((&write).to_string()))
            .bind(&"last_synced_at")
            .bind(&json!(due))
            .fetch_one(&mut *transaction)
            .await?;
        let row = sqlx::query(sqlx::AssertSqlSafe((&read).to_string()))
            .bind(&"last_synced_at")
            .fetch_one(&mut *transaction)
            .await?;
        assert_eq!(row.get::<Value, _>(0), json!(due));
    }
    for expected in [1, 0] {
        let rows = sqlx::query(sqlx::AssertSqlSafe((&append).to_string()))
            .bind(&"delivery-1")
            .bind(&"item-1")
            .bind(&"pushed")
            .bind(&None::<Value>)
            .fetch_all(&mut *transaction)
            .await?;
        assert_eq!(rows.len(), expected);
    }
    assert_eq!(
        sqlx::query("SELECT count(record_id) FROM log")
            .fetch_one(&mut *transaction)
            .await?
            .get::<i64, _>(0),
        1
    );
    assert!(
        !sqlx::query(
            "SELECT has_column_privilege(current_user, 'wamn_integration.log', 'outcome', 'UPDATE')"
        )
        .fetch_one(&mut *transaction)
        .await?
        .get::<bool, _>(0)
    );
    transaction.commit().await?;
    let row =
        sqlx::query("SELECT recorded_at IS NOT NULL, detail IS NULL FROM wamn_integration.log")
            .fetch_one(&mut client)
            .await?;
    assert!(row.get::<bool, _>(0));
    assert!(row.get::<bool, _>(1));
    drop(client);
    Ok(())
}
