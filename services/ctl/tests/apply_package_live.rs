//! Disposable-PostgreSQL closure proof for the exact-byte package runner.

mod support;

use std::path::{Path, PathBuf};

use tokio_postgres::{Client, NoTls};
use wamn_ctl::apply_package::{self, ApplyPackageArgs};

const CATALOG_SCHEMA: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
const TENANT: &str = "package-runner-live";

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to disposable PostgreSQL");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

async fn install(client: &Client) {
    client
        .batch_execute(
            "DROP SCHEMA IF EXISTS receiving CASCADE; \
             DROP SCHEMA IF EXISTS app_system CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             DO $roles$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                 CREATE ROLE wamn_app NOLOGIN; \
               END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN \
                 CREATE ROLE wamn_scenario_author NOLOGIN; \
               END IF; \
             END $roles$;",
        )
        .await
        .expect("reset package-runner schemas");
    client
        .batch_execute(CATALOG_SCHEMA)
        .await
        .expect("install production package catalog schema");
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("apply-package-live-{}", std::process::id()))
}

fn copy_receiving_package(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root.join("migrations")).expect("create package fixture directory");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/receiving");
    std::fs::copy(source.join("wamn.json"), root.join("wamn.json"))
        .expect("copy strict package manifest");
    std::fs::copy(
        source.join("migrations/0001_initial.sql"),
        root.join("migrations/0001_initial.sql"),
    )
    .expect("copy exact initial migration");
}

fn set_package_identity(root: &Path, version: &str, predecessor: Option<&str>) {
    let manifest_path = root.join("wamn.json");
    let mut manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&manifest_path).expect("read package manifest identity"),
    )
    .expect("parse package manifest identity");
    manifest["package"]["version"] = serde_json::Value::String(version.to_owned());
    match predecessor {
        Some(predecessor) => {
            manifest["package"]["predecessor_version"] =
                serde_json::Value::String(predecessor.to_owned());
        }
        None => {
            manifest["package"]
                .as_object_mut()
                .expect("package identity is an object")
                .remove("predecessor_version");
        }
    }
    std::fs::write(
        manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("serialize package manifest identity"),
    )
    .expect("write package manifest identity");
}

async fn apply(url: &str, package: &Path) -> anyhow::Result<()> {
    apply_package::run(ApplyPackageArgs {
        package: package.to_path_buf(),
        database_url: url.to_owned(),
        tenant: TENANT.to_owned(),
    })
    .await
}

async fn write_identity(client: &Client) -> Vec<String> {
    client
        .query(
            "SELECT identity FROM ( \
               SELECT 'package:' || xmin::text AS identity \
                 FROM catalog.packages WHERE tenant_id = $1 \
               UNION ALL \
               SELECT 'migration:' || ordinal::text || ':' || xmin::text \
                 FROM catalog.package_migrations WHERE tenant_id = $1 \
               UNION ALL \
               SELECT 'entity:' || package_id || ':' || entity_id || ':' || xmin::text \
                 FROM receiving.wamn_entities \
             ) AS observed ORDER BY identity COLLATE \"C\"",
            &[&TENANT],
        )
        .await
        .expect("read package, migration, and entity-map write identities")
        .into_iter()
        .map(|row| row.get(0))
        .collect()
}

#[tokio::test]
async fn exact_runner_commits_once_refuses_drift_and_rolls_back_a_failing_suffix() {
    let Some(url) = support::LockedUrl::optional() else {
        eprintln!("skipping apply-package live proof; WAMN_CTL_PG_URL is unset");
        return;
    };
    let client = connect(&url).await;
    let package = fixture_root();
    copy_receiving_package(&package);
    install(&client).await;

    apply(&url, &package)
        .await
        .expect("first package apply commits");
    assert!(
        client
            .query_one("SELECT to_regnamespace('receiving') IS NOT NULL", &[])
            .await
            .unwrap()
            .get::<_, bool>(0),
        "apply-package creates the manifest-declared schema"
    );
    assert!(
        client
            .query_one(
                "SELECT to_regclass('receiving.purchase_order') IS NOT NULL",
                &[]
            )
            .await
            .unwrap()
            .get::<_, bool>(0)
    );
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM catalog.package_migrations WHERE tenant_id = $1",
                &[&TENANT],
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    let first_identity = write_identity(&client).await;
    apply(&url, &package)
        .await
        .expect("exact replay observes no pending work");
    assert_eq!(write_identity(&client).await, first_identity);

    assert_eq!(
        client
            .execute(
                "UPDATE receiving.wamn_entities \
                    SET table_name = 'stale_purchase_order' \
                  WHERE package_id = 'wamn_receiving' AND entity_id = 'purchase_order'",
                &[],
            )
            .await
            .expect("seed stale informational table name"),
        1
    );
    apply(&url, &package)
        .await
        .expect("same entity identity may converge its informational table name");
    assert_eq!(
        client
            .query_one(
                "SELECT table_name FROM receiving.wamn_entities \
                  WHERE package_id = 'wamn_receiving' AND entity_id = 'purchase_order'",
                &[],
            )
            .await
            .expect("read converged entity map")
            .get::<_, String>(0),
        "purchase_order"
    );

    assert_eq!(
        client
            .execute(
                "UPDATE receiving.wamn_entities \
                    SET package_id = 'foreign_package' \
                  WHERE package_id = 'wamn_receiving' AND entity_id = 'purchase_order'",
                &[],
            )
            .await
            .expect("seed an existing OID owned by a different package identity"),
        1
    );
    assert_eq!(
        client
            .execute(
                &wamn_control_provision::sql::upsert_entity_map_sql("receiving"),
                &[&"wamn_receiving", &"purchase_order", &"purchase_order"],
            )
            .await
            .expect("run guarded generated entity-map upsert"),
        0,
        "the generated upsert must not rebind an existing relation OID"
    );
    let rebind = apply(&url, &package)
        .await
        .expect_err("an existing relation OID cannot be rebound to another package/entity");
    assert!(format!("{rebind:#}").contains("package-entity-oid-rebind-refused"));
    assert_eq!(
        client
            .query_one(
                "SELECT package_id FROM receiving.wamn_entities \
                  WHERE entity_id = 'purchase_order'",
                &[],
            )
            .await
            .expect("read refused entity-map rebind")
            .get::<_, String>(0),
        "foreign_package"
    );
    assert_eq!(
        client
            .execute(
                "UPDATE receiving.wamn_entities \
                    SET package_id = 'wamn_receiving' \
                  WHERE package_id = 'foreign_package' AND entity_id = 'purchase_order'",
                &[],
            )
            .await
            .expect("restore package fixture identity"),
        1
    );

    let migration = package.join("migrations/0001_initial.sql");
    let original = std::fs::read(&migration).expect("read copied initial migration");
    let mut edited = original.clone();
    edited.extend_from_slice(b"\n");
    std::fs::write(&migration, edited).expect("edit applied migration bytes");
    let drift = apply(&url, &package)
        .await
        .expect_err("edited applied bytes must refuse");
    let drift = format!("{drift:#}");
    assert!(drift.contains("migrations/0001_initial.sql"));
    assert!(drift.contains("recorded-sha256="));
    assert!(drift.contains("actual-sha256="));
    std::fs::write(&migration, &original).expect("restore exact applied bytes");

    std::fs::write(
        package.join("migrations/0002_candidate.sql"),
        "CREATE TABLE receiving.candidate (id int);",
    )
    .expect("write the first pending migration");
    std::fs::write(
        package.join("migrations/0003_failure.sql"),
        "CREATE TABLE receiving.purchase_order (id int);",
    )
    .expect("write the server-refused migration after it");
    apply(&url, &package)
        .await
        .expect_err("a failing later statement must roll back the whole suffix");
    assert!(
        !client
            .query_one("SELECT to_regclass('receiving.candidate') IS NOT NULL", &[])
            .await
            .unwrap()
            .get::<_, bool>(0)
    );
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM catalog.package_migrations WHERE tenant_id = $1",
                &[&TENANT],
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );

    std::fs::remove_file(package.join("migrations/0002_candidate.sql"))
        .expect("remove rolled-back candidate migration");
    std::fs::remove_file(package.join("migrations/0003_failure.sql"))
        .expect("remove server-refused migration");
    client
        .execute(
            "INSERT INTO catalog.effective_releases \
                 (tenant_id, effective_release_id, environment) \
             VALUES ($1, $2, $3)",
            &[&TENANT, &1_i32, &"development"],
        )
        .await
        .expect("seed an effective release");
    client
        .execute(
            "INSERT INTO catalog.effective_release_packages \
                 (tenant_id, effective_release_id, package_id, package_version) \
             VALUES ($1, $2, $3, $4)",
            &[&TENANT, &1_i32, &"wamn_receiving", &"1.0.0"],
        )
        .await
        .expect("seal the applied package coordinate through release membership");
    std::fs::write(
        package.join("migrations/0002_after_seal.sql"),
        "CREATE TABLE receiving.after_seal (id int);",
    )
    .expect("write a migration after the package coordinate was sealed");
    let sealed = apply(&url, &package)
        .await
        .expect_err("a sealed package version refuses an additional migration");
    let sealed = sealed
        .downcast_ref::<apply_package::ApplyPackageError>()
        .expect("the server seal is translated at the apply-package boundary");
    assert_eq!(
        sealed.kind(),
        apply_package::ApplyPackageErrorKind::PackageVersionSealed
    );
    assert_eq!(sealed.coordinate(), "wamn_receiving@1.0.0");
    assert!(
        sealed
            .to_string()
            .contains("create and apply a new package version")
    );
    assert!(
        !client
            .query_one(
                "SELECT to_regclass('receiving.after_seal') IS NOT NULL",
                &[]
            )
            .await
            .unwrap()
            .get::<_, bool>(0),
        "the DDL before the sealed ledger write rolls back with the transaction"
    );
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM catalog.package_migrations \
                  WHERE tenant_id = $1 AND package_id = 'wamn_receiving' \
                    AND package_version = '1.0.0'",
                &[&TENANT],
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );

    set_package_identity(&package, "1.0.1", None);
    let undeclared = apply(&url, &package)
        .await
        .expect_err("a new coordinate over existing history must declare its predecessor");
    let undeclared = undeclared
        .downcast_ref::<apply_package::ApplyPackageError>()
        .expect("an undeclared predecessor is a typed apply-package refusal");
    assert_eq!(
        undeclared.kind(),
        apply_package::ApplyPackageErrorKind::PredecessorNotCurrent
    );
    assert_eq!(undeclared.coordinate(), "wamn_receiving@1.0.1");
    assert_eq!(undeclared.predecessor_version(), None);
    assert_eq!(undeclared.current_version(), Some("1.0.0"));
    assert_eq!(undeclared.path(), None);

    set_package_identity(&package, "1.0.1", Some("0.9.0"));
    let absent = apply(&url, &package)
        .await
        .expect_err("an upgrade cannot substitute another installed version for its predecessor");
    let absent = absent
        .downcast_ref::<apply_package::ApplyPackageError>()
        .expect("an absent predecessor is a typed apply-package refusal");
    assert_eq!(
        absent.kind(),
        apply_package::ApplyPackageErrorKind::PredecessorNotCurrent
    );
    assert_eq!(absent.coordinate(), "wamn_receiving@1.0.1");
    assert_eq!(absent.predecessor_version(), Some("0.9.0"));
    assert_eq!(absent.current_version(), Some("1.0.0"));
    assert_eq!(absent.path(), None);

    set_package_identity(&package, "1.0.1", Some("1.0.0"));
    let mut divergent = original.clone();
    divergent.extend_from_slice(b"\n");
    std::fs::write(&migration, divergent).expect("diverge the cumulative predecessor prefix");
    let mismatch = apply(&url, &package)
        .await
        .expect_err("a divergent predecessor prefix refuses before writes");
    let mismatch = mismatch
        .downcast_ref::<apply_package::ApplyPackageError>()
        .expect("a divergent predecessor is a typed apply-package refusal");
    assert_eq!(
        mismatch.kind(),
        apply_package::ApplyPackageErrorKind::PredecessorPrefixMismatch
    );
    assert_eq!(mismatch.predecessor_version(), Some("1.0.0"));
    assert_eq!(mismatch.path(), Some("migrations/0001_initial.sql"));
    assert!(
        !client
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM catalog.packages \
                  WHERE tenant_id = $1 AND package_id = 'wamn_receiving' \
                    AND package_version = '1.0.1')",
                &[&TENANT],
            )
            .await
            .unwrap()
            .get::<_, bool>(0),
        "predecessor mismatch refuses before registering the new root"
    );
    assert!(
        !client
            .query_one(
                "SELECT to_regclass('receiving.after_seal') IS NOT NULL",
                &[]
            )
            .await
            .unwrap()
            .get::<_, bool>(0),
        "predecessor mismatch refuses before executing the suffix"
    );
    std::fs::write(&migration, &original).expect("restore the cumulative predecessor prefix");

    apply(&url, &package)
        .await
        .expect("upgrade inherits the verified prefix and executes only the suffix");
    assert!(
        client
            .query_one(
                "SELECT to_regclass('receiving.after_seal') IS NOT NULL",
                &[]
            )
            .await
            .unwrap()
            .get::<_, bool>(0)
    );
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM catalog.package_migrations \
                  WHERE tenant_id = $1 AND package_id = 'wamn_receiving' \
                    AND package_version = '1.0.1'",
                &[&TENANT],
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        2
    );
    assert_eq!(
        client
            .query_one(
                "SELECT predecessor_version FROM catalog.packages \
                  WHERE tenant_id = $1 AND package_id = 'wamn_receiving' \
                    AND package_version = '1.0.1'",
                &[&TENANT],
            )
            .await
            .unwrap()
            .get::<_, Option<String>>(0)
            .as_deref(),
        Some("1.0.0")
    );
    assert!(
        client
            .query_one(
                "SELECT old.sha256 = new.sha256 \
                   FROM catalog.package_migrations AS old \
                   JOIN catalog.package_migrations AS new \
                     ON new.tenant_id = old.tenant_id \
                    AND new.package_id = old.package_id \
                    AND new.ordinal = old.ordinal \
                  WHERE old.tenant_id = $1 \
                    AND old.package_id = 'wamn_receiving' \
                    AND old.package_version = '1.0.0' \
                    AND new.package_version = '1.0.1' \
                    AND old.ordinal = 1",
                &[&TENANT],
            )
            .await
            .unwrap()
            .get::<_, bool>(0),
        "upgrade records the predecessor's exact bytes under the new coordinate"
    );
    let upgraded_identity = write_identity(&client).await;
    apply(&url, &package)
        .await
        .expect("exact cumulative upgrade replay is a no-op");
    assert_eq!(write_identity(&client).await, upgraded_identity);

    install(&client).await;
    apply(&url, &package)
        .await
        .expect("a fresh target executes the complete cumulative stream");
    for relation in ["receiving.purchase_order", "receiving.after_seal"] {
        assert!(
            client
                .query_one("SELECT to_regclass($1) IS NOT NULL", &[&relation])
                .await
                .unwrap()
                .get::<_, bool>(0),
            "fresh cumulative apply creates {relation}"
        );
    }
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM catalog.package_migrations \
                  WHERE tenant_id = $1 AND package_id = 'wamn_receiving' \
                    AND package_version = '1.0.1'",
                &[&TENANT],
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        2
    );

    client
        .batch_execute(
            "DROP SCHEMA IF EXISTS receiving CASCADE; \
             DROP SCHEMA IF EXISTS app_system CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE;",
        )
        .await
        .expect("clean package-runner schemas");
    std::fs::remove_dir_all(package).expect("remove package fixture directory");
}
