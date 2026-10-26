//! Org connections materialize into independent catalogs and resolve one host-held secret.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use tokio_postgres::{Client, NoTls};
use wamn_catalog::{ComponentConnectionRequirement, ConnectionTypeDescriptor, RequirementType};
use wamn_control::environment::analyse::resolve_connections;
use wamn_control::environment::document::EnvironmentDocument;
use wamn_control::environment::expand::materialize_connections;
use wamn_control::org_connection::{self, DeclareConnectionRequest, ResolvedConnection};
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_postgres::{
    ClassCredentials, ConnectionEffectLookup, ConnectionEffectSnapshot, ConnectionEntryLookup,
    DEFAULT_PROJECT, WamnPostgres, WamnPostgresConfig,
};

const COMPONENTS: [&str; 2] = [
    "sha256:1111111111111111111111111111111111111111111111111111111111111111",
    "sha256:2222222222222222222222222222222222222222222222222222222222222222",
];
const SECRET: &str = r#"{"headers":{"Authorization":"Bearer shared-org-secret"}}"#;

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    client
}

fn document(project: &str) -> EnvironmentDocument {
    EnvironmentDocument::from_json(
        json!({
            "org": "acme", "project": project, "env": "dev", "release": "none",
            "route_host": "app.acme.example", "policy": "dev",
            "applications": {"erp": {"packages": ["org_fixture"]}},
            "connections": {"erp": {"org": "shared_erp"}}
        })
        .to_string()
        .as_bytes(),
    )
    .unwrap()
}

fn definition(host: &str) -> Value {
    json!({"primary-authority": format!("https://{host}/"), "tls-verification": "verify-authority"})
}

async fn seed(client: &Client, tenant: &str) -> Vec<(String, String, String)> {
    client
        .batch_execute(&wamn_control_provision::sql::ensure_workload_acl_role_sql(
            wamn_control_provision::WorkloadRoleFamily::HttpAdmitter,
        ))
        .await
        .unwrap();
    client
        .execute("SELECT set_config('app.tenant', $1, false)", &[&tenant])
        .await
        .unwrap();
    let components: Vec<Value> = COMPONENTS
        .iter()
        .map(|digest| {
            json!({
                "package-id": "org_fixture",
                "descriptor": {
                    "component": "http-call", "component-digest": digest,
                    "interface-version": "0.1.0",
                    "operations": {"call": {}},
                    "connections": [{"requirement-type": "http", "store-alias": "erp"}]
                }
            })
        })
        .collect();
    let bytes = serde_json::to_vec(&json!({
        "release": {"packages": [{"package-id": "org_fixture", "package-version": "1.0.0"}]},
        "components": components,
    }))
    .unwrap();
    let digest = format!(
        "sha256:{}",
        hex::encode(ring::digest::digest(&ring::digest::SHA256, &bytes))
    );
    client.execute("INSERT INTO catalog.releases (tenant_id, manifest_digest, canonical_bytes) VALUES ($1, $2, $3)", &[&tenant, &digest, &bytes]).await.unwrap();
    for component in COMPONENTS {
        let requirement = ComponentConnectionRequirement::new(
            component,
            "erp",
            ConnectionTypeDescriptor::http_v1(),
        );
        client
            .execute(
                wamn_schema_control::connections::insert_component_connection_requirement_sql(),
                &[
                    &tenant,
                    &component,
                    &"erp",
                    &String::from_utf8(requirement.canonical_bytes()).unwrap(),
                    &requirement.requirement_hash(),
                ],
            )
            .await
            .unwrap();
    }
    COMPONENTS
        .iter()
        .map(|component| (digest.clone(), (*component).to_owned(), "erp".to_owned()))
        .collect()
}

async fn selected(
    url: &str,
    tenant: &str,
    target: &(String, String, String),
) -> ConnectionEffectSnapshot {
    let postgres = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(ClassCredentials::every_class(url.to_owned())),
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
        row_limit: 1_000,
    })
    .unwrap();
    postgres.set_tenant("fixture", tenant).unwrap();
    let snapshot = postgres
        .connection_effect_snapshot(
            "fixture",
            DEFAULT_PROJECT,
            tenant,
            &ConnectionEffectLookup {
                entry: ConnectionEntryLookup {
                    package_id: "org_fixture",
                    wiring: None,
                },
                origin_package_id: "org_fixture",
                origin_component_digest: &target.1,
                origin_component: "http-call",
                origin_interface_version: "0.1.0",
                origin_operation: "call",
                package_id: "org_fixture",
                operation: "call",
                manifest_digest: &target.0,
                environment: "dev",
                component_digest: &target.1,
                store_alias: "erp",
                candidate_binding: None,
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert!(
        snapshot.node_permitted
            && snapshot.binding_active
            && snapshot.binding_valid
            && snapshot.instance_enabled
    );
    assert_eq!(snapshot.generation, snapshot.active_generation);
    snapshot
}

async fn counts(client: &Client) -> (i64, i64) {
    let row = client.query_one("SELECT (SELECT count(*) FROM catalog.connection_generations), (SELECT count(*) FROM catalog.connection_bindings)", &[]).await.unwrap();
    (row.get(0), row.get(1))
}

async fn apply(
    url: &str,
    document: &EnvironmentDocument,
    inputs: &BTreeMap<String, ResolvedConnection>,
    targets: &[(String, String, String)],
) {
    materialize_connections(url, document, inputs, targets)
        .await
        .unwrap();
}

#[tokio::test]
async fn org_connection_materializes_once_per_environment() {
    let _lock = wamn_test_postgres::lock();
    let control_database = wamn_control_provision::test_database::system();
    let database_a = wamn_project_state::test_database::tenant_platform();
    let database_b = wamn_project_state::test_database::tenant_platform();
    let control = connect(control_database.url()).await;
    let a = connect(database_a.url()).await;
    let b = connect(database_b.url()).await;

    // Exercise the upgrade DDL as well as the fresh floors, on these same databases.
    control
        .batch_execute("SET ROLE wamn_system; DROP TABLE registry.org_connections")
        .await
        .unwrap();
    let migration = wamn_control_provision::schema_migrations::SYSTEM_MIGRATIONS
        .iter()
        .find(|m| m.ordinal() == 27)
        .unwrap();
    control
        .batch_execute(&format!("BEGIN; {}; COMMIT", migration.sql))
        .await
        .unwrap();
    b.batch_execute("ALTER TABLE catalog.connection_instances DROP COLUMN org_connection_name")
        .await
        .unwrap();
    let migration = wamn_control_provision::schema_migrations::PROJECT_MIGRATIONS
        .iter()
        .find(|m| m.ordinal() == 23)
        .unwrap();
    b.batch_execute(migration.sql).await.unwrap();
    control.batch_execute("INSERT INTO registry.orgs (id, placement_type) VALUES ('acme', 'dedicated'), ('other', 'dedicated')").await.unwrap();

    let doc_a = document("alpha");
    let doc_b = document("beta");
    let tenant_a = wamn_control_provision::project_env_tenant("acme", "alpha", "dev");
    let tenant_b = wamn_control_provision::project_env_tenant("acme", "beta", "dev");
    let targets_a = seed(&a, &tenant_a).await;
    let targets_b = seed(&b, &tenant_b).await;
    let mut declaration = DeclareConnectionRequest {
        system_database_url: control_database.url().to_owned(),
        org: "acme".to_owned(),
        name: "shared_erp".to_owned(),
        requirement_type: RequirementType::Http,
        definition: definition("erp.example"),
    };
    assert!(
        resolve_connections(
            &doc_a,
            &org_connection::snapshot(&control, "acme").await.unwrap()
        )
        .is_err()
    );
    declaration.org = "missing".to_owned();
    assert!(org_connection::declare(&declaration).await.is_err());
    declaration.org = "acme".to_owned();
    assert!(org_connection::declare(&declaration).await.unwrap());
    let xmin: String = control
        .query_one("SELECT xmin::text FROM registry.org_connections", &[])
        .await
        .unwrap()
        .get(0);
    assert!(!org_connection::declare(&declaration).await.unwrap());
    assert_eq!(
        control
            .query_one("SELECT xmin::text FROM registry.org_connections", &[])
            .await
            .unwrap()
            .get::<_, String>(0),
        xmin
    );
    declaration.requirement_type = RequirementType::Blobstore;
    assert!(org_connection::declare(&declaration).await.is_err());
    declaration.requirement_type = RequirementType::Http;
    assert!(
        resolve_connections(
            &doc_a,
            &org_connection::snapshot(&control, "other").await.unwrap()
        )
        .is_err()
    );

    let original = org_connection::snapshot(&control, "acme").await.unwrap();
    let inputs_a = resolve_connections(&doc_a, &original).unwrap();
    let inputs_b = resolve_connections(&doc_b, &original).unwrap();
    // A retry after the first binding committed must retain the first generation.
    let mut partial = targets_a.clone();
    partial[1].1 = format!("sha256:{}", "9".repeat(64));
    assert!(
        materialize_connections(database_a.url(), &doc_a, &inputs_a, &partial)
            .await
            .is_err()
    );
    assert_eq!(counts(&a).await, (1, 1));
    apply(database_a.url(), &doc_a, &inputs_a, &targets_a).await;
    apply(database_b.url(), &doc_b, &inputs_b, &targets_b).await;
    assert_eq!(counts(&a).await, (1, 2));
    assert_eq!(counts(&b).await, (1, 2));

    let path =
        std::env::temp_dir().join(format!("wamn-org-connections-{}.json", std::process::id()));
    std::fs::write(
        &path,
        json!({"projects": {}, "orgs": {"acme": {"shared_erp": SECRET}}}).to_string(),
    )
    .unwrap();
    let vault = WamnCredentials::from_file(&path).unwrap().with_org("acme");
    std::fs::remove_file(path).unwrap();
    for (database, tenant, targets) in [
        (&database_a, &tenant_a, &targets_a),
        (&database_b, &tenant_b, &targets_b),
    ] {
        for target in targets {
            let snapshot = selected(database.url(), tenant, target).await;
            assert_eq!(snapshot.definition, Some(declaration.definition.clone()));
            assert_eq!(
                vault
                    .lookup_connection(tenant, snapshot.credential_handle.as_deref().unwrap())
                    .as_deref(),
                Some(SECRET)
            );
        }
    }
    let revision: i64 = a
        .query_one("SELECT revision FROM catalog.connection_instances", &[])
        .await
        .unwrap()
        .get(0);
    apply(database_a.url(), &doc_a, &inputs_a, &targets_a).await;
    assert_eq!(counts(&a).await, (1, 2));
    assert_eq!(
        a.query_one("SELECT revision FROM catalog.connection_instances", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        revision
    );

    // Repair source metadata without creating another immutable generation.
    a.execute("UPDATE catalog.connection_instances SET org_connection_name = NULL, revision = revision + 1", &[]).await.unwrap();
    apply(database_a.url(), &doc_a, &inputs_a, &targets_a).await;
    assert_eq!(counts(&a).await, (1, 2));
    assert_eq!(
        a.query_one("SELECT revision FROM catalog.connection_instances", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        revision + 2
    );

    // A -> B -> A between applies compares with selected A and creates nothing.
    declaration.definition = definition("changed.example");
    org_connection::declare(&declaration).await.unwrap();
    declaration.definition = definition("erp.example");
    org_connection::declare(&declaration).await.unwrap();
    apply(
        database_a.url(),
        &doc_a,
        &resolve_connections(
            &doc_a,
            &org_connection::snapshot(&control, "acme").await.unwrap(),
        )
        .unwrap(),
        &targets_a,
    )
    .await;
    assert_eq!(counts(&a).await, (1, 2));

    declaration.definition = definition("changed.example");
    org_connection::declare(&declaration).await.unwrap();
    // A declaration after a snapshot takes effect only in the next snapshot.
    apply(database_a.url(), &doc_a, &inputs_a, &targets_a).await;
    assert_eq!(counts(&a).await, (1, 2));
    let changed = org_connection::snapshot(&control, "acme").await.unwrap();
    let next_a = resolve_connections(&doc_a, &changed).unwrap();
    apply(database_a.url(), &doc_a, &next_a, &targets_a).await;
    apply(database_a.url(), &doc_a, &next_a, &targets_a).await;
    assert_eq!(counts(&a).await, (2, 2));
    assert_eq!(counts(&b).await, (1, 2));
    let previous_b = selected(database_b.url(), &tenant_b, &targets_b[0]).await;
    assert_eq!(previous_b.definition, Some(definition("erp.example")));
    apply(
        database_b.url(),
        &doc_b,
        &resolve_connections(&doc_b, &changed).unwrap(),
        &targets_b,
    )
    .await;
    assert_eq!(counts(&b).await, (2, 2));
    for (database, client, tenant, targets) in [
        (&database_a, &a, &tenant_a, &targets_a),
        (&database_b, &b, &tenant_b, &targets_b),
    ] {
        let source: String = client
            .query_one(
                "SELECT org_connection_name FROM catalog.connection_instances",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(source, "shared_erp");
        for target in targets {
            let snapshot = selected(database.url(), tenant, target).await;
            assert_eq!(snapshot.generation, Some(2));
            assert_eq!(snapshot.definition, Some(declaration.definition.clone()));
            assert_eq!(
                vault
                    .lookup_connection(tenant, snapshot.credential_handle.as_deref().unwrap())
                    .as_deref(),
                Some(SECRET)
            );
        }
    }
}

#[tokio::test]
async fn binding_refuses_a_handle_from_another_org_before_connecting() {
    let mut instance = wamn_control::bind_connection::ConnectionInstance {
        database_url: "invalid database URL".into(),
        org: "acme".into(),
        project: "erp".into(),
        environment: "dev".into(),
        instance_id: "erp".into(),
        requirement_type: RequirementType::Http,
        definition: definition("erp.example"),
        credential_handle: Some("org:other:shared_erp".into()),
        org_connection_name: Some("shared_erp".into()),
    };
    let error = wamn_control::bind_connection::ensure_instance(&instance)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("org connection source and credential handle differ")
    );
    instance.credential_handle = Some("org:acme:shared_erp".into());
    let error = wamn_control::bind_connection::ensure_instance(&instance)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("connect to the project-environment database")
    );
}
