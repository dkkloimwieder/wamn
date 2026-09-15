//! Disposable-PostgreSQL closure test for the exact-byte package runner.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio_postgres::{Client, NoTls};
use wamn_control::apply_package::{self, ApplyPackageRequest};
use wamn_control_provision::PlatformComponent;
use wamn_control_provision::operation_grants::{OPERATION_GRANT_LOCK_SQL, operation_grant_tokens};
use wamn_schema_introspection::migration_policy::{MigrationPolicyError, MigrationPolicyErrorKind};
use wamn_test_infrastructure::locked_database;

const CATALOG_SCHEMA: &str = wamn_catalog::CATALOG_SCHEMA_SQL;
const APP_SCHEMA: &str = include_str!("../../../../deploy/sql/app-schema.sql");
const TENANT: &str = "package-runner-live";
/// The test principal that the fixture seed writes as.
const FIXTURE_PRINCIPAL: &str = "00000000-0000-4000-8000-0000000000f1";

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
             DROP SCHEMA IF EXISTS wamn_history CASCADE; \
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
        .batch_execute(wamn_control_provision::sql::ensure_db_owner_role_sql())
        .await
        .expect("ensure the production package-owner role");
    client
        .batch_execute(
            "DO $grant$ BEGIN \
               EXECUTE format(\
                 'GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()\
               ); \
             END $grant$;",
        )
        .await
        .expect("grant the package-owner role its production-equivalent database authority");
    client
        .batch_execute(CATALOG_SCHEMA)
        .await
        .expect("install production package catalog schema");
    client
        .batch_execute(APP_SCHEMA)
        .await
        .expect("install production application authorization floor");
    client
        .batch_execute(
            "ALTER TABLE catalog.package_migrations \
               ADD COLUMN apply_effective_role name NOT NULL DEFAULT CURRENT_USER;",
        )
        .await
        .expect("instrument the server-visible role used for trusted record writes");
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("apply-package-live-{}", std::process::id()))
}

fn overlay_package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/client_acme_receiving")
}

fn copy_receiving_package(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root.join("migrations")).expect("create package fixture directory");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/wamn_receiving");
    std::fs::copy(source.join("wamn.json"), root.join("wamn.json"))
        .expect("copy strict package manifest");
    std::fs::copy(
        source.join("migrations/0001_initial.sql"),
        root.join("migrations/0001_initial.sql"),
    )
    .expect("copy exact initial migration");
    declare_missing_audit_logs(root);
}

/// Copy the real client overlay so that its fixture declares every audit_log.
fn copy_real_overlay_package(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root.join("migrations")).expect("create overlay fixture directory");
    let source = overlay_package_root();
    std::fs::copy(source.join("wamn.json"), root.join("wamn.json"))
        .expect("copy strict overlay manifest");
    for entry in std::fs::read_dir(source.join("migrations")).expect("list overlay migrations") {
        let path = entry.expect("read overlay migration entry").path();
        std::fs::copy(
            &path,
            root.join("migrations").join(path.file_name().unwrap()),
        )
        .expect("copy exact overlay migration");
    }
    declare_missing_audit_logs(root);
}

/// Give each owned model without a declaration `"columns": []`.
///
/// The fixture then applies before the application declares audit_log, and it
/// keeps any declaration that the application already has.
fn declare_missing_audit_logs(root: &Path) {
    let manifest_path = root.join("wamn.json");
    let mut manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&manifest_path).expect("read fixture manifest declarations"),
    )
    .expect("parse fixture manifest declarations");
    let package_id = manifest["package"]["id"].clone();
    for model in manifest["models"]
        .as_object_mut()
        .expect("manifest models are an object")
        .values_mut()
    {
        if model["owner"] == package_id {
            model
                .as_object_mut()
                .expect("manifest model is an object")
                .entry("audit_log")
                .or_insert_with(|| serde_json::json!({"columns": [], "retention": "none"}));
        }
    }
    std::fs::write(
        manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("serialize fixture manifest declarations"),
    )
    .expect("write fixture manifest declarations");
}

fn set_audit_log_columns(root: &Path, model_id: &str, columns: &[&str]) {
    let manifest_path = root.join("wamn.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).expect("read audit_log manifest"))
            .expect("parse audit_log manifest");
    manifest["models"][model_id]["audit_log"] =
        serde_json::json!({"columns": columns, "retention": "none"});
    std::fs::write(
        manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("serialize audit_log manifest"),
    )
    .expect("write audit_log manifest");
}

fn set_audit_log_retention(root: &Path, model_id: &str, retention: &str) {
    let manifest_path = root.join("wamn.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).expect("read retention manifest"))
            .expect("parse retention manifest");
    manifest["models"][model_id]["audit_log"]["retention"] =
        serde_json::Value::String(retention.to_owned());
    std::fs::write(
        manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("serialize retention manifest"),
    )
    .expect("write retention manifest");
}

/// Every user trigger in the receiving schema, as the server renders it, with
/// the identity of its catalog row.
async fn receiving_triggers(client: &Client) -> Vec<(String, String)> {
    client
        .query(
            "SELECT pg_catalog.pg_get_triggerdef(t.oid), t.oid::text || ':' || t.xmin::text \
               FROM pg_catalog.pg_trigger AS t \
               JOIN pg_catalog.pg_class AS c ON c.oid = t.tgrelid \
               JOIN pg_catalog.pg_namespace AS n ON n.oid = c.relnamespace \
              WHERE n.nspname = 'receiving' AND NOT t.tgisinternal \
              ORDER BY c.relname, t.tgname",
            &[],
        )
        .await
        .expect("read installed receiving triggers")
        .into_iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect()
}

fn trigger_definitions(triggers: &[(String, String)]) -> Vec<&str> {
    triggers
        .iter()
        .map(|(definition, _)| definition.as_str())
        .collect()
}

fn copy_receiving_package_as(root: &Path, package_id: &str, schema: &str) {
    copy_receiving_package(root);
    let manifest_path = root.join("wamn.json");
    let mut manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&manifest_path).expect("read copied package manifest"),
    )
    .expect("parse copied package manifest");
    manifest["package"]["id"] = serde_json::Value::String(package_id.to_owned());
    for model in manifest["models"]
        .as_object_mut()
        .expect("manifest models are an object")
        .values_mut()
    {
        model["schema"] = serde_json::Value::String(schema.to_owned());
        model["owner"] = serde_json::Value::String(package_id.to_owned());
    }
    for relation in manifest["internal_relations"]
        .as_object_mut()
        .expect("manifest internal relations are an object")
        .values_mut()
    {
        relation["schema"] = serde_json::Value::String(schema.to_owned());
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
    manifest["internal_relations"] = serde_json::json!({});

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
    model
        .as_object_mut()
        .expect("overlay model is an object")
        .remove("audit_log");
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

fn declare_ownership_only_model(root: &Path, model_id: &str, table: &str) {
    let manifest_path = root.join("wamn.json");
    let mut manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&manifest_path).expect("read package manifest model vocabulary"),
    )
    .expect("parse package manifest model vocabulary");
    manifest["models"][model_id] = serde_json::json!({
        "schema": "receiving",
        "table": table,
        "owner": "wamn_receiving",
        "server_owned_fields": ["id"],
        "enum_fields": {},
        "audit_log": {"columns": [], "retention": "none"},
        "operations": {}
    });
    std::fs::write(
        manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("serialize package manifest model vocabulary"),
    )
    .expect("write package manifest model vocabulary");
}

fn rename_internal_relation(root: &Path, from: &str, to: &str) {
    let manifest_path = root.join("wamn.json");
    let mut manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&manifest_path).expect("read internal-relation manifest"),
    )
    .expect("parse internal-relation manifest");
    let relations = manifest["internal_relations"]
        .as_object_mut()
        .expect("internal relations are an object");
    let relation = relations
        .remove(from)
        .expect("source internal relation exists");
    assert!(relations.insert(to.to_owned(), relation).is_none());
    std::fs::write(
        manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("serialize internal-relation manifest"),
    )
    .expect("write internal-relation manifest");
}

async fn apply(url: &str, package: &Path) -> anyhow::Result<apply_package::ApplyOutcome> {
    apply_for_tenant(url, package, TENANT).await
}

async fn apply_for_tenant(
    url: &str,
    package: &Path,
    tenant: &str,
) -> anyhow::Result<apply_package::ApplyOutcome> {
    apply_package::apply_package(ApplyPackageRequest {
        package: package.to_path_buf(),
        database_url: url.to_owned(),
        tenant: tenant.to_owned(),
    })
    .await
}

async fn assert_concurrent_package_grants_share_one_carrier(url: &str) {
    const RACE_TENANT: &str = "package-runner-race";
    let alpha =
        fixture_root().with_file_name(format!("apply-package-race-alpha-{}", std::process::id()));
    let beta =
        fixture_root().with_file_name(format!("apply-package-race-beta-{}", std::process::id()));
    copy_receiving_package_as(&alpha, "race_alpha", "race_alpha");
    copy_receiving_package_as(&beta, "race_beta", "race_beta");
    let expected_grants = [&alpha, &beta]
        .into_iter()
        .flat_map(|package| {
            operation_grant_tokens(&std::fs::read(package.join("wamn.json")).unwrap()).unwrap()
        })
        .collect::<BTreeSet<_>>();

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

    // The per-database audit retention lock also serializes the two log
    // trigger reconciliations. So one package waits on the carrier while it
    // holds that lock, and the other package waits on that lock.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let waiting = observer
                .query_one(
                    "SELECT count(*) FILTER (WHERE query LIKE '%wamn.operation-grants:%'), \
                            count(*) FILTER (WHERE query LIKE '%wamn.audit-retention%') \
                       FROM pg_stat_activity \
                      WHERE datname = current_database() \
                        AND wait_event_type = 'Lock' AND wait_event = 'advisory'",
                    &[],
                )
                .await
                .expect("observe package grant lock waiters");
            if (waiting.get::<_, i64>(0), waiting.get::<_, i64>(1)) == (1, 1) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("one package family must wait on the shared carrier lock and one on the audit retention lock");

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
    let actual_grants = observer
        .query(
            "SELECT permission FROM app_system.permissions \
              WHERE tenant_id = $1 AND role_name = 'route-caller' \
                AND (permission LIKE 'race-alpha:%@1.0.0' \
                     OR permission LIKE 'race-beta:%@1.0.0')",
            &[&RACE_TENANT],
        )
        .await
        .expect("read both package grant sets")
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        actual_grants, expected_grants,
        "concurrent packages must retain every declared grant"
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
               SELECT 'excluded:' || package_id || ':' || relation_id || ':' || xmin::text \
                 FROM receiving.wamn_cdc_exclusions \
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
    let url = locked_database::database(wamn_test_postgres::database);
    let client = connect(&url).await;
    let package = fixture_root();
    copy_receiving_package(&package);
    install(&client).await;

    let distinct_identity = package.with_file_name(format!(
        "apply-package-distinct-internal-relation-{}",
        std::process::id()
    ));
    copy_receiving_package(&distinct_identity);
    rename_internal_relation(
        &distinct_identity,
        "record_receipt_command",
        "receipt_command_record",
    );
    apply(&url, &distinct_identity)
        .await
        .expect("apply a relation whose manifest identity differs from its table");
    let mapping = client
        .query_one(
            "SELECT relation_id, table_name FROM receiving.wamn_cdc_exclusions \
              WHERE table_name NOT LIKE '%\\_history'",
            &[],
        )
        .await
        .expect("read distinct internal relation identity");
    assert_eq!(mapping.get::<_, String>(0), "receipt_command_record");
    assert_eq!(mapping.get::<_, String>(1), "record_receipt_command");
    std::fs::remove_dir_all(&distinct_identity).expect("remove distinct internal-relation fixture");
    install(&client).await;

    let undeclared = package.with_file_name(format!(
        "apply-package-undeclared-relation-{}",
        std::process::id()
    ));
    copy_receiving_package(&undeclared);
    std::fs::write(
        undeclared.join("migrations/0002_undeclared.sql"),
        "CREATE TABLE receiving.undeclared_relation (id int);",
    )
    .expect("write undeclared-relation fixture");
    let error = apply(&url, &undeclared)
        .await
        .expect_err("an undeclared created relation must refuse before writes");
    assert!(
        format!("{error:#}").contains(apply_package::DEFINITION_OWNER_DECLARATION_MISSING_REFUSAL),
        "unexpected undeclared-relation refusal: {error:#}"
    );
    assert!(
        !client
            .query_one(
                "SELECT to_regclass('receiving.undeclared_relation') IS NOT NULL",
                &[],
            )
            .await
            .unwrap()
            .get::<_, bool>(0),
        "the undeclared relation reached PostgreSQL"
    );
    std::fs::remove_dir_all(&undeclared).expect("remove undeclared-relation fixture");

    let missing_exclusion = package.with_file_name(format!(
        "apply-package-missing-exclusion-{}",
        std::process::id()
    ));
    copy_receiving_package(&missing_exclusion);
    let manifest_path = missing_exclusion.join("wamn.json");
    let mut manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&manifest_path).expect("read missing-exclusion manifest"),
    )
    .expect("parse missing-exclusion manifest");
    manifest["internal_relations"]["missing_command"] = serde_json::json!({
        "schema": "receiving",
        "table": "missing_command",
        "cdc": "excluded"
    });
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("serialize missing-exclusion manifest"),
    )
    .expect("write missing-exclusion manifest");
    let error = apply(&url, &missing_exclusion)
        .await
        .expect_err("an exclusion absent from migrations must refuse before writes");
    assert!(
        format!("{error:#}").contains("cdc-excluded-relation-missing"),
        "unexpected missing-exclusion refusal: {error:#}"
    );
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM catalog.packages", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0,
        "a classification refusal wrote a package root"
    );
    std::fs::remove_dir_all(&missing_exclusion).expect("remove missing-exclusion fixture");

    assert_concurrent_package_grants_share_one_carrier(&url).await;
    client
        .batch_execute(&format!(
            "BEGIN; \
             SELECT set_config('app.user_id', '{FIXTURE_PRINCIPAL}', true), \
                    set_config('app.operation', 'admin:seed-grant-residue-fixture', true); \
             INSERT INTO app_system.users (tenant_id, id, type, email) \
                 VALUES ('{TENANT}', '{FIXTURE_PRINCIPAL}', 'person', 'fixture@example.invalid'); \
             INSERT INTO app_system.roles (tenant_id, name, is_system) \
                 VALUES ('{TENANT}', 'route-caller', false); \
             INSERT INTO app_system.permissions (tenant_id, role_name, permission) VALUES \
                 ('{TENANT}', 'route-caller', 'wamn-receiving:obsolete/operation@1.0.0'), \
                 ('{TENANT}', 'route-caller', 'client-overlay:receipt/get@1.0.0'); \
             COMMIT;"
        ))
        .await
        .expect("seed exact-coordinate grant residue and a sibling coordinate");

    apply(&url, &package)
        .await
        .expect("first package apply commits");
    assert_eq!(
        client
            .query_one(
                "SELECT apply_effective_role::text = current_user::text \
                   FROM catalog.package_migrations \
                  WHERE tenant_id = $1 AND package_id = 'wamn_receiving' \
                    AND package_version = '1.0.0' AND ordinal = 1",
                &[&TENANT],
            )
            .await
            .expect("read server-visible authority used for the migration records")
            .get::<_, bool>(0),
        true,
        "apply-package did not RESET ROLE before its trusted migration-record write"
    );
    let ownership = client
        .query_one(
            "SELECT pg_catalog.pg_get_userbyid(namespace.nspowner) = $1, \
                    count(*) FILTER (\
                        WHERE pg_catalog.pg_get_userbyid(relation.relowner) <> $1\
                    ) = 0 \
               FROM pg_catalog.pg_namespace AS namespace \
               JOIN pg_catalog.pg_class AS relation \
                 ON relation.relnamespace = namespace.oid \
              WHERE namespace.nspname = 'receiving' \
                AND relation.relkind = 'r' \
                AND relation.relname NOT IN ('wamn_entities', 'wamn_cdc_exclusions') \
              GROUP BY namespace.nspowner",
            &[&wamn_control_provision::DB_OWNER_ROLE],
        )
        .await
        .expect("read server-derived package schema and relation owners");
    assert!(
        ownership.get::<_, bool>(0) && ownership.get::<_, bool>(1),
        "schema creation and exact package DDL must run as wamn_db_owner"
    );
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
    let actual_grants = client
        .query(
            "SELECT permission FROM app_system.permissions \
              WHERE tenant_id = $1 AND role_name = 'route-caller' \
                AND permission LIKE 'wamn-receiving:%@1.0.0'",
            &[&TENANT],
        )
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<BTreeSet<_>>();
    let expected_grants = operation_grant_tokens(
        &std::fs::read(package.join("wamn.json")).expect("read applied manifest"),
    )
    .expect("derive the declared package grants");
    assert_eq!(actual_grants, expected_grants);
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM app_system.permissions \
                  WHERE tenant_id = $1 AND role_name = 'route-caller' \
                    AND permission = 'client-overlay:receipt/get@1.0.0'",
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
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM receiving.wamn_entities", &[])
            .await
            .expect("count package entity mappings")
            .get::<_, i64>(0),
        6,
        "every POC-listed base model has one entity mapping"
    );
    let exclusion = client
        .query_one(
            "SELECT package_id, relation_id, table_name \
               FROM receiving.wamn_cdc_exclusions \
              WHERE table_name NOT LIKE '%\\_history'",
            &[],
        )
        .await
        .expect("read explicit package CDC exclusion");
    assert_eq!(
        (
            exclusion.get::<_, String>(0),
            exclusion.get::<_, String>(1),
            exclusion.get::<_, String>(2),
        ),
        (
            "wamn_receiving".into(),
            "record_receipt_command".into(),
            "record_receipt_command".into(),
        )
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
                "SELECT count(*) FROM catalog.package_migrations \
                 WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3",
                &[&TENANT, &"wamn_receiving", &"1.0.0"],
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
        fixture_root().with_file_name(format!("apply-package-real-overlay-{}", std::process::id()));
    copy_real_overlay_package(&overlay);
    apply(&url, &overlay)
        .await
        .expect("the exact client overlay applies after its exact base");
    let registration: String = client
        .query_one(
            "SELECT registration::text FROM catalog.event_registrations \
              WHERE tenant_id = $1 AND package_id = 'client_acme_receiving' \
                AND registration_id = 'quality.create_inspection'",
            &[&TENANT],
        )
        .await
        .expect("apply-package projects the real overlay handler registration")
        .get(0);
    let registration: serde_json::Value =
        serde_json::from_str(&registration).expect("parse projected registration");
    assert_eq!(registration["registration-id"], "quality.create_inspection");
    assert_eq!(registration["source-package-id"], "wamn_receiving");
    assert_eq!(registration["entity"], "receipt");
    assert_eq!(registration["ops"], serde_json::json!(["insert"]));
    assert!(registration.get("flow-id").is_none());
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

    assert_eq!(
        client
            .execute(
                "UPDATE receiving.wamn_cdc_exclusions \
                    SET table_name = 'stale_record_receipt_command' \
                  WHERE package_id = 'wamn_receiving' \
                    AND relation_id = 'record_receipt_command'",
                &[],
            )
            .await
            .expect("seed stale CDC-exclusion table name"),
        1
    );
    apply(&url, &package)
        .await
        .expect("same exclusion identity may converge its informational table name");
    assert_eq!(
        client
            .query_one(
                "SELECT table_name FROM receiving.wamn_cdc_exclusions \
                  WHERE package_id = 'wamn_receiving' \
                    AND relation_id = 'record_receipt_command'",
                &[],
            )
            .await
            .expect("read converged CDC exclusion map")
            .get::<_, String>(0),
        "record_receipt_command"
    );

    assert_eq!(
        client
            .execute(
                "UPDATE receiving.wamn_cdc_exclusions \
                    SET package_id = 'foreign_package', relation_id = 'foreign_relation' \
                  WHERE package_id = 'wamn_receiving' \
                    AND relation_id = 'record_receipt_command'",
                &[],
            )
            .await
            .expect("seed an exclusion OID owned by another package relation"),
        1
    );
    assert_eq!(
        client
            .execute(
                &wamn_control_provision::sql::upsert_cdc_exclusion_map_sql("receiving"),
                &[
                    &"wamn_receiving",
                    &"record_receipt_command",
                    &"record_receipt_command",
                ],
            )
            .await
            .expect("run guarded generated CDC-exclusion upsert"),
        0,
        "the generated upsert must not rebind an existing relation OID"
    );
    let rebind = apply(&url, &package)
        .await
        .expect_err("an exclusion OID cannot be rebound to another package relation");
    assert!(format!("{rebind:#}").contains("package-cdc-exclusion-oid-rebind-refused"));
    let refused = client
        .query_one(
            "SELECT package_id, relation_id FROM receiving.wamn_cdc_exclusions \
              WHERE table_name = 'record_receipt_command'",
            &[],
        )
        .await
        .expect("read refused CDC-exclusion rebind");
    assert_eq!(refused.get::<_, String>(0), "foreign_package");
    assert_eq!(refused.get::<_, String>(1), "foreign_relation");
    assert_eq!(
        client
            .execute(
                "UPDATE receiving.wamn_cdc_exclusions \
                    SET package_id = 'wamn_receiving', \
                        relation_id = 'record_receipt_command' \
                  WHERE package_id = 'foreign_package' \
                    AND relation_id = 'foreign_relation'",
                &[],
            )
            .await
            .expect("restore CDC-exclusion fixture identity"),
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
        "ALTER TABLE receiving.receipt \
           ADD COLUMN rollback_probe text NOT NULL DEFAULT 'not_required';",
    )
    .expect("write the first pending migration");
    std::fs::write(
        package.join("migrations/0003_failure.sql"),
        "ALTER TABLE receiving.receipt \
           ADD COLUMN rollback_probe text NOT NULL DEFAULT 'not_required';",
    )
    .expect("write the server-refused migration after it");
    apply(&url, &package)
        .await
        .expect_err("a failing later statement must roll back the whole suffix");
    assert!(
        !client
            .query_one(
                "SELECT EXISTS ( \
                   SELECT 1 FROM information_schema.columns \
                    WHERE table_schema = 'receiving' AND table_name = 'receipt' \
                      AND column_name = 'rollback_probe')",
                &[],
            )
            .await
            .unwrap()
            .get::<_, bool>(0)
    );
    assert_eq!(
        client
            .query_one(
                "SELECT count(*) FROM catalog.package_migrations \
                 WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3",
                &[&TENANT, &"wamn_receiving", &"1.0.0"],
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
        "ALTER TABLE receiving.receipt \
           ADD COLUMN sealed_probe text NOT NULL DEFAULT 'not_required';",
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
                "SELECT EXISTS ( \
                   SELECT 1 FROM information_schema.columns \
                    WHERE table_schema = 'receiving' AND table_name = 'receipt' \
                      AND column_name = 'sealed_probe')",
                &[]
            )
            .await
            .unwrap()
            .get::<_, bool>(0),
        "the DDL before the sealed record write rolls back with the transaction"
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
    declare_ownership_only_model(&package, "after_seal", "after_seal");
    std::fs::write(
        package.join("migrations/0002_after_seal.sql"),
        "CREATE TABLE receiving.after_seal (id int);",
    )
    .expect("replace the refused suffix with the new version's declared model");
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
    assert!(
        client
            .query_one(
                "SELECT to_regclass('receiving.purchase_order') IS NOT NULL",
                &[],
            )
            .await
            .unwrap()
            .get::<_, bool>(0),
        "fresh cumulative apply creates the base relation"
    );
    assert!(
        client
            .query_one(
                "SELECT to_regclass('receiving.after_seal') IS NOT NULL",
                &[]
            )
            .await
            .unwrap()
            .get::<_, bool>(0),
        "fresh cumulative apply executes the suffix"
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

    client
        .batch_execute(
            "DROP SCHEMA IF EXISTS receiving CASCADE; \
             DROP SCHEMA IF EXISTS app_system CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             DROP SCHEMA IF EXISTS wamn_history CASCADE;",
        )
        .await
        .expect("clean package-runner schemas");
    std::fs::remove_dir_all(package).expect("remove package fixture directory");
    for fixture in [alter_base, drop_base, nonextensible, overlay] {
        std::fs::remove_dir_all(fixture).expect("remove overlay package fixture directory");
    }
}

/// Spec test 13: the declaration is the trigger.
#[tokio::test]
async fn record_history_triggers_follow_the_declaration() {
    let url = locked_database::database(wamn_test_postgres::database);
    let client = connect(&url).await;
    install(&client).await;
    let package = fixture_root().with_file_name(format!(
        "apply-package-record-history-{}",
        std::process::id()
    ));
    copy_receiving_package(&package);
    set_audit_log_columns(&package, "purchase_order", &["updated_at", "created_at"]);
    set_audit_log_columns(&package, "receipt", &[]);

    apply(&url, &package)
        .await
        .expect("apply a declaration that selects stamp columns");
    let installed = receiving_triggers(&client).await;
    assert_eq!(
        trigger_definitions(&installed),
        [
            "CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE \
             ON receiving.purchase_order FOR EACH ROW \
             EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'updated_at')",
            "CREATE TRIGGER wamn_record_history_log AFTER INSERT OR DELETE OR UPDATE \
             ON receiving.purchase_order_line FOR EACH ROW \
             EXECUTE FUNCTION wamn_history.log_row_change('P30D')",
        ],
        "one stamp trigger for the relation that selects columns, none for [], \
         and the declared log trigger"
    );
    // Spec test 11: the operation grants stamp wamn:apply-package.
    let grant_stamps = client
        .query_one(
            "SELECT count(*), count(*) FILTER (WHERE created_by = $2::text::uuid \
                                                AND updated_by = $2::text::uuid) \
               FROM (SELECT created_by, updated_by FROM app_system.roles WHERE tenant_id = $1 \
                     UNION ALL \
                     SELECT created_by, updated_by FROM app_system.permissions \
                      WHERE tenant_id = $1) AS grants",
            &[
                &TENANT,
                &PlatformComponent::ApplyPackage.principal_id().to_string(),
            ],
        )
        .await
        .expect("read the operation grant stamps");
    assert!(
        grant_stamps.get::<_, i64>(0) > 1
            && grant_stamps.get::<_, i64>(0) == grant_stamps.get::<_, i64>(1),
        "every operation grant row must stamp wamn:apply-package"
    );
    apply(&url, &package)
        .await
        .expect("an exact replay keeps the installed trigger");
    assert_eq!(receiving_triggers(&client).await, installed);

    set_package_identity(&package, "1.0.1", Some("1.0.0"));
    set_audit_log_columns(&package, "purchase_order", &[]);
    set_audit_log_columns(&package, "receipt", &["created_at"]);
    apply(&url, &package)
        .await
        .expect("an upgrade moves the trigger with its declaration");
    assert_eq!(
        trigger_definitions(&receiving_triggers(&client).await),
        [
            "CREATE TRIGGER wamn_record_history_log AFTER INSERT OR DELETE OR UPDATE \
             ON receiving.purchase_order_line FOR EACH ROW \
             EXECUTE FUNCTION wamn_history.log_row_change('P30D')",
            "CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE \
             ON receiving.receipt FOR EACH ROW \
             EXECUTE FUNCTION wamn_history.stamp_row('created_at')",
        ],
        "the trigger that the declaration no longer needs is removed"
    );
    let upgraded = receiving_triggers(&client).await;

    set_package_identity(&package, "1.0.2", Some("1.0.1"));
    std::fs::write(
        package.join("migrations/0002_trigger.sql"),
        "CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON receiving.item \
           FOR EACH ROW EXECUTE FUNCTION wamn_history.stamp_row('created_at');",
    )
    .expect("write a migration that carries a trigger");
    let refused = apply(&url, &package)
        .await
        .expect_err("a migration that carries a trigger still refuses");
    assert_eq!(
        refused
            .downcast_ref::<MigrationPolicyError>()
            .map(MigrationPolicyError::kind),
        Some(MigrationPolicyErrorKind::RuledOperation),
        "unexpected migration refusal: {refused:#}"
    );
    assert_eq!(receiving_triggers(&client).await, upgraded);
    std::fs::remove_file(package.join("migrations/0002_trigger.sql"))
        .expect("remove the refused trigger migration");

    set_package_identity(&package, "1.0.1", Some("1.0.0"));
    client
        .batch_execute(
            "CREATE FUNCTION public.record_history_foreign() RETURNS trigger \
               LANGUAGE plpgsql AS 'BEGIN RETURN NEW; END'; \
             CREATE TRIGGER foreign_trigger BEFORE INSERT ON receiving.item \
               FOR EACH ROW EXECUTE FUNCTION public.record_history_foreign();",
        )
        .await
        .expect("install a trigger outside the platform shape");
    let refused = apply(&url, &package)
        .await
        .expect_err("apply-package refuses a trigger that the declarations do not derive");
    assert!(
        format!("{refused:#}").contains("record-history-trigger-mismatch"),
        "unexpected trigger refusal: {refused:#}"
    );

    client
        .batch_execute(
            "DROP SCHEMA IF EXISTS receiving CASCADE; \
             DROP SCHEMA IF EXISTS app_system CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             DROP SCHEMA IF EXISTS wamn_history CASCADE; \
             DROP FUNCTION public.record_history_foreign();",
        )
        .await
        .expect("clean record-history schemas");
    std::fs::remove_dir_all(package).expect("remove record-history package fixture");
}

/// The log triggers in the receiving schema, as the server renders them.
async fn receiving_log_triggers(client: &Client) -> Vec<String> {
    receiving_triggers(client)
        .await
        .into_iter()
        .map(|(definition, _)| definition)
        .filter(|definition| definition.contains("wamn_record_history_log"))
        .collect()
}

/// Each history table in the receiving schema with its owner and entry count.
async fn receiving_history_tables(client: &Client) -> Vec<(String, String, i64)> {
    let tables = client
        .query(
            "SELECT c.relname::text, pg_catalog.pg_get_userbyid(c.relowner)::text \
               FROM pg_catalog.pg_class AS c \
               JOIN pg_catalog.pg_namespace AS n ON n.oid = c.relnamespace \
              WHERE n.nspname = 'receiving' AND c.relkind = 'r' \
                AND c.relname LIKE '%\\_history' \
              ORDER BY c.relname",
            &[],
        )
        .await
        .expect("read receiving history tables");
    let mut observed = Vec::new();
    for table in tables {
        let name = table.get::<_, String>(0);
        let entries = client
            .query_one(&format!("SELECT count(*) FROM receiving.{name}"), &[])
            .await
            .expect("count history entries")
            .get::<_, i64>(0);
        observed.push((name, table.get(1), entries));
    }
    observed
}

/// Every CDC exclusion row in the receiving schema, with whether its OID names its table.
async fn receiving_cdc_exclusions(client: &Client) -> Vec<(String, String, String, bool)> {
    client
        .query(
            "SELECT table_name, package_id, relation_id, \
                    relation_oid = pg_catalog.to_regclass('receiving.' || table_name)::oid \
               FROM receiving.wamn_cdc_exclusions ORDER BY table_name",
            &[],
        )
        .await
        .expect("read receiving CDC exclusions")
        .into_iter()
        .map(|row| (row.get(0), row.get(1), row.get(2), row.get(3)))
        .collect()
}

/// The direct grants of the audit retention role in the current database.
async fn audit_retention_grants(client: &Client) -> Vec<String> {
    client
        .query(
            wamn_control_provision::sql::role_database_grants_sql(),
            &[&wamn_control_provision::AUDIT_RETENTION_ROLE],
        )
        .await
        .expect("read the audit retention grants")
        .into_iter()
        .map(|row| {
            format!(
                "{} {}.{} {}",
                row.get::<_, String>("object_kind"),
                row.get::<_, String>("schema_name"),
                row.get::<_, String>("object_name"),
                row.get::<_, String>("privilege_type"),
            )
        })
        .collect()
}

/// The audit retention grants on the history table of one P<n>D relation.
fn retention_grants_on(history: &str) -> Vec<String> {
    vec![
        format!("column receiving.{history}.changed_at SELECT"),
        format!("column receiving.{history}.position SELECT"),
        format!("column receiving.{history}.row_key SELECT"),
        format!("relation receiving.{history} DELETE"),
        "schema receiving.receiving USAGE".to_owned(),
    ]
}

/// Level-2 spec tests 13 and 14: the declaration derives the history table,
/// the log trigger with its retention, and the CDC exclusion. apply-package
/// grants the audit retention role its privileges only on the history table of
/// a P<n>D relation, and revokes them when the retention changes.
#[tokio::test]
async fn record_history_log_follows_the_declaration() {
    let url = locked_database::database(wamn_test_postgres::database);
    let client = connect(&url).await;
    install(&client).await;
    let package = fixture_root().with_file_name(format!(
        "apply-package-record-history-log-{}",
        std::process::id()
    ));
    copy_receiving_package(&package);
    set_audit_log_retention(&package, "purchase_order", "unlimited");
    set_audit_log_retention(&package, "purchase_order_line", "P30D");

    apply(&url, &package)
        .await
        .expect("apply declarations that keep a log");
    assert_eq!(
        receiving_log_triggers(&client).await,
        [
            "CREATE TRIGGER wamn_record_history_log AFTER INSERT OR DELETE OR UPDATE \
             ON receiving.purchase_order FOR EACH ROW \
             EXECUTE FUNCTION wamn_history.log_row_change('unlimited')",
            "CREATE TRIGGER wamn_record_history_log AFTER INSERT OR DELETE OR UPDATE \
             ON receiving.purchase_order_line FOR EACH ROW \
             EXECUTE FUNCTION wamn_history.log_row_change('P30D')",
        ]
    );
    let owner = wamn_control_provision::DB_OWNER_ROLE.to_owned();
    assert_eq!(
        receiving_history_tables(&client).await,
        [
            ("purchase_order_history".to_owned(), owner.clone(), 0),
            ("purchase_order_line_history".to_owned(), owner.clone(), 0),
        ]
    );
    let exclusions = [
        (
            "purchase_order_history".to_owned(),
            "wamn_receiving".to_owned(),
            "purchase_order_history".to_owned(),
            true,
        ),
        (
            "purchase_order_line_history".to_owned(),
            "wamn_receiving".to_owned(),
            "purchase_order_line_history".to_owned(),
            true,
        ),
        (
            "record_receipt_command".to_owned(),
            "wamn_receiving".to_owned(),
            "record_receipt_command".to_owned(),
            true,
        ),
    ];
    assert_eq!(receiving_cdc_exclusions(&client).await, exclusions);
    // Spec test 20: the unlimited history table carries no retention grant.
    assert_eq!(
        audit_retention_grants(&client).await,
        retention_grants_on("purchase_order_line_history")
    );

    // A fixture write binds a test principal and an administrative operation.
    client
        .batch_execute(&format!(
            "BEGIN; \
             SELECT set_config('app.user_id', '{FIXTURE_PRINCIPAL}', true), \
                    set_config('app.operation', 'admin:seed-history-fixture', true); \
             INSERT INTO receiving.item (id, item_number) \
               VALUES ('00000000-0000-4000-8000-00000000a001', 'history-item'); \
             INSERT INTO receiving.purchase_order (id, purchase_order_number, supplier_id) \
               VALUES ('00000000-0000-4000-8000-00000000a002', 'history-po', gen_random_uuid()); \
             INSERT INTO receiving.purchase_order_line \
               (id, purchase_order_id, line_number, item_id, ordered_quantity) \
               VALUES ('00000000-0000-4000-8000-00000000a003', \
                       '00000000-0000-4000-8000-00000000a002', 1, \
                       '00000000-0000-4000-8000-00000000a001', 5); \
             COMMIT;"
        ))
        .await
        .expect("write logged rows as the fixture principal");
    assert_eq!(
        receiving_history_tables(&client).await,
        [
            ("purchase_order_history".to_owned(), owner.clone(), 1),
            ("purchase_order_line_history".to_owned(), owner.clone(), 1),
        ]
    );

    let installed = receiving_triggers(&client).await;
    apply(&url, &package)
        .await
        .expect("an exact replay keeps the log triggers and the history tables");
    assert_eq!(receiving_triggers(&client).await, installed);

    // Spec test 14: a retention of none removes the trigger and keeps the table.
    set_package_identity(&package, "1.0.1", Some("1.0.0"));
    set_audit_log_retention(&package, "purchase_order", "P90D");
    set_audit_log_retention(&package, "purchase_order_line", "none");
    apply(&url, &package)
        .await
        .expect("an upgrade moves the log triggers with the declarations");
    assert_eq!(
        receiving_log_triggers(&client).await,
        [
            "CREATE TRIGGER wamn_record_history_log AFTER INSERT OR DELETE OR UPDATE \
          ON receiving.purchase_order FOR EACH ROW \
          EXECUTE FUNCTION wamn_history.log_row_change('P90D')"
        ]
    );
    client
        .batch_execute(&format!(
            "BEGIN; \
             SELECT set_config('app.user_id', '{FIXTURE_PRINCIPAL}', true), \
                    set_config('app.operation', 'admin:seed-history-fixture', true); \
             UPDATE receiving.purchase_order_line SET received_quantity = 1 \
              WHERE id = '00000000-0000-4000-8000-00000000a003'; \
             COMMIT;"
        ))
        .await
        .expect("write a relation that no longer keeps a log");
    assert_eq!(
        receiving_history_tables(&client).await,
        [
            ("purchase_order_history".to_owned(), owner.clone(), 1),
            ("purchase_order_line_history".to_owned(), owner.clone(), 1),
        ],
        "the history table and its entries stay, and the relation writes no new entry"
    );
    assert_eq!(receiving_cdc_exclusions(&client).await, exclusions);
    // unlimited to P90D grants, and P30D to none revokes.
    assert_eq!(
        audit_retention_grants(&client).await,
        retention_grants_on("purchase_order_history")
    );

    // apply-package compares the installed retention with the declaration.
    client
        .batch_execute(
            "CREATE OR REPLACE TRIGGER wamn_record_history_log \
               AFTER INSERT OR UPDATE OR DELETE ON receiving.purchase_order \
               FOR EACH ROW EXECUTE FUNCTION wamn_history.log_row_change('P1D')",
        )
        .await
        .expect("change the installed retention outside apply-package");
    apply(&url, &package)
        .await
        .expect("a replay repairs the installed retention");
    assert_eq!(
        receiving_log_triggers(&client).await,
        [
            "CREATE TRIGGER wamn_record_history_log AFTER INSERT OR DELETE OR UPDATE \
          ON receiving.purchase_order FOR EACH ROW \
          EXECUTE FUNCTION wamn_history.log_row_change('P90D')"
        ]
    );

    // A grant outside the exact set does not survive a replay.
    client
        .batch_execute(
            "GRANT SELECT ON receiving.purchase_order_line_history TO wamn_audit_retention; \
             GRANT UPDATE (after) ON receiving.purchase_order_history TO wamn_audit_retention;",
        )
        .await
        .expect("widen the audit retention grants outside apply-package");
    apply(&url, &package)
        .await
        .expect("a replay repairs the audit retention grants");
    assert_eq!(
        audit_retention_grants(&client).await,
        retention_grants_on("purchase_order_history")
    );

    // P90D to unlimited revokes.
    set_package_identity(&package, "1.0.2", Some("1.0.1"));
    set_audit_log_retention(&package, "purchase_order", "unlimited");
    apply(&url, &package)
        .await
        .expect("an upgrade to unlimited retention revokes the retention grants");
    assert_eq!(audit_retention_grants(&client).await, Vec::<String>::new());

    // A migration cannot create a table with the reserved history suffix.
    set_package_identity(&package, "1.0.3", Some("1.0.2"));
    std::fs::write(
        package.join("migrations/0002_history.sql"),
        "CREATE TABLE receiving.receipt_history (\
           id uuid CONSTRAINT receipt_history_id_pkey PRIMARY KEY);",
    )
    .expect("write a migration that creates a history name");
    let refused = apply(&url, &package)
        .await
        .expect_err("a migration that creates a history name refuses");
    assert!(
        format!("{refused:#}").contains("history-table-name-reserved"),
        "unexpected history name refusal: {refused:#}"
    );
    assert!(
        client
            .query_one(
                "SELECT pg_catalog.to_regclass('receiving.receipt_history') IS NULL",
                &[],
            )
            .await
            .expect("read the refused history name")
            .get::<_, bool>(0),
        "the refused migration left no history name"
    );

    client
        .batch_execute(
            "DROP SCHEMA IF EXISTS receiving CASCADE; \
             DROP SCHEMA IF EXISTS app_system CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             DROP SCHEMA IF EXISTS wamn_history CASCADE;",
        )
        .await
        .expect("clean record-history log schemas");
    std::fs::remove_dir_all(package).expect("remove record-history log package fixture");
}

/// The full comment of the connected database.
async fn database_comment(client: &Client) -> Option<String> {
    client
        .query_one(
            "SELECT pg_catalog.shobj_description(oid, 'pg_database') \
               FROM pg_catalog.pg_database WHERE datname = pg_catalog.current_database()",
            &[],
        )
        .await
        .expect("read the database comment")
        .get(0)
}

async fn set_database_comment(client: &Client, comment: Option<&str>) {
    let comment = comment.map_or_else(|| "NULL".to_owned(), wamn_pg_core::quote_literal);
    client
        .batch_execute(&format!(
            "DO $comment$ BEGIN \
               EXECUTE format('COMMENT ON DATABASE %I IS %L', current_database(), {comment}::text); \
             END $comment$;"
        ))
        .await
        .expect("write the database comment");
}

fn manifest_sha256(package: &Path) -> String {
    let directory =
        apply_package::read_package_directory(package).expect("read local target package");
    wamn_schema_control::plan_package_migrations(&directory, None)
        .expect("plan local target package")
        .manifest_sha256
}

fn package_migration_error(
    error: &anyhow::Error,
) -> wamn_schema_control::PackageMigrationErrorKind {
    error
        .downcast_ref::<wamn_schema_control::PackageMigrationError>()
        .unwrap_or_else(|| panic!("not a package migration refusal: {error:#}"))
        .kind()
}

async fn receiving_column_present(client: &Client, table: &str, column: &str) -> bool {
    client
        .query_one(
            "SELECT EXISTS ( \
               SELECT 1 FROM information_schema.columns \
                WHERE table_schema = 'receiving' AND table_name = $1 AND column_name = $2)",
            &[&table, &column],
        )
        .await
        .expect("read receiving column")
        .get(0)
}

/// Insert one migration record for the sealed coordinate in its own transaction.
async fn record_after_seal(
    client: &Client,
    setting: Option<&str>,
) -> Result<(), tokio_postgres::Error> {
    client.batch_execute("BEGIN").await?;
    let result = async {
        client
            .query_one("SELECT set_config('app.tenant', $1, true)", &[&TENANT])
            .await?;
        if let Some(setting) = setting {
            client
                .query_one(
                    "SELECT set_config('wamn.local_target_comment', $1, true)",
                    &[&setting],
                )
                .await?;
        }
        client
            .execute(
                "INSERT INTO catalog.package_migrations \
                     (tenant_id, package_id, package_version, ordinal, relative_path, sha256) \
                 VALUES ($1, 'wamn_receiving', '1.0.0', 99, 'migrations/0099_seal_probe.sql', $2)",
                &[&TENANT, &format!("sha256:{}", "0".repeat(64))],
            )
            .await
            .map(|_| ())
    }
    .await;
    client
        .batch_execute("ROLLBACK")
        .await
        .expect("roll back the seal probe");
    result
}

fn is_sealed(error: &tokio_postgres::Error) -> bool {
    error
        .as_db_error()
        .is_some_and(|database| database.message() == apply_package::PACKAGE_VERSION_SEALED_REFUSAL)
}

/// Owner rulings 1, 2, and 4 of wamn-ri4b: only a marked local target takes a
/// changed wamn.json and an appended migration at an applied coordinate, also
/// after release membership, and its comment records the current manifest hash.
#[tokio::test]
async fn a_local_target_takes_an_appended_migration_and_a_changed_manifest() {
    const ENVIRONMENT: &str = "development";
    let url = locked_database::database(wamn_test_postgres::database);
    let client = connect(&url).await;
    install(&client).await;
    let package =
        fixture_root().with_file_name(format!("apply-package-local-target-{}", std::process::id()));
    copy_receiving_package(&package);
    let instance: u32 = client
        .query_one(
            "SELECT oid FROM pg_catalog.pg_database WHERE datname = pg_catalog.current_database()",
            &[],
        )
        .await
        .expect("read the database instance")
        .get(0);
    let marker =
        wamn_runtime::local_application::local_target_marker(TENANT, ENVIRONMENT, instance);
    set_database_comment(&client, Some(&marker)).await;
    let apply_local = || {
        apply_package::apply_local_package(
            ApplyPackageRequest {
                package: package.clone(),
                database_url: url.to_string(),
                tenant: TENANT.to_owned(),
            },
            ENVIRONMENT,
        )
    };
    let recorded_comment = |hash: &str| {
        let mut comment = wamn_runtime::local_application::parse_local_target_comment(&marker)
            .expect("parse the marker");
        comment
            .manifests
            .insert("wamn_receiving@1.0.0".to_owned(), hash.to_owned());
        comment.to_string()
    };

    apply_local()
        .await
        .expect("apply the package to the local target");
    let first = manifest_sha256(&package);
    assert_eq!(
        database_comment(&client).await,
        Some(recorded_comment(&first))
    );

    // Before release membership: an audit_log change and an appended column.
    set_audit_log_retention(&package, "location", "unlimited");
    std::fs::write(
        package.join("migrations/0002_location_description.sql"),
        "ALTER TABLE receiving.location ADD COLUMN description text NOT NULL DEFAULT 'not_required';",
    )
    .expect("append the column migration");
    let refused = apply(&url, &package)
        .await
        .expect_err("plain apply-package refuses the changed manifest on a marked target");
    assert_eq!(
        package_migration_error(&refused),
        wamn_schema_control::PackageMigrationErrorKind::ManifestDrift
    );
    let outcome = apply_local()
        .await
        .expect("the local target takes the appended migration and the changed manifest");
    assert_eq!(outcome.migrations_applied, 1);
    let second = manifest_sha256(&package);
    assert_ne!(second, first);
    assert_eq!(
        database_comment(&client).await,
        Some(recorded_comment(&second))
    );
    assert!(receiving_column_present(&client, "location", "description").await);
    assert!(
        client
            .query_one(
                "SELECT pg_catalog.to_regclass('receiving.location_history') IS NOT NULL",
                &[],
            )
            .await
            .expect("read the reconciled history table")
            .get::<_, bool>(0),
        "the local apply reconciles the changed audit_log"
    );
    assert_eq!(
        client
            .query_one(
                "SELECT manifest_sha256 FROM catalog.packages \
                  WHERE tenant_id = $1 AND package_id = 'wamn_receiving' \
                    AND package_version = '1.0.0'",
                &[&TENANT],
            )
            .await
            .expect("read the immutable package row")
            .get::<_, String>(0),
        first,
        "catalog.packages keeps the first recorded manifest hash"
    );

    client
        .execute(
            "INSERT INTO catalog.effective_releases \
                 (tenant_id, effective_release_id, environment) \
             VALUES ($1, 1, $2)",
            &[&TENANT, &ENVIRONMENT],
        )
        .await
        .expect("seed a local effective release");
    client
        .execute(
            "INSERT INTO catalog.effective_release_packages \
                 (tenant_id, effective_release_id, package_id, package_version) \
             VALUES ($1, 1, 'wamn_receiving', '1.0.0')",
            &[&TENANT],
        )
        .await
        .expect("seal the coordinate through local release membership");

    // A marked target without the setting keeps the seal, and a setting that
    // holds only the marker, a prefix of the comment, does not lift it.
    let comment = database_comment(&client)
        .await
        .expect("the target is marked");
    let unset = record_after_seal(&client, None).await.expect_err("sealed");
    assert!(is_sealed(&unset), "{unset}");
    let prefix = record_after_seal(&client, Some(&marker))
        .await
        .expect_err("sealed");
    assert!(is_sealed(&prefix), "{prefix}");
    record_after_seal(&client, Some(&comment))
        .await
        .expect("the full comment lifts the seal for its transaction");
    let first_bytes_package = package.with_file_name(format!(
        "apply-package-local-target-first-{}",
        std::process::id()
    ));
    copy_receiving_package(&first_bytes_package);
    std::fs::copy(
        package.join("migrations/0002_location_description.sql"),
        first_bytes_package.join("migrations/0002_location_description.sql"),
    )
    .expect("copy the applied column migration");
    std::fs::write(
        first_bytes_package.join("migrations/0003_location_note.sql"),
        "ALTER TABLE receiving.location ADD COLUMN note text NOT NULL DEFAULT 'not_required';",
    )
    .expect("append a migration after the seal");
    let sealed = apply(&url, &first_bytes_package)
        .await
        .expect_err("plain apply-package keeps the seal on a marked target");
    assert_eq!(
        sealed
            .downcast_ref::<apply_package::ApplyPackageError>()
            .expect("the seal is translated at the apply-package boundary")
            .kind(),
        apply_package::ApplyPackageErrorKind::PackageVersionSealed
    );
    std::fs::remove_dir_all(&first_bytes_package).expect("remove first-bytes fixture");

    // After release membership: another appended migration and manifest change.
    std::fs::write(
        package.join("migrations/0003_location_note.sql"),
        "ALTER TABLE receiving.location ADD COLUMN note text NOT NULL DEFAULT 'not_required';",
    )
    .expect("append a migration after the seal");
    let mut changed = std::fs::read(package.join("wamn.json")).expect("read manifest");
    changed.push(b'\n');
    std::fs::write(package.join("wamn.json"), changed).expect("change manifest bytes");
    apply_local()
        .await
        .expect("the local target records an appended migration after the seal");
    let third = manifest_sha256(&package);
    assert_eq!(
        database_comment(&client).await,
        Some(recorded_comment(&third))
    );
    assert!(receiving_column_present(&client, "location", "note").await);

    // An edited or removed applied migration still refuses.
    let edited_path = package.join("migrations/0002_location_description.sql");
    let edited_bytes = std::fs::read(&edited_path).expect("read applied migration");
    std::fs::write(
        &edited_path,
        b"ALTER TABLE receiving.location ADD COLUMN detail text NOT NULL DEFAULT 'not_required';",
    )
    .expect("edit an applied migration");
    let edited = apply_local()
        .await
        .expect_err("an edited applied migration refuses on a marked target");
    assert_eq!(
        package_migration_error(&edited),
        wamn_schema_control::PackageMigrationErrorKind::MigrationDrift
    );
    std::fs::write(&edited_path, edited_bytes).expect("restore the edited migration");
    let removed_path = package.join("migrations/0003_location_note.sql");
    let removed_bytes = std::fs::read(&removed_path).expect("read applied migration");
    std::fs::remove_file(&removed_path).expect("remove an applied migration");
    let removed = apply_local()
        .await
        .expect_err("a removed applied migration refuses on a marked target");
    assert_eq!(
        package_migration_error(&removed),
        wamn_schema_control::PackageMigrationErrorKind::MigrationDrift
    );
    std::fs::write(&removed_path, removed_bytes).expect("restore the removed migration");
    assert_eq!(
        database_comment(&client).await,
        Some(recorded_comment(&third))
    );

    // A comment with only the marker prefix, or no comment, is not a local target.
    std::fs::write(
        package.join("migrations/0004_location_code.sql"),
        "ALTER TABLE receiving.location ADD COLUMN code text NOT NULL DEFAULT 'not_required';",
    )
    .expect("append a migration for the unmarked cases");
    for comment in [Some("wamn-local-target:"), None] {
        set_database_comment(&client, comment).await;
        let unset = record_after_seal(&client, None).await.expect_err("sealed");
        assert!(is_sealed(&unset), "{unset}");
        apply_local()
            .await
            .expect_err("the local apply requires the full marker");
        let unmarked = apply(&url, &package)
            .await
            .expect_err("an unmarked target refuses the changed manifest");
        assert_eq!(
            package_migration_error(&unmarked),
            wamn_schema_control::PackageMigrationErrorKind::ManifestDrift
        );
    }
    assert!(!receiving_column_present(&client, "location", "code").await);

    client
        .batch_execute(
            "DROP SCHEMA IF EXISTS receiving CASCADE; \
             DROP SCHEMA IF EXISTS app_system CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             DROP SCHEMA IF EXISTS wamn_history CASCADE;",
        )
        .await
        .expect("clean local target schemas");
    std::fs::remove_dir_all(package).expect("remove local target package fixture");
}

/// Owner rulings 3 and 6 of wamn-ri4b: a kept local target is recreated only
/// for a changed applied migration or a history table with rows whose model no
/// longer keeps a log.
#[tokio::test]
async fn a_local_target_recreates_for_a_changed_migration_or_a_history_table_with_rows() {
    let url = locked_database::database(wamn_test_postgres::database);
    let client = connect(&url).await;
    install(&client).await;
    let package = fixture_root().with_file_name(format!(
        "apply-package-local-target-reuse-{}",
        std::process::id()
    ));
    copy_receiving_package(&package);
    set_audit_log_retention(&package, "item", "unlimited");
    set_audit_log_retention(&package, "location", "unlimited");
    apply(&url, &package)
        .await
        .expect("apply declarations that keep a log");
    let reason = || {
        apply_package::local_target_recreate_reason(&url, TENANT, std::slice::from_ref(&package))
    };
    assert_eq!(reason().await.expect("check the applied package"), None);

    client
        .batch_execute(&format!(
            "BEGIN; \
             SELECT set_config('app.user_id', '{FIXTURE_PRINCIPAL}', true), \
                    set_config('app.operation', 'admin:seed-history-fixture', true); \
             INSERT INTO receiving.location (id, location_code) \
               VALUES ('00000000-0000-4000-8000-00000000b001', 'DOCK-1'); \
             COMMIT;"
        ))
        .await
        .expect("write a logged location row as the fixture principal");
    set_audit_log_retention(&package, "item", "none");
    assert_eq!(
        reason().await.expect("check an empty history table"),
        None,
        "an empty history table whose model keeps no log keeps the target"
    );
    set_audit_log_retention(&package, "location", "none");
    let history = reason()
        .await
        .expect("check a history table with rows")
        .expect("a history table with rows whose model keeps no log recreates the target");
    assert!(history.contains("receiving.location_history"), "{history}");
    set_audit_log_retention(&package, "location", "unlimited");

    std::fs::write(
        package.join("migrations/0002_location_note.sql"),
        "ALTER TABLE receiving.location ADD COLUMN note text NOT NULL DEFAULT 'not_required';",
    )
    .expect("append a migration");
    assert_eq!(
        reason().await.expect("check an appended migration"),
        None,
        "an appended migration keeps the target"
    );
    let migration = package.join("migrations/0001_initial.sql");
    let mut edited = std::fs::read(&migration).expect("read the applied migration");
    edited.extend_from_slice(b"\n");
    std::fs::write(&migration, edited).expect("edit the applied migration");
    let drift = reason()
        .await
        .expect("check an edited migration")
        .expect("an edited applied migration recreates the target");
    assert!(
        drift.contains(wamn_schema_control::PACKAGE_MIGRATION_DRIFT_REFUSAL),
        "{drift}"
    );

    client
        .batch_execute(
            "DROP SCHEMA IF EXISTS receiving CASCADE; \
             DROP SCHEMA IF EXISTS app_system CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             DROP SCHEMA IF EXISTS wamn_history CASCADE;",
        )
        .await
        .expect("clean local target reuse schemas");
    std::fs::remove_dir_all(package).expect("remove local target reuse package fixture");
}
