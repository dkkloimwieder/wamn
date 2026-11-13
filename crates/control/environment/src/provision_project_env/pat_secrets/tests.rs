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
    let (client, connection) = tokio_postgres::connect(url, NoTls).await.unwrap();
    tokio::spawn(connection);
    client.batch_execute("SET ROLE wamn_system").await.unwrap();
    client
}

fn fixed_service_id(system: &wamn_test_postgres::Database) {
    system
        .execute(&[&format!(
            "ALTER TABLE identity.principals ALTER COLUMN id SET DEFAULT '{PRINCIPAL}'::uuid"
        )])
        .unwrap();
}

#[tokio::test]
async fn auth_drops_create_service_records_the_created_identity() {
    let _lock = wamn_test_postgres::lock();
    let system = wamn_control_provision::test_database::system();
    fixed_service_id(&system);
    let mut client = system_client(system.url()).await;
    let tx = provisioning_transaction(&mut client).await.unwrap();
    let mut drops = AuthDrops::default();
    let principal = create_service(&tx, "new-service", "New service", &mut drops)
        .await
        .unwrap();
    assert_eq!(principal.id().to_string().as_str(), PRINCIPAL);
    tx.commit().await.unwrap();
    let mut expected = AuthDrops::default();
    expected.identity(PRINCIPAL);
    assert_eq!(drops, expected);
    assert!(
        resolve_subject(&client, PrincipalType::Service, "new-service")
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn auth_drops_assign_project_role_records_the_changed_identity() {
    let _lock = wamn_test_postgres::lock();
    let system = wamn_control_provision::test_database::system();
    fixed_service_id(&system);
    let mut client = system_client(system.url()).await;
    let principal = resolve_or_create_service(
        &mut client,
        "role-service",
        "Role service",
        &AuthChanges::default(),
    )
    .await
    .unwrap();
    let tx = provisioning_transaction(&mut client).await.unwrap();
    tx.batch_execute("INSERT INTO registry.orgs (id, placement_type, pool_cluster) VALUES ('org', 'pooled', 'test'); INSERT INTO registry.projects (org, id) VALUES ('org', 'project')").await.unwrap();
    tx.commit().await.unwrap();
    let tx = provisioning_transaction(&mut client).await.unwrap();
    let mut drops = AuthDrops::default();
    assign_project_role(
        &tx,
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
        wamn_platform_identity::project_roles(&client, principal.id(), "org", "project")
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn auth_drops_resolve_or_create_service_returns_only_the_created_identity() {
    let _lock = wamn_test_postgres::lock();
    let system = wamn_control_provision::test_database::system();
    fixed_service_id(&system);
    let mut client = system_client(system.url()).await;
    let (principal, drops) =
        resolve_or_create_service_drops(&mut client, "resolved-service", "Resolved service")
            .await
            .unwrap();
    let mut expected = AuthDrops::default();
    expected.identity(PRINCIPAL);
    assert_eq!(drops, expected);
    assert_eq!(principal.id().to_string().as_str(), PRINCIPAL);
    let reader = system_client(system.url()).await;
    assert_eq!(
        resolve_subject(&reader, PrincipalType::Service, "resolved-service")
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
