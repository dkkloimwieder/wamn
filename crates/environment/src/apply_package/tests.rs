use super::operation_grants::ensure_admin_role;
use super::*;
use sqlx::Connection as _;

#[sqlx::test(migrations = false)]
async fn auth_drops_ensure_admin_role_records_only_changed_grants(pool: sqlx::PgPool) {
    use wamn_broker::auth_changes::AuthDrops;

    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    wamn_platform_identity::test_database::project_platform(database.as_str()).await;
    let mut client = wamn_postgres::connect(database.as_str()).await.unwrap();

    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::platform_principals_sql(
            &"example.invalid"
                .parse::<wamn_registry::platform_domain::PlatformDomain>()
                .unwrap(),
        ))
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .unwrap();
    sqlx::raw_sql("CREATE ROLE authdrops_role_owner NOLOGIN NOSUPERUSER BYPASSRLS; \
         GRANT USAGE ON SCHEMA platform, wamn_history, wamn_authority TO authdrops_role_owner; \
         GRANT ALL ON ALL TABLES IN SCHEMA platform TO authdrops_role_owner; \
         GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA wamn_history, wamn_authority TO authdrops_role_owner; \
         SET ROLE authdrops_role_owner;").execute(&mut client).await.map(drop).unwrap();
    let mut expected = AuthDrops::default();
    expected.project_env_key(&"admin-drops--dev--project".parse().unwrap());

    let mut tx = client.begin().await.unwrap();
    bind_apply_package_principal(&mut tx).await.unwrap();
    let mut rolled_back = AuthDrops::default();
    assert!(
        ensure_admin_role(
            &mut tx,
            &"admin-drops--dev--project".parse().unwrap(),
            &mut rolled_back
        )
        .await
        .unwrap()
    );
    assert_eq!(rolled_back, expected);
    tx.rollback().await.unwrap();

    let mut tx = client.begin().await.unwrap();
    bind_apply_package_principal(&mut tx).await.unwrap();
    let mut drops = AuthDrops::default();
    assert!(
        ensure_admin_role(
            &mut tx,
            &"admin-drops--dev--project".parse().unwrap(),
            &mut drops
        )
        .await
        .unwrap()
    );
    tx.commit().await.unwrap();
    assert_eq!(drops, expected);
    let mut tx = client.begin().await.unwrap();
    bind_apply_package_principal(&mut tx).await.unwrap();
    let mut unchanged = AuthDrops::default();
    assert!(
        !ensure_admin_role(
            &mut tx,
            &"admin-drops--dev--project".parse().unwrap(),
            &mut unchanged
        )
        .await
        .unwrap()
    );
    assert!(unchanged.is_empty());
    tx.commit().await.unwrap();
}
