//! Org connections materialize into independent catalogs and resolve one host-held secret.

use std::collections::BTreeMap;
use std::sync::Arc;
use wamn_credential_store::Store as _;

use serde_json::{Value, json};
use tokio_postgres::{Client, NoTls};
use wamn_catalog::{ComponentConnectionRequirement, RequirementType};
use wamn_control::environment::analyse::resolve_connections;
use wamn_control::environment::document::EnvironmentDocument;
use wamn_control::environment::expand::{ConnectionTarget, materialize_connections};
use wamn_control::org_connection::ResolvedConnection;
use wamn_runtime::connection_credentials::ConnectionCredentials;
use wamn_runtime::plugins::wamn_postgres::{
    ClassCredentials, ConnectionEffectLookup, ConnectionEffectSnapshot, ConnectionEntryLookup,
    DEFAULT_PROJECT, WamnPostgres, WamnPostgresConfig,
};

struct DeclareConnectionRequest {
    system_database_url: String,
    org: String,
    name: String,
    requirement_type: RequirementType,
    definition: Value,
}

async fn declare(
    request: &DeclareConnectionRequest,
    changes: &wamn_platform_identity::auth_changes::AuthChanges,
) -> anyhow::Result<bool> {
    let mut system = connect(&request.system_database_url).await;
    system.batch_execute("SET ROLE wamn_control").await?;
    let tx = system.transaction().await?;
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await?;
    let before = tx
        .query_opt(
            "SELECT parameters::text FROM registry.connection_rows($1) WHERE name=$2",
            &[&request.org, &request.name],
        )
        .await?;
    let mut drops = wamn_platform_identity::auth_changes::AuthDrops::default();
    wamn_platform_identity::connections::write_connection(
        &tx,
        &request.org,
        &wamn_platform_identity::connections::ConnectionWrite {
            integration: if request.requirement_type == RequirementType::Http {
                "http"
            } else {
                "gcs"
            }
            .into(),
            external_system: None,
            name: request.name.clone(),
            transport: "http".into(),
            exchange: "request".into(),
            parameters: request.definition.clone(),
            status: "active".into(),
        },
        &mut drops,
    )
    .await?;
    for project in ["alpha", "beta"] {
        wamn_platform_identity::connections::enable_connection(
            &tx,
            &request.org,
            project,
            &request.name,
        )
        .await?;
        for (scope, id) in [
            ("org", request.org.clone()),
            ("project", project.into()),
            ("environment", format!("{project}/dev")),
        ] {
            wamn_platform_identity::connections::allow_integration(
                &tx,
                &request.org,
                scope,
                &id,
                "http",
            )
            .await?;
        }
    }
    tx.commit().await?;
    changes.publish(drops).await;
    Ok(
        before.map(|row| serde_json::from_str::<Value>(row.get(0)).unwrap())
            != Some(request.definition.clone()),
    )
}

async fn binding_inputs(
    system: &Client,
    document: &EnvironmentDocument,
) -> anyhow::Result<BTreeMap<String, ResolvedConnection>> {
    system.batch_execute("SET ROLE wamn_control").await?;
    let package =
        wamn_schema_generator::PackageManifest::from_slice(&serde_json::to_vec(&json!({
            "package":{"id":"org_fixture","version":"1.0.0"},
            "required_platform_policy_contract":{"id":"fixture_data_access","state":"satisfied"},
            "models":{}, "components":{}, "connections":{"erp":{"integration":"http","mode":"read"}}
        }))?)?;
    let bindings =
        wamn_control::environment::bindings::analyse(system, document, &[package]).await?;
    Ok(bindings
        .into_iter()
        .map(|binding| {
            let resolved = binding.resolved.expect("host HTTP fixture");
            (resolved.org_connection_name.clone().unwrap(), resolved)
        })
        .collect())
}

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

async fn credential_reader(
    database: &wamn_test_postgres::Database,
    project: &str,
) -> ConnectionCredentials {
    let mut admin = connect(database.url()).await;
    let role = format!("c35_{}_{}", database.name(), project);
    admin
        .batch_execute(
            &wamn_control_provision::sql::prepare_workload_generation_sql(
                wamn_control_provision::WorkloadRoleFamily::ConnectionReader,
                database.name(),
                &role,
                "fixture-password",
                "2099-01-01T00:00:00Z",
            ),
        )
        .await
        .unwrap();
    let transaction = wamn_control::provision_project_env::provisioning_transaction(&mut admin)
        .await
        .unwrap();
    transaction
        .execute(
            wamn_control_provision::sql::upsert_control_author_env_key_mapping_sql(),
            &[
                &role,
                &format!("acme--{project}--dev"),
                &"acme",
                &project,
                &"dev",
            ],
        )
        .await
        .unwrap();
    transaction.commit().await.unwrap();
    let mut url = url::Url::parse(database.url()).unwrap();
    url.set_username(&role).unwrap();
    url.set_password(Some("fixture-password")).unwrap();
    ConnectionCredentials::from_database(std::sync::Arc::new(connect(url.as_str()).await), "acme")
}

async fn write_connection(
    client: &mut Client,
    org: &str,
    payload: &Value,
) -> anyhow::Result<Value> {
    let tx = client.transaction().await?;
    tx.batch_execute("SET LOCAL ROLE wamn_control").await?;
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await?;
    let result = wamn_platform_identity::connections::write(
        &tx,
        org,
        "connection.write",
        &payload.to_string(),
        &mut wamn_platform_identity::auth_changes::AuthDrops::default(),
    )
    .await?;
    tx.commit().await?;
    Ok(result)
}

fn document(project: &str) -> EnvironmentDocument {
    EnvironmentDocument::from_json(
        json!({
            "org": "acme", "project": project, "env": "dev", "release": "none",
            "target": "cloud", "route_host": "app.acme.example", "policy": "dev",
            "applications": {"erp": {"packages": ["org_fixture"], "connectors": {"http":"host"}, "connections": {"erp": "shared_erp"}}}
        })
        .to_string()
        .as_bytes(),
    )
    .unwrap()
}

fn definition(host: &str) -> Value {
    json!({"primary-authority": format!("https://{host}/"), "tls-verification": "verify-authority"})
}

async fn seed(client: &Client, env_key: &str) -> Vec<ConnectionTarget> {
    seed_typed(client, env_key, RequirementType::Http).await
}

async fn seed_typed(
    client: &Client,
    env_key: &str,
    requirement_type: RequirementType,
) -> Vec<ConnectionTarget> {
    client.batch_execute(&format!(
        "SELECT set_config('app.user_id', '{}', false), set_config('app.operation', 'admin:seed-catalog-fixture', false)",
        wamn_control_provision::PlatformComponent::Provisioning.principal_id()
    )).await.unwrap();
    client
        .batch_execute(&wamn_control_provision::sql::ensure_workload_acl_role_sql(
            wamn_control_provision::WorkloadRoleFamily::HttpAdmitter,
        ))
        .await
        .unwrap();
    client
        .execute("SELECT set_config('app.env_key', $1, false)", &[&env_key])
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
                    "connections": [{"requirement-type": if requirement_type == RequirementType::Http { "http" } else { "blobstore" }, "store-alias": "erp"}]
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
    client.execute("INSERT INTO catalog.releases (env_key, manifest_digest, canonical_bytes) VALUES ($1, $2, $3)", &[&env_key, &digest, &bytes]).await.unwrap();
    for component in COMPONENTS {
        let requirement =
            ComponentConnectionRequirement::new(component, "erp", requirement_type.descriptor());
        client
            .execute(
                wamn_schema_control::connections::insert_component_connection_requirement_sql(),
                &[
                    &env_key,
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
        .enumerate()
        .map(|(index, component)| ConnectionTarget {
            instance_id: "erp/erp".into(),
            manifest_digest: digest.clone(),
            component_digest: (*component).to_owned(),
            store_alias: "erp".into(),
            package_id: "org_fixture".into(),
            mode: if index == 0 {
                wamn_schema_generator::ConnectionMode::Read
            } else {
                wamn_schema_generator::ConnectionMode::Write
            },
        })
        .collect()
}

async fn selected(url: &str, env_key: &str, target: &ConnectionTarget) -> ConnectionEffectSnapshot {
    let postgres = WamnPostgres::new(WamnPostgresConfig {
        credentials: Some(ClassCredentials::every_class(url.to_owned())),
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 2_000,
        statement_timeout_ms: 5_000,
        row_limit: 1_000,
    })
    .unwrap();
    postgres.set_env_key("fixture", env_key).unwrap();
    let snapshot = postgres
        .connection_effect_snapshot(
            "fixture",
            DEFAULT_PROJECT,
            env_key,
            &ConnectionEffectLookup {
                entry: ConnectionEntryLookup {
                    package_id: "org_fixture",
                    wiring: None,
                },
                origin_package_id: "org_fixture",
                origin_component_digest: &target.component_digest,
                origin_component: "http-call",
                origin_interface_version: "0.1.0",
                origin_operation: "call",
                package_id: "org_fixture",
                operation: "call",
                manifest_digest: &target.manifest_digest,
                environment: "dev",
                component_digest: &target.component_digest,
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
    let stamped: bool = client.query_one(
        "SELECT count(*) > 0 AND bool_and(created_by = $1::text::uuid AND updated_by = $1::text::uuid AND created_at = updated_at) FROM catalog.connection_generations",
        &[&wamn_control_provision::PlatformComponent::Provisioning.principal_id().to_string()],
    ).await.unwrap().get(0);
    assert!(stamped, "instance generations stamp wamn:provisioning");
    let row = client.query_one("SELECT (SELECT count(*) FROM catalog.connection_generations), (SELECT count(*) FROM catalog.connection_bindings)", &[]).await.unwrap();
    (row.get(0), row.get(1))
}

async fn apply(
    url: &str,
    document: &EnvironmentDocument,
    inputs: &BTreeMap<String, ResolvedConnection>,
    targets: &[ConnectionTarget],
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
    let mut control = connect(control_database.url()).await;
    let evidence = connect(control_database.url()).await;
    let a = connect(database_a.url()).await;
    let b = connect(database_b.url()).await;

    control.batch_execute("SET ROLE wamn_system; SET app.user_id = '770df186-ac15-579e-b46b-c297cae2011b'; INSERT INTO registry.orgs (id, placement_type) VALUES ('acme', 'dedicated'), ('other', 'dedicated')").await.unwrap();

    control.batch_execute("INSERT INTO registry.projects(org,id) VALUES ('acme','alpha'),('acme','beta'); INSERT INTO registry.env_policies(org,name,recovery_domain,promotion_rank,instances,storage,cpu,memory,image) VALUES ('acme','dev','{}',0,1,'1Gi','1','1Gi','image'); INSERT INTO registry.project_envs(org,project,env,secret_name,instance_suffix,policy_name) VALUES ('acme','alpha','dev','secret','aaaaaaaa','dev'),('acme','beta','dev','secret','bbbbbbbb','dev')").await.unwrap();
    let doc_a = document("alpha");
    let doc_b = document("beta");
    let env_key_a = wamn_control_provision::project_env_key("acme", "alpha", "dev");
    let env_key_b = wamn_control_provision::project_env_key("acme", "beta", "dev");
    let targets_a = seed(&a, &env_key_a).await;
    let targets_b = seed(&b, &env_key_b).await;
    let mut declaration = DeclareConnectionRequest {
        system_database_url: control_database.url().to_owned(),
        org: "acme".to_owned(),
        name: "shared_erp".to_owned(),
        requirement_type: RequirementType::Http,
        definition: definition("erp.example"),
    };
    assert!(binding_inputs(&control, &doc_a).await.is_err());
    let payload = json!({
        "integration":"http", "name":declaration.name,
        "transport":"http", "exchange":"request",
        "parameters":declaration.definition, "status":"active"
    });
    assert!(
        write_connection(&mut control, "missing", &payload)
            .await
            .is_err()
    );
    assert!(write_connection(&mut control, "", &payload).await.is_err());
    let mut invalid = payload.clone();
    invalid["name"] = "bad:name".into();
    assert!(
        write_connection(&mut control, "acme", &invalid)
            .await
            .is_err()
    );
    invalid["name"] = payload["name"].clone();
    invalid["parameters"] = json!({});
    assert!(
        write_connection(&mut control, "acme", &invalid)
            .await
            .is_err()
    );
    invalid["parameters"] = payload["parameters"].clone();
    invalid["parameters"]["secret"] = "forbidden".into();
    assert!(
        write_connection(&mut control, "acme", &invalid)
            .await
            .is_err()
    );
    let mut invalid = payload.clone();
    invalid["integration"] = "s3".into();
    assert!(
        write_connection(&mut control, "acme", &invalid)
            .await
            .is_err()
    );
    declaration.org = "missing".to_owned();
    assert!(
        declare(
            &declaration,
            &wamn_platform_identity::auth_changes::AuthChanges::default()
        )
        .await
        .is_err()
    );
    declaration.org = "acme".to_owned();
    assert!(
        declare(
            &declaration,
            &wamn_platform_identity::auth_changes::AuthChanges::default()
        )
        .await
        .unwrap()
    );
    let xmin: String = evidence
        .query_one("SELECT xmin::text FROM registry.connection", &[])
        .await
        .unwrap()
        .get(0);
    assert!(
        !declare(
            &declaration,
            &wamn_platform_identity::auth_changes::AuthChanges::default()
        )
        .await
        .unwrap()
    );
    assert_eq!(
        evidence
            .query_one("SELECT xmin::text FROM registry.connection", &[])
            .await
            .unwrap()
            .get::<_, String>(0),
        xmin
    );
    declaration.requirement_type = RequirementType::Blobstore;
    assert!(
        declare(
            &declaration,
            &wamn_platform_identity::auth_changes::AuthChanges::default()
        )
        .await
        .is_err()
    );
    declaration.requirement_type = RequirementType::Http;
    assert!(
        binding_inputs(
            &control,
            &EnvironmentDocument {
                org: "other".into(),
                ..doc_a.clone()
            }
        )
        .await
        .is_err()
    );

    let credential_store =
        Arc::new(wamn_test_infrastructure::credentials::LocalCredentials::create().unwrap());
    let tx = control.transaction().await.unwrap();
    for project in ["alpha", "beta"] {
        let stored = credential_store
            .put(uuid::Uuid::new_v4(), SECRET.as_bytes())
            .await
            .unwrap();
        wamn_test_infrastructure::credentials::set_reference(
            &tx,
            "acme",
            project,
            "dev",
            "shared_erp",
            &stored,
        )
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    let original = binding_inputs(&control, &doc_a).await.unwrap();
    let inputs_a = resolve_connections(&doc_a, &original).unwrap();
    let inputs_b = resolve_connections(&doc_b, &original).unwrap();
    // A retry after the first binding committed must retain the first generation.
    let mut partial = targets_a.clone();
    partial[1].component_digest = format!("sha256:{}", "9".repeat(64));
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
    for client in [&a, &b] {
        let rows = client.query("SELECT mode, package_id, instance_id FROM catalog.connection_bindings ORDER BY component_digest", &[]).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get::<_, String>(0), "read");
        assert_eq!(rows[1].get::<_, String>(0), "write");
        for row in rows {
            assert_eq!(row.get::<_, String>(1), "org_fixture");
            assert_eq!(row.get::<_, String>(2), "erp/erp");
        }
    }

    for (database, env_key, targets) in [
        (&database_a, &env_key_a, &targets_a),
        (&database_b, &env_key_b, &targets_b),
    ] {
        let project = env_key.split("--").nth(1).unwrap();
        let vault = credential_reader(&control_database, project)
            .await
            .with_store(credential_store.clone());
        for target in targets {
            let snapshot = selected(database.url(), env_key, target).await;
            assert_eq!(snapshot.definition, Some(declaration.definition.clone()));
            assert_eq!(
                vault
                    .read_connection(project, snapshot.credential_handle.as_deref().unwrap())
                    .await
                    .unwrap()
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

    // Current authority generations remain pinned even when parameters return to A.
    declaration.definition = definition("changed.example");
    declare(
        &declaration,
        &wamn_platform_identity::auth_changes::AuthChanges::default(),
    )
    .await
    .unwrap();
    declaration.definition = definition("erp.example");
    declare(
        &declaration,
        &wamn_platform_identity::auth_changes::AuthChanges::default(),
    )
    .await
    .unwrap();
    apply(
        database_a.url(),
        &doc_a,
        &resolve_connections(&doc_a, &binding_inputs(&control, &doc_a).await.unwrap()).unwrap(),
        &targets_a,
    )
    .await;
    assert_eq!(counts(&a).await, (2, 2));

    declaration.definition = definition("changed.example");
    declare(
        &declaration,
        &wamn_platform_identity::auth_changes::AuthChanges::default(),
    )
    .await
    .unwrap();
    // An older apply keeps its pinned credential generation.
    apply(database_a.url(), &doc_a, &inputs_a, &targets_a).await;
    assert_eq!(counts(&a).await, (3, 2));
    let changed = binding_inputs(&control, &doc_a).await.unwrap();
    let next_a = resolve_connections(&doc_a, &changed).unwrap();
    apply(database_a.url(), &doc_a, &next_a, &targets_a).await;
    let revision: i64 = a
        .query_one("SELECT revision FROM catalog.connection_instances", &[])
        .await
        .unwrap()
        .get(0);
    apply(database_a.url(), &doc_a, &next_a, &targets_a).await;
    assert_eq!(
        a.query_one("SELECT revision FROM catalog.connection_instances", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        revision
    );
    assert_eq!(counts(&a).await, (4, 2));
    assert_eq!(counts(&b).await, (1, 2));
    let previous_b = selected(database_b.url(), &env_key_b, &targets_b[0]).await;
    assert_eq!(previous_b.definition, Some(definition("erp.example")));
    apply(
        database_b.url(),
        &doc_b,
        &resolve_connections(&doc_b, &changed).unwrap(),
        &targets_b,
    )
    .await;
    assert_eq!(counts(&b).await, (2, 2));
    for (database, client, env_key, targets) in [
        (&database_a, &a, &env_key_a, &targets_a),
        (&database_b, &b, &env_key_b, &targets_b),
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
        let project = env_key.split("--").nth(1).unwrap();
        let vault = credential_reader(&control_database, project)
            .await
            .with_store(credential_store.clone());
        for target in targets {
            let snapshot = selected(database.url(), env_key, target).await;
            assert_eq!(
                snapshot.generation,
                Some(if project == "alpha" { 4 } else { 2 })
            );
            assert_eq!(snapshot.definition, Some(declaration.definition.clone()));
            assert_eq!(
                vault
                    .read_connection(project, snapshot.credential_handle.as_deref().unwrap())
                    .await
                    .unwrap()
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

#[tokio::test]
async fn declaration_validation_refuses_invalid_coordinates_types_and_definitions() {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_control_provision::test_database::system();
    let system = connect(database.url()).await;
    system.batch_execute(&format!(
        "SET ROLE wamn_system; {} INSERT INTO registry.orgs(id,placement_type) VALUES ('acme','dedicated')",
        wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ),
    )).await.unwrap();
    let valid = json!({"name":"erp","integration":"http","external_system":null,"transport":"http","exchange":"request","parameters":definition("erp.example"),"status":"active"});
    let changes = wamn_platform_identity::auth_changes::AuthChanges::default();
    wamn_control::connection_configuration::write(
        database.url(),
        "acme",
        "connection.write",
        &valid.to_string(),
        &changes,
        None,
    )
    .await
    .unwrap();
    assert!(
        wamn_control::connection_configuration::write(
            database.url(),
            "",
            "connection.write",
            &valid.to_string(),
            &changes,
            None
        )
        .await
        .is_err()
    );
    let mut invalid = valid.clone();
    invalid["name"] = "bad:name".into();
    assert!(
        wamn_control::connection_configuration::write(
            database.url(),
            "acme",
            "connection.write",
            &invalid.to_string(),
            &changes,
            None
        )
        .await
        .is_err()
    );
    let mut invalid = valid.clone();
    invalid["integration"] = "gcs".into();
    assert!(
        wamn_control::connection_configuration::write(
            database.url(),
            "acme",
            "connection.write",
            &invalid.to_string(),
            &changes,
            None
        )
        .await
        .is_err()
    );
    let mut invalid = valid.clone();
    invalid["parameters"] = json!({});
    assert!(
        wamn_control::connection_configuration::write(
            database.url(),
            "acme",
            "connection.write",
            &invalid.to_string(),
            &changes,
            None
        )
        .await
        .is_err()
    );
    let mut invalid = valid;
    invalid["parameters"]["secret"] = "forbidden".into();
    assert!(
        wamn_control::connection_configuration::write(
            database.url(),
            "acme",
            "connection.write",
            &invalid.to_string(),
            &changes,
            None
        )
        .await
        .is_err()
    );
}
