use super::{installed_environment_fingerprint, load_applied_package};
use crate::apply_package::{ApplyPackageRequest, apply_package, apply_package_schema};
use std::path::Path;
use tokio_postgres::{Client, NoTls};

const ENV_KEY: &str = "schema-records--fixture--dev";

fn request(root: &Path, database_url: &str) -> ApplyPackageRequest {
    ApplyPackageRequest {
        component_source: None,
        package: crate::package_artifact::fixture_package_root(root).unwrap(),
        changes: wamn_platform_identity::auth_changes::AuthChanges::default(),
        database_url: database_url.into(),
        org: "schema-records".into(),
        project: "fixture".into(),
        environment: "dev".into(),
    }
}

async fn state(client: &mut Client) -> wamn_schema_control::AppliedPackage {
    let tx = client.transaction().await.unwrap();
    let state = load_applied_package(&tx, ENV_KEY, "wamn_inventory")
        .await
        .unwrap()
        .unwrap();
    tx.rollback().await.unwrap();
    state
}

#[tokio::test]
async fn explicit_sql_advances_across_versions_without_selecting_code() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_project_state::test_database::tenant_platform();
    database.execute(&[
        include_str!("../../../../../../deploy/sql/app-schema-baseline.sql"),
        wamn_control_provision::sql::ensure_db_owner_role_sql(),
        "DO $grant$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()); END $grant$;",
    ]).unwrap();
    let (mut client, connection) = tokio_postgres::connect(database.url(), NoTls)
        .await
        .unwrap();
    tokio::spawn(connection);
    crate::schema_baseline::record_project(&client)
        .await
        .unwrap();
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
    let old = scratch.path().join("old");
    let next = scratch.path().join("next");
    crate::test_support::copy_schema_fixture(&old);
    crate::test_support::copy_schema_fixture(&next);
    apply_package(request(&old, database.url())).await.unwrap();
    let baseline = state(&mut client).await;
    assert_eq!(
        baseline.baseline_fingerprint,
        baseline.physical_schema_fingerprint
    );
    let environment = installed_environment_fingerprint(&client, ENV_KEY)
        .await
        .unwrap();
    client
        .batch_execute("INSERT INTO inventory.dock (dock_code) VALUES ('retained')")
        .await
        .unwrap();

    // The cache remains byte-identical during schema work. It selects no code.
    let cache_tx = client.transaction().await.unwrap();
    crate::apply_package::bind_apply_package_principal(&cache_tx)
        .await
        .unwrap();
    cache_tx
        .execute(
            "INSERT INTO catalog.releases (env_key, manifest_digest, canonical_bytes) \
         SELECT $1, 'sha256:' || encode(sha256('{}'::bytea), 'hex'), '{}'::bytea",
            &[&ENV_KEY],
        )
        .await
        .unwrap();
    cache_tx.commit().await.unwrap();
    let cache_before: String = client
        .query_one("SELECT xmin::text FROM catalog.releases", &[])
        .await
        .unwrap()
        .get(0);
    let manifest_path = next.join("wamn.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["package"]["version"] = "2.0.0".into();
    // No predecessor code version is needed to continue the applied SQL prefix.
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    assert!(
        !apply_package(request(&next, database.url()))
            .await
            .unwrap()
            .changed
    );
    assert_eq!(state(&mut client).await, baseline);

    assert!(
        !apply_package_schema(request(&next, database.url()))
            .await
            .unwrap()
            .changed
    );
    assert_eq!(state(&mut client).await, baseline);

    manifest["package"]["version"] = "3.0.0".into();
    manifest["models"]["dock"]["audit_log"]["retention"] = "unlimited".into();
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    std::fs::write(
        next.join("migrations/0002_dock_note.sql"),
        "ALTER TABLE inventory.dock ADD COLUMN note text;",
    )
    .unwrap();
    let result = apply_package_schema(request(&next, database.url()))
        .await
        .unwrap();
    assert_eq!(result.migrations_applied, 1);
    let installed = state(&mut client).await;
    assert_eq!(
        installed.baseline_fingerprint,
        baseline.baseline_fingerprint
    );
    assert_ne!(
        installed.physical_schema_fingerprint,
        baseline.physical_schema_fingerprint
    );
    assert_eq!(installed.coordinate.package_version(), "3.0.0");
    assert_eq!(
        installed.artifact_digest,
        request(&next, database.url()).package.artifact_digest
    );
    assert_ne!(
        installed_environment_fingerprint(&client, ENV_KEY)
            .await
            .unwrap(),
        environment
    );
    assert!(
        client
            .query_one(
                "SELECT to_regclass('inventory.dock_history') IS NOT NULL",
                &[]
            )
            .await
            .unwrap()
            .get::<_, bool>(0)
    );
    assert_eq!(
        client
            .query_one("SELECT dock_code FROM inventory.dock", &[])
            .await
            .unwrap()
            .get::<_, String>(0),
        "retained"
    );
    assert_eq!(
        client
            .query_one("SELECT xmin::text FROM catalog.releases", &[])
            .await
            .unwrap()
            .get::<_, String>(0),
        cache_before
    );
    let record_before: String = client
        .query_one("SELECT xmin::text FROM catalog.packages", &[])
        .await
        .unwrap()
        .get(0);
    assert!(
        !apply_package(request(&old, database.url()))
            .await
            .unwrap()
            .changed
    );
    assert_eq!(state(&mut client).await, installed);
    assert_eq!(
        client
            .query_one("SELECT xmin::text FROM catalog.packages", &[])
            .await
            .unwrap()
            .get::<_, String>(0),
        record_before
    );
    assert!(
        !apply_package_schema(request(&next, database.url()))
            .await
            .unwrap()
            .changed
    );

    // A later failing statement rolls back DDL, checksums, and current state.
    manifest["package"]["version"] = "4.0.0".into();
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    std::fs::write(
        next.join("migrations/0003_candidate.sql"),
        "ALTER TABLE inventory.dock ADD COLUMN failed text;",
    )
    .unwrap();
    std::fs::write(
        next.join("migrations/0004_failure.sql"),
        "ALTER TABLE inventory.dock ADD COLUMN failed text;",
    )
    .unwrap();
    assert!(
        apply_package_schema(request(&next, database.url()))
            .await
            .is_err()
    );
    assert_eq!(state(&mut client).await, installed);
    assert!(!client.query_one("SELECT EXISTS (SELECT FROM information_schema.columns WHERE table_schema='inventory' AND table_name='dock' AND column_name='failed')", &[]).await.unwrap().get::<_, bool>(0));

    // Reconstruct the retained SQL source, even after older code was selected.
    std::fs::remove_file(next.join("migrations/0003_candidate.sql")).unwrap();
    std::fs::remove_file(next.join("migrations/0004_failure.sql")).unwrap();
    manifest["package"]["version"] = "3.0.0".into();
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let fresh = wamn_project_state::test_database::tenant_platform();
    fresh.execute(&["DO $grant$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()); END $grant$;"]).unwrap();
    apply_package(request(&next, fresh.url())).await.unwrap();
    let (mut fresh_client, fresh_connection) =
        tokio_postgres::connect(fresh.url(), NoTls).await.unwrap();
    tokio::spawn(fresh_connection);
    let replacement = state(&mut fresh_client).await;
    assert_eq!(
        replacement.baseline_fingerprint,
        installed.physical_schema_fingerprint
    );
    assert_eq!(
        replacement.physical_schema_fingerprint,
        installed.physical_schema_fingerprint
    );
    assert_eq!(replacement.migrations, installed.migrations);
    assert_eq!(replacement.artifact_digest, installed.artifact_digest);
    assert_eq!(state(&mut client).await, installed);
}
