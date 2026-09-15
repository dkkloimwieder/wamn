use super::*;

/// Ruling 69: the local configuration path runs the relation classification
/// check of apply-package. A manifest that drops a model whose relation the
/// installed migrations create gets the refusal of apply.
#[tokio::test]
async fn local_configuration_refuses_a_removed_model_with_the_apply_refusal() {
    const TENANT: &str = "local-configuration";
    let _lock = wamn_test_postgres::lock();
    let database = wamn_project_state::test_database::tenant_app_system();
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
    let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/wamn_receiving");
    run(ApplyPackageArgs {
        package: shipped.clone(),
        database_url: url.clone(),
        tenant: TENANT.to_owned(),
    })
    .await
    .expect("apply the shipped package");

    let mut directory = read_package_directory(&shipped).unwrap();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&directory.manifest_bytes).unwrap();
    manifest["models"]
        .as_object_mut()
        .unwrap()
        .remove("receipt_line")
        .expect("the shipped package models receipt_line");
    directory.manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    let root =
        std::env::temp_dir().join(format!("wamn-local-configuration-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("migrations")).unwrap();
    std::fs::write(root.join("wamn.json"), &directory.manifest_bytes).unwrap();
    for migration in &directory.migrations {
        std::fs::write(root.join(&migration.relative_path), &migration.bytes).unwrap();
    }
    let applied = run(ApplyPackageArgs {
        package: root.clone(),
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
    let local = reconcile_local_package_configuration(&tx, TENANT, &directory)
        .await
        .expect_err("the local configuration path admitted a removed model");
    assert_eq!(format!("{local:#}"), format!("{applied:#}"));
}
