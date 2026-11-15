//! Platform extensions support application exclusion constraints.

use wamn_control_provision::sql;

const OVERLAP_PROBE_TABLE: &str = "CREATE TABLE overlap_probe.appointment (\
     id uuid PRIMARY KEY, dock_id uuid NOT NULL, during tstzrange NOT NULL, \
     CONSTRAINT appointment_no_overlap \
     EXCLUDE USING gist (dock_id WITH =, during WITH &&));";

#[sqlx::test(migrations = false)]
async fn the_platform_extensions_make_an_exclusion_constraint_reachable(
    pool: sqlx::PgPool,
) -> anyhow::Result<()> {
    let mut connection = pool.acquire().await?;
    sqlx::raw_sql("CREATE SCHEMA overlap_probe")
        .execute(&mut *connection)
        .await?;
    let missing = sqlx::raw_sql(OVERLAP_PROBE_TABLE)
        .execute(&mut *connection)
        .await
        .unwrap_err();
    assert_eq!(
        missing
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("42704")
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql::install_platform_extensions_sql()))
        .execute(&mut *connection)
        .await?;
    sqlx::raw_sql(OVERLAP_PROBE_TABLE)
        .execute(&mut *connection)
        .await?;
    sqlx::query("INSERT INTO overlap_probe.appointment VALUES (gen_random_uuid(), $1::uuid, tstzrange('2026-10-01 09:00Z','2026-10-01 10:00Z'))")
        .bind("11111111-1111-1111-1111-111111111111").execute(&mut *connection).await?;
    let overlap = sqlx::query("INSERT INTO overlap_probe.appointment VALUES (gen_random_uuid(), $1::uuid, tstzrange('2026-10-01 09:30Z','2026-10-01 10:30Z'))")
        .bind("11111111-1111-1111-1111-111111111111").execute(&mut *connection).await.unwrap_err();
    assert_eq!(
        overlap
            .as_database_error()
            .and_then(|error| error.code())
            .as_deref(),
        Some("23P01")
    );
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM overlap_probe.appointment")
        .fetch_one(&mut *connection)
        .await?;
    assert_eq!(count, 1);
    Ok(())
}
