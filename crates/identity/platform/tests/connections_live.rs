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
        integration: integration.into(),
        external_system: None,
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
    tx.batch_execute("SET LOCAL ROLE wamn_control")
        .await
        .unwrap();
}

#[tokio::test]
async fn write_connection_refuses_an_unread_http_key() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut client = connect(database.url()).await;
    client.batch_execute("SET ROLE wamn_system; SET app.user_id = '770df186-ac15-579e-b46b-c297cae2011b'; INSERT INTO registry.orgs(id, placement_type) VALUES ('acme','dedicated')").await.unwrap();
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
            "SELECT count(*) FROM registry.connection_rows('acme') WHERE org = 'acme'",
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
async fn a_catalog_row_without_a_connector_refuses_as_unavailable() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut client = connect(database.url()).await;
    client.batch_execute(&format!(
        "SET ROLE wamn_system; {} INSERT INTO registry.orgs(id,placement_type) VALUES ('acme','dedicated'); INSERT INTO registry.integration_type(code,runtime_type,exclusive) VALUES ('unimplemented','native',false)",
        wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ),
    )).await.unwrap();
    let tx = client.transaction().await.unwrap();
    bind(&tx).await;
    let mut drops = AuthDrops::default();
    assert_eq!(
        connections::write_connection(
            &tx,
            "acme",
            &connection("unimplemented", "missing"),
            &mut drops
        )
        .await
        .unwrap_err()
        .code,
        "integration_unavailable"
    );
    assert!(drops.is_empty());
}

#[tokio::test]
async fn identical_configuration_writes_keep_xmin_and_generation() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut client = connect(database.url()).await;
    client.batch_execute(&format!(
        "SET ROLE wamn_system; {} INSERT INTO registry.orgs(id,placement_type) VALUES ('acme','dedicated')",
        wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ),
    )).await.unwrap();
    let mut system = ExternalSystemWrite {
        integration: "http".into(),
        name: "erp".into(),
        parameters: json!({}),
        status: "active".into(),
    };
    let mut http = connection("http", "erp");
    http.external_system = Some("erp".into());
    let mut snapshots = Vec::new();
    for attempt in 0..3 {
        if attempt == 2 {
            system.parameters = json!({"region":"second"});
            http.parameters["primary-authority"] = "https://second.invalid".into();
        }
        let tx = client.transaction().await.unwrap();
        bind(&tx).await;
        let mut drops = AuthDrops::default();
        connections::write_external_system(&tx, "acme", &system)
            .await
            .unwrap();
        connections::write_connection(&tx, "acme", &http, &mut drops)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        snapshots.push(client.query_one("SELECT s.xmin::text,c.xmin::text,c.current_generation FROM registry.external_system s JOIN registry.connection c ON c.external_system_id=s.id WHERE c.org='acme' AND c.name='erp'", &[]).await.unwrap());
    }
    for column in [0, 1] {
        assert_eq!(
            snapshots[0].get::<_, String>(column),
            snapshots[1].get::<_, String>(column)
        );
        assert_ne!(
            snapshots[1].get::<_, String>(column),
            snapshots[2].get::<_, String>(column)
        );
    }
    assert_eq!(snapshots[0].get::<_, i64>(2), snapshots[1].get::<_, i64>(2));
    assert_eq!(
        snapshots[2].get::<_, i64>(2),
        snapshots[1].get::<_, i64>(2) + 1
    );
}
