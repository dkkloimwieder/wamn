//! Apply refusals against project integrations and environment connections.

use serde_json::json;
use sqlx::{Connection as _, Row as _};
use wamn_compiler::manifest::PackageManifest;
use wamn_environment::environment::{analyse, bindings, document::EnvironmentDocument};
use wamn_connections::connections::administration;

async fn connect(url: &str) -> sqlx::PgConnection {
    let client = wamn_postgres::connect(url).await.unwrap();

    client
}
fn document() -> EnvironmentDocument {
    let mut document = EnvironmentDocument::from_json(&serde_json::to_vec(&json!({
        "org":"acme","project":"one","env":"dev","target":"cloud","policy":"dev","revocation_seconds":60,
        "integrations":{"erp":{"version":1}},
        "applications": {"app":{"revocation_seconds":5,"hostname": "one.example","connections":{"upstream":"erp"}}}})).unwrap()).unwrap();
    let selection = document.applications.get_mut("app").unwrap();
    selection.current = wamn_environment::environment::document::DeclaredRelease::Digest(
        format!("sha256:{:064x}", 1).parse().unwrap(),
    );
    selection
        .packages
        .insert("fixture".parse().expect("valid name"));
    document
}
fn package(integration_type: &wamn_registry::integration_code::IntegrationCode) -> PackageManifest {
    let interface = match integration_type.as_str() {
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
        (&wamn_control_provision::test_database::seed_org_project_env_sql(
            &"acme".parse().unwrap(),
            &"one".parse().unwrap(),
            &"dev".parse().unwrap(),
        ))
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
    let store = wamn_connections::credentials::MemoryStore::default();
    wamn_connections::credentials::define(&mut tx,&"acme".parse().unwrap(),&json!({"project":"one","env":"dev","name":"erp","integration_type":"http","auth_type":"none","parameters":{"primary-authority":"https://erp.example/","tls-verification":"verify-authority"}}),&store).await.unwrap();
    tx.commit().await.unwrap();
}

#[sqlx::test(migrations = false)]
async fn apply_refuses_invalid_names_interfaces_environments_and_versions(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    wamn_control_provision::test_database::system(database.as_str()).await;
    let mut client = connect(database.as_str()).await;
    seed(&mut client).await;
    sqlx::raw_sql("SET ROLE wamn_db_owner")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    let mut doc = document();
    let packages = [package(&"http".parse().unwrap())];
    // Neither an unreferenced integration nor another application's connection is a prerequisite.
    doc.integrations.insert(
        "unavailable".into(),
        wamn_environment::environment::document::Integration { version: 1 },
    );
    let mut unrelated = doc.applications["app"].clone();
    unrelated.packages =
        std::collections::BTreeSet::from(["unrelated".parse().expect("valid name")]);
    unrelated.connections.insert(
        "upstream".into(),
        wamn_catalog::configuration::ConnectionReference {
            name: "unavailable".into(),
            version: None,
        },
    );
    doc.applications
        .insert("other".parse().expect("valid name"), unrelated);
    let selected = bindings::analyse(&mut client, &doc, &packages)
        .await
        .unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].version, 1);
    doc.applications
        .get_mut("app")
        .unwrap()
        .connections
        .get_mut("upstream")
        .unwrap()
        .version = Some(2);
    assert!(
        bindings::analyse(&mut client, &doc, &packages)
            .await
            .is_err()
    );
    doc.applications
        .get_mut("app")
        .unwrap()
        .connections
        .get_mut("upstream")
        .unwrap()
        .version = Some(1);
    let selected = bindings::analyse(&mut client, &doc, &packages)
        .await
        .unwrap();
    assert_eq!(selected[0].version, 1);

    let mut missing = package(&"http".parse().unwrap());
    missing.connections.get_mut("upstream").unwrap().interface =
        "wamn:connection/metadata@0.1.0".into();
    for invalid in [package(&"gcs".parse().unwrap()), missing] {
        assert!(
            bindings::analyse(&mut client, &doc, &[invalid])
                .await
                .is_err()
        );
    }
    doc.applications.get_mut("app").unwrap().connections.insert(
        "other".into(),
        wamn_catalog::configuration::ConnectionReference {
            name: "erp".into(),
            version: None,
        },
    );
    assert!(
        bindings::analyse(&mut client, &doc, &packages)
            .await
            .is_err()
    );
    doc.applications.get_mut("app").unwrap().connections.clear();
    assert!(
        bindings::analyse(&mut client, &doc, &packages)
            .await
            .is_err()
    );
    doc = document();
    doc.integrations.clear();
    assert!(
        bindings::analyse(&mut client, &doc, &packages)
            .await
            .is_err()
    );
    doc = document();
    doc.integrations.get_mut("erp").unwrap().version = 99;
    assert!(
        bindings::analyse(&mut client, &doc, &packages)
            .await
            .is_err()
    );
    doc = document();
    doc.env = "other".parse().unwrap();
    assert!(
        bindings::analyse(&mut client, &doc, &packages)
            .await
            .is_err()
    );
    doc = document();
    doc.target = "other".into();
    assert!(
        bindings::analyse(&mut client, &doc, &packages)
            .await
            .is_err()
    );
    sqlx::raw_sql("SET ROLE wamn_system; INSERT INTO registry.integration_type_version(integration_type,version,parameter_schema,operations,exports,published_at) SELECT integration_type,2,parameter_schema,operations,exports,now() FROM registry.integration_type_version WHERE integration_type='http' AND version=1").execute(&mut client).await.map(drop).unwrap();
    sqlx::raw_sql("SET ROLE wamn_db_owner")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    doc = document();
    doc.integrations.get_mut("erp").unwrap().version = 2;
    assert!(
        bindings::analyse(&mut client, &doc, &packages)
            .await
            .is_err()
    );
    let mut tx = client.begin().await.unwrap();
    sqlx::raw_sql("SET LOCAL ROLE wamn_control")
        .execute(&mut *tx)
        .await
        .map(drop)
        .unwrap();
    administration::execute(
        &mut tx,
        &"acme".parse().unwrap(),
        "connection.disable",
        &json!({"project":"one","env":"dev","name":"erp"}).to_string(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(
        bindings::analyse(&mut client, &document(), &packages)
            .await
            .is_err()
    );
}

#[sqlx::test(migrations = false)]
async fn two_packages_can_bind_distinct_names_to_one_connection_without_authority_writes(
    pool: sqlx::PgPool,
) {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    wamn_control_provision::test_database::system(database.as_str()).await;
    let mut client = connect(database.as_str()).await;
    seed(&mut client).await;
    let mut doc = document();
    doc.applications
        .get_mut("app")
        .unwrap()
        .packages
        .insert("other".parse().expect("valid name"));
    doc.applications.get_mut("app").unwrap().connections.insert(
        "second".into(),
        wamn_catalog::configuration::ConnectionReference {
            name: "erp".into(),
            version: None,
        },
    );
    let first = package(&"http".parse().unwrap());
    let mut second = first.clone();
    second.package.id = "other".parse().expect("valid name");
    let declaration = second.connections.remove("upstream").unwrap();
    second
        .connections
        .insert("second".parse().unwrap(), declaration);
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

#[sqlx::test(migrations = false)]
async fn a_version_change_refuses_stored_values_that_do_not_validate(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    wamn_control_provision::test_database::system(database.as_str()).await;
    let mut client = connect(database.as_str()).await;
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
    let store = wamn_connections::credentials::MemoryStore::default();
    wamn_connections::credentials::define(&mut tx,&"acme".parse().unwrap(),&json!({"project":"one","env":"dev","name":"meter","integration_type":"modbus","auth_type":"none","parameters":{"authority":"127.0.0.1:502"}}),&store).await.unwrap();
    tx.commit().await.unwrap();
    sqlx::raw_sql("SET ROLE wamn_system; INSERT INTO registry.integration_type_version(integration_type,version,digest,parameter_schema,operations,exports,write_interfaces,published_at) SELECT integration_type,2,digest,'{\"type\":\"object\",\"properties\":{\"authority\":{\"type\":\"string\"},\"unit\":{\"type\":\"integer\"}},\"required\":[\"authority\",\"unit\"]}',operations,exports,write_interfaces,now() FROM registry.integration_type_version WHERE integration_type='modbus' AND version=1; SET ROLE wamn_db_owner").execute(&mut client).await.map(drop).unwrap();
    let mut doc = document();
    doc.integrations.clear();
    doc.integrations.insert(
        "meter".into(),
        wamn_environment::environment::document::Integration { version: 2 },
    );
    doc.applications.get_mut("app").unwrap().connections.insert(
        "upstream".into(),
        wamn_catalog::configuration::ConnectionReference {
            name: "meter".into(),
            version: None,
        },
    );
    assert!(
        bindings::analyse(&mut client, &doc, &[package(&"modbus".parse().unwrap())])
            .await
            .is_err()
    );
}

#[sqlx::test(migrations = false)]
async fn blobstore_interfaces_accept_another_storage_provider(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    wamn_control_provision::test_database::system(database.as_str()).await;
    let mut client = connect(database.as_str()).await;
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
    let store = wamn_connections::credentials::MemoryStore::default();
    wamn_connections::credentials::define(
        &mut tx,
        &"acme".parse().unwrap(),
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
        wamn_environment::environment::document::Integration { version: 1 },
    )]);
    doc.applications.get_mut("app").unwrap().connections.insert(
        "upstream".into(),
        wamn_catalog::configuration::ConnectionReference {
            name: "labels".into(),
            version: None,
        },
    );
    let bindings = bindings::analyse(&mut client, &doc, &[package(&"s3".parse().unwrap())])
        .await
        .unwrap();
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].integration_type, "gcs".parse().unwrap());
}

#[sqlx::test(migrations = false)]
async fn publish_refuses_wasm_and_native_worlds_without_health(pool: sqlx::PgPool) {
    let database = wamn_test_postgres::url(&pool);
    wamn_control_provision::test_database::system(database.as_str()).await;
    let mut client = connect(database.as_str()).await;
    sqlx::raw_sql("SET ROLE wamn_system; INSERT INTO registry.integration_type(code,runtime_type,exclusive) VALUES ('native-health','native',false); SET ROLE wamn_control").execute(&mut client).await.map(drop).unwrap();
    for code in ["modbus", "native-health"] {
        let result = sqlx::query("SELECT registry.publish_integration($1,2,$2,$3,$4,$5,'{}')").bind(&code).bind(&(format!("sha256:{}", "a".repeat(64))).parse::<wamn_digest::Digest>().unwrap()).bind(&json!({"type":"object","properties":{"authority":{"type":"string","immutable":true}},"required":["authority"]})).bind(&"wamn:modbus/client@1.0.0").bind(&vec!["wamn:modbus/read@1.0.0".to_owned()]).fetch_one(&mut client).await;
        assert!(result.is_err());
    }
}

#[sqlx::test(migrations = false)]
async fn apply_refuses_a_read_requirement_for_a_write_interface(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    wamn_control_provision::test_database::system(database.as_str()).await;
    let mut client = connect(database.as_str()).await;
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
    let store = wamn_connections::credentials::MemoryStore::default();
    wamn_connections::credentials::define(&mut tx,&"acme".parse().unwrap(),&json!({"project":"one","env":"dev","name":"meter","integration_type":"modbus","auth_type":"none","parameters":{"authority":"192.168.1.2:502"}}),&store).await.unwrap();
    tx.commit().await.unwrap();
    sqlx::raw_sql("SET ROLE wamn_db_owner")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    let mut doc = document();
    doc.integrations = std::collections::BTreeMap::from([(
        "meter".into(),
        wamn_environment::environment::document::Integration { version: 1 },
    )]);
    doc.applications.get_mut("app").unwrap().connections.insert(
        "upstream".into(),
        wamn_catalog::configuration::ConnectionReference {
            name: "meter".into(),
            version: None,
        },
    );
    let mut manifest = package(&"modbus".parse().unwrap());
    let declaration = manifest.connections.get_mut("upstream").unwrap();
    declaration.interface = "wamn:modbus/write@1.0.0".into();
    declaration.mode = wamn_compiler::manifest::ConnectionMode::Read;
    assert!(
        bindings::analyse(&mut client, &doc, &[manifest.clone()])
            .await
            .is_err()
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

#[sqlx::test(migrations = false)]
async fn apply_requires_one_explicit_replica_for_exclusive_cloud_bindings(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    wamn_control_provision::test_database::system(database.as_str()).await;
    let mut client = connect(database.as_str()).await;
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
    let store = wamn_connections::credentials::MemoryStore::default();
    wamn_connections::credentials::define(
        &mut tx,
        &"acme".parse().unwrap(),
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
    let shared = bindings::analyse(&mut client, &doc, &[package(&"http".parse().unwrap())])
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
        wamn_environment::environment::document::Integration { version: 1 },
    )]);
    doc.applications.get_mut("app").unwrap().connections.insert(
        "upstream".into(),
        wamn_catalog::configuration::ConnectionReference {
            name: "meter".into(),
            version: None,
        },
    );
    sqlx::raw_sql("SET ROLE wamn_db_owner")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    let exclusive = bindings::analyse(&mut client, &doc, &[package(&"modbus".parse().unwrap())])
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
        assert!(
            !analyse::replica_refusals(&mut client, &doc, &exclusive, Some(&group))
                .await
                .unwrap()
                .is_empty()
        );
    }
    assert!(
        !analyse::replica_refusals(&mut client, &doc, &exclusive, None)
            .await
            .unwrap()
            .is_empty()
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
    let local = bindings::analyse(&mut client, &doc, &[package(&"modbus".parse().unwrap())])
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

#[sqlx::test(migrations = false)]
async fn publish_refuses_write_interfaces_outside_the_exports(pool: sqlx::PgPool) {
    let database = wamn_test_postgres::url(&pool);
    wamn_control_provision::test_database::system(database.as_str()).await;
    let mut client = connect(database.as_str()).await;
    sqlx::raw_sql("SET ROLE wamn_control")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    let result = sqlx::query(
        "SELECT registry.publish_integration('http',2,NULL,$1,'wamn:connection/http',$2,$3)",
    )
    .bind(&json!({"type":"object","properties":{}}))
    .bind(&vec!["wamn:connection/http@0.1.0"])
    .bind(&vec!["wamn:modbus/write@1.0.0"])
    .fetch_one(&mut client)
    .await;
    assert!(result.is_err());
}

#[sqlx::test(migrations = false)]
async fn project_observation_preserves_the_baseline_and_refuses_unknown_state(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    wamn_platform_identity::test_database::project_platform(database.as_str()).await;
    let mut client = connect(database.as_str()).await;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (wamn_control_provision::sql::ensure_db_owner_role_sql()).to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)
    .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("GRANT wamn_platform TO wamn_db_owner WITH INHERIT TRUE, SET FALSE; ALTER DATABASE {} OWNER TO wamn_db_owner; ALTER SCHEMA catalog OWNER TO wamn_db_owner; GRANT USAGE ON SCHEMA platform TO wamn_db_owner; GRANT SELECT ON ALL TABLES IN SCHEMA catalog, platform TO wamn_db_owner", postgres_protocol::escape::escape_identifier(database.path().trim_start_matches('/'))))).execute(&mut client).await.map(drop).unwrap();
    let fingerprint = (format!("sha256:{}", "a".repeat(64)))
        .parse::<wamn_digest::Digest>()
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
    let observed = analyse::read_project(database.as_str(), &"acme--dev--one".parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        observed.installed["fixture"].current,
        "1.0.0".parse().unwrap()
    );

    sqlx::raw_sql("REVOKE SELECT ON catalog.packages FROM wamn_db_owner")
        .execute(&mut client)
        .await
        .map(drop)
        .unwrap();
    assert!(
        analyse::read_project(database.as_str(), &"acme--dev--one".parse().unwrap())
            .await
            .is_err()
    );
    let mut missing = url::Url::parse(database.as_str()).unwrap();
    missing.set_path("/unprovisioned_fixture");
    assert!(
        analyse::read_project(missing.as_str(), &"acme--dev--one".parse().unwrap())
            .await
            .unwrap()
            .is_none()
    );
}
