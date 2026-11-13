use tokio_postgres::{Client, NoTls};
use wamn_broker::Publisher;
use wamn_broker::auth_changes::AuthDrops;
use wamn_control_registry::Triple;
use wamn_environment_control::provision_project_env::pat_secrets::{
    MANAGEMENT_AUTHOR, assign_pat_role, mint_pat, resolve_or_create_service,
};
use wamn_environment_control::provision_project_env::provisioning_transaction;
use wamn_identity_client::PatClient;
use wamn_platform_identity::authenticate_pat;

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
        &Publisher::default(),
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
    client.batch_execute("RESET ROLE").await.unwrap();
    client
        .batch_execute(wamn_control_provision::apply::ensure_apply_role_sql())
        .await
        .unwrap();
    client
        .batch_execute(wamn_control_provision::apply::APPLY_GRANTS_SQL)
        .await
        .unwrap();
    client
        .batch_execute("SET SESSION AUTHORIZATION wamn_apply")
        .await
        .unwrap();
    let pat_client = PatClient::new(&bootstrap.args).unwrap();
    let (actual, issued) = mint_pat(
        &mut client,
        &pat_client,
        &triple,
        MANAGEMENT_AUTHOR,
        &Publisher::default(),
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
