use super::*;

#[test]
fn bootstrap_checks_the_role_the_operator_secret_carries() {
    let script = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../deploy/mvp/bootstrap.sh"),
    )
    .unwrap();
    let roles: Vec<&str> = script
        .lines()
        .filter_map(|line| line.strip_prefix("operator_role="))
        .collect();
    assert_eq!(roles, [OPERATOR.role]);
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
    assert_eq!(principal.id().as_str(), PRINCIPAL);
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
    assert_eq!(principal.id().as_str(), PRINCIPAL);
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

#[tokio::test]
async fn auth_drops_mint_pat_records_the_committed_project_role_drop() {
    let _lock = wamn_test_postgres::lock();
    let system = wamn_control_provision::test_database::system();
    fixed_service_id(&system);
    system
        .execute(&[&format!(
            "CREATE DATABASE wamn_system WITH TEMPLATE \"{}\" OWNER wamn_system",
            system.name()
        )])
        .unwrap();
    system
        .execute(&[
            wamn_control_provision::sql::revoke_public_connect_floor_sql(),
            "REVOKE TEMPORARY ON DATABASE wamn_system FROM PUBLIC",
        ])
        .unwrap();
    let mut url = url::Url::parse(system.url()).unwrap();
    url.set_path("/wamn_system");
    let mut client = system_client(url.as_str()).await;
    let triple = Triple::new("org", "project", "prod");
    let principal = resolve_or_create_service(
        &mut client,
        &MANAGEMENT_AUTHOR.subject(&triple).unwrap(),
        "Author",
        &AuthChanges::default(),
    )
    .await
    .unwrap();
    let tx = provisioning_transaction(&mut client).await.unwrap();
    tx.batch_execute("INSERT INTO registry.orgs (id, placement_type, pool_cluster) VALUES ('org', 'pooled', 'test'); INSERT INTO registry.projects (org, id) VALUES ('org', 'project')").await.unwrap();
    tx.commit().await.unwrap();
    let tx = provisioning_transaction(&mut client).await.unwrap();
    let drops = assign_pat_role(&tx, &principal, &triple, MANAGEMENT_AUTHOR)
        .await
        .unwrap();
    let mut expected = AuthDrops::default();
    expected.identity(PRINCIPAL);
    assert_eq!(drops, expected);
    tx.rollback().await.unwrap();
    let root = std::env::temp_dir().join(format!("wamn-authdrops-pat-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let bootstrap = crate::dev::pat_issuer::start(url.as_str(), &root)
        .await
        .unwrap();
    let pat_client = PatClient::new(&bootstrap.args).unwrap();
    let (actual, issued) = mint_pat(
        &mut client,
        &pat_client,
        &triple,
        MANAGEMENT_AUTHOR,
        &AuthChanges::default(),
    )
    .await
    .unwrap();
    assert_eq!(actual, principal);
    assert!(
        authenticate_pat(&client, &issued.token)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        wamn_platform_identity::project_roles(&client, principal.id(), "org", "project")
            .await
            .unwrap()
            .len(),
        1
    );
    bootstrap.stop().await.unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    drop(client);
    system
        .execute(&["DROP DATABASE wamn_system WITH (FORCE)"])
        .unwrap();
}
