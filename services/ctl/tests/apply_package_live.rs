//! Disposable-PostgreSQL closure proof for the exact-byte package runner.

mod support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio_postgres::{Client, NoTls};
use wamn_control_provision::operation_grants::OPERATION_GRANT_LOCK_SQL;
use wamn_ctl::apply_package::{self, ApplyPackageArgs};

const CATALOG_SCHEMA: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
const APP_SCHEMA: &str = include_str!("../../../deploy/sql/app-schema.sql");
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
             DROP SCHEMA IF EXISTS race_alpha CASCADE; \
             DROP SCHEMA IF EXISTS race_beta CASCADE; \
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
    client
        .batch_execute(APP_SCHEMA)
        .await
        .expect("install production application authorization floor");
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

    let manifest_path = root.join("wamn.json");
    let mut manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&manifest_path).expect("read copied package manifest"),
    )
    .expect("parse copied package manifest");
    manifest["custom_operations"]["quality.create_inspection"] = serde_json::json!({
        "kind": "event_handler",
        "visibility": "private",
        "registration": {
            "source_package": "wamn_receiving",
            "entity": "receipt",
            "ops": ["insert"]
        }
    });
    manifest["components"]["quality_create_inspection"] = serde_json::json!({
        "operations": ["quality.create_inspection"],
        "connections": ["postgres"]
    });
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("serialize handler package manifest"),
    )
    .expect("write handler package manifest");
}

fn copy_receiving_package_as(root: &Path, package_id: &str, schema: &str) {
    copy_receiving_package(root);
    let manifest_path = root.join("wamn.json");
    let mut manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&manifest_path).expect("read copied package manifest"),
    )
    .expect("parse copied package manifest");
    manifest["package"]["id"] = serde_json::Value::String(package_id.to_owned());
    manifest["custom_operations"]["quality.create_inspection"]["registration"]["source_package"] =
        serde_json::Value::String(package_id.to_owned());
    for model in manifest["models"]
        .as_object_mut()
        .expect("manifest models are an object")
        .values_mut()
    {
        model["schema"] = serde_json::Value::String(schema.to_owned());
        model["owner"] = serde_json::Value::String(package_id.to_owned());
    }
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("serialize copied package manifest"),
    )
    .expect("write copied package manifest");

    let migration_path = root.join("migrations/0001_initial.sql");
    let migration = std::fs::read_to_string(&migration_path)
        .expect("read copied package migration")
        .replace("receiving.", &format!("{schema}."));
    std::fs::write(migration_path, migration).expect("write copied package migration");
}

fn copy_overlay_package(
    root: &Path,
    package_id: &str,
    model_id: &str,
    operation: &str,
    fields: &[&str],
    constraints: &[&str],
    migration: &str,
) {
    copy_receiving_package(root);
    let manifest_path = root.join("wamn.json");
    let mut manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&manifest_path).expect("read copied overlay manifest"),
    )
    .expect("parse copied overlay manifest");
    manifest["package"] = serde_json::json!({
        "id": package_id,
        "version": "3.0.0"
    });
    manifest["base_dependencies"] = serde_json::json!({
        "base_receiving": {
            "package": "wamn_receiving",
            "version": "1.0.0",
            "digest": format!("sha256:{}", "a".repeat(64)),
            "operations": ["receiving.record_receipt"]
        }
    });
    manifest["custom_operations"] = serde_json::json!({});

    let models = manifest["models"]
        .as_object_mut()
        .expect("overlay models are an object");
    models.retain(|name, _| name == model_id);
    let model = models
        .get_mut(model_id)
        .expect("selected base model exists");
    model["owner"] = serde_json::Value::String("wamn_receiving".into());
    model
        .as_object_mut()
        .expect("overlay model is an object")
        .remove("client_field_extensible");
    model["field_owners"] = serde_json::Value::Object(
        fields
            .iter()
            .map(|field| {
                (
                    (*field).to_owned(),
                    serde_json::Value::String(package_id.to_owned()),
                )
            })
            .collect(),
    );
    model["constraint_owners"] = serde_json::Value::Object(
        constraints
            .iter()
            .map(|constraint| {
                (
                    (*constraint).to_owned(),
                    serde_json::Value::String(package_id.to_owned()),
                )
            })
            .collect(),
    );
    model["operations"]
        .as_object_mut()
        .expect("model operations are an object")
        .retain(|name, _| format!("{model_id}.{name}") == operation);
    manifest["components"]
        .as_object_mut()
        .expect("components are an object")
        .retain(|_, component| {
            component["operations"]
                .as_array()
                .is_some_and(|operations| operations == &[serde_json::json!(operation)])
        });

    std::fs::write(
        manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("serialize overlay manifest"),
    )
    .expect("write overlay manifest");
    std::fs::write(root.join("migrations/0001_initial.sql"), migration)
        .expect("write exact overlay migration");
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
    apply_for_tenant(url, package, TENANT).await
}

async fn apply_for_tenant(url: &str, package: &Path, tenant: &str) -> anyhow::Result<()> {
    apply_package::run(ApplyPackageArgs {
        package: package.to_path_buf(),
        database_url: url.to_owned(),
        tenant: tenant.to_owned(),
    })
    .await
}

async fn prove_concurrent_package_grants_share_one_carrier(url: &str) {
    const RACE_TENANT: &str = "package-runner-race";
    let alpha =
        fixture_root().with_file_name(format!("apply-package-race-alpha-{}", std::process::id()));
    let beta =
        fixture_root().with_file_name(format!("apply-package-race-beta-{}", std::process::id()));
    copy_receiving_package_as(&alpha, "race_alpha", "race_alpha");
    copy_receiving_package_as(&beta, "race_beta", "race_beta");

    let mut blocker = connect(url).await;
    let observer = connect(url).await;
    let blocker_tx = blocker
        .transaction()
        .await
        .expect("begin grant lock blocker");
    blocker_tx
        .query_one(OPERATION_GRANT_LOCK_SQL, &[&RACE_TENANT])
        .await
        .expect("hold the shared tenant grant lock");

    let alpha_url = url.to_owned();
    let alpha_task =
        tokio::spawn(async move { apply_for_tenant(&alpha_url, &alpha, RACE_TENANT).await });
    let beta_url = url.to_owned();
    let beta_task =
        tokio::spawn(async move { apply_for_tenant(&beta_url, &beta, RACE_TENANT).await });

    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let waiting: i64 = observer
                .query_one(
                    "SELECT count(*) FROM pg_stat_activity \
                      WHERE datname = current_database() \
                        AND wait_event_type = 'Lock' AND wait_event = 'advisory' \
                        AND query LIKE '%wamn.operation-grants:%'",
                    &[],
                )
                .await
                .expect("observe package grant lock waiters")
                .get(0);
            if waiting == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("both package families must wait on the shared carrier lock");

    blocker_tx
        .commit()
        .await
        .expect("release grant lock blocker");
    alpha_task
        .await
        .expect("join alpha package apply")
        .expect("apply alpha package");
    beta_task
        .await
        .expect("join beta package apply")
        .expect("apply beta package");

    assert_eq!(
        observer
            .query_one(
                "SELECT count(*) FROM app_system.roles \
                  WHERE tenant_id = $1 AND name = 'route-caller' AND is_system",
                &[&RACE_TENANT],
            )
            .await
            .expect("read the shared route-caller role")
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        observer
            .query_one(
                "SELECT count(*) FROM app_system.permissions \
                  WHERE tenant_id = $1 AND role_name = 'route-caller' \
                    AND (permission LIKE 'race_alpha@1.0.0::%' \
                         OR permission LIKE 'race_beta@1.0.0::%')",
                &[&RACE_TENANT],
            )
            .await
            .expect("read both package grant sets")
            .get::<_, i64>(0),
        12,
        "a concurrent first package lost its six grants"
    );
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
               SELECT 'definition:' || schema_name || ':' || relation_name || ':' || \
                      definition_kind || ':' || definition_name || ':' || \
                      owner_package_id || ':' || client_field_extensible::text || ':' || xmin::text \
                 FROM catalog.package_definition_owners WHERE tenant_id = $1 \
               UNION ALL \
               SELECT 'entity:' || package_id || ':' || entity_id || ':' || xmin::text \
                 FROM receiving.wamn_entities \
               UNION ALL \
               SELECT 'role:' || name || ':' || xmin::text \
                 FROM app_system.roles WHERE tenant_id = $1 \
               UNION ALL \
               SELECT 'permission:' || permission || ':' || xmin::text \
                 FROM app_system.permissions WHERE tenant_id = $1 \
               UNION ALL \
               SELECT 'registration:' || registration_id || ':' || xmin::text \
                 FROM catalog.event_registrations WHERE tenant_id = $1 \
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
    prove_concurrent_package_grants_share_one_carrier(&url).await;
    client
        .batch_execute(&format!(
            "INSERT INTO app_system.roles (tenant_id, name, is_system) \
                 VALUES ('{TENANT}', 'route-caller', false); \
             INSERT INTO app_system.permissions (tenant_id, role_name, permission) VALUES \
                 ('{TENANT}', 'route-caller', 'wamn_receiving@1.0.0::obsolete.operation'), \
                 ('{TENANT}', 'route-caller', 'client_overlay@1.0.0::receipt.get');"
        ))
        .await
        .expect("seed exact-coordinate grant residue and a sibling coordinate");

    apply(&url, &package)
        .await
        .expect("first package apply commits");
    assert!(
        client
            .query_one(
                "SELECT is_system FROM app_system.roles \
                  WHERE tenant_id = $1 AND name = 'route-caller'",
                &[&TENANT],
            )
            .await
            .unwrap()
            .get::<_, bool>(0),
        "apply-package hardens the package grant carrier"
    );
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM app_system.permissions \
                  WHERE tenant_id = $1 AND role_name = 'route-caller' \
                    AND permission LIKE 'wamn_receiving@1.0.0::%'",
                &[&TENANT],
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        6
    );
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM app_system.permissions \
                  WHERE tenant_id = $1 AND role_name = 'route-caller' \
                    AND permission = 'client_overlay@1.0.0::receipt.get'",
                &[&TENANT],
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        1,
        "one package coordinate cannot delete a sibling package grant"
    );
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
    let registration: String = client
        .query_one(
            "SELECT registration::text FROM catalog.event_registrations \
              WHERE tenant_id = $1 AND package_id = 'wamn_receiving' \
                AND registration_id = 'quality.create_inspection'",
            &[&TENANT],
        )
        .await
        .expect("apply-package projects the inline handler registration")
        .get(0);
    let registration: serde_json::Value =
        serde_json::from_str(&registration).expect("parse projected registration");
    assert_eq!(registration["registration-id"], "quality.create_inspection");
    assert_eq!(registration["source-package-id"], "wamn_receiving");
    assert_eq!(registration["entity"], "receipt");
    assert_eq!(registration["ops"], serde_json::json!(["insert"]));
    assert!(registration.get("flow-id").is_none());
    let first_identity = write_identity(&client).await;
    apply(&url, &package)
        .await
        .expect("exact replay observes no pending work");
    assert_eq!(write_identity(&client).await, first_identity);

    let alter_base =
        fixture_root().with_file_name(format!("apply-package-alter-base-{}", std::process::id()));
    copy_overlay_package(
        &alter_base,
        "client_alter_receiving",
        "purchase_order",
        "purchase_order.update",
        &[],
        &[],
        "ALTER TABLE receiving.purchase_order ALTER COLUMN status SET DEFAULT 'complete';",
    );
    let alter_error = apply(&url, &alter_base)
        .await
        .expect_err("an overlay cannot alter a base-owned field");
    let alter_error = alter_error
        .downcast_ref::<apply_package::ApplyPackageError>()
        .expect("base field alteration is a typed ownership refusal");
    assert_eq!(
        alter_error.kind(),
        apply_package::ApplyPackageErrorKind::BaseDefinitionMutation
    );
    assert_eq!(alter_error.schema(), Some("receiving"));
    assert_eq!(alter_error.relation(), Some("purchase_order"));
    assert_eq!(alter_error.definition(), Some("status"));
    assert_eq!(alter_error.owner_package(), Some("wamn_receiving"));
    assert_eq!(write_identity(&client).await, first_identity);
    assert_eq!(
        client
            .query_one(
                "SELECT column_default FROM information_schema.columns \
                  WHERE table_schema = 'receiving' AND table_name = 'purchase_order' \
                    AND column_name = 'status'",
                &[],
            )
            .await
            .expect("read base status default after refused alteration")
            .get::<_, Option<String>>(0)
            .as_deref(),
        Some("'open'::text")
    );

    let drop_base =
        fixture_root().with_file_name(format!("apply-package-drop-base-{}", std::process::id()));
    copy_overlay_package(
        &drop_base,
        "client_drop_receiving",
        "purchase_order",
        "purchase_order.update",
        &[],
        &[],
        "ALTER TABLE receiving.purchase_order DROP COLUMN status;",
    );
    let drop_error = apply(&url, &drop_base)
        .await
        .expect_err("an overlay cannot drop a base-owned field");
    let drop_error = drop_error
        .downcast_ref::<apply_package::ApplyPackageError>()
        .expect("base field removal is a typed ownership refusal");
    assert_eq!(
        drop_error.kind(),
        apply_package::ApplyPackageErrorKind::BaseDefinitionMutation
    );
    assert_eq!(drop_error.definition(), Some("status"));
    assert_eq!(write_identity(&client).await, first_identity);
    assert!(
        client
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
                  WHERE table_schema = 'receiving' AND table_name = 'purchase_order' \
                    AND column_name = 'status')",
                &[],
            )
            .await
            .expect("read base field after refused removal")
            .get::<_, bool>(0)
    );

    let nonextensible = fixture_root().with_file_name(format!(
        "apply-package-nonextensible-{}",
        std::process::id()
    ));
    copy_overlay_package(
        &nonextensible,
        "client_receipt_extension",
        "receipt",
        "receipt.get",
        &["acme_receipt_flag"],
        &[],
        "ALTER TABLE receiving.receipt ADD COLUMN acme_receipt_flag boolean NOT NULL DEFAULT false;",
    );
    let nonextensible_error = apply(&url, &nonextensible)
        .await
        .expect_err("a base relation must explicitly admit client definitions");
    let nonextensible_error = nonextensible_error
        .downcast_ref::<apply_package::ApplyPackageError>()
        .expect("missing extensibility is a typed ownership refusal");
    assert_eq!(
        nonextensible_error.kind(),
        apply_package::ApplyPackageErrorKind::RelationNotClientExtensible
    );
    assert_eq!(write_identity(&client).await, first_identity);

    let overlay =
        fixture_root().with_file_name(format!("apply-package-acme-overlay-{}", std::process::id()));
    copy_overlay_package(
        &overlay,
        "client_acme_receiving",
        "purchase_order",
        "purchase_order.update",
        &["acme_inspection_required", "acme_quality_status"],
        &["purchase_order_acme_quality_status_check"],
        "ALTER TABLE receiving.purchase_order \
             ADD COLUMN acme_inspection_required boolean NOT NULL DEFAULT false; \
         ALTER TABLE receiving.purchase_order \
             ADD COLUMN acme_quality_status text NOT NULL DEFAULT 'not_required'; \
         ALTER TABLE receiving.purchase_order \
             ADD CONSTRAINT purchase_order_acme_quality_status_check \
             CHECK (acme_quality_status IN ('not_required', 'pending', 'approved', 'rejected'));",
    );
    apply(&url, &overlay)
        .await
        .expect("the exact client overlay applies after its exact base");
    assert_eq!(
        client
            .query(
                "SELECT definition_kind, definition_name, owner_package_id, \
                        client_field_extensible \
                   FROM catalog.package_definition_owners \
                  WHERE tenant_id = $1 AND schema_name = 'receiving' \
                    AND relation_name = 'purchase_order' \
                    AND ((definition_kind = 'relation' AND definition_name = 'purchase_order') \
                      OR (definition_kind = 'field' AND definition_name IN \
                          ('status', 'acme_inspection_required', 'acme_quality_status')) \
                      OR (definition_kind = 'constraint' AND definition_name = \
                          'purchase_order_acme_quality_status_check')) \
                  ORDER BY definition_kind, definition_name COLLATE \"C\"",
                &[&TENANT],
            )
            .await
            .expect("read exact base and overlay definition owners")
            .into_iter()
            .map(|row| {
                (
                    row.get::<_, String>(0),
                    row.get::<_, String>(1),
                    row.get::<_, String>(2),
                    row.get::<_, bool>(3),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                "constraint".into(),
                "purchase_order_acme_quality_status_check".into(),
                "client_acme_receiving".into(),
                false,
            ),
            (
                "field".into(),
                "acme_inspection_required".into(),
                "client_acme_receiving".into(),
                false,
            ),
            (
                "field".into(),
                "acme_quality_status".into(),
                "client_acme_receiving".into(),
                false,
            ),
            (
                "field".into(),
                "status".into(),
                "wamn_receiving".into(),
                false,
            ),
            (
                "relation".into(),
                "purchase_order".into(),
                "wamn_receiving".into(),
                true,
            ),
        ]
    );
    assert_eq!(
        client
            .query_one(
                "SELECT package_id FROM receiving.wamn_entities \
                  WHERE entity_id = 'purchase_order'",
                &[],
            )
            .await
            .expect("read shared relation source identity")
            .get::<_, String>(0),
        "wamn_receiving",
        "definition ownership must not rebind the base CDC entity identity"
    );
    let overlay_identity = write_identity(&client).await;
    apply(&url, &overlay)
        .await
        .expect("exact overlay replay is a no-op");
    assert_eq!(write_identity(&client).await, overlay_identity);

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
    for fixture in [alter_base, drop_base, nonextensible, overlay] {
        std::fs::remove_dir_all(fixture).expect("remove overlay package fixture directory");
    }
}
