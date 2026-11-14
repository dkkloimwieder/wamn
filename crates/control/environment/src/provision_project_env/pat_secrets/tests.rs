use super::*;

#[test]
fn pat_prefix_accepts_only_sixteen_lowercase_hex_digits() {
    assert_eq!(
        parse_pat_prefix("0123456789abcdef").unwrap(),
        "0123456789abcdef"
    );
    for prefix in [
        "0123456789abcde",
        "0123456789abcdef0",
        "0123456789ABCDEF",
        "0123456789abcdeg",
        "",
    ] {
        assert_eq!(
            parse_pat_prefix(prefix).unwrap_err(),
            "PAT prefix must be 16 lowercase hex digits"
        );
    }
}

const PRINCIPAL: &str = "a792950e-150c-4c6e-8000-000000000004";

async fn system_client(url: &str) -> Client {
    let mut client = wamn_postgres::connect(url).await.unwrap();

    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    client
}

async fn fixed_service_id(system: &url::Url) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "ALTER TABLE identity.principals ALTER COLUMN id SET DEFAULT '{PRINCIPAL}'::uuid"
    )))
    .execute(&mut wamn_postgres::connect(system.as_str()).await.unwrap())
    .await
    .unwrap();
}

#[sqlx::test(migrations = false)]
async fn auth_drops_create_service_records_the_created_identity(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let mut system = wamn_test_postgres::url(&pool);
    wamn_control_provision::test_database::system(system.as_str()).await;
    fixed_service_id(&system).await;
    let mut client = system_client(system.as_str()).await;
    let mut tx = provisioning_transaction(&mut client).await.unwrap();
    let mut drops = AuthDrops::default();
    let principal = create_service(&mut tx, "new-service", "New service", &mut drops)
        .await
        .unwrap();
    assert_eq!(principal.id().to_string().as_str(), PRINCIPAL);
    tx.commit().await.unwrap();
    let mut expected = AuthDrops::default();
    expected.identity(PRINCIPAL);
    assert_eq!(drops, expected);
    assert!(
        resolve_subject(&mut client, PrincipalType::Service, "new-service")
            .await
            .unwrap()
            .is_some()
    );
}

#[sqlx::test(migrations = false)]
async fn auth_drops_assign_project_role_records_the_changed_identity(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let mut system = wamn_test_postgres::url(&pool);
    wamn_control_provision::test_database::system(system.as_str()).await;
    fixed_service_id(&system).await;
    let mut client = system_client(system.as_str()).await;
    let principal = resolve_or_create_service(
        &mut client,
        "role-service",
        "Role service",
        &Publisher::default(),
    )
    .await
    .unwrap();
    let mut tx = provisioning_transaction(&mut client).await.unwrap();
    sqlx::raw_sql("INSERT INTO registry.orgs (id, placement_type, pool_cluster) VALUES ('org', 'pooled', 'test'); INSERT INTO registry.projects (org, id) VALUES ('org', 'project')").execute(&mut *tx).await.map(drop).unwrap();
    tx.commit().await.unwrap();
    let mut tx = provisioning_transaction(&mut client).await.unwrap();
    let mut drops = AuthDrops::default();
    assign_project_role(
        &mut tx,
        principal.id(),
        "org",
        "project",
        "project-author",
        &mut drops,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let mut expected = AuthDrops::default();
    expected.identity(PRINCIPAL);
    assert_eq!(drops, expected);
    assert_eq!(
        wamn_platform_identity::project_roles(&mut client, principal.id(), "org", "project")
            .await
            .unwrap()
            .len(),
        1
    );
}

#[sqlx::test(migrations = false)]
async fn auth_drops_resolve_or_create_service_returns_only_the_created_identity(
    pool: sqlx::PgPool,
) {
    let _lock = wamn_test_postgres::lock();
    let mut system = wamn_test_postgres::url(&pool);
    wamn_control_provision::test_database::system(system.as_str()).await;
    fixed_service_id(&system).await;
    let mut client = system_client(system.as_str()).await;
    let (principal, drops) =
        resolve_or_create_service_drops(&mut client, "resolved-service", "Resolved service")
            .await
            .unwrap();
    let mut expected = AuthDrops::default();
    expected.identity(PRINCIPAL);
    assert_eq!(drops, expected);
    assert_eq!(principal.id().to_string().as_str(), PRINCIPAL);
    let mut reader = system_client(system.as_str()).await;
    assert_eq!(
        resolve_subject(&mut reader, PrincipalType::Service, "resolved-service")
            .await
            .unwrap(),
        Some(principal.clone())
    );
    let (resolved, unchanged) =
        resolve_or_create_service_drops(&mut client, "resolved-service", "Resolved service")
            .await
            .unwrap();
    assert_eq!(resolved, principal);
    assert!(unchanged.is_empty(), "a read records no change");
}
