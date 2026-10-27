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
async fn fresh_connections_keep_principals_generations_and_enabled_projections() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut client = connect(database.url()).await;
    client.batch_execute("SET ROLE wamn_system; INSERT INTO registry.orgs(id, placement_type) VALUES ('acme','dedicated'),('other','dedicated'); INSERT INTO registry.projects(org,id) VALUES ('acme','one'),('acme','two')").await.unwrap();
    let tx = client.transaction().await.unwrap();
    bind(&tx).await;
    let mut drops = AuthDrops::default();
    let system = ExternalSystemWrite {
        integration: "http".into(),
        name: "erp".into(),
        parameters: json!({}),
        status: "active".into(),
    };
    let system_id = connections::write_external_system(&tx, "acme", &system)
        .await
        .unwrap();
    let mut http = connection("http", "erp");
    http.external_system = Some(system_id.clone());
    assert_eq!(
        connections::write_connection(&tx, "acme", &http, &mut drops)
            .await
            .unwrap(),
        "erp"
    );
    let id: String = tx
        .query_one(
            "SELECT id::text FROM registry.connection_rows('acme') WHERE name='erp'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let row = tx.query_one("SELECT p.type, (SELECT count(*) FROM identity.project_roles r WHERE r.principal_id = p.id), c.current_generation FROM registry.connection_rows('acme') c JOIN identity.principals p ON p.id = c.service_principal_id WHERE c.id = $1::text::uuid", &[&id]).await.unwrap();
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
    connections::enable_connection(&tx, "acme", "one", "erp")
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
    mismatch.external_system = Some(system_id.clone());
    assert_eq!(
        connections::write_connection(&tx, "acme", &mismatch, &mut drops)
            .await
            .unwrap_err()
            .code,
        "integration_mismatch"
    );
    for (operation, payload) in [
        (
            "external_system.write",
            json!({"id": system_id, "name": "renamed", "integration": "http", "parameters": {}, "status": "active"}),
        ),
        (
            "connection.write",
            json!({"id": id, "name": "renamed", "integration": "http", "transport": "http", "exchange": "request", "parameters": http.parameters, "status": "active"}),
        ),
    ] {
        let error = connections::write(&tx, "acme", operation, &payload.to_string(), &mut drops)
            .await
            .unwrap_err();
        assert_eq!(
            error.code, "invalid_input",
            "an id cannot select a name to change"
        );
        let result: String = tx
            .query_one(
                "SELECT registry.configure_connection($1,$2,$3::text::jsonb)::text",
                &[&"acme", &operation, &payload.to_string()],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&result).unwrap()["error"],
            "invalid_input"
        );
    }
    let mut new_system = ExternalSystemWrite {
        name: "renamed".into(),
        ..system
    };
    assert_eq!(
        connections::write_external_system(&tx, "acme", &new_system)
            .await
            .unwrap(),
        "renamed"
    );
    new_system.parameters = json!({"region":"second"});
    connections::write_external_system(&tx, "acme", &new_system)
        .await
        .unwrap();
    let mut new_connection = connection("http", "renamed");
    connections::write_connection(&tx, "acme", &new_connection, &mut drops)
        .await
        .unwrap();
    new_connection.parameters["primary-authority"] = "https://new.invalid".into();
    connections::write_connection(&tx, "acme", &new_connection, &mut drops)
        .await
        .unwrap();
    assert_eq!(
        tx.query_one(
            "SELECT count(*) FROM registry.connection_rows('acme') WHERE name IN ('erp','renamed')",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        2
    );
    assert_eq!(tx.query_one("SELECT count(*) FROM registry.connection_system_rows('acme') WHERE name IN ('erp','renamed')", &[]).await.unwrap().get::<_,i64>(0), 2);
    http.parameters["primary-authority"] = "https://second.invalid".into();
    connections::write_connection(&tx, "acme", &http, &mut drops)
        .await
        .unwrap();
    assert_eq!(tx.query_one("SELECT current_generation FROM registry.connection_rows('acme') WHERE id = $1::text::uuid", &[&id]).await.unwrap().get::<_,i64>(0), 2);
    assert_eq!(
        connections::allow_integration(&tx, "acme", "project", "one", "http")
            .await
            .unwrap_err()
            .code,
        "integration_not_allowed"
    );
    connections::allow_integration(&tx, "acme", "org", "acme", "http")
        .await
        .unwrap();
    connections::allow_integration(&tx, "acme", "project", "one", "http")
        .await
        .unwrap();
    connections::write_connection(&tx, "other", &connection("http", "erp"), &mut drops)
        .await
        .unwrap();
    let foreign_generation: i64 = tx
        .query_one(
            "SELECT current_generation FROM registry.connection_rows('other') WHERE name='erp'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    for file in [
        json!({"orgs":{"other":{"erp":"foreign-secret"}},"projects":{}}),
        json!({"orgs":{"acme":{"erp":"first-secret"},"other":{"erp":"foreign-secret"}},"projects":{}}),
    ] {
        let error = connections::import_credentials(&tx, "acme", &file.to_string())
            .await
            .unwrap_err();
        assert_eq!(error.code, "invalid_input");
        assert_eq!(error.field, "org");
    }
    assert_eq!(
        tx.query_one(
            "SELECT current_generation FROM registry.connection_rows('other') WHERE name='erp'",
            &[],
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        foreign_generation
    );
    // String project maps, missing connections and conflicting maps refuse before writes.
    for file in [
        json!({"orgs":{},"projects":{"one":{"old":"project-secret"}}}),
        json!({"orgs":{},"projects":{"one":{"old":{"connection":"missing","secret":"secret"}}}}),
        json!({"orgs":{"acme":{"erp":"first-secret"}},"projects":{"one":{"old":{"connection":"erp","secret":"different"}}}}),
    ] {
        assert!(
            connections::import_credentials(&tx, "acme", &file.to_string())
                .await
                .is_err()
        );
    }
    let file = json!({"orgs":{"acme":{"erp":"first-secret"}},"projects":{"one":{"old":{"connection":"erp","secret":"first-secret"}}}});
    connections::import_credentials(&tx, "acme", &file.to_string())
        .await
        .unwrap();
    assert_eq!(
        connections::import_credentials(&tx, "acme", &file.to_string())
            .await
            .unwrap_err()
            .code,
        "already_imported"
    );
    let previous: i64 = tx
        .query_one(
            "SELECT current_generation FROM registry.connection_rows('acme') WHERE name='erp'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        connections::rotate(&tx, "acme", "erp", "second-secret")
            .await
            .unwrap(),
        previous + 1
    );
    assert_eq!(
        tx.query_one(
            "SELECT current_generation FROM registry.connection_rows('acme') WHERE name='erp'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        previous + 1
    );
    connections::enable_connection(&tx, "acme", "two", "erp")
        .await
        .unwrap();
    connections::disable_connection(&tx, "acme", "one", "erp")
        .await
        .unwrap();
    assert!(
        connections::projection(&tx, "acme", "one", "dev")
            .await
            .unwrap()
            .connections
            .is_empty()
    );
    assert_eq!(
        connections::projection(&tx, "acme", "two", "dev")
            .await
            .unwrap()
            .connections
            .len(),
        1
    );
    http.status = "disabled".into();
    connections::write_connection(&tx, "acme", &http, &mut drops)
        .await
        .unwrap();
    assert_eq!(
        connections::projection(&tx, "acme", "two", "dev")
            .await
            .unwrap()
            .connections[0]
            .status,
        "disabled"
    );
    tx.batch_execute("SET LOCAL ROLE wamn_system")
        .await
        .unwrap();
    let secrets: Vec<String> = tx.query("SELECT k.secret FROM registry.connection_generation g JOIN identity.credential k ON k.id = g.secret_id WHERE g.connection_id = $1::text::uuid ORDER BY g.generation", &[&id]).await.unwrap().iter().map(|row| row.get(0)).collect();
    assert_eq!(secrets, ["first-secret", "second-secret"]);
    for table in ["registry.connection", "registry.external_system"] {
        tx.batch_execute("SAVEPOINT immutable_name").await.unwrap();
        assert!(
            tx.execute(
                &format!("UPDATE {table} SET name='forged' WHERE org='acme' AND name='erp'"),
                &[]
            )
            .await
            .is_err()
        );
        tx.batch_execute("ROLLBACK TO SAVEPOINT immutable_name")
            .await
            .unwrap();
    }
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
    tx.batch_execute("SET LOCAL ROLE wamn_system")
        .await
        .unwrap();
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

#[tokio::test]
async fn a_catalog_row_without_a_connector_refuses_as_unavailable() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut client = connect(database.url()).await;
    client.batch_execute("SET ROLE wamn_system; INSERT INTO registry.orgs(id,placement_type) VALUES ('acme','dedicated'); INSERT INTO registry.integrations(code,form,exclusive) VALUES ('unimplemented','native',false)").await.unwrap();
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
async fn package_binding_backfill_preserves_each_release_owner_and_refuses_conflicts() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_project_state::test_database::tenant_platform();
    let mut client = connect(database.url()).await;
    let tx = client.transaction().await.unwrap();
    tx.batch_execute("CREATE SCHEMA IF NOT EXISTS wamn_run; ALTER TABLE catalog.connection_bindings DROP CONSTRAINT connection_bindings_package_id_nonempty; ALTER TABLE catalog.connection_bindings ALTER COLUMN package_id SET DEFAULT '';").await.unwrap();
    let component = format!("sha256:{}", "a".repeat(64));
    let release = json!({"components":[
        {"package-id":"first","descriptor":{"component-digest":component}},
        {"package-id":"second","descriptor":{"component-digest":component}},
    ]})
    .to_string();
    let manifest: String = tx.query_one("INSERT INTO catalog.releases(tenant_id,manifest_digest,canonical_bytes) VALUES ('t1','sha256:' || encode(sha256(convert_to($1,'UTF8')),'hex'),convert_to($1,'UTF8')) RETURNING manifest_digest", &[&release]).await.unwrap().get(0);
    tx.execute("INSERT INTO catalog.connection_requirements(tenant_id,component_digest,store_alias,requirement_json,requirement_hash) VALUES ('t1',$1,'upstream','{}',$1)", &[&component]).await.unwrap();
    tx.batch_execute("INSERT INTO catalog.connection_instances(tenant_id,environment,instance_id,requirement_type,contract) VALUES ('t1','dev','chosen','http','http'),('t1','dev','conflict','http','http')").await.unwrap();
    tx.execute("INSERT INTO catalog.connection_bindings(tenant_id,manifest_digest,component_digest,store_alias,environment,instance_id,mode,binding_status,validation_status,validation_hash) VALUES ('t1',$1,$2,'upstream','dev','chosen','read','active','valid',$2)", &[&manifest,&component]).await.unwrap();
    let migration = wamn_control_provision::schema_migrations::PROJECT_MIGRATIONS
        .iter()
        .find(|migration| {
            migration
                .relative_path
                .ends_with("_c35_connection_bindings.sql")
        })
        .expect("the package binding migration exists");
    // A scoped row with a different selection must refuse the whole backfill.
    tx.batch_execute("SAVEPOINT conflict").await.unwrap();
    tx.execute("INSERT INTO catalog.connection_bindings(tenant_id,manifest_digest,component_digest,store_alias,environment,instance_id,package_id,mode,binding_status,validation_status,validation_hash) VALUES ('t1',$1,$2,'upstream','dev','conflict','second','read','active','valid',$2)", &[&manifest,&component]).await.unwrap();
    assert_eq!(
        tx.batch_execute(migration.sql).await.unwrap_err().code(),
        Some(&tokio_postgres::error::SqlState::OBJECT_NOT_IN_PREREQUISITE_STATE)
    );
    tx.batch_execute("ROLLBACK TO SAVEPOINT conflict")
        .await
        .unwrap();
    // A release with no matching component owner must also refuse atomically.
    tx.batch_execute("SAVEPOINT missing_owner").await.unwrap();
    let other_release = json!({"components":[{"package-id":"unrelated","descriptor":{"component-digest":format!("sha256:{}", "b".repeat(64))}}]}).to_string();
    let other_manifest: String = tx.query_one("INSERT INTO catalog.releases(tenant_id,manifest_digest,canonical_bytes) VALUES ('t1','sha256:' || encode(sha256(convert_to($1,'UTF8')),'hex'),convert_to($1,'UTF8')) RETURNING manifest_digest", &[&other_release]).await.unwrap().get(0);
    tx.execute("INSERT INTO catalog.connection_bindings(tenant_id,manifest_digest,component_digest,store_alias,environment,instance_id,mode,binding_status,validation_status,validation_hash) VALUES ('t1',$1,$2,'upstream','dev','chosen','read','active','valid',$2)", &[&other_manifest,&component]).await.unwrap();
    assert_eq!(
        tx.batch_execute(migration.sql).await.unwrap_err().code(),
        Some(&tokio_postgres::error::SqlState::OBJECT_NOT_IN_PREREQUISITE_STATE)
    );
    tx.batch_execute("ROLLBACK TO SAVEPOINT missing_owner")
        .await
        .unwrap();
    tx.batch_execute(migration.sql).await.unwrap();
    let rows = tx.query("SELECT package_id,instance_id,mode,manifest_digest FROM catalog.connection_bindings ORDER BY package_id", &[]).await.unwrap();
    assert_eq!(rows.len(), 2);
    for (row, package) in rows.iter().zip(["first", "second"]) {
        assert_eq!(row.get::<_, String>(0), package);
        assert_eq!(row.get::<_, String>(1), "chosen");
        assert_eq!(row.get::<_, String>(2), "read");
        assert_eq!(row.get::<_, String>(3), manifest);
    }
    tx.batch_execute("SAVEPOINT unscoped_write").await.unwrap();
    assert!(tx.execute("INSERT INTO catalog.connection_bindings(tenant_id,manifest_digest,component_digest,store_alias,environment,instance_id,mode,binding_status,validation_status,validation_hash) VALUES ('t1',$1,$2,'upstream','dev','chosen','read','active','valid',$2)", &[&manifest,&component]).await.is_err());
    tx.batch_execute("ROLLBACK TO SAVEPOINT unscoped_write")
        .await
        .unwrap();
}

#[tokio::test]
async fn identical_configuration_writes_keep_xmin_and_generation() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let mut client = connect(database.url()).await;
    client.batch_execute("SET ROLE wamn_system; INSERT INTO registry.orgs(id,placement_type) VALUES ('acme','dedicated')").await.unwrap();
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
