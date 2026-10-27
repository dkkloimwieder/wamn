//! Environment declarations resolve only through enabled org connections.
use serde_json::json;
use wamn_control::environment::{bindings, document::EnvironmentDocument};
use wamn_platform_identity::{
    auth_changes::AuthDrops,
    connections::{self, ConnectionWrite},
};
use wamn_schema_generator::PackageManifest;

async fn connect(url: &str) -> Result<tokio_postgres::Client, tokio_postgres::Error> {
    let (client, driver) = tokio_postgres::connect(url, tokio_postgres::NoTls).await?;
    tokio::spawn(driver);
    Ok(client)
}

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
    client.batch_execute("SET ROLE wamn_system; SET app.user_id = '770df186-ac15-579e-b46b-c297cae2011b'; INSERT INTO registry.orgs(id,placement_type) VALUES ('acme','dedicated'); INSERT INTO registry.projects(org,id) VALUES ('acme','one'); INSERT INTO registry.env_policies(org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image) VALUES ('acme','dev','{}',0,1,'1Gi','1','1Gi','image'); INSERT INTO registry.project_envs(org,project,env,secret_name,instance_suffix,policy_name) VALUES ('acme','one','dev','secret','aaaaaaaa','dev'),('acme','one','other','secret','bbbbbbbb','dev')").await.unwrap();
    let tx = client.transaction().await.unwrap();
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await
    .unwrap();
    let _name = connections::write_connection(&tx, "acme", &ConnectionWrite {
        integration:"http".into(), external_system: None, name:"erp".into(),
        transport:"http".into(), exchange:"request".into(),
        parameters:json!({"primary-authority":"https://erp.example", "tls-verification":"verify-authority"}), status:"active".into()
    }, &mut AuthDrops::default()).await.unwrap();
    tx.commit().await.unwrap();
    let id: String = client
        .query_one(
            "SELECT id::text FROM registry.connection WHERE name='erp'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let (authority, authority_driver) =
        tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
            .await
            .unwrap();
    let authority_driver = tokio::spawn(authority_driver);
    authority
        .batch_execute("SET ROLE wamn_control")
        .await
        .unwrap();
    assert_eq!(
        authority
            .query_one("SELECT current_user::text", &[])
            .await
            .unwrap()
            .get::<_, String>(0),
        "wamn_control"
    );
    assert!(
        authority
            .query("SELECT * FROM registry.connection", &[])
            .await
            .is_err()
    );
    let mut doc = document();
    let platform = wamn_control::environment::Platform {
        system_database_url: database.url().into(),
        target: wamn_control::release_chart::Target {
            kubeconfig: "unused".into(),
            context: "unused".into(),
            namespace: "unused".into(),
        },
        chart: "unused".into(),
        release_artifact_base: "unused".into(),
        registry_auth_file: "unused".into(),
        oci_ca_paths: vec![],
        package_artifact_base: None,
        database_namespace: "unused".into(),
        database_host: None,
        database_port: 5432,
        host_group: None,
        pat_issuer: wamn_control::pat_client::PatIssuerConfig::default(),
        events: None,
        auth_nats: None,
    };
    let invalid_file =
        std::env::temp_dir().join(format!("wamn-c35-invalid-binding-{}.k", std::process::id()));
    let kcl = doc.to_kcl();
    assert!(kcl.contains("\"upstream\" = \"erp\""));
    std::fs::write(
        &invalid_file,
        kcl.replace(
            "\"upstream\" = \"erp\"",
            "\"upstream\" = {type = \"http\", definition = {}}",
        ),
    )
    .unwrap();
    let before: String = client
        .query_one(
            "SELECT xmin::text FROM registry.project_envs WHERE env='dev'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(
        wamn_control::environment::apply(&platform, &invalid_file)
            .await
            .is_err()
    );
    assert_eq!(
        client
            .query_one(
                "SELECT xmin::text FROM registry.project_envs WHERE env='dev'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, String>(0),
        before
    );
    std::fs::remove_file(invalid_file).unwrap();
    let packages = [package("http")];
    doc.applications.get_mut("app").unwrap().connections.clear();
    assert_eq!(
        bindings::analyse(&authority, &doc, &packages)
            .await
            .unwrap_err()
            .code,
        "connection_unbound"
    );
    doc = document();
    assert_eq!(
        bindings::analyse(&authority, &doc, &packages)
            .await
            .unwrap_err()
            .code,
        "connection_not_enabled"
    );
    let tx = client.transaction().await.unwrap();
    connections::enable_connection(&tx, "acme", "one", "erp")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        bindings::analyse(&authority, &doc, &[package("s3")])
            .await
            .unwrap_err()
            .code,
        "connection_integration_mismatch"
    );
    assert_eq!(
        bindings::analyse(&authority, &doc, &[package("unregistered")])
            .await
            .unwrap_err()
            .code,
        "integration_not_found"
    );
    for (scope, name) in [
        ("org", "acme"),
        ("project", "one"),
        ("environment", "one/dev"),
    ] {
        let error = bindings::analyse(&authority, &doc, &packages)
            .await
            .unwrap_err();
        assert_eq!(error.code, "integration_not_allowed");
        assert!(error.context.contains(scope));
        let tx = client.transaction().await.unwrap();
        connections::allow_integration(&tx, "acme", scope, name, "http")
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    let resolved = bindings::analyse(&authority, &doc, &packages)
        .await
        .unwrap();
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
        bindings::analyse(&authority, &doc, &[packages[0].clone(), second_package])
            .await
            .unwrap_err()
            .to_string(),
        "exclusive_connection_bound: [\"second\", \"upstream\"]"
    );
    doc.applications.remove("second");
    let tx = client.transaction().await.unwrap();
    tx.batch_execute("SET LOCAL ROLE wamn_control")
        .await
        .unwrap();
    bindings::reserve(&tx, &doc, &packages).await.unwrap();
    tx.commit().await.unwrap();
    let binding_xmin: String = client
        .query_one(
            "SELECT xmin::text FROM registry.environment_connection_binding WHERE env='dev'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let environment_xmin: String = client
        .query_one(
            "SELECT xmin::text FROM registry.project_envs WHERE env='dev'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let replay = client.transaction().await.unwrap();
    replay
        .batch_execute("SET LOCAL ROLE wamn_control")
        .await
        .unwrap();
    bindings::reserve(&replay, &doc, &packages).await.unwrap();
    replay.commit().await.unwrap();
    assert_eq!(
        client
            .query_one(
                "SELECT xmin::text FROM registry.environment_connection_binding WHERE env='dev'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, String>(0),
        binding_xmin
    );
    assert_eq!(
        client
            .query_one(
                "SELECT xmin::text FROM registry.project_envs WHERE env='dev'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, String>(0),
        environment_xmin
    );
    let retained = bindings::analyse(&authority, &doc, &[]).await.unwrap();
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
        bindings::analyse(&authority, &unbound, &[])
            .await
            .unwrap_err()
            .code,
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
        bindings::analyse(&authority, &other, &packages)
            .await
            .unwrap_err()
            .to_string(),
        "exclusive_connection_bound: [\"app\", \"upstream\"]"
    );
    client
        .execute("INSERT INTO registry.target(name,type,status) SELECT 'other',type,status FROM registry.target WHERE name='cloud'", &[])
        .await
        .unwrap();
    doc.target = "other".into();
    assert_eq!(
        bindings::analyse(&authority, &doc, &packages)
            .await
            .unwrap_err()
            .code,
        "environment_target_changed"
    );
    let changed = client.execute("UPDATE registry.project_envs SET target_id=(SELECT id FROM registry.target WHERE name='other') WHERE env='dev'", &[]).await.unwrap_err();
    assert_eq!(
        changed.as_db_error().unwrap().message(),
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
            integration: "gcs".into(),
            external_system: None,
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
    let files = bindings::analyse(&authority, &doc, &[package("gcs")])
        .await
        .unwrap();
    assert!(files[0].resolved.credential_handle.is_none());
    wamn_control::bind_connection::prepare_org_instance(&files[0].resolved).unwrap();
    authority_driver.abort();
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
    client.batch_execute("SET ROLE wamn_system; SET app.user_id = '770df186-ac15-579e-b46b-c297cae2011b'; INSERT INTO registry.orgs(id,placement_type) VALUES ('acme','dedicated'); INSERT INTO registry.projects(org,id) VALUES ('acme','one'); INSERT INTO registry.env_policies(org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image) VALUES ('acme','dev','{}',0,1,'1Gi','1','1Gi','image'); INSERT INTO registry.project_envs(org,project,env,secret_name,instance_suffix,policy_name) VALUES ('acme','one','dev','secret','aaaaaaaa','dev')").await.unwrap();
    let tx = client.transaction().await.unwrap();
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await
    .unwrap();
    let _name = connections::write_connection(&tx, "acme", &ConnectionWrite {
        integration: "http".into(), external_system: None, name: "erp".into(),
        transport: "http".into(), exchange: "request".into(),
        parameters: json!({"primary-authority":"https://erp.example", "tls-verification":"verify-authority"}), status: "active".into(),
    }, &mut AuthDrops::default()).await.unwrap();
    connections::enable_connection(&tx, "acme", "one", "erp")
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
    let id: String = client
        .query_one(
            "SELECT id::text FROM registry.connection WHERE name='erp'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let (authority, authority_driver) =
        tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
            .await
            .unwrap();
    let authority_driver = tokio::spawn(authority_driver);
    authority
        .batch_execute("SET ROLE wamn_control")
        .await
        .unwrap();
    assert_eq!(
        authority
            .query_one("SELECT current_user::text", &[])
            .await
            .unwrap()
            .get::<_, String>(0),
        "wamn_control"
    );
    assert!(
        authority
            .query("SELECT * FROM registry.connection", &[])
            .await
            .is_err()
    );
    let mut doc = document();
    let app = doc.applications.get_mut("app").unwrap();
    app.packages.insert("second_fixture".into());
    app.connections.insert("netsuite".into(), "erp".into());
    let packages = [first, second];
    let tx = client.transaction().await.unwrap();
    tx.batch_execute("SET LOCAL ROLE wamn_control")
        .await
        .unwrap();
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
        bindings::analyse(&authority, &doc, &packages)
            .await
            .unwrap_err()
            .to_string(),
        "exclusive_connection_bound: [\"app\", \"upstream\"]"
    );
    authority_driver.abort();
    driver.abort();
}

#[tokio::test]
async fn projection_retains_references_and_reapply_repairs_without_history_writes() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let environment = wamn_project_state::test_database::tenant_platform();
    let mut system = connect(database.url()).await.unwrap();
    system.batch_execute("SET ROLE wamn_system; INSERT INTO registry.orgs(id,placement_type) VALUES ('acme','dedicated'); INSERT INTO registry.projects(org,id) VALUES ('acme','one'); UPDATE registry.meta SET platform_domain='example.test'; INSERT INTO registry.env_policies(org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image) VALUES ('acme','dev','{}',0,1,'1Gi','1','1Gi','image'); INSERT INTO registry.project_envs(org,project,env,secret_name,instance_suffix,policy_name) VALUES ('acme','one','dev','secret','aaaaaaaa','dev')").await.unwrap();
    let project = connect(environment.url()).await.unwrap();
    project
        .batch_execute("CREATE SCHEMA IF NOT EXISTS wamn_run")
        .await
        .unwrap();
    project
        .batch_execute(&wamn_control_provision::sql::grant_administration_surface_sql("wamn_run"))
        .await
        .unwrap();
    let doc = document();
    let tx = system.transaction().await.unwrap();
    tx.batch_execute("SET LOCAL ROLE wamn_control")
        .await
        .unwrap();
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await
    .unwrap();
    connections::write_external_system(
        &tx,
        "acme",
        &connections::ExternalSystemWrite {
            integration: "http".into(),
            name: "shared".into(),
            parameters: json!({}),
            status: "active".into(),
        },
    )
    .await
    .unwrap();
    for name in ["erp", "other"] {
        connections::write_connection(&tx,"acme",&ConnectionWrite {
            integration:"http".into(),external_system:Some("shared".into()),name:name.into(),transport:"http".into(),exchange:"request".into(),
            parameters:json!({"primary-authority":"https://erp.example","tls-verification":"verify-authority"}),status:"active".into(),
        },&mut AuthDrops::default()).await.unwrap();
        connections::enable_connection(&tx, "acme", "one", name)
            .await
            .unwrap();
    }
    for (scope, id) in [
        ("org", "acme"),
        ("project", "one"),
        ("environment", "one/dev"),
    ] {
        connections::allow_integration(&tx, "acme", scope, id, "http")
            .await
            .unwrap();
    }
    bindings::reserve(&tx, &doc, &[package("http")])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let _ = bindings::project_authority(database.url(), environment.url(), &doc)
        .await
        .unwrap();
    project.batch_execute("CREATE TABLE public.connection_reference (connection_id uuid REFERENCES platform.connection(id), external_system_id uuid REFERENCES platform.external_system(id)); INSERT INTO public.connection_reference SELECT id,external_system_id FROM platform.connection WHERE name='erp'").await.unwrap();
    let history_sql = "SELECT (SELECT count(*) FROM platform.connection_history) + (SELECT count(*) FROM platform.external_system_history) + (SELECT count(*) FROM platform.integration_allow_history) + (SELECT count(*) FROM platform.project_connection_history) + (SELECT count(*) FROM platform.user_history)";
    let history: i64 = project.query_one(history_sql, &[]).await.unwrap().get(0);
    let _ = bindings::project_authority(database.url(), environment.url(), &doc)
        .await
        .unwrap();
    assert_eq!(
        project
            .query_one(history_sql, &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        history,
        "replay appends no history"
    );
    let tx = system.transaction().await.unwrap();
    tx.batch_execute("SET LOCAL ROLE wamn_control")
        .await
        .unwrap();
    connections::disable_connection(&tx, "acme", "one", "erp")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let _ = bindings::project_authority(database.url(), environment.url(), &doc)
        .await
        .unwrap();
    assert_eq!(project.query_one("SELECT c.status FROM platform.connection c JOIN public.connection_reference r ON r.connection_id=c.id",&[]).await.unwrap().get::<_,String>(0),"disabled");
    assert_eq!(
        project
            .query_one(
                "SELECT status FROM platform.external_system WHERE name='shared'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, String>(0),
        "active",
        "another enabled connection retains the system"
    );
    system.batch_execute("SET ROLE wamn_control").await.unwrap();
    assert_eq!(
        bindings::analyse(&system, &doc, &[package("http")])
            .await
            .unwrap_err()
            .code,
        "connection_not_enabled"
    );
    let tx = system.transaction().await.unwrap();
    connections::disable_connection(&tx, "acme", "one", "other")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let _ = bindings::project_authority(database.url(), environment.url(), &doc)
        .await
        .unwrap();
    assert_eq!(project.query_one("SELECT s.status FROM platform.external_system s JOIN public.connection_reference r ON r.external_system_id=s.id",&[]).await.unwrap().get::<_,String>(0),"disabled");
    assert!(
        project
            .query("SELECT FROM platform.project_connection", &[])
            .await
            .unwrap()
            .is_empty()
    );
    // Repair a stale route projection through the same reconciliation that env apply calls.
    project
        .batch_execute(&format!(
            "BEGIN; {} UPDATE platform.connection SET status='active' WHERE name='erp'; COMMIT;",
            wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning
            )
        ))
        .await
        .unwrap();
    let _ = bindings::project_authority(database.url(), environment.url(), &doc)
        .await
        .unwrap();
    assert_eq!(
        project
            .query_one(
                "SELECT status FROM platform.connection WHERE name='erp'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, String>(0),
        "disabled"
    );
    let history: i64 = project.query_one(history_sql, &[]).await.unwrap().get(0);
    let _ = bindings::project_authority(database.url(), environment.url(), &doc)
        .await
        .unwrap();
    assert_eq!(
        project
            .query_one(history_sql, &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        history
    );
}
