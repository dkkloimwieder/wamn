//! The session reader can read its approved columns and cannot write.
use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleIdentity, workload_login};

#[tokio::test]
async fn dedicated_session_reader_columns_execute_on_postgres() -> anyhow::Result<()> {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::database();
    let family = WorkloadRoleFamily::SessionRoleReader;
    let role = workload_login(
        family,
        WorkloadRoleIdentity::ProjectEnvironment {
            org: "acme",
            project: "widgets",
            environment: "dev",
            database: database.name(),
        },
    )?;
    database.execute(&[
        &wamn_control_provision::sql::ensure_app_acl_role_sql(),
        wamn_catalog::CATALOG_SCHEMA_SQL,
        include_str!("../../../../deploy/sql/run-state.sql"),
        include_str!("../../../../deploy/sql/run-queue.sql"),
        include_str!("../../../../deploy/sql/app-schema.sql"),
        &wamn_control_provision::test_database::login_sql(
            family,
            database.name(),
            &role,
            "fixture-password",
        ),
    ])?;
    let mut url = url::Url::parse(database.url())?;
    url.set_username(&role).unwrap();
    url.set_password(Some("fixture-password")).unwrap();
    let mut reader = wamn_postgres::connect(url.as_str()).await?;
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
    Ok(())
}
