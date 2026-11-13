use super::operation_grants::ensure_admin_role;
use super::*;

#[tokio::test]
async fn auth_drops_ensure_admin_role_records_only_changed_grants() {
    use wamn_platform_identity::auth_changes::AuthDrops;

    let _lock = wamn_test_postgres::lock();
    let database = wamn_platform_identity::test_database::tenant_platform();
    let (mut client, connection) = tokio_postgres::connect(database.url(), NoTls)
        .await
        .unwrap();
    tokio::spawn(connection);
    client
        .batch_execute(&wamn_control_provision::platform_principals_sql("example.invalid").unwrap())
        .await
        .unwrap();
    client.batch_execute(
        "CREATE ROLE authdrops_role_owner NOLOGIN NOSUPERUSER BYPASSRLS; \
         GRANT USAGE ON SCHEMA platform, wamn_history, wamn_authority TO authdrops_role_owner; \
         GRANT ALL ON ALL TABLES IN SCHEMA platform TO authdrops_role_owner; \
         GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA wamn_history, wamn_authority TO authdrops_role_owner; \
         SET ROLE authdrops_role_owner;"
    ).await.unwrap();
    let mut expected = AuthDrops::default();
    expected.env_key("admin-drops");

    let tx = client.transaction().await.unwrap();
    bind_apply_package_principal(&tx).await.unwrap();
    let mut rolled_back = AuthDrops::default();
    assert!(
        ensure_admin_role(&tx, "admin-drops", &mut rolled_back)
            .await
            .unwrap()
    );
    assert_eq!(rolled_back, expected);
    tx.rollback().await.unwrap();

    let tx = client.transaction().await.unwrap();
    bind_apply_package_principal(&tx).await.unwrap();
    let mut drops = AuthDrops::default();
    assert!(
        ensure_admin_role(&tx, "admin-drops", &mut drops)
            .await
            .unwrap()
    );
    tx.commit().await.unwrap();
    assert_eq!(drops, expected);
    let tx = client.transaction().await.unwrap();
    bind_apply_package_principal(&tx).await.unwrap();
    let mut unchanged = AuthDrops::default();
    assert!(
        !ensure_admin_role(&tx, "admin-drops", &mut unchanged)
            .await
            .unwrap()
    );
    assert!(unchanged.is_empty());
    tx.commit().await.unwrap();
}

#[tokio::test]
#[ignore = "requires: env:WAMN_NATIVE_C_NATS_BIN"]
async fn auth_drops_apply_package_publishes_the_committed_admin_role_drop() {
    use std::sync::Arc;
    use std::time::Duration;
    use wamn_runtime::auth_cache::{
        AuthCache, PrincipalGrants,
        test_util::{nats, until, watched},
    };
    wamn_test_postgres::require_ignored_prerequisites("requires: env:WAMN_NATIVE_C_NATS_BIN");

    let _lock = wamn_test_postgres::lock();
    let database = wamn_platform_identity::test_database::tenant_platform();
    database.execute(&[
        &wamn_control_provision::sql::install_platform_extensions_sql(),
        wamn_control_provision::sql::ensure_db_owner_role_sql(),
        "DO $grant$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()); END $grant$;",
    ]).unwrap();
    database.execute(&[
        "CREATE ROLE authdrops_apply LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE BYPASSRLS PASSWORD 'authdrops'; GRANT wamn_db_owner TO authdrops_apply;",
        "GRANT USAGE, CREATE ON SCHEMA catalog, platform, wamn_history, wamn_authority TO authdrops_apply; GRANT ALL ON ALL TABLES IN SCHEMA catalog, platform, wamn_history, wamn_authority TO authdrops_apply; GRANT ALL ON ALL SEQUENCES IN SCHEMA catalog, platform, wamn_history, wamn_authority TO authdrops_apply; GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA catalog, platform, wamn_history, wamn_authority TO authdrops_apply;",
    ]).unwrap();
    database.execute(&[
        &wamn_control_provision::sql::ensure_package_owner_role_sql("wamn_inventory"),
        &wamn_control_provision::sql::package_owner_grants_sql("wamn_inventory"),
        &format!("GRANT {} TO authdrops_apply; GRANT USAGE, CREATE ON SCHEMA wamn_history TO authdrops_apply WITH GRANT OPTION; GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA wamn_history TO authdrops_apply WITH GRANT OPTION;", wamn_control_provision::package_owner_role("wamn_inventory")),
    ]).unwrap();
    let mut owner_url = url::Url::parse(database.url()).unwrap();
    owner_url.set_username("authdrops_apply").unwrap();
    owner_url.set_password(Some("authdrops")).unwrap();
    let (_server, nats) = nats().await;
    let cache = Arc::new(AuthCache::with_ttl(Duration::from_secs(3600)));
    let changes = watched(&nats, Some("apply-drops--fixture--dev"), Arc::clone(&cache)).await;
    let ticket = cache
        .grants("apply-drops--fixture--dev", "principal")
        .unwrap_err();
    cache
        .keep_grants(
            "apply-drops--fixture--dev",
            "principal",
            ticket,
            PrincipalGrants {
                user: Some(false),
                ..PrincipalGrants::default()
            },
        )
        .await;
    assert!(
        cache
            .grants("apply-drops--fixture--dev", "principal")
            .is_ok()
    );
    let shipped = std::env::temp_dir().join(format!("wamn-local-shipped-{}", std::process::id()));
    crate::test_support::copy_schema_fixture(&shipped);
    apply_package(ApplyPackageRequest {
        component_source: crate::apply_package::component_source_from_env(None).unwrap(),
        package: crate::package_artifact::fixture_package_root(&shipped).unwrap(),
        database_url: owner_url.to_string(),
        org: "apply-drops".into(),
        project: "fixture".into(),
        environment: "dev".into(),
        changes,
    })
    .await
    .unwrap();
    until("package apply publishes the admin role drop", || {
        cache
            .grants("apply-drops--fixture--dev", "principal")
            .is_err()
    })
    .await;
    assert_eq!(
        database
            .execute(&["SELECT count(*) FROM platform.role WHERE name = 'admin'"])
            .unwrap()
            .trim(),
        "1"
    );
}
