//! Environment declarations resolve only through enabled org connections.
use serde_json::json;
use wamn_control::environment::{bindings, document::EnvironmentDocument};
use wamn_platform_identity::{
    auth_changes::AuthDrops,
    connections::{self, ConnectionWrite},
};
use wamn_schema_generator::PackageManifest;

fn document() -> EnvironmentDocument {
    EnvironmentDocument::from_json(
        &serde_json::to_vec(&json!({
            "org":"acme", "project":"one", "env":"dev", "target":"cloud",
            "release":"none", "route_host":"one.example", "policy":"dev",
            "applications":{"app":{"packages":["fixture"], "connections":{"upstream":"erp"}}}
        }))
        .unwrap(),
    )
    .unwrap()
}
fn package(integration: &str) -> PackageManifest {
    PackageManifest::from_slice(
        &serde_json::to_vec(&json!({
            "package":{"id":"fixture","version":"0.1.0"},
        "required_platform_policy_contract":{"id":"fixture_data_access","state":"satisfied"},
            "connections":{"upstream":{"integration":integration,"mode":"read"}},
            "models":{}, "components":{}
        }))
        .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn apply_refuses_each_invalid_binding_and_target_change() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let (mut client, driver) = tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let driver = tokio::spawn(driver);
    client.batch_execute("SET ROLE wamn_system; INSERT INTO registry.orgs(id,placement_type) VALUES ('acme','dedicated'); INSERT INTO registry.projects(org,id) VALUES ('acme','one'); INSERT INTO registry.env_policies(org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image) VALUES ('acme','dev','{}',0,1,'1Gi','1','1Gi','image'); INSERT INTO registry.project_envs(org,project,env,secret_name,instance_suffix,policy_name) VALUES ('acme','one','dev','secret','aaaaaaaa','dev'),('acme','one','other','secret','bbbbbbbb','dev')").await.unwrap();
    let tx = client.transaction().await.unwrap();
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await
    .unwrap();
    let id = connections::write_connection(&tx, "acme", &ConnectionWrite {
        id:None, integration:"http".into(), external_system_id:None, name:"erp".into(),
        transport:"http".into(), exchange:"request".into(),
        parameters:json!({"primary-authority":"https://erp.example", "tls-verification":"verify-authority"}), status:"active".into()
    }, &mut AuthDrops::default()).await.unwrap();
    tx.commit().await.unwrap();
    let mut doc = document();
    let packages = [package("http")];
    doc.applications.get_mut("app").unwrap().connections.clear();
    assert_eq!(
        bindings::analyse(&client, &doc, &packages)
            .await
            .unwrap_err()
            .kind,
        "connection_unbound"
    );
    doc = document();
    assert_eq!(
        bindings::analyse(&client, &doc, &packages)
            .await
            .unwrap_err()
            .kind,
        "connection_not_enabled"
    );
    let tx = client.transaction().await.unwrap();
    connections::enable_connection(&tx, "acme", "one", &id)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        bindings::analyse(&client, &doc, &[package("s3")])
            .await
            .unwrap_err()
            .kind,
        "connection_integration_mismatch"
    );
    assert_eq!(
        bindings::analyse(&client, &doc, &[package("unregistered")])
            .await
            .unwrap_err()
            .kind,
        "integration_not_found"
    );
    for (scope, name) in [
        ("org", "acme"),
        ("project", "one"),
        ("environment", "one/dev"),
    ] {
        let error = bindings::analyse(&client, &doc, &packages)
            .await
            .unwrap_err();
        assert_eq!(error.kind, "integration_not_allowed");
        assert!(error.context.contains(scope));
        let tx = client.transaction().await.unwrap();
        connections::allow_integration(&tx, "acme", scope, name, "http")
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    let resolved = bindings::analyse(&client, &doc, &packages).await.unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].connection_id, id);
    // A test-only exclusive flag exercises the rule without inventing a device connector.
    client
        .execute(
            "UPDATE registry.integrations SET exclusive = true WHERE code = 'http'",
            &[],
        )
        .await
        .unwrap();
    let mut second_package = packages[0].clone();
    second_package.package.id = "other_fixture".into();
    let mut second = doc.applications["app"].clone();
    second.packages = ["other_fixture".to_owned()].into();
    doc.applications.insert("second".into(), second);
    assert_eq!(
        bindings::analyse(&client, &doc, &[packages[0].clone(), second_package])
            .await
            .unwrap_err()
            .to_string(),
        "exclusive_connection_bound: second.upstream"
    );
    doc.applications.remove("second");
    let tx = client.transaction().await.unwrap();
    bindings::reserve(&tx, &doc, &packages).await.unwrap();
    tx.commit().await.unwrap();
    let retained = bindings::analyse(&client, &doc, &[]).await.unwrap();
    assert_eq!(
        retained[0].mode,
        wamn_schema_generator::ConnectionMode::Read
    );
    let mut unbound = doc.clone();
    unbound
        .applications
        .get_mut("app")
        .unwrap()
        .connections
        .clear();
    assert_eq!(
        bindings::analyse(&client, &unbound, &[])
            .await
            .unwrap_err()
            .kind,
        "connection_unbound"
    );
    let tx = client.transaction().await.unwrap();
    connections::allow_integration(&tx, "acme", "environment", "one/other", "http")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut other = doc.clone();
    other.env = "other".into();
    assert_eq!(
        bindings::analyse(&client, &other, &packages)
            .await
            .unwrap_err()
            .to_string(),
        "exclusive_connection_bound: app.upstream"
    );
    client
        .execute("INSERT INTO registry.target(name,type,status) SELECT 'other',type,status FROM registry.target WHERE name='cloud'", &[])
        .await
        .unwrap();
    doc.target = "other".into();
    assert_eq!(
        bindings::analyse(&client, &doc, &packages)
            .await
            .unwrap_err()
            .kind,
        "environment_target_changed"
    );
    let error = client
        .execute(
            "UPDATE registry.project_envs SET target_id = NULL WHERE env = 'dev'",
            &[],
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.as_db_error().unwrap().message(),
        "environment_target_changed"
    );
    let tx = client.transaction().await.unwrap();
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await
    .unwrap();
    let files = connections::write_connection(
        &tx,
        "acme",
        &ConnectionWrite {
            id: None,
            integration: "gcs".into(),
            external_system_id: None,
            name: "files".into(),
            transport: "http".into(),
            exchange: "request".into(),
            parameters: json!({"provider":"gcs","container":"files","prefix":"app/"}),
            status: "active".into(),
        },
        &mut AuthDrops::default(),
    )
    .await
    .unwrap();
    connections::enable_connection(&tx, "acme", "one", &files)
        .await
        .unwrap();
    for (scope, name) in [
        ("org", "acme"),
        ("project", "one"),
        ("environment", "one/dev"),
    ] {
        connections::allow_integration(&tx, "acme", scope, name, "gcs")
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();
    doc.target = "cloud".into();
    doc.applications
        .get_mut("app")
        .unwrap()
        .connections
        .insert("upstream".into(), "files".into());
    let files = bindings::analyse(&client, &doc, &[package("gcs")])
        .await
        .unwrap();
    assert!(files[0].resolved.credential_handle.is_none());
    wamn_control::bind_connection::prepare_org_instance(&files[0].resolved).unwrap();
    driver.abort();
}

#[tokio::test]
async fn two_packages_bind_distinct_aliases_to_one_shared_org_connection() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let (mut client, driver) = tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
        .await
        .unwrap();
    let driver = tokio::spawn(driver);
    client.batch_execute("SET ROLE wamn_system; INSERT INTO registry.orgs(id,placement_type) VALUES ('acme','dedicated'); INSERT INTO registry.projects(org,id) VALUES ('acme','one'); INSERT INTO registry.env_policies(org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image) VALUES ('acme','dev','{}',0,1,'1Gi','1','1Gi','image'); INSERT INTO registry.project_envs(org,project,env,secret_name,instance_suffix,policy_name) VALUES ('acme','one','dev','secret','aaaaaaaa','dev')").await.unwrap();
    let tx = client.transaction().await.unwrap();
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await
    .unwrap();
    let id = connections::write_connection(&tx, "acme", &ConnectionWrite {
        id: None, integration: "http".into(), external_system_id: None, name: "erp".into(),
        transport: "http".into(), exchange: "request".into(),
        parameters: json!({"primary-authority":"https://erp.example", "tls-verification":"verify-authority"}), status: "active".into(),
    }, &mut AuthDrops::default()).await.unwrap();
    connections::enable_connection(&tx, "acme", "one", &id)
        .await
        .unwrap();
    for (scope, name) in [
        ("org", "acme"),
        ("project", "one"),
        ("environment", "one/dev"),
    ] {
        connections::allow_integration(&tx, "acme", scope, name, "http")
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();
    let first = package("http");
    let mut second = package("http");
    second.package.id = "second_fixture".into();
    let declaration = second.connections.remove("upstream").unwrap();
    second.connections.insert("netsuite".into(), declaration);
    let mut doc = document();
    let app = doc.applications.get_mut("app").unwrap();
    app.packages.insert("second_fixture".into());
    app.connections.insert("netsuite".into(), "erp".into());
    let packages = [first, second];
    let tx = client.transaction().await.unwrap();
    let resolved = bindings::reserve(&tx, &doc, &packages).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(resolved.len(), 2);
    assert_eq!(
        resolved
            .iter()
            .map(|binding| binding.alias.as_str())
            .collect::<Vec<_>>(),
        ["netsuite", "upstream"]
    );
    for binding in &resolved {
        assert_eq!(binding.application, "app");
        assert_eq!(binding.connection_id, id);
    }
    let snapshot = [("erp".into(), resolved[0].resolved.clone())].into();
    let local = wamn_control::environment::analyse::resolve_connections(&doc, &snapshot).unwrap();
    assert_eq!(local.len(), 2);
    assert_eq!(local["app/netsuite"], local["app/upstream"]);
    client
        .execute(
            "UPDATE registry.integrations SET exclusive = true WHERE code = 'http'",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(
        bindings::analyse(&client, &doc, &packages)
            .await
            .unwrap_err()
            .to_string(),
        "exclusive_connection_bound: app.upstream"
    );
    driver.abort();
}
