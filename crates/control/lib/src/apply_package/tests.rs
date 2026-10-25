use super::*;

/// Ruling 69: the local configuration path runs the relation classification
/// check of apply-package. A manifest that drops a model whose relation the
/// installed migrations create gets the refusal of apply.
#[tokio::test]
async fn local_configuration_refuses_a_removed_model_with_the_apply_refusal() {
    const TENANT: &str = "local-configuration";
    let _lock = wamn_test_postgres::lock();
    let database = wamn_project_state::test_database::tenant_platform();
    let url = database.url().to_owned();
    let (mut client, connection) = tokio_postgres::connect(&url, NoTls).await.unwrap();
    tokio::spawn(connection);
    client
        .batch_execute(wamn_control_provision::sql::ensure_db_owner_role_sql())
        .await
        .unwrap();
    client
        .batch_execute(
            "DO $grant$ BEGIN \
               EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()); \
             END $grant$;",
        )
        .await
        .unwrap();
    let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/apply_package/base");
    apply_package(ApplyPackageRequest {
        changes: wamn_platform_identity::auth_changes::AuthChanges::default(),
        package: crate::package_artifact::fixture_package_root(&shipped).unwrap(),
        database_url: url.clone(),
        tenant: TENANT.to_owned(),
    })
    .await
    .expect("apply the package fixture");

    let mut directory = read_package_directory(&shipped).unwrap();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&directory.manifest_bytes).unwrap();
    manifest["models"]
        .as_object_mut()
        .unwrap()
        .remove("rack_line")
        .expect("the package fixture models rack_line");
    directory.manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    let root =
        std::env::temp_dir().join(format!("wamn-local-configuration-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("migrations")).unwrap();
    std::fs::write(root.join("wamn.json"), &directory.manifest_bytes).unwrap();
    for migration in &directory.migrations {
        std::fs::write(root.join(&migration.relative_path), &migration.bytes).unwrap();
    }
    let applied = apply_package(ApplyPackageRequest {
        changes: wamn_platform_identity::auth_changes::AuthChanges::default(),
        package: crate::package_artifact::fixture_package_root(&root).unwrap(),
        database_url: url.clone(),
        tenant: TENANT.to_owned(),
    })
    .await
    .expect_err("apply-package admitted a removed model");
    std::fs::remove_dir_all(&root).unwrap();
    assert!(
        applied
            .to_string()
            .starts_with(DEFINITION_OWNER_DECLARATION_MISSING_REFUSAL),
        "apply-package refused the removed model for another reason: {applied:#}"
    );

    let tx = client.transaction().await.unwrap();
    tx.query_one(CLAIM_TENANT_SQL, &[&TENANT]).await.unwrap();
    let local = reconcile_local_package_configuration(
        &tx,
        TENANT,
        &directory,
        &mut wamn_platform_identity::auth_changes::AuthDrops::default(),
    )
    .await
    .expect_err("the local configuration path admitted a removed model");
    assert_eq!(format!("{local:#}"), format!("{applied:#}"));
}

#[tokio::test]
#[ignore = "requires: WAMN_NATIVE_C_NATS_BIN"]
async fn auth_drops_ensure_admin_role_drops_cached_grants_only_after_commit() {
    use std::sync::Arc;
    use std::time::Duration;
    use wamn_platform_identity::auth_changes::AuthDrops;
    use wamn_runtime::auth_cache::{
        AuthCache, PrincipalGrants,
        test_util::{nats, until, watched},
    };

    wamn_test_postgres::require_prerequisites(&["WAMN_NATIVE_C_NATS_BIN"]);
    let _lock = wamn_test_postgres::lock();
    let database = wamn_project_state::test_database::tenant_platform();
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
    let (_server, nats) = nats().await;
    let cache = Arc::new(AuthCache::with_ttl(Duration::from_secs(3600)));
    let changes = watched(&nats, Some("admin-drops"), cache.clone()).await;
    let ticket = cache.grants("admin-drops", "principal").unwrap_err();
    cache
        .keep_grants(
            "admin-drops",
            "principal",
            ticket,
            PrincipalGrants::default(),
        )
        .await;
    assert!(cache.grants("admin-drops", "principal").is_ok());

    let tx = client.transaction().await.unwrap();
    bind_apply_package_principal(&tx).await.unwrap();
    let mut rolled_back = AuthDrops::default();
    assert!(
        ensure_admin_role(&tx, "admin-drops", &mut rolled_back)
            .await
            .unwrap()
    );
    assert!(!rolled_back.is_empty());
    assert!(cache.grants("admin-drops", "principal").is_ok());
    tx.rollback().await.unwrap();
    assert!(cache.grants("admin-drops", "principal").is_ok());

    let tx = client.transaction().await.unwrap();
    bind_apply_package_principal(&tx).await.unwrap();
    let mut drops = AuthDrops::default();
    assert!(
        ensure_admin_role(&tx, "admin-drops", &mut drops)
            .await
            .unwrap()
    );
    assert!(cache.grants("admin-drops", "principal").is_ok());
    tx.commit().await.unwrap();
    changes.publish(drops).await;
    until("the admin role drop clears cached grants", || {
        cache.grants("admin-drops", "principal").is_err()
    })
    .await;
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
#[ignore = "requires: WAMN_NATIVE_C_NATS_BIN"]
async fn auth_drops_apply_package_publishes_the_committed_admin_role_drop() {
    use std::sync::Arc;
    use std::time::Duration;
    use wamn_runtime::auth_cache::{
        AuthCache, PrincipalGrants,
        test_util::{nats, until, watched},
    };

    wamn_test_postgres::require_prerequisites(&["WAMN_NATIVE_C_NATS_BIN"]);
    let _lock = wamn_test_postgres::lock();
    let database = wamn_project_state::test_database::tenant_platform();
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
    let changes = watched(&nats, Some("apply-drops"), cache.clone()).await;
    let ticket = cache.grants("apply-drops", "principal").unwrap_err();
    cache
        .keep_grants(
            "apply-drops",
            "principal",
            ticket,
            PrincipalGrants::default(),
        )
        .await;
    assert!(cache.grants("apply-drops", "principal").is_ok());
    let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/apply_package/base");
    apply_package(ApplyPackageRequest {
        package: crate::package_artifact::fixture_package_root(&shipped).unwrap(),
        database_url: owner_url.to_string(),
        tenant: "apply-drops".to_owned(),
        changes,
    })
    .await
    .unwrap();
    until("package apply publishes the admin role drop", || {
        cache.grants("apply-drops", "principal").is_err()
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
