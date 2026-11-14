//! Apply refusals against project integrations and environment connections.

use serde_json::json;
use sqlx::{Connection as _, Row as _};
use wamn_broker::auth_changes::AuthDrops;
use wamn_compiler::manifest::PackageManifest;
use wamn_environment_control::environment::{analyse, bindings, document::EnvironmentDocument};
use wamn_platform_identity::connections::administration;

async fn connect(url: &str) -> sqlx::PgConnection {
    let client = wamn_postgres::connect(url).await.unwrap();

    client
}
fn document() -> EnvironmentDocument {
    EnvironmentDocument::from_json(&serde_json::to_vec(&json!({
        "org":"acme","project":"one","env":"dev","target":"cloud","policy":"dev","revocation_seconds":60,
        "integrations":{"erp":{"version":1}},
        "applications": {"app":{"current": format!("sha256:{}","a".repeat(64)), "revocation_seconds":5,"hostname": "one.example", "packages":["fixture"],"connections":{"upstream":"erp"}}}})).unwrap()).unwrap()
}
fn package(integration_type: &str) -> PackageManifest {
    let interface = match integration_type {
        "http" => "wamn:connection/http@0.1.0",
        "gcs" | "s3" => "wasmcloud:blobstore/blobstore@0.1.0",
        _ => "wamn:modbus/read@1.0.0",
    };
    PackageManifest::from_slice(&serde_json::to_vec(&json!({
        "package":{"id":"fixture","version":"1"},"required_platform_policy_contract":{"id":"fixture_access","state":"satisfied"},
        "models":{},"components":{},"connections":{"upstream":{"interface":interface,"mode":"write"}}
    })).unwrap()).unwrap()
}
async fn seed(client: &mut sqlx::PgConnection) {
    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut *client)
        .await
        .map(drop)
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::test_database::seed_org_project_env_sql("acme", "one", "dev"))
            .to_string(),
    ))
    .execute(&mut *client)
    .await
    .map(drop)
    .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("BEGIN; {} INSERT INTO registry.target(name,type,status) VALUES('cloud','cloud','active') ON CONFLICT DO NOTHING; UPDATE registry.meta SET platform_domain='example.invalid'; UPDATE registry.environments SET target_id=(SELECT id FROM registry.target WHERE name='cloud'); COMMIT;",wamn_control_provision::bind_platform_principal_sql(wamn_control_provision::PlatformComponent::Provisioning)))).execute(&mut *client).await.map(drop).unwrap();
    let mut tx = client.begin().await.unwrap();
    sqlx::raw_sql("SET LOCAL ROLE wamn_control")
        .execute(&mut *tx)
        .await
        .map(drop)
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
            .to_string(),
    ))
    .execute(&mut *tx)
    .await
    .map(drop)
    .unwrap();
    let store = wamn_test_infrastructure::credentials::MemoryStore::default();
    wamn_test_infrastructure::credentials::define(&mut tx,"acme",&json!({"project":"one","env":"dev","name":"erp","integration_type":"http","auth_type":"none","parameters":{"primary-authority":"https://erp.example/","tls-verification":"verify-authority"}}),&store).await.unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn apply_refuses_invalid_names_interfaces_environments_and_versions() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut client = connect(database.url()).await;
    seed(&mut client).await;
    sqlx::raw_sql("SET ROLE wamn_db_owner")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    let mut doc = document();
    let packages = [package("http")];
    // Neither an unreferenced integration nor another application's connection is a prerequisite.
    doc.integrations.insert(
        "unavailable".into(),
        wamn_environment_control::environment::document::Integration { version: 1 },
    );
    let mut unrelated = doc.applications["app"].clone();
    unrelated.packages = std::collections::BTreeSet::from(["unrelated".into()]);
    unrelated
        .connections
        .insert("upstream".into(), "unavailable".into());
    doc.applications.insert("other".into(), unrelated);
    let selected = bindings::analyse(&mut client, &doc, &packages)
        .await
        .unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].version, 1);
    assert_eq!(
        bindings::analyse(&mut client, &doc, &[package("gcs")])
            .await
            .unwrap_err()
            .code,
        "integration_import_unsatisfied"
    );
    doc.applications
        .get_mut("app")
        .unwrap()
        .connections
        .insert("other".into(), "erp".into());
    assert_eq!(
        bindings::analyse(&mut client, &doc, &packages)
            .await
            .unwrap_err()
            .code,
        "connection_undeclared"
    );
    doc.applications.get_mut("app").unwrap().connections.clear();
    assert_eq!(
        bindings::analyse(&mut client, &doc, &packages)
            .await
            .unwrap_err()
            .code,
        "connection_unbound"
    );
    doc = document();
    doc.integrations.clear();
    assert_eq!(
        bindings::analyse(&mut client, &doc, &packages)
            .await
            .unwrap_err()
            .code,
        "integration_version_missing"
    );
    doc = document();
    doc.integrations.get_mut("erp").unwrap().version = 99;
    assert_eq!(
        bindings::analyse(&mut client, &doc, &packages)
            .await
            .unwrap_err()
            .code,
        "integration_version_unpublished"
    );
    doc = document();
    doc.env = "other".into();
    assert_eq!(
        bindings::analyse(&mut client, &doc, &packages)
            .await
            .unwrap_err()
            .code,
        "connection_unavailable"
    );
    doc = document();
    doc.target = "other".into();
    assert_eq!(
        bindings::analyse(&mut client, &doc, &packages)
            .await
            .unwrap_err()
            .code,
        "connection_unavailable"
    );
    sqlx::raw_sql("SET ROLE wamn_system; INSERT INTO registry.integration_type_version(integration_type,version,parameter_schema,operations,exports,published_at) SELECT integration_type,2,parameter_schema,operations,exports,now() FROM registry.integration_type_version WHERE integration_type='http' AND version=1").execute(&mut client).await.map(drop).unwrap();
    sqlx::raw_sql("SET ROLE wamn_db_owner")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    doc = document();
    doc.integrations.get_mut("erp").unwrap().version = 2;
    assert_eq!(
        bindings::analyse(&mut client, &doc, &packages)
            .await
            .unwrap_err()
            .code,
        "integration_version_unserved"
    );
    let mut tx = client.begin().await.unwrap();
    sqlx::raw_sql("SET LOCAL ROLE wamn_control")
        .execute(&mut *tx)
        .await
        .map(drop)
        .unwrap();
    administration::execute(
        &mut tx,
        "acme",
        "connection.disable",
        &json!({"project":"one","env":"dev","name":"erp"}).to_string(),
        &mut AuthDrops::default(),
        &mut wamn_broker::connection_changes::ConnectionDrops::default(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        bindings::analyse(&mut client, &document(), &packages)
            .await
            .unwrap_err()
            .code,
        "connection_unavailable"
    );
}

#[tokio::test]
async fn two_packages_can_bind_distinct_names_to_one_connection_without_authority_writes() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut client = connect(database.url()).await;
    seed(&mut client).await;
    let mut doc = document();
    doc.applications
        .get_mut("app")
        .unwrap()
        .packages
        .insert("other".into());
    doc.applications
        .get_mut("app")
        .unwrap()
        .connections
        .insert("second".into(), "erp".into());
    let first = package("http");
    let mut second = first.clone();
    second.package.id = "other".into();
    let declaration = second.connections.remove("upstream").unwrap();
    second.connections.insert("second".into(), declaration);
    sqlx::raw_sql("SET ROLE wamn_db_owner")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    let selected = bindings::analyse(&mut client, &doc, &[first, second])
        .await
        .unwrap();
    assert_eq!(selected.len(), 2);
    assert_eq!(selected[0].connection_id, selected[1].connection_id);
    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    assert_eq!(
        sqlx::query("SELECT count(*) FROM registry.connection_generation")
            .fetch_one(&mut client)
            .await
            .unwrap()
            .get::<i64, _>(0),
        1
    );
}

#[tokio::test]
async fn a_version_change_refuses_stored_values_that_do_not_validate() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut client = connect(database.url()).await;
    seed(&mut client).await;
    let mut tx = client.begin().await.unwrap();
    sqlx::raw_sql("SET LOCAL ROLE wamn_control")
        .execute(&mut *tx)
        .await
        .map(drop)
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
            .to_string(),
    ))
    .execute(&mut *tx)
    .await
    .map(drop)
    .unwrap();
    let store = wamn_test_infrastructure::credentials::MemoryStore::default();
    wamn_test_infrastructure::credentials::define(&mut tx,"acme",&json!({"project":"one","env":"dev","name":"meter","integration_type":"modbus","auth_type":"none","parameters":{"authority":"127.0.0.1:502"}}),&store).await.unwrap();
    tx.commit().await.unwrap();
    sqlx::raw_sql("SET ROLE wamn_system; INSERT INTO registry.integration_type_version(integration_type,version,digest,parameter_schema,operations,exports,write_interfaces,published_at) SELECT integration_type,2,digest,'{\"type\":\"object\",\"properties\":{\"authority\":{\"type\":\"string\"},\"unit\":{\"type\":\"integer\"}},\"required\":[\"authority\",\"unit\"]}',operations,exports,write_interfaces,now() FROM registry.integration_type_version WHERE integration_type='modbus' AND version=1; SET ROLE wamn_db_owner").execute(&mut client).await.map(drop).unwrap();
    let mut doc = document();
    doc.integrations.clear();
    doc.integrations.insert(
        "meter".into(),
        wamn_environment_control::environment::document::Integration { version: 2 },
    );
    doc.applications
        .get_mut("app")
        .unwrap()
        .connections
        .insert("upstream".into(), "meter".into());
    assert_eq!(
        bindings::analyse(&mut client, &doc, &[package("modbus")])
            .await
            .unwrap_err()
            .code,
        "connection_parameters_invalid"
    );
}

#[tokio::test]
async fn administration_refuses_values_incompatible_with_a_retained_release_version() {
    let _lock = wamn_test_postgres::lock();
    let system_database = wamn_control_provision::test_database::system();
    let mut system = connect(system_database.url()).await;
    seed(&mut system).await;
    sqlx::raw_sql("INSERT INTO registry.integration_type_version(integration_type,version,parameter_schema,operations,exports,published_at) SELECT integration_type,2,jsonb_set(parameter_schema,'{required}',(parameter_schema->'required') || '\"unit\"'::jsonb) || jsonb_build_object('properties',(parameter_schema->'properties') || '{\"unit\":{\"type\":\"integer\"}}'::jsonb),operations,exports,now() FROM registry.integration_type_version WHERE integration_type='http' AND version=1").execute(&mut system).await.map(drop).unwrap();
    let environment_database = wamn_platform_identity::test_database::project_platform();
    let mut environment = connect(environment_database.url()).await;
    sqlx::raw_sql("CREATE SCHEMA IF NOT EXISTS wamn_run")
        .execute(&mut environment)
        .await
        .map(drop)
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::sql::grant_administration_surface_sql("wamn_run")).to_string(),
    ))
    .execute(&mut environment)
    .await
    .map(drop)
    .unwrap();
    let document = document();
    let _ =
        bindings::project_authority(system_database.url(), environment_database.url(), &document)
            .await
            .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "BEGIN; {}",
            wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning
            )
        ))
            .to_string(),
    ))
    .execute(&mut environment)
    .await
    .map(drop)
    .unwrap();
    let connection: String = sqlx::query("SELECT id::text FROM platform.connection")
        .fetch_one(&mut environment)
        .await
        .unwrap()
        .get(0);
    // The sha256 of the canonical bytes `{}`, which catalog.releases tests.
    let digest = "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a";
    sqlx::query("INSERT INTO catalog.releases(project_env_key,manifest_digest,canonical_bytes) VALUES('acme--dev--one',$1,$2)").bind(&digest).bind(&b"{}".as_slice()).execute(&mut environment).await.map(|result| result.rows_affected()).unwrap();
    sqlx::query("INSERT INTO catalog.release_connection(project_env_key,manifest_digest,connection_id,version) VALUES('acme--dev--one',$1,$2::text::uuid,2)").bind(&digest).bind(&connection).execute(&mut environment).await.map(|result| result.rows_affected()).unwrap();
    sqlx::raw_sql("COMMIT")
        .execute(&mut environment)
        .await
        .map(drop)
        .unwrap();
    let request = json!({"project":"one","env":"dev","name":"erp","parameters":{"primary-authority":"https://erp.example/","tls-verification":"disabled"}}).to_string();
    let mut source = system.begin().await.unwrap();
    sqlx::raw_sql("SET LOCAL ROLE wamn_control")
        .execute(&mut *source)
        .await
        .map(drop)
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
            .to_string(),
    ))
    .execute(&mut *source)
    .await
    .map(drop)
    .unwrap();
    let mut target = environment.begin().await.unwrap();
    let error = administration::execute_in_environment(
        &mut source,
        &mut target,
        "acme",
        "connection.write",
        &request,
        &mut AuthDrops::default(),
        &mut wamn_broker::connection_changes::ConnectionDrops::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "invalid_input");
    source.rollback().await.unwrap();
    target.rollback().await.unwrap();
    assert_eq!(
        sqlx::query("SELECT active_generation FROM registry.connection WHERE id=$1::text::uuid")
            .bind(&connection)
            .fetch_one(&mut system)
            .await
            .unwrap()
            .get::<i64, _>(0),
        1
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "BEGIN; {} DELETE FROM catalog.release_connection; COMMIT",
            wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning
            )
        ))
            .to_string(),
    ))
    .execute(&mut environment)
    .await
    .map(drop)
    .unwrap();
    let mut source = system.begin().await.unwrap();
    sqlx::raw_sql("SET LOCAL ROLE wamn_control")
        .execute(&mut *source)
        .await
        .map(drop)
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
            .to_string(),
    ))
    .execute(&mut *source)
    .await
    .map(drop)
    .unwrap();
    let mut target = environment.begin().await.unwrap();
    administration::execute_in_environment(
        &mut source,
        &mut target,
        "acme",
        "connection.write",
        &request,
        &mut AuthDrops::default(),
        &mut wamn_broker::connection_changes::ConnectionDrops::default(),
    )
    .await
    .unwrap();
    source.commit().await.unwrap();
    target.rollback().await.unwrap();
    assert_eq!(
        sqlx::query("SELECT active_generation FROM registry.connection WHERE id=$1::text::uuid")
            .bind(&connection)
            .fetch_one(&mut system)
            .await
            .unwrap()
            .get::<i64, _>(0),
        2
    );
}

#[tokio::test]
async fn apply_refuses_a_connection_version_missing_one_required_interface() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut client = connect(database.url()).await;
    seed(&mut client).await;
    sqlx::raw_sql("SET ROLE wamn_db_owner")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    let mut package = package("http");
    package.connections.get_mut("upstream").unwrap().interface =
        "wamn:connection/metadata@0.1.0".into();
    assert_eq!(
        bindings::analyse(&mut client, &document(), &[package])
            .await
            .unwrap_err()
            .code,
        "integration_import_unsatisfied"
    );
}

#[tokio::test]
async fn blobstore_interfaces_accept_another_storage_provider() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut client = connect(database.url()).await;
    seed(&mut client).await;
    let mut tx = client.begin().await.unwrap();
    sqlx::raw_sql("SET LOCAL ROLE wamn_control")
        .execute(&mut *tx)
        .await
        .map(drop)
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
            .to_string(),
    ))
    .execute(&mut *tx)
    .await
    .map(drop)
    .unwrap();
    let store = wamn_test_infrastructure::credentials::MemoryStore::default();
    wamn_test_infrastructure::credentials::define(
        &mut tx,
        "acme",
        &json!({"project":"one","env":"dev","name":"labels",
        "integration_type":"gcs","auth_type":"custom","parameters":{
            "provider":"gcs","container":"labels","prefix":"labels/"}}),
        &store,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    sqlx::raw_sql("SET ROLE wamn_db_owner")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    let mut doc = document();
    doc.integrations = std::collections::BTreeMap::from([(
        "labels".into(),
        wamn_environment_control::environment::document::Integration { version: 1 },
    )]);
    doc.applications
        .get_mut("app")
        .unwrap()
        .connections
        .insert("upstream".into(), "labels".into());
    let bindings = bindings::analyse(&mut client, &doc, &[package("s3")])
        .await
        .unwrap();
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].integration_type, "gcs");
}

#[tokio::test]
async fn publish_refuses_wasm_and_native_worlds_without_health() {
    let database = wamn_control_provision::test_database::system();
    let mut client = connect(database.url()).await;
    sqlx::raw_sql("SET ROLE wamn_system; INSERT INTO registry.integration_type(code,runtime_type,exclusive) VALUES ('native-health','native',false); SET ROLE wamn_control").execute(&mut client).await.map(drop).unwrap();
    for code in ["modbus", "native-health"] {
        let error = sqlx::query("SELECT registry.publish_integration($1,2,$2,$3,$4,$5,'{}')").bind(&code).bind(&format!("sha256:{}", "a".repeat(64))).bind(&json!({"type":"object","properties":{"authority":{"type":"string","immutable":true}},"required":["authority"]})).bind(&"wamn:modbus/client@1.0.0").bind(&vec!["wamn:modbus/read@1.0.0".to_owned()]).fetch_one(&mut client).await.unwrap_err();
        assert_eq!(
            error.as_database_error().unwrap().message(),
            "health_export_required"
        );
    }
}

#[tokio::test]
async fn apply_refuses_a_read_requirement_for_a_write_interface() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut client = connect(database.url()).await;
    seed(&mut client).await;
    let mut tx = client.begin().await.unwrap();
    sqlx::raw_sql("SET LOCAL ROLE wamn_control")
        .execute(&mut *tx)
        .await
        .map(drop)
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
            .to_string(),
    ))
    .execute(&mut *tx)
    .await
    .map(drop)
    .unwrap();
    let store = wamn_test_infrastructure::credentials::MemoryStore::default();
    wamn_test_infrastructure::credentials::define(&mut tx,"acme",&json!({"project":"one","env":"dev","name":"meter","integration_type":"modbus","auth_type":"none","parameters":{"authority":"192.168.1.2:502"}}),&store).await.unwrap();
    tx.commit().await.unwrap();
    sqlx::raw_sql("SET ROLE wamn_db_owner")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    let mut doc = document();
    doc.integrations = std::collections::BTreeMap::from([(
        "meter".into(),
        wamn_environment_control::environment::document::Integration { version: 1 },
    )]);
    doc.applications
        .get_mut("app")
        .unwrap()
        .connections
        .insert("upstream".into(), "meter".into());
    let mut manifest = package("modbus");
    let declaration = manifest.connections.get_mut("upstream").unwrap();
    declaration.interface = "wamn:modbus/write@1.0.0".into();
    declaration.mode = wamn_compiler::manifest::ConnectionMode::Read;
    assert_eq!(
        bindings::analyse(&mut client, &doc, &[manifest.clone()])
            .await
            .unwrap_err()
            .code,
        "connection_read_imports_write_interface"
    );
    manifest.connections.get_mut("upstream").unwrap().mode =
        wamn_compiler::manifest::ConnectionMode::Write;
    assert_eq!(
        bindings::analyse(&mut client, &doc, &[manifest.clone()])
            .await
            .unwrap()
            .len(),
        1
    );
    let declaration = manifest.connections.get_mut("upstream").unwrap();
    declaration.mode = wamn_compiler::manifest::ConnectionMode::Read;
    declaration.interface = "wamn:modbus/read@1.0.0".into();
    assert_eq!(
        bindings::analyse(&mut client, &doc, &[manifest])
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn apply_requires_one_explicit_replica_for_exclusive_cloud_bindings() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut client = connect(database.url()).await;
    seed(&mut client).await;
    let mut tx = client.begin().await.unwrap();
    sqlx::raw_sql("SET LOCAL ROLE wamn_control")
        .execute(&mut *tx)
        .await
        .map(drop)
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
            .to_string(),
    ))
    .execute(&mut *tx)
    .await
    .map(drop)
    .unwrap();
    let store = wamn_test_infrastructure::credentials::MemoryStore::default();
    wamn_test_infrastructure::credentials::define(
        &mut tx,
        "acme",
        &json!({"project":"one","env":"dev","name":"meter",
            "integration_type":"modbus","auth_type":"none",
            "parameters":{"authority":"192.168.1.2:502"}}),
        &store,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    // A cloud target's name does not determine its type.
    sqlx::raw_sql("UPDATE registry.target SET name='remote' WHERE name='cloud'")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    let mut doc = document();
    doc.target = "remote".into();
    sqlx::raw_sql("SET ROLE wamn_db_owner")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    let shared = bindings::analyse(&mut client, &doc, &[package("http")])
        .await
        .unwrap();
    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    assert!(
        analyse::replica_refusals(&mut client, &doc, &shared, None)
            .await
            .unwrap()
            .is_empty()
    );
    doc.integrations = std::collections::BTreeMap::from([(
        "meter".into(),
        wamn_environment_control::environment::document::Integration { version: 1 },
    )]);
    doc.applications
        .get_mut("app")
        .unwrap()
        .connections
        .insert("upstream".into(), "meter".into());
    sqlx::raw_sql("SET ROLE wamn_db_owner")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    let exclusive = bindings::analyse(&mut client, &doc, &[package("modbus")])
        .await
        .unwrap();
    assert!(exclusive[0].exclusive);
    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    let directory = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
    let group = directory.path().join("host-group.yaml");
    for yaml in ["replicas: 2\n", "{}\n", "replicas: '1'\n"] {
        std::fs::write(&group, yaml).unwrap();
        assert_eq!(
            analyse::replica_refusals(&mut client, &doc, &exclusive, Some(&group))
                .await
                .unwrap(),
            [
                "connection meter is exclusive on a cloud target: WAMN_RELEASE_HOST_GROUP must name replicas: 1"
            ],
            "{yaml}"
        );
    }
    assert_eq!(
        analyse::replica_refusals(&mut client, &doc, &exclusive, None)
            .await
            .unwrap()
            .len(),
        1
    );
    std::fs::write(&group, "replicas: 1\n").unwrap();
    assert!(
        analyse::replica_refusals(&mut client, &doc, &exclusive, Some(&group))
            .await
            .unwrap()
            .is_empty()
    );
    sqlx::raw_sql("UPDATE registry.target SET name='cloud', type='local' WHERE name='remote'")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    doc.target = "cloud".into();
    sqlx::raw_sql("SET ROLE wamn_db_owner")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    let local = bindings::analyse(&mut client, &doc, &[package("modbus")])
        .await
        .unwrap();
    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    std::fs::write(&group, "replicas: 2\n").unwrap();
    for path in [Some(group.as_path()), None] {
        assert!(
            analyse::replica_refusals(&mut client, &doc, &local, path)
                .await
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn publish_refuses_write_interfaces_outside_the_exports() {
    let database = wamn_control_provision::test_database::system();
    let mut client = connect(database.url()).await;
    sqlx::raw_sql("SET ROLE wamn_control")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    let error = sqlx::query(
        "SELECT registry.publish_integration('http',2,NULL,$1,'wamn:connection/http',$2,$3)",
    )
    .bind(&json!({"type":"object","properties":{}}))
    .bind(&vec!["wamn:connection/http@0.1.0"])
    .bind(&vec!["wamn:modbus/write@1.0.0"])
    .fetch_one(&mut client)
    .await
    .unwrap_err();
    assert_eq!(
        error.as_database_error().unwrap().message(),
        "write_interface_not_exported"
    );
}

#[tokio::test]
async fn project_observation_preserves_the_baseline_and_refuses_unknown_state() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_platform_identity::test_database::project_platform();
    let mut client = connect(database.url()).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (wamn_control_provision::sql::ensure_db_owner_role_sql()).to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("GRANT wamn_platform TO wamn_db_owner WITH INHERIT TRUE, SET FALSE; ALTER DATABASE {} OWNER TO wamn_db_owner; ALTER SCHEMA catalog OWNER TO wamn_db_owner; GRANT USAGE ON SCHEMA platform TO wamn_db_owner; GRANT SELECT ON ALL TABLES IN SCHEMA catalog, platform TO wamn_db_owner", postgres_protocol::escape::escape_identifier(database.name())))).execute(&mut client).await.map(drop).unwrap();
    let fingerprint = format!("sha256:{}", "a".repeat(64));
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "BEGIN; {}",
            wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning
            )
        ))
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .unwrap();
    sqlx::query("INSERT INTO catalog.packages (project_env_key, package_id, package_version, artifact_digest) VALUES ('acme--dev--one', 'fixture', '1.0.0', $1)").bind(&fingerprint).execute(&mut client).await.map(|result| result.rows_affected()).unwrap();
    sqlx::raw_sql("COMMIT")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    let observed = analyse::read_project(database.url(), "acme--dev--one")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(observed.installed["fixture"].current, "1.0.0");
    assert_eq!(
        observed.installed["fixture"].physical_schema_fingerprint,
        fingerprint
    );
    sqlx::raw_sql("REVOKE SELECT ON catalog.packages FROM wamn_db_owner")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    assert!(
        analyse::read_project(database.url(), "acme--dev--one")
            .await
            .is_err()
    );
    let mut missing = url::Url::parse(database.url()).unwrap();
    missing.set_path(&format!("/{}_absent", database.name()));
    assert!(
        analyse::read_project(missing.as_str(), "acme--dev--one")
            .await
            .unwrap()
            .is_none()
    );
}
