//! Installed role constraints and the built-in administrator write.

use sqlx::Connection as _;
use wamn_provision::operation_grants::{
    ENSURE_ADMIN_ROLE_SQL, OPERATION_GRANT_TRANSACTION_PRELUDE_SQL,
    operation_base_grants_requirements_sql,
};
use wamn_provision::{PlatformComponent, bind_platform_principal_sql};

async fn admin_write(connection: &mut sqlx::PgConnection) -> Result<(), sqlx::Error> {
    let mut transaction = connection.begin().await?;
    for batch in [
        bind_platform_principal_sql(PlatformComponent::ApplyPackage),
        OPERATION_GRANT_TRANSACTION_PRELUDE_SQL.to_owned(),
        operation_base_grants_requirements_sql(),
        ENSURE_ADMIN_ROLE_SQL.to_owned(),
    ] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(batch))
            .execute(&mut *transaction)
            .await?;
    }
    transaction.commit().await
}

#[sqlx::test(migrations = false)]
async fn admin_role_has_no_rows_and_authored_rows_are_grants_live(
    pool: sqlx::PgPool,
) -> anyhow::Result<()> {
    let _serialized = wamn_test_postgres::lock();
    let mut connection = pool.acquire().await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_provision::sql::ensure_app_acl_role_sql(),
    ))
    .execute(&mut *connection)
    .await?;
    sqlx::raw_sql(include_str!("../../../deploy/sql/record-history.sql"))
        .execute(&mut *connection)
        .await?;
    let missing = admin_write(&mut connection).await.unwrap_err();
    let missing = missing.as_database_error().unwrap();
    assert_eq!(missing.code().as_deref(), Some("55000"));

    for batch in [
        wamn_catalog::CATALOG_SCHEMA_SQL,
        include_str!("../../../deploy/sql/app-schema.sql"),
    ] {
        sqlx::raw_sql(sqlx::AssertSqlSafe(batch))
            .execute(&mut *connection)
            .await?;
    }
    let mut transaction = connection.begin().await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(bind_platform_principal_sql(
        PlatformComponent::Provisioning,
    )))
    .execute(&mut *transaction)
    .await?;
    sqlx::raw_sql(
        "INSERT INTO platform.role (name) VALUES ('clerk');
        INSERT INTO platform.permission (role_name, permission) VALUES
        ('clerk','platform-fixture:widget/create'), ('clerk','platform-fixture:widget/get')",
    )
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    admin_write(&mut connection).await?;
    admin_write(&mut connection).await?;
    let roles: Vec<String> = sqlx::query_scalar("SELECT name FROM platform.role ORDER BY name")
        .fetch_all(&mut *connection)
        .await?;
    assert_eq!(roles, ["admin", "clerk"]);
    let permissions: i64 = sqlx::query_scalar("SELECT count(*) FROM platform.permission")
        .fetch_one(&mut *connection)
        .await?;
    assert_eq!(permissions, 2);

    for (role, permission, constraint) in [
        (
            "admin",
            "platform-fixture:widget/get",
            "permission_admin_check",
        ),
        (
            "clerk",
            "platform-fixture:widget/list@2.1.0",
            "permission_reference_check",
        ),
    ] {
        let mut transaction = connection.begin().await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(bind_platform_principal_sql(
            PlatformComponent::Provisioning,
        )))
        .execute(&mut *transaction)
        .await?;
        let refused =
            sqlx::query("INSERT INTO platform.permission (role_name, permission) VALUES ($1,$2)")
                .bind(role)
                .bind(permission)
                .execute(&mut *transaction)
                .await
                .unwrap_err();
        let refused = refused.as_database_error().unwrap();
        assert_eq!(refused.code().as_deref(), Some("23514"));
        assert_eq!(refused.constraint(), Some(constraint));
    }
    for name in ["Clerk", "-clerk", "clerk role", &"a".repeat(65)] {
        let mut transaction = connection.begin().await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(bind_platform_principal_sql(
            PlatformComponent::Provisioning,
        )))
        .execute(&mut *transaction)
        .await?;
        let refused = sqlx::query("INSERT INTO platform.role (name) VALUES ($1)")
            .bind(name)
            .execute(&mut *transaction)
            .await
            .unwrap_err();
        assert_eq!(
            refused.as_database_error().unwrap().code().as_deref(),
            Some("23514")
        );
    }
    let mut transaction = connection.begin().await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(bind_platform_principal_sql(
        PlatformComponent::Provisioning,
    )))
    .execute(&mut *transaction)
    .await?;
    sqlx::query("DELETE FROM platform.permission WHERE role_name='clerk' AND permission='platform-fixture:widget/create'").execute(&mut *transaction).await?;
    transaction.commit().await?;
    let remaining: Vec<String> = sqlx::query_scalar("SELECT permission FROM platform.permission")
        .fetch_all(&mut *connection)
        .await?;
    assert_eq!(remaining, ["platform-fixture:widget/get"]);
    Ok(())
}
