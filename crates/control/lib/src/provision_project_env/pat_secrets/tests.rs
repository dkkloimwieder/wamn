use super::*;
use std::sync::Arc;
use wamn_runtime::auth_cache::{
    AuthCache,
    test_util::{nats, until, watched},
};

const PRINCIPAL: &str = "a792950e-150c-4c6e-8000-000000000004";
const TOKEN: &str = "cached-identity";

async fn system_client(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await.unwrap();
    tokio::spawn(connection);
    client.batch_execute("SET ROLE wamn_system").await.unwrap();
    client
}

fn keep_identity(cache: &AuthCache) {
    cache.keep_session_active(cache.identity_ticket(), TOKEN, PRINCIPAL);
    assert!(cache.session_active(TOKEN));
}

async fn removed(cache: &AuthCache) {
    until("the committed identity drop clears the cache", || {
        !cache.session_active(TOKEN)
    })
    .await;
}

fn fixed_service_id(system: &wamn_test_postgres::Database) {
    system
        .execute(&[&format!(
            "ALTER TABLE identity.principals ALTER COLUMN id SET DEFAULT '{PRINCIPAL}'::uuid"
        )])
        .unwrap();
}

#[tokio::test]
#[ignore = "requires: WAMN_NATIVE_C_NATS_BIN"]
async fn auth_drops_create_service_drops_the_cached_identity_after_commit() {
    wamn_test_postgres::require_prerequisites(&["WAMN_NATIVE_C_NATS_BIN"]);
    let _lock = wamn_test_postgres::lock();
    let system = wamn_control_provision::test_database::system();
    fixed_service_id(&system);
    let mut client = system_client(system.url()).await;
    let (_server, nats) = nats().await;
    let cache = Arc::new(AuthCache::with_ttl(Duration::from_secs(3600)));
    let changes = watched(&nats, None, cache.clone()).await;
    keep_identity(&cache);
    let tx = provisioning_transaction(&mut client).await.unwrap();
    let mut drops = AuthDrops::default();
    let principal = create_service(&tx, "new-service", "New service", &mut drops)
        .await
        .unwrap();
    assert_eq!(principal.id().as_str(), PRINCIPAL);
    assert!(cache.session_active(TOKEN), "no drop before commit");
    tx.commit().await.unwrap();
    assert!(
        cache.session_active(TOKEN),
        "only publication clears the cache"
    );
    changes.publish(drops).await;
    removed(&cache).await;
    assert!(
        resolve_subject(&client, PrincipalType::Service, "new-service")
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
#[ignore = "requires: WAMN_NATIVE_C_NATS_BIN"]
async fn auth_drops_assign_project_role_drops_the_cached_identity_after_commit() {
    wamn_test_postgres::require_prerequisites(&["WAMN_NATIVE_C_NATS_BIN"]);
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
    let (_server, nats) = nats().await;
    let cache = Arc::new(AuthCache::with_ttl(Duration::from_secs(3600)));
    let changes = watched(&nats, None, cache.clone()).await;
    keep_identity(&cache);
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
    assert!(cache.session_active(TOKEN), "no drop before commit");
    tx.commit().await.unwrap();
    changes.publish(drops).await;
    removed(&cache).await;
    assert_eq!(
        wamn_platform_identity::project_roles(&client, principal.id(), "org", "project")
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
#[ignore = "requires: WAMN_NATIVE_C_NATS_BIN"]
async fn auth_drops_resolve_or_create_service_publishes_its_committed_drop() {
    wamn_test_postgres::require_prerequisites(&["WAMN_NATIVE_C_NATS_BIN"]);
    let _lock = wamn_test_postgres::lock();
    let system = wamn_control_provision::test_database::system();
    fixed_service_id(&system);
    let mut client = system_client(system.url()).await;
    let (_server, nats) = nats().await;
    let cache = Arc::new(AuthCache::with_ttl(Duration::from_secs(3600)));
    let changes = watched(&nats, None, cache.clone()).await;
    keep_identity(&cache);
    let principal = resolve_or_create_service(
        &mut client,
        "resolved-service",
        "Resolved service",
        &changes,
    )
    .await
    .unwrap();
    removed(&cache).await;
    assert_eq!(principal.id().as_str(), PRINCIPAL);
    let reader = system_client(system.url()).await;
    assert_eq!(
        resolve_subject(&reader, PrincipalType::Service, "resolved-service")
            .await
            .unwrap(),
        Some(principal.clone())
    );
    keep_identity(&cache);
    assert_eq!(
        resolve_or_create_service(
            &mut client,
            "resolved-service",
            "Resolved service",
            &changes
        )
        .await
        .unwrap(),
        principal
    );
    assert!(cache.session_active(TOKEN), "a read records no change");
}

#[tokio::test]
#[ignore = "requires: WAMN_NATIVE_C_NATS_BIN"]
async fn auth_drops_mint_pat_publishes_the_committed_project_role_drop() {
    wamn_test_postgres::require_prerequisites(&["WAMN_NATIVE_C_NATS_BIN"]);
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
    let (_server, nats) = nats().await;
    let cache = Arc::new(AuthCache::with_ttl(Duration::from_secs(3600)));
    let changes = watched(&nats, None, cache.clone()).await;
    keep_identity(&cache);
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
        &changes,
    )
    .await
    .unwrap();
    removed(&cache).await;
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
    std::fs::remove_dir(&root).unwrap();
    drop(client);
    system
        .execute(&["DROP DATABASE wamn_system WITH (FORCE)"])
        .unwrap();
}
