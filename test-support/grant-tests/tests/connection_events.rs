//! Project connection events are append-only and private to the platform login.

use wamn_identity_protocol::workload_role::WorkloadRoleFamily;
use wamn_postgres::provisioning_transaction::provisioning_transaction;
use wamn_test_infrastructure::database;

#[tokio::test]
#[ignore = "requires: docker, bin:helm"]
async fn project_connection_event_grants_and_immutability() -> anyhow::Result<()> {
    let project = database::project().await?;
    let mut admin = project.admin().await?;
    let mut tx = provisioning_transaction(&mut admin).await?;
    let connection: sqlx::types::Uuid = sqlx::query_scalar(
        "INSERT INTO platform.connection(name,integration_type,auth_type,target_id,active_generation,service_principal_id)
         VALUES ('orders','http','none',uuidv7(),1,uuidv7()) RETURNING id",
    )
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO platform.connection_generation(connection_id,generation) VALUES ($1,1)",
    )
    .bind(connection)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let login = WorkloadRoleFamily::Platform.login_name();
    let url = project.login_url(login)?;
    let mut platform = wamn_postgres::connect(url.as_str()).await?;
    sqlx::query(
        "INSERT INTO platform.connection_event (connection_id,generation,principal_id,event_type,outcome)
         VALUES ($1,1,uuidv7(),'activate','succeeded')",
    )
    .bind(connection)
    .execute(&mut platform)
    .await?;
    let outcome: String =
        sqlx::query_scalar("SELECT outcome FROM platform.connection_event WHERE connection_id=$1")
            .bind(connection)
            .fetch_one(&mut platform)
            .await?;
    assert_eq!(outcome, "succeeded");

    let relations: Vec<String> = sqlx::query_scalar(
        "SELECT 'platform.connection_event'
         UNION ALL
         SELECT confrelid::regclass::text FROM pg_constraint
         WHERE contype='f' AND conrelid='platform.connection_event'::regclass
           AND confrelid<>'platform.connection'::regclass",
    )
    .fetch_all(&mut admin)
    .await?;
    for relation in &relations {
        for family in [WorkloadRoleFamily::App, WorkloadRoleFamily::Query] {
            let can_read: bool = sqlx::query_scalar(
                "SELECT has_table_privilege($1::text::name, $2::text::regclass, 'SELECT')
                  OR has_any_column_privilege($1::text::name, $2::text::regclass, 'SELECT')",
            )
            .bind(family.login_name())
            .bind(relation)
            .fetch_one(&mut admin)
            .await?;
            assert!(!can_read, "{family:?} can read {relation}");
        }
        let (can_read, can_mutate): (bool, bool) = sqlx::query_as(
            "SELECT has_table_privilege($1::text::name, $2::text::regclass, 'SELECT'),
                    has_table_privilege($1::text::name, $2::text::regclass, 'UPDATE,DELETE,TRUNCATE')",
        )
        .bind(login)
        .bind(relation)
        .fetch_one(&mut admin)
        .await?;
        assert!(
            can_read && !can_mutate,
            "unexpected platform grants on {relation}"
        );
    }
    for statement in [
        "UPDATE platform.connection_event SET outcome='failed'",
        "DELETE FROM platform.connection_event",
        "TRUNCATE platform.connection_event",
    ] {
        assert!(sqlx::raw_sql(statement).execute(&mut admin).await.is_err());
    }
    Ok(())
}
