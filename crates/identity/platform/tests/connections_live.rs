//! Org connection authority, immutable generations, import and project projection.

use serde_json::json;
use tokio_postgres::{Client, NoTls};
use wamn_platform_identity::auth_changes::AuthDrops;
use wamn_platform_identity::connections::{self, ConnectionWrite, ExternalSystemWrite};

async fn connect(url: &str) -> Client {
    let (client, driver) = tokio_postgres::connect(url, NoTls).await.unwrap();
    tokio::spawn(async move {
        driver.await.unwrap();
    });
    client
}

fn connection(integration: &str, name: &str) -> ConnectionWrite {
    ConnectionWrite {
        id: None,
        integration: integration.into(),
        external_system_id: None,
        name: name.into(),
        transport: "http".into(),
        exchange: "request".into(),
        parameters: match integration {
            "http" => {
                json!({"primary-authority":"https://example.invalid", "tls-verification":"verify-authority"})
            }
            "gcs" => json!({"provider":"gcs", "container":"bucket", "prefix":"test"}),
            _ => {
                json!({"provider":"s3", "endpoint":"https://example.invalid", "container":"bucket", "prefix":"test"})
            }
        },
        status: "active".into(),
    }
}

async fn bind(tx: &tokio_postgres::Transaction<'_>) {
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await
    .unwrap();
}

#[tokio::test]
async fn write_connection_refuses_an_unread_http_key() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut client = connect(database.url()).await;
    client.batch_execute("SET ROLE wamn_system; INSERT INTO registry.orgs(id, placement_type) VALUES ('acme','dedicated')").await.unwrap();
    let tx = client.transaction().await.unwrap();
    bind(&tx).await;
    let mut drops = AuthDrops::default();
    let mut http = connection("http", "erp");
    http.parameters["headers"] = json!("unused");
    assert_eq!(
        connections::write_connection(&tx, "acme", &http, &mut drops)
            .await
            .unwrap_err()
            .to_string(),
        "invalid_input: parameters"
    );
    assert!(drops.is_empty());
    assert_eq!(
        tx.query_one(
            "SELECT count(*) FROM registry.connection WHERE org = 'acme'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    http.parameters.as_object_mut().unwrap().remove("headers");
    connections::write_connection(&tx, "acme", &http, &mut drops)
        .await
        .unwrap();
}

#[tokio::test]
async fn fresh_connections_keep_principals_generations_and_enabled_projections() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut client = connect(database.url()).await;
    client.batch_execute("SET ROLE wamn_system; INSERT INTO registry.orgs(id, placement_type) VALUES ('acme','dedicated'); INSERT INTO registry.projects(org,id) VALUES ('acme','one'),('acme','two')").await.unwrap();
    let tx = client.transaction().await.unwrap();
    bind(&tx).await;
    let mut drops = AuthDrops::default();
    let mut system = ExternalSystemWrite {
        id: None,
        integration: "http".into(),
        name: "erp".into(),
        parameters: json!({}),
        status: "active".into(),
    };
    let system_id = connections::write_external_system(&tx, "acme", &system)
        .await
        .unwrap();
    let mut http = connection("http", "erp");
    http.external_system_id = Some(system_id.clone());
    let id = connections::write_connection(&tx, "acme", &http, &mut drops)
        .await
        .unwrap();
    let row = tx.query_one("SELECT p.type, (SELECT count(*) FROM identity.project_roles r WHERE r.principal_id = p.id), c.current_generation FROM registry.connection c JOIN identity.principals p ON p.id = c.service_principal_id WHERE c.id = $1::text::uuid", &[&id]).await.unwrap();
    assert_eq!(row.get::<_, String>(0), "service");
    assert_eq!(row.get::<_, i64>(1), 0);
    assert_eq!(row.get::<_, i64>(2), 1);
    assert!(!drops.is_empty());
    for integration in ["gcs", "s3"] {
        connections::write_connection(
            &tx,
            "acme",
            &connection(integration, integration),
            &mut drops,
        )
        .await
        .unwrap();
    }
    connections::enable_connection(&tx, "acme", "one", &id)
        .await
        .unwrap();
    assert_eq!(
        connections::projection(&tx, "acme", "one", "dev")
            .await
            .unwrap()
            .connections
            .len(),
        1
    );
    assert!(
        connections::projection(&tx, "acme", "two", "dev")
            .await
            .unwrap()
            .is_empty()
    );
    let mut mismatch = connection("gcs", "mismatch");
    mismatch.external_system_id = Some(system_id.clone());
    assert_eq!(
        connections::write_connection(&tx, "acme", &mismatch, &mut drops)
            .await
            .unwrap_err()
            .kind,
        "integration_mismatch"
    );
    system.id = Some(system_id);
    system.name = "renamed".into();
    assert_eq!(
        connections::write_external_system(&tx, "acme", &system)
            .await
            .unwrap_err()
            .kind,
        "referenced"
    );
    http.id = Some(id.clone());
    http.name = "renamed".into();
    assert_eq!(
        connections::write_connection(&tx, "acme", &http, &mut drops)
            .await
            .unwrap_err()
            .kind,
        "referenced"
    );
    http.name = "erp".into();
    http.parameters["primary-authority"] = "https://second.invalid".into();
    connections::write_connection(&tx, "acme", &http, &mut drops)
        .await
        .unwrap();
    assert_eq!(tx.query_one("SELECT count(*) FROM registry.connection_generation WHERE connection_id = $1::text::uuid", &[&id]).await.unwrap().get::<_,i64>(0), 2);
    assert_eq!(
        connections::allow_integration(&tx, "acme", "project", "one", "http")
            .await
            .unwrap_err()
            .kind,
        "integration_not_allowed"
    );
    connections::allow_integration(&tx, "acme", "org", "acme", "http")
        .await
        .unwrap();
    connections::allow_integration(&tx, "acme", "project", "one", "http")
        .await
        .unwrap();
    let file = json!({"orgs":{"acme":{"erp":"first-secret"}},"projects":{"one":{"legacy":"project-secret"}}});
    connections::import_credentials(&tx, "acme", &file.to_string())
        .await
        .unwrap();
    assert_eq!(
        connections::import_credentials(&tx, "acme", &file.to_string())
            .await
            .unwrap_err()
            .kind,
        "already_imported"
    );
    assert_eq!(
        connections::rotate(&tx, "acme", &id, "second-secret")
            .await
            .unwrap(),
        4
    );
    let secrets: Vec<String> = tx.query("SELECT k.secret FROM registry.connection_generation g JOIN identity.credential k ON k.id = g.secret_id WHERE g.connection_id = $1::text::uuid ORDER BY g.generation", &[&id]).await.unwrap().iter().map(|row| row.get(0)).collect();
    assert_eq!(secrets, ["first-secret", "second-secret"]);
    tx.commit().await.unwrap();
    assert!(
        client
            .execute(
                "UPDATE registry.connection_generation SET parameters = '{}'",
                &[]
            )
            .await
            .is_err()
    );
    assert!(
        client
            .execute("DELETE FROM identity.credential", &[])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn migration_preserves_every_org_connection_and_its_definition() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut client = connect(database.url()).await;
    client.batch_execute("SET ROLE wamn_system; DROP TABLE registry.credential_import, registry.integration_allow, registry.project_connection, registry.connection_generation, identity.credential, registry.connection, registry.external_system CASCADE; DROP FUNCTION registry.reject_connection_generation_change(); INSERT INTO registry.orgs(id, placement_type) VALUES ('acme','dedicated'); INSERT INTO registry.org_connections(org,name,type,definition) VALUES ('acme','first','http','{\"primary-authority\":\"https://one.invalid\"}'), ('acme','second','http','{\"primary-authority\":\"https://two.invalid\"}')").await.unwrap();
    let tx = client.transaction().await.unwrap();
    bind(&tx).await;
    tx.batch_execute(include_str!(
        "../../../../deploy/sql/migrations/system/0030_org_connection_authority.sql"
    ))
    .await
    .unwrap();
    let row = tx.query_one("SELECT (SELECT count(*) FROM registry.org_connections), (SELECT count(*) FROM registry.connection c JOIN registry.org_connections o ON (o.org,o.name,o.definition) = (c.org,c.name,c.parameters)), (SELECT count(DISTINCT service_principal_id) FROM registry.connection)", &[]).await.unwrap();
    assert_eq!(
        (
            row.get::<_, i64>(0),
            row.get::<_, i64>(1),
            row.get::<_, i64>(2)
        ),
        (2, 2, 2)
    );
    tx.commit().await.unwrap();
}
