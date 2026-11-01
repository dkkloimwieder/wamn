//! Apply refusals against project integrations and environment connections.

use serde_json::json;
use wamn_control::environment::{analyse, bindings, document::EnvironmentDocument};
use wamn_platform_identity::{auth_changes::AuthDrops, connections::administration};
use wamn_schema_generator::PackageManifest;

async fn connect(url: &str) -> tokio_postgres::Client {
    let (client, driver) = tokio_postgres::connect(url, tokio_postgres::NoTls)
        .await
        .unwrap();
    tokio::spawn(driver);
    client
}
fn document() -> EnvironmentDocument {
    EnvironmentDocument::from_json(&serde_json::to_vec(&json!({
        "org":"acme","project":"one","env":"dev","target":"cloud",
        "release":format!("sha256:{}","a".repeat(64)),"route_host":"one.example","policy":"dev",
        "integrations":{"erp":{"version":1}},"applications":{"app":{"packages":["fixture"],"connections":{"upstream":"erp"}}}
    })).unwrap()).unwrap()
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
async fn seed(client: &mut tokio_postgres::Client) {
    client.batch_execute("SET ROLE wamn_system").await.unwrap();
    client
        .batch_execute(
            &wamn_control_provision::test_database::seed_org_project_env_sql("acme", "one", "dev"),
        )
        .await
        .unwrap();
    client.batch_execute(&format!("BEGIN; {} INSERT INTO registry.target(name,type,status) VALUES('cloud','cloud','active') ON CONFLICT DO NOTHING; UPDATE registry.meta SET platform_domain='example.invalid'; UPDATE registry.project_envs SET target_id=(SELECT id FROM registry.target WHERE name='cloud'); COMMIT;",wamn_control_provision::bind_platform_principal_sql(wamn_control_provision::PlatformComponent::Provisioning))).await.unwrap();
    let tx = client.transaction().await.unwrap();
    tx.batch_execute("SET LOCAL ROLE wamn_control")
        .await
        .unwrap();
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await
    .unwrap();
    let store = wamn_test_infrastructure::credentials::LocalCredentials::create().unwrap();
    wamn_test_infrastructure::credentials::define(&tx,"acme",&json!({"project":"one","env":"dev","name":"erp","integration_type":"http","auth_type":"none","parameters":{"primary-authority":"https://erp.example/","tls-verification":"verify-authority"}}),&store).await.unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn apply_refuses_invalid_names_interfaces_environments_and_versions() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut client = connect(database.url()).await;
    seed(&mut client).await;
    client
        .batch_execute("SET ROLE wamn_db_owner")
        .await
        .unwrap();
    let mut doc = document();
    let packages = [package("http")];
    let selected = bindings::analyse(&client, &doc, &packages).await.unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].version, 1);
    assert_eq!(
        bindings::analyse(&client, &doc, &[package("gcs")])
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
        bindings::analyse(&client, &doc, &packages)
            .await
            .unwrap_err()
            .code,
        "connection_undeclared"
    );
    doc.applications.get_mut("app").unwrap().connections.clear();
    assert_eq!(
        bindings::analyse(&client, &doc, &packages)
            .await
            .unwrap_err()
            .code,
        "connection_unbound"
    );
    doc = document();
    doc.integrations.clear();
    assert_eq!(
        bindings::analyse(&client, &doc, &packages)
            .await
            .unwrap_err()
            .code,
        "integration_version_missing"
    );
    doc = document();
    doc.integrations.get_mut("erp").unwrap().version = 99;
    assert_eq!(
        bindings::analyse(&client, &doc, &packages)
            .await
            .unwrap_err()
            .code,
        "integration_version_unpublished"
    );
    doc = document();
    doc.env = "other".into();
    assert_eq!(
        bindings::analyse(&client, &doc, &packages)
            .await
            .unwrap_err()
            .code,
        "connection_unavailable"
    );
    doc = document();
    doc.target = "other".into();
    assert_eq!(
        bindings::analyse(&client, &doc, &packages)
            .await
            .unwrap_err()
            .code,
        "connection_unavailable"
    );
    client.batch_execute("SET ROLE wamn_system; INSERT INTO registry.integration_type_version(integration_type,version,parameter_schema,operations,exports,published_at) SELECT integration_type,2,parameter_schema,operations,exports,now() FROM registry.integration_type_version WHERE integration_type='http' AND version=1").await.unwrap();
    client
        .batch_execute("SET ROLE wamn_db_owner")
        .await
        .unwrap();
    doc = document();
    doc.integrations.get_mut("erp").unwrap().version = 2;
    assert_eq!(
        bindings::analyse(&client, &doc, &packages)
            .await
            .unwrap_err()
            .code,
        "integration_version_unserved"
    );
    let tx = client.transaction().await.unwrap();
    tx.batch_execute("SET LOCAL ROLE wamn_control")
        .await
        .unwrap();
    administration::execute(
        &tx,
        "acme",
        "connection.disable",
        &json!({"project":"one","env":"dev","name":"erp"}).to_string(),
        &mut AuthDrops::default(),
        &mut wamn_platform_identity::connection_changes::ConnectionDrops::default(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        bindings::analyse(&client, &document(), &packages)
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
    client
        .batch_execute("SET ROLE wamn_db_owner")
        .await
        .unwrap();
    let selected = bindings::analyse(&client, &doc, &[first, second])
        .await
        .unwrap();
    assert_eq!(selected.len(), 2);
    assert_eq!(selected[0].connection_id, selected[1].connection_id);
    client.batch_execute("SET ROLE wamn_system").await.unwrap();
    assert_eq!(
        client
            .query_one("SELECT count(*) FROM registry.connection_generation", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
}

#[tokio::test]
async fn a_version_change_refuses_stored_values_that_do_not_validate() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut client = connect(database.url()).await;
    seed(&mut client).await;
    let tx = client.transaction().await.unwrap();
    tx.batch_execute("SET LOCAL ROLE wamn_control")
        .await
        .unwrap();
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await
    .unwrap();
    let store = wamn_test_infrastructure::credentials::LocalCredentials::create().unwrap();
    wamn_test_infrastructure::credentials::define(&tx,"acme",&json!({"project":"one","env":"dev","name":"meter","integration_type":"modbus","auth_type":"none","parameters":{"authority":"127.0.0.1:502"}}),&store).await.unwrap();
    tx.commit().await.unwrap();
    client.batch_execute("SET ROLE wamn_system; INSERT INTO registry.integration_type_version(integration_type,version,digest,parameter_schema,operations,exports,write_interfaces,published_at) SELECT integration_type,2,digest,'{\"type\":\"object\",\"properties\":{\"authority\":{\"type\":\"string\"},\"unit\":{\"type\":\"integer\"}},\"required\":[\"authority\",\"unit\"]}',operations,exports,write_interfaces,now() FROM registry.integration_type_version WHERE integration_type='modbus' AND version=1; SET ROLE wamn_db_owner").await.unwrap();
    let mut doc = document();
    doc.integrations.clear();
    doc.integrations.insert(
        "meter".into(),
        wamn_control::environment::document::Integration { version: 2 },
    );
    doc.applications
        .get_mut("app")
        .unwrap()
        .connections
        .insert("upstream".into(), "meter".into());
    assert_eq!(
        bindings::analyse(&client, &doc, &[package("modbus")])
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
    system.batch_execute("INSERT INTO registry.integration_type_version(integration_type,version,parameter_schema,operations,exports,published_at) SELECT integration_type,2,jsonb_set(parameter_schema,'{required}',(parameter_schema->'required') || '\"unit\"'::jsonb) || jsonb_build_object('properties',(parameter_schema->'properties') || '{\"unit\":{\"type\":\"integer\"}}'::jsonb),operations,exports,now() FROM registry.integration_type_version WHERE integration_type='http' AND version=1").await.unwrap();
    let environment_database = wamn_project_state::test_database::tenant_platform();
    let mut environment = connect(environment_database.url()).await;
    environment
        .batch_execute("CREATE SCHEMA IF NOT EXISTS wamn_run")
        .await
        .unwrap();
    environment
        .batch_execute(&wamn_control_provision::sql::grant_administration_surface_sql("wamn_run"))
        .await
        .unwrap();
    let document = document();
    let _ =
        bindings::project_authority(system_database.url(), environment_database.url(), &document)
            .await
            .unwrap();
    environment
        .batch_execute(&format!(
            "BEGIN; {}",
            wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning
            )
        ))
        .await
        .unwrap();
    let connection: String = environment
        .query_one("SELECT id::text FROM platform.connection", &[])
        .await
        .unwrap()
        .get(0);
    // The sha256 of the canonical bytes `{}`, which catalog.releases checks.
    let digest = "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a";
    environment.execute("INSERT INTO catalog.releases(env_key,manifest_digest,canonical_bytes) VALUES('acme--one--dev',$1,$2)",&[&digest,&b"{}".as_slice()]).await.unwrap();
    environment.execute("INSERT INTO catalog.release_connection(env_key,manifest_digest,connection_id,version) VALUES('acme--one--dev',$1,$2::text::uuid,2)",&[&digest,&connection]).await.unwrap();
    environment.batch_execute("COMMIT").await.unwrap();
    let request = json!({"project":"one","env":"dev","name":"erp","parameters":{"primary-authority":"https://erp.example/","tls-verification":"disabled"}}).to_string();
    let source = system.transaction().await.unwrap();
    source
        .batch_execute("SET LOCAL ROLE wamn_control")
        .await
        .unwrap();
    source
        .batch_execute(&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
        .await
        .unwrap();
    let target = environment.transaction().await.unwrap();
    let error = administration::execute_in_environment(
        &source,
        &target,
        "acme",
        "connection.write",
        &request,
        &mut AuthDrops::default(),
        &mut wamn_platform_identity::connection_changes::ConnectionDrops::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "invalid_input");
    source.rollback().await.unwrap();
    target.rollback().await.unwrap();
    assert_eq!(
        system
            .query_one(
                "SELECT active_generation FROM registry.connection WHERE id=$1::text::uuid",
                &[&connection]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    environment
        .batch_execute(&format!(
            "BEGIN; {} DELETE FROM catalog.release_connection; COMMIT",
            wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning
            )
        ))
        .await
        .unwrap();
    let source = system.transaction().await.unwrap();
    source
        .batch_execute("SET LOCAL ROLE wamn_control")
        .await
        .unwrap();
    source
        .batch_execute(&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
        .await
        .unwrap();
    let target = environment.transaction().await.unwrap();
    administration::execute_in_environment(
        &source,
        &target,
        "acme",
        "connection.write",
        &request,
        &mut AuthDrops::default(),
        &mut wamn_platform_identity::connection_changes::ConnectionDrops::default(),
    )
    .await
    .unwrap();
    source.commit().await.unwrap();
    target.rollback().await.unwrap();
    assert_eq!(
        system
            .query_one(
                "SELECT active_generation FROM registry.connection WHERE id=$1::text::uuid",
                &[&connection]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        2
    );
}

#[tokio::test]
async fn apply_refuses_a_connection_version_missing_one_required_interface() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut client = connect(database.url()).await;
    seed(&mut client).await;
    client
        .batch_execute("SET ROLE wamn_db_owner")
        .await
        .unwrap();
    let mut package = package("http");
    package.connections.get_mut("upstream").unwrap().interface =
        "wamn:connection/metadata@0.1.0".into();
    assert_eq!(
        bindings::analyse(&client, &document(), &[package])
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
    let tx = client.transaction().await.unwrap();
    tx.batch_execute("SET LOCAL ROLE wamn_control")
        .await
        .unwrap();
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await
    .unwrap();
    let store = wamn_test_infrastructure::credentials::LocalCredentials::create().unwrap();
    wamn_test_infrastructure::credentials::define(
        &tx,
        "acme",
        &json!({"project":"one","env":"dev","name":"labels",
        "integration_type":"gcs","auth_type":"custom","parameters":{
            "provider":"gcs","container":"labels","prefix":"labels/"}}),
        &store,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    client
        .batch_execute("SET ROLE wamn_db_owner")
        .await
        .unwrap();
    let mut doc = document();
    doc.integrations = std::collections::BTreeMap::from([(
        "labels".into(),
        wamn_control::environment::document::Integration { version: 1 },
    )]);
    doc.applications
        .get_mut("app")
        .unwrap()
        .connections
        .insert("upstream".into(), "labels".into());
    let bindings = bindings::analyse(&client, &doc, &[package("s3")])
        .await
        .unwrap();
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].integration_type, "gcs");
}

#[tokio::test]
async fn publish_refuses_wasm_and_native_worlds_without_health() {
    let database = wamn_control_provision::test_database::system();
    let client = connect(database.url()).await;
    client.batch_execute("SET ROLE wamn_system; INSERT INTO registry.integration_type(code,runtime_type,exclusive) VALUES ('native-health','native',false); SET ROLE wamn_control").await.unwrap();
    for code in ["modbus", "native-health"] {
        let error = client.query_one("SELECT registry.publish_integration($1,2,$2,$3,$4,$5,'{}')", &[
            &code, &format!("sha256:{}", "a".repeat(64)),
            &json!({"type":"object","properties":{"authority":{"type":"string","immutable":true}},"required":["authority"]}),
            &"wamn:modbus/client@1.0.0", &vec!["wamn:modbus/read@1.0.0".to_owned()],
        ]).await.unwrap_err();
        assert_eq!(
            error.as_db_error().unwrap().message(),
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
    let tx = client.transaction().await.unwrap();
    tx.batch_execute("SET LOCAL ROLE wamn_control")
        .await
        .unwrap();
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await
    .unwrap();
    let store = wamn_test_infrastructure::credentials::LocalCredentials::create().unwrap();
    wamn_test_infrastructure::credentials::define(&tx,"acme",&json!({"project":"one","env":"dev","name":"meter","integration_type":"modbus","auth_type":"none","parameters":{"authority":"192.168.1.2:502"}}),&store).await.unwrap();
    tx.commit().await.unwrap();
    client
        .batch_execute("SET ROLE wamn_db_owner")
        .await
        .unwrap();
    let mut doc = document();
    doc.integrations = std::collections::BTreeMap::from([(
        "meter".into(),
        wamn_control::environment::document::Integration { version: 1 },
    )]);
    doc.applications
        .get_mut("app")
        .unwrap()
        .connections
        .insert("upstream".into(), "meter".into());
    let mut manifest = package("modbus");
    let declaration = manifest.connections.get_mut("upstream").unwrap();
    declaration.interface = "wamn:modbus/write@1.0.0".into();
    declaration.mode = wamn_schema_generator::ConnectionMode::Read;
    assert_eq!(
        bindings::analyse(&client, &doc, &[manifest.clone()])
            .await
            .unwrap_err()
            .code,
        "connection_read_imports_write_interface"
    );
    manifest.connections.get_mut("upstream").unwrap().mode =
        wamn_schema_generator::ConnectionMode::Write;
    assert_eq!(
        bindings::analyse(&client, &doc, &[manifest.clone()])
            .await
            .unwrap()
            .len(),
        1
    );
    let declaration = manifest.connections.get_mut("upstream").unwrap();
    declaration.mode = wamn_schema_generator::ConnectionMode::Read;
    declaration.interface = "wamn:modbus/read@1.0.0".into();
    assert_eq!(
        bindings::analyse(&client, &doc, &[manifest])
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
    let tx = client.transaction().await.unwrap();
    tx.batch_execute("SET LOCAL ROLE wamn_control")
        .await
        .unwrap();
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await
    .unwrap();
    let store = wamn_test_infrastructure::credentials::LocalCredentials::create().unwrap();
    wamn_test_infrastructure::credentials::define(
        &tx,
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
    client
        .batch_execute("UPDATE registry.target SET name='remote' WHERE name='cloud'")
        .await
        .unwrap();
    let mut doc = document();
    doc.target = "remote".into();
    client
        .batch_execute("SET ROLE wamn_db_owner")
        .await
        .unwrap();
    let shared = bindings::analyse(&client, &doc, &[package("http")])
        .await
        .unwrap();
    client.batch_execute("SET ROLE wamn_system").await.unwrap();
    assert!(
        analyse::replica_refusals(&client, &doc, &shared, None)
            .await
            .unwrap()
            .is_empty()
    );
    doc.integrations = std::collections::BTreeMap::from([(
        "meter".into(),
        wamn_control::environment::document::Integration { version: 1 },
    )]);
    doc.applications
        .get_mut("app")
        .unwrap()
        .connections
        .insert("upstream".into(), "meter".into());
    client
        .batch_execute("SET ROLE wamn_db_owner")
        .await
        .unwrap();
    let exclusive = bindings::analyse(&client, &doc, &[package("modbus")])
        .await
        .unwrap();
    assert!(exclusive[0].exclusive);
    client.batch_execute("SET ROLE wamn_system").await.unwrap();
    let directory = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
    let group = directory.path().join("host-group.yaml");
    for yaml in ["replicas: 2\n", "{}\n", "replicas: '1'\n"] {
        std::fs::write(&group, yaml).unwrap();
        assert_eq!(
            analyse::replica_refusals(&client, &doc, &exclusive, Some(&group))
                .await
                .unwrap(),
            [
                "connection meter is exclusive on a cloud target: WAMN_RELEASE_HOST_GROUP must name replicas: 1"
            ],
            "{yaml}"
        );
    }
    assert_eq!(
        analyse::replica_refusals(&client, &doc, &exclusive, None)
            .await
            .unwrap()
            .len(),
        1
    );
    std::fs::write(&group, "replicas: 1\n").unwrap();
    assert!(
        analyse::replica_refusals(&client, &doc, &exclusive, Some(&group))
            .await
            .unwrap()
            .is_empty()
    );
    client
        .batch_execute("UPDATE registry.target SET name='cloud', type='local' WHERE name='remote'")
        .await
        .unwrap();
    doc.target = "cloud".into();
    client
        .batch_execute("SET ROLE wamn_db_owner")
        .await
        .unwrap();
    let local = bindings::analyse(&client, &doc, &[package("modbus")])
        .await
        .unwrap();
    client.batch_execute("SET ROLE wamn_system").await.unwrap();
    std::fs::write(&group, "replicas: 2\n").unwrap();
    for path in [Some(group.as_path()), None] {
        assert!(
            analyse::replica_refusals(&client, &doc, &local, path)
                .await
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn publish_refuses_write_interfaces_outside_the_exports() {
    let database = wamn_control_provision::test_database::system();
    let client = connect(database.url()).await;
    client.batch_execute("SET ROLE wamn_control").await.unwrap();
    let error = client
        .query_one(
            "SELECT registry.publish_integration('http',2,NULL,$1,'wamn:connection/http',$2,$3)",
            &[
                &json!({"type":"object","properties":{}}),
                &vec!["wamn:connection/http@0.1.0"],
                &vec!["wamn:modbus/write@1.0.0"],
            ],
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.as_db_error().unwrap().message(),
        "write_interface_not_exported"
    );
}
