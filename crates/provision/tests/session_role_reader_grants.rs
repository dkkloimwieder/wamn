//! The session reader reads only its approved columns.

#[path = "support/grants.rs"]
mod grants;
use wamn_control_provision::WorkloadRoleFamily;

#[sqlx::test(migrations = false)]
async fn session_role_reader_refusals_and_catalog_rules(pool: sqlx::PgPool) {
    let (fixture, _lock) = grants::family(&pool, WorkloadRoleFamily::SessionRoleReader).await;
    let mut reader = wamn_postgres::connect(&fixture.url).await.unwrap();
    assert!(
        sqlx::query("SELECT id,status FROM platform.\"user\"")
            .fetch_all(&mut reader)
            .await
            .is_ok()
    );
    assert!(
        sqlx::query("SELECT user_id,role_name FROM platform.user_role")
            .fetch_all(&mut reader)
            .await
            .is_ok()
    );
    for statement in [
        "SELECT email FROM platform.\"user\"",
        "SELECT * FROM platform.role",
        "DELETE FROM platform.user_role",
        "SELECT * FROM catalog.components",
        "SELECT * FROM wamn_run.runs",
    ] {
        assert!(
            sqlx::raw_sql(sqlx::AssertSqlSafe(statement.to_owned()))
                .execute(&mut reader)
                .await
                .is_err()
        );
    }
}
