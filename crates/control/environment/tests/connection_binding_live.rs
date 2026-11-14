//! Release bindings, target refusals, and native read authority on the environment catalog.

use sqlx::Connection as _;
use sqlx::PgConnection as Client;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use serde_json::json;

use wamn_broker::connection_changes::ConnectionDrops;
use wamn_catalog::{AdmittedComponentOperation, WiringDocument, WiringNode, WiringTerminal};
use wamn_environment::apply_package::{self, ApplyPackageRequest};
use wamn_environment::environment::{bindings, document::EnvironmentDocument};
use wamn_runtime::connection_credentials::ConnectionCredentials;
use wamn_runtime::connection_facts::ConnectionFacts;
use wamn_runtime::connection_memory::ConnectionMemory;
use wamn_runtime::plugins::wamn_postgres::{
    CandidateConnectionBinding, ClassCredentials, ConnectionEffectLookup, DEFAULT_PROJECT,
    WamnPostgres, WamnPostgresConfig,
};

const PROJECT_ENV_KEY: &str = "connection-binding-project--dev--fixture";
const ENVIRONMENT: &str = "dev";
const PACKAGE: &str = "connection_binding_fixture";
const PACKAGE_VERSION: &str = "1.0.0";
const COMPONENT_ID: &str = "connection-binding-component";
const HTTP_SIDECAR: &str =
    "sha256:3333333333333333333333333333333333333333333333333333333333333333";
async fn connect(url: &str) -> Client {
    let client = wamn_postgres::connect(url)
        .await
        .expect("connect to a disposable database");

    client
}

/// Provision the project and return the admitted components of the package.
async fn provision_project(project: &mut Client, project_url: &str) {
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "SELECT set_config('app.user_id', '{}', false), set_config('app.operation', 'admin:seed-connection-fixture', false)",
        wamn_control_provision::PlatformComponent::Provisioning.principal_id()
    ))).execute(&mut *project).await.map(drop).unwrap();
    // apply-package narrows migration authority to wamn_db_owner, a
    // CLUSTER-wide role. On a fresh server it does not exist; the sibling
    // fixture that omits this passes only where an earlier run left the role
    // behind -- a leftover healthy object masking an incomplete fixture.
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (wamn_control_provision::sql::ensure_db_owner_role_sql()).to_string(),
    ))
    .execute(&mut *project)
    .await
    .map(drop)
    .expect("ensure the production package-owner role");
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("DO $grant$ BEGIN \
               EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()); \
             END $grant$;")
            .to_string(),
    ))
    .execute(&mut *project)
    .await
    .map(drop)
    .expect("let the package owner create schemas in this database");
    // The plugin's platform checkout runs an R18 membership probe on every new
    // connection: the connecting role must be a member of the class's ACL role
    // (CallableHttp -> wamn_http_admitter). A superuser passes the probe once
    // the role EXISTS; on a fresh server it does not.
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::sql::ensure_workload_acl_role_sql(
            wamn_control_provision::WorkloadRoleFamily::HttpAdmitter,
        ))
            .to_string(),
    ))
    .execute(&mut *project)
    .await
    .map(drop)
    .expect("ensure the CallableHttp class ACL role the pool probes for");
    sqlx::raw_sql("CREATE SCHEMA IF NOT EXISTS wamn_run")
        .execute(&mut *project)
        .await
        .map(drop)
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::sql::grant_administration_surface_sql("wamn_run")).to_string(),
    ))
    .execute(&mut *project)
    .await
    .map(drop)
    .unwrap();
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../package/tests/fixtures/connection_binding");
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
    let package = scratch.path().join("package");
    std::fs::create_dir_all(package.join("migrations")).unwrap();
    std::fs::create_dir_all(package.join("generated")).unwrap();
    std::fs::copy(
        source.join("generated/wamn.json"),
        package.join("generated/wamn.json"),
    )
    .unwrap();
    for entry in std::fs::read_dir(source.join("migrations")).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(
            entry.path(),
            package.join("migrations").join(entry.file_name()),
        )
        .unwrap();
    }
    wamn_test_infrastructure::fixture_digest::write_build_record(&package);
    apply_package::apply_package(ApplyPackageRequest {
        migration_database_url: wamn_control_provision::test_database::package_migration_input(
            &project_url,
            &"connection-binding-project",
            &"fixture",
            &"dev",
        )
        .await
        .unwrap(),
        component_source: wamn_environment::apply_package::component_source_from_env(None).unwrap(),
        changes: wamn_broker::Publisher::default(),
        package: wamn_environment::apply_package::PackageRoot {
            path: package.clone(),
            artifact_digest: Some(
                wamn_test_infrastructure::fixture_digest::fixture_artifact_digest(&package)
                    .expect("read the fixture package root"),
            ),
        },
        database_url: project_url.to_string(),
        org: "connection-binding-project".into(),
        project: "fixture".into(),
        environment: "dev".into(),
    })
    .await
    .expect("apply the connection-binding package fixture");
    sqlx::query("SELECT set_config('app.project_env_key', $1, false)")
        .bind(&PROJECT_ENV_KEY)
        .execute(&mut *project)
        .await
        .map(|result| result.rows_affected())
        .expect("target the project seed session");
    sqlx::query("INSERT INTO catalog.connection_requirement(project_env_key,component_digest,package_id,name,interface,mode) VALUES($1,$2,$3,'erp','wamn:connection/http@0.1.0','read')")
        .bind(PROJECT_ENV_KEY).bind(HTTP_SIDECAR).bind(PACKAGE)
        .execute(&mut *project).await.unwrap();
}

struct Authority {
    database: wamn_test_postgres::Database,
    system: Client,
    _store: Arc<wamn_test_infrastructure::credentials::MemoryStore>,
}
impl Authority {
    async fn new(http_origin: &str) -> Self {
        let database = wamn_control_provision::test_database::system();
        let mut system = connect(database.url()).await;
        sqlx::raw_sql("SET ROLE wamn_system")
            .execute(&mut system)
            .await
            .map(drop)
            .unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&wamn_control_provision::test_database::seed_org_project_env_sql(
                "connection-binding-project",
                DEFAULT_PROJECT,
                "dev",
            ))
                .to_string(),
        ))
        .execute(&mut system)
        .await
        .map(drop)
        .unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("BEGIN; {} INSERT INTO registry.target(name,type,status) VALUES('cloud','cloud','active') ON CONFLICT DO NOTHING; UPDATE registry.meta SET platform_domain='example.invalid'; UPDATE registry.environments SET target_id=(SELECT id FROM registry.target WHERE name='cloud'); COMMIT;",wamn_control_provision::bind_platform_principal_sql(wamn_control_provision::PlatformComponent::Provisioning)))).execute(&mut system).await.map(drop).unwrap();
        let store = Arc::new(wamn_test_infrastructure::credentials::MemoryStore::default());
        let mut tx = system.begin().await.unwrap();
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
        for (name, integration_type, auth_type, parameters, credential) in [
            (
                "labels-store",
                "s3",
                "custom",
                json!({"provider":"s3","endpoint":"http://10.0.0.7:9000","container":"labels","prefix":"fixture/"}),
                json!({"ACCESS_KEY_ID":"fixture","ACCESS_SECRET_KEY":"private"}),
            ),
            (
                "http-read",
                "http",
                "http_basic",
                json!({"primary-authority":http_origin,"tls-verification":"disabled"}),
                json!({"username":"fixture","password":"private"}),
            ),
        ] {
            wamn_test_infrastructure::credentials::define(&mut tx,"connection-binding-project",&json!({"project":DEFAULT_PROJECT,"env":"dev","name":name,"integration_type":integration_type,"auth_type":auth_type,"parameters":parameters,"credential":credential.to_string()}),store.as_ref()).await.unwrap();
        }
        tx.commit().await.unwrap();
        Self {
            database,
            system,
            _store: store,
        }
    }
    fn document(digest: &str) -> EnvironmentDocument {
        let mut document: EnvironmentDocument = serde_json::from_value(json!({"org":"connection-binding-project","project":DEFAULT_PROJECT,"env":"dev","target":"cloud","policy":"dev","revocation_seconds":60, "integrations":{"labels-store":{"version":1},"http-read":{"version":1}},
        "applications": {PACKAGE:{"revocation_seconds":5,"hostname": "fixture.example","connections":{"labels":"labels-store","erp":"http-read"}}}})).unwrap();
        let selection = document.applications.get_mut(PACKAGE).unwrap();
        selection.current =
            wamn_environment::environment::document::DeclaredRelease::Digest(digest.to_owned());
        selection.packages = [PACKAGE, "nested_origin", "nested_wiring"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        document
    }
    async fn apply(
        &mut self,
        project: &mut Client,
        url: &str,
        document: &EnvironmentDocument,
        packages: &[wamn_compiler::manifest::PackageManifest],
    ) {
        sqlx::raw_sql("SET ROLE wamn_db_owner")
            .execute(&mut self.system)
            .await
            .map(drop)
            .unwrap();
        let selected = bindings::analyse(&mut self.system, document, packages)
            .await
            .unwrap();
        let _ = bindings::project_authority(self.database.url(), url, document)
            .await
            .unwrap();
        let digest = match document.legacy_release() {
            wamn_environment::environment::document::DeclaredRelease::Digest(digest) => {
                digest.as_str()
            }
            wamn_environment::environment::document::DeclaredRelease::None => {
                unreachable!()
            }
        };
        // A write records the memory key of its release only, and a repeated
        // write that changes no row records nothing.
        for repeat in [false, true] {
            let mut drops = ConnectionDrops::default();
            let mut tx = project.begin().await.unwrap();
            bindings::write(
                &mut tx,
                PROJECT_ENV_KEY,
                digest,
                document,
                &selected,
                &mut drops,
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
            let mut expected = ConnectionDrops::default();
            if !repeat && !drops.is_empty() {
                expected.release(PROJECT_ENV_KEY, digest);
            }
            assert_eq!(drops, expected, "repeated write: {repeat}");
        }
    }
    async fn reader(&mut self) -> ConnectionCredentials {
        let role = format!("reader_{}", self.database.name());
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&format!(
                "RESET ROLE; BEGIN; {} {}",
                wamn_control_provision::bind_platform_principal_sql(
                    wamn_control_provision::PlatformComponent::Provisioning
                ),
                wamn_control_provision::test_database::login_sql(
                    wamn_control_provision::WorkloadRoleFamily::ConnectionReader,
                    self.database.name(),
                    &role,
                    "fixture-password"
                )
            ))
                .to_string(),
        ))
        .execute(&mut self.system)
        .await
        .map(drop)
        .unwrap();
        sqlx::query(sqlx::AssertSqlSafe(
            (wamn_control_provision::sql::upsert_control_author_project_env_key_mapping_sql())
                .to_string(),
        ))
        .bind(&role)
        .bind(&PROJECT_ENV_KEY)
        .bind(&"connection-binding-project")
        .bind(&DEFAULT_PROJECT)
        .bind(&ENVIRONMENT)
        .execute(&mut self.system)
        .await
        .map(|result| result.rows_affected())
        .unwrap();
        sqlx::raw_sql("COMMIT")
            .execute(&mut self.system)
            .await
            .map(drop)
            .unwrap();
        let mut url = url::Url::parse(self.database.url()).unwrap();
        url.set_username(&role).unwrap();
        url.set_password(Some("fixture-password")).unwrap();
        ConnectionCredentials::from_database(
            wamn_postgres::pool(url.as_str(), 4).await.unwrap(),
            "connection-binding-project",
        )
    }
}
fn declarations(integration_type: &str, mode: &str) -> wamn_compiler::manifest::PackageManifest {
    let interface = "wamn:connection/http@0.1.0";
    let connections = if integration_type == "s3" {
        json!({"labels":{"interface":"wasmcloud:blobstore/blobstore@0.1.0","mode":"write"},"erp":{"interface":"wamn:connection/http@0.1.0","mode":"read"}})
    } else {
        json!({"erp":{"interface":interface,"mode":mode}})
    };
    serde_json::from_value(json!({"package":{"id":PACKAGE,"version":PACKAGE_VERSION},"required_platform_policy_contract":{"id":"fixture_access","state":"satisfied"},"models":{},"components":{},"connections":connections})).unwrap()
}
fn provision_project_components_for_http_manifest()
-> std::collections::BTreeSet<wamn_catalog::ServingComponent> {
    let operation = "wamn:node/handler@0.1.0";
    let mut descriptor =
        wamn_catalog::ComponentDescriptor::named("http-sidecar", "0.1.0", HTTP_SIDECAR);
    descriptor
        .connections
        .push(wamn_catalog::ComponentConnection {
            store_alias: "erp".into(),
            requirement_type: wamn_catalog::ComponentConnectionType::Http,
            mode: wamn_catalog::ConnectionMode::Read,
        });
    descriptor.operations.insert(
        operation.into(),
        AdmittedComponentOperation {
            connections: descriptor
                .connections
                .iter()
                .cloned()
                .map(Into::into)
                .collect(),
            committed_result_schema: None,
            registered_operation: None,
            host_transaction: false,
            input_ports: Vec::new(),
            output_ports: Vec::new(),
            parameters: Vec::new(),
            statements: BTreeMap::new(),
        },
    );
    [wamn_catalog::ServingComponent {
        package_id: PACKAGE.into(),
        runtime_schema: None,
        descriptor: {
            let mut descriptor = descriptor;
            descriptor.operations = BTreeMap::from([(
                operation.into(),
                wamn_catalog::AdmittedComponentOperation {
                    connections: descriptor
                        .connections
                        .iter()
                        .cloned()
                        .map(Into::into)
                        .collect(),
                    host_transaction: false,
                    input_ports: Vec::new(),
                    output_ports: Vec::new(),
                    parameters: Vec::new(),
                    registered_operation: None,
                    statements: BTreeMap::new(),
                    committed_result_schema: None,
                },
            )]);
            descriptor
        },
        statement_instances: BTreeMap::new(),
    }]
    .into()
}

#[tokio::test]
async fn read_binding_refuses_post_and_forged_candidate_mode_before_dispatch() {
    use wamn_runtime::plugins::connection_http::{
        ConnectionError, ConnectionExecutionClosure, ConnectionHttp, ConnectionInvocation,
        ConnectionOrigin, InvocationEntry, WiringPosition, transport::HttpTransport,
    };
    let _lock = wamn_test_postgres::lock();
    let database = wamn_platform_identity::test_database::project_platform();
    let mut project = connect(database.url()).await;
    provision_project(&mut project, database.url()).await;
    let document = WiringDocument::new(
        "http_read",
        1,
        "read",
        BTreeMap::from([(
            "read".into(),
            WiringNode {
                component: "http-sidecar".into(),
                interface_version: "0.1.0".into(),
                operation: "wamn:node/handler@0.1.0".into(),
                operation_dependency: None,
                params: BTreeMap::new(),
                terminal: Some(WiringTerminal::Respond),
            },
        )]),
        Vec::new(),
    )
    .unwrap();
    sqlx::query("INSERT INTO catalog.workflows (project_env_key,environment,application,workflow_id,version,graph_json,wiring_hash,trigger,execution_type,created_by) VALUES ($1,$2,$3,'http_read',1,$4::text::jsonb,$5,'manual','invoker',$6::text::uuid)").bind(&PROJECT_ENV_KEY).bind(&ENVIRONMENT).bind(&PACKAGE).bind(&serde_json::to_string(&document).unwrap()).bind(&document.wiring_hash().as_str()).bind(&wamn_identity_protocol::workflow_user_id().to_string()).execute(&mut project).await.map(|result| result.rows_affected()).unwrap();
    let components = provision_project_components_for_http_manifest();
    let manifest = wamn_catalog::ServingManifest::new(
        wamn_catalog::ServingRelease {
            packages: [wamn_catalog::PackageCoordinate::new(PACKAGE, PACKAGE_VERSION).unwrap()]
                .into(),
        },
        components,
        std::collections::BTreeSet::new(),
        BTreeMap::new(),
    )
    .unwrap();
    let manifest_digest = manifest.digest().as_str().to_owned();
    sqlx::query("INSERT INTO catalog.releases(project_env_key,manifest_digest,canonical_bytes) VALUES ($1,$2,$3)").bind(&PROJECT_ENV_KEY).bind(&manifest_digest).bind(&manifest.canonical_bytes()).execute(&mut project).await.map(|result| result.rows_affected()).unwrap();
    let address = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
    address.connect("192.0.2.1:9").unwrap();
    let listener = tokio::net::TcpListener::bind((address.local_addr().unwrap().ip(), 0))
        .await
        .unwrap();
    let mut authority = Authority::new(&format!("http://{}", listener.local_addr().unwrap())).await;
    let mut declaration = Authority::document(&manifest_digest);
    declaration
        .applications
        .get_mut(PACKAGE)
        .unwrap()
        .connections = BTreeMap::from([("erp".into(), "http-read".into())]);
    declaration.integrations.remove("labels-store");
    authority
        .apply(
            &mut project,
            database.url(),
            &declaration,
            &[declarations("http", "read")],
        )
        .await;
    let postgres = Arc::new(
        WamnPostgres::new(WamnPostgresConfig {
            credentials: Some(ClassCredentials::every_class(database.url().to_owned())),
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 2000,
            statement_timeout_ms: 5000,
            row_limit: 100,
        })
        .unwrap(),
    );
    postgres
        .set_project_env_key(COMPONENT_ID, PROJECT_ENV_KEY)
        .unwrap();
    let facts = Arc::new(ConnectionFacts::new(
        Arc::new(ConnectionMemory::default()),
        wamn_broker::Publisher::default(),
        Arc::new(authority.reader().await),
        &postgres,
        PROJECT_ENV_KEY,
        DEFAULT_PROJECT,
        ENVIRONMENT,
    ));
    let release = Arc::new(
        wamn_engine::release_manifest::LoadedRelease::load_canonical_bytes(
            &manifest.canonical_bytes(),
            "read binding fixture",
            wamn_engine::release_manifest::ReleaseTarget::new(PROJECT_ENV_KEY, ENVIRONMENT)
                .with_applications(
                    serde_json::from_value(json!({PACKAGE: {"packages": [PACKAGE]}})).unwrap(),
                ),
        )
        .unwrap(),
    );
    let mut invocation = ConnectionInvocation {
        origin: ConnectionOrigin {
            package_id: PACKAGE.into(),
            component_digest: HTTP_SIDECAR.into(),
            component: "http-sidecar".into(),
            interface_version: "0.1.0".into(),
            operation: "wamn:node/handler@0.1.0".into(),
        },
        entry: InvocationEntry::Wiring(WiringPosition {
            package_id: PACKAGE.into(),
            wiring_id: "http_read".into(),
            wiring_version: 1,
            node_id: "read".into(),
            occurrence: 0,
            configuration: Default::default(),
            wiring_hash: document.wiring_hash().as_str().into(),
        }),
        package_id: PACKAGE.into(),
        component_digest: HTTP_SIDECAR.into(),
        component: "http-sidecar".into(),
        operation: "wamn:node/handler@0.1.0".into(),
        closure: ConnectionExecutionClosure::Released,
        effects: None,
    };
    wamn_runtime::invocation::bind_test_context(
        &postgres,
        COMPONENT_ID,
        Arc::clone(&facts),
        Arc::clone(&release),
    );
    let plugin = ConnectionHttp::new(
        Arc::clone(&postgres),
        Arc::new(HttpTransport::new().unwrap()),
        vec![
            format!("http://{}", listener.local_addr().unwrap())
                .parse()
                .unwrap(),
        ]
        .into(),
    );
    postgres
        .bind_invocation(COMPONENT_ID, invocation.clone())
        .unwrap();
    assert!(matches!(
        plugin.send_for_test(COMPONENT_ID, "erp", "POST").await,
        Err(ConnectionError::AuthorityDenied)
    ));
    assert!(matches!(
        plugin.send_for_test(COMPONENT_ID, "erp", "GET").await,
        Err(ConnectionError::CredentialUnavailable)
    ));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
    let entry = wamn_runtime::plugins::wamn_postgres::ConnectionEntryLookup {
        package_id: PACKAGE,
        wiring: Some(wamn_runtime::plugins::wamn_postgres::WiringLookup {
            wiring_id: "http_read",
            wiring_version: 1,
            node_id: "read",
        }),
    };
    let snapshot = postgres
        .connection_effect_snapshot(
            COMPONENT_ID,
            PROJECT_ENV_KEY,
            &ConnectionEffectLookup {
                entry,
                origin_package_id: PACKAGE,
                origin_component_digest: HTTP_SIDECAR,
                origin_component: "http-sidecar",
                origin_interface_version: "0.1.0",
                origin_operation: "wamn:node/handler@0.1.0",
                package_id: PACKAGE,
                operation: "wamn:node/handler@0.1.0",
                manifest_digest: &manifest_digest,
                environment: ENVIRONMENT,
                component_digest: HTTP_SIDECAR,
                store_alias: "erp",
                candidate_binding: None,
            },
            &facts,
            &release,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(snapshot.read_only);
    postgres.revoke_invocation(COMPONENT_ID);
    let forged = CandidateConnectionBinding {
        read_only: false,
        component_digest: HTTP_SIDECAR.into(),
        store_alias: "erp".into(),
        requirement_hash: snapshot.requirement_hash.unwrap(),
        instance_id: snapshot.instance_id.unwrap(),
        instance_revision: snapshot.instance_revision.unwrap(),
        requirement_type: snapshot.requirement_type.unwrap(),
        contract: snapshot.contract.unwrap(),
        validation_hash: snapshot.validation_hash.unwrap(),
        generation: snapshot.generation.unwrap(),
        definition_hash: snapshot.definition_hash.unwrap(),
        credential_set_handle: snapshot.credential_handle,
    };
    invocation.closure = ConnectionExecutionClosure::Candidate {
        manifest_digest,
        environment: ENVIRONMENT.into(),
        wiring_hash: document.wiring_hash().as_str().into(),
        component: "http-sidecar".into(),
        interface_version: "0.1.0".into(),
        binding_world: Arc::new(
            wamn_runtime::plugins::wamn_postgres::CandidateBindingWorld::from_json(
                serde_json::to_value([forged]).unwrap(),
            )
            .unwrap(),
        ),
    };
    wamn_runtime::invocation::bind_test_context(
        &postgres,
        COMPONENT_ID,
        Arc::clone(&facts),
        Arc::clone(&release),
    );
    let plugin = ConnectionHttp::new(
        Arc::clone(&postgres),
        Arc::new(HttpTransport::new().unwrap()),
        vec![
            format!("http://{}", listener.local_addr().unwrap())
                .parse()
                .unwrap(),
        ]
        .into(),
    );
    postgres.bind_invocation(COMPONENT_ID, invocation).unwrap();
    assert!(matches!(
        plugin.send_for_test(COMPONENT_ID, "erp", "POST").await,
        Err(ConnectionError::AttestationInvalid)
    ));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
    drop(project);
}

#[tokio::test]
async fn release_bindings_round_trip_through_the_plugins_own_resolution() {
    use wamn_catalog::{
        BlobstoreProvider, ComponentConnection, ComponentConnectionAlias, ComponentConnectionType,
        ComponentDescriptor, ConnectionMode, PALETTE_PACKAGE_ID,
    };
    use wamn_runtime::plugins::connection_http::{
        ConnectionExecutionClosure, ConnectionInvocation, ConnectionOrigin, InvocationEntry,
        WiringPosition,
    };
    use wamn_runtime::plugins::wamn_blobstore::binding;
    use wamn_runtime::plugins::wamn_postgres::CandidateBindingWorld;
    use wamn_runtime::resolve_connection_for_test as resolve;
    const DIGEST: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
    const OPERATION: &str = "wamn:node/handler@0.1.0";
    let _lock = wamn_test_postgres::lock();
    let database = wamn_platform_identity::test_database::project_platform();
    let mut project = connect(database.url()).await;
    provision_project(&mut project, database.url()).await;
    let mut descriptor = ComponentDescriptor::named("blob-put", "0.1.0", DIGEST);
    descriptor.operations.insert(
        OPERATION.into(),
        serde_json::from_value(json!({
            "input-ports":[], "output-ports":[], "parameters":[]
        }))
        .unwrap(),
    );
    descriptor.connections.push(ComponentConnection {
        store_alias: ComponentConnectionAlias::Configuration {
            parameter: "binding".into(),
        },
        requirement_type: ComponentConnectionType::Blobstore,
        mode: ConnectionMode::ByBinding(
            [PACKAGE, "nested_origin"]
                .into_iter()
                .map(|package| {
                    (
                        package.into(),
                        BTreeMap::from([("labels".into(), ConnectionMode::Write)]),
                    )
                })
                .collect(),
        ),
    });
    descriptor
        .operations
        .get_mut(OPERATION)
        .unwrap()
        .connections = descriptor
        .connections
        .iter()
        .cloned()
        .map(Into::into)
        .collect();
    let manifest = wamn_catalog::ServingManifest::new(
        wamn_catalog::ServingRelease {
            packages: [PACKAGE, "nested_origin"]
                .into_iter()
                .map(|package| {
                    wamn_catalog::PackageCoordinate::new(package, PACKAGE_VERSION).unwrap()
                })
                .collect(),
        },
        [wamn_catalog::ServingComponent {
            package_id: PALETTE_PACKAGE_ID.into(),
            runtime_schema: None,
            descriptor,
            statement_instances: BTreeMap::new(),
        }]
        .into(),
        Default::default(),
        BTreeMap::new(),
    )
    .unwrap();
    let digest = manifest.digest().as_str().to_owned();
    sqlx::query("INSERT INTO catalog.releases(project_env_key,manifest_digest,canonical_bytes) VALUES ($1,$2,$3)")
        .bind(PROJECT_ENV_KEY).bind(&digest).bind(manifest.canonical_bytes())
        .execute(&mut project).await.unwrap();
    for package in [PACKAGE, "nested_origin"] {
        sqlx::query("INSERT INTO catalog.connection_requirement(project_env_key,component_digest,package_id,name,interface,mode) VALUES($1,$2,$3,'labels','wasmcloud:blobstore/blobstore@0.1.0','write')")
            .bind(PROJECT_ENV_KEY).bind(DIGEST).bind(package).execute(&mut project).await.unwrap();
    }
    let mut authority = Authority::new("http://10.0.0.9:8080").await;
    let mut document = Authority::document(&digest);
    document.applications.get_mut(PACKAGE).unwrap().packages =
        [PACKAGE.to_owned(), "nested_origin".into()].into();
    let packages: Vec<_> = [PACKAGE, "nested_origin"]
        .into_iter()
        .map(|package| {
            let mut declaration = declarations("s3", "write");
            declaration.package.id = package.into();
            declaration
        })
        .collect();
    authority
        .apply(&mut project, database.url(), &document, &packages)
        .await;
    let release = wamn_engine::release_manifest::LoadedRelease::load_canonical_bytes(
        &manifest.canonical_bytes(),
        "shared blob alias fixture",
        wamn_engine::release_manifest::ReleaseTarget::new(PROJECT_ENV_KEY, ENVIRONMENT)
            .with_applications(document.membership()),
    )
    .unwrap();
    let postgres = Arc::new(
        WamnPostgres::new(WamnPostgresConfig {
            credentials: Some(ClassCredentials::every_class(database.url().to_owned())),
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 2000,
            statement_timeout_ms: 5000,
            row_limit: 100,
        })
        .unwrap(),
    );
    postgres
        .set_project_env_key(COMPONENT_ID, PROJECT_ENV_KEY)
        .unwrap();
    let facts = ConnectionFacts::new(
        Arc::new(ConnectionMemory::default()),
        wamn_broker::Publisher::default(),
        Arc::new(authority.reader().await),
        &postgres,
        PROJECT_ENV_KEY,
        DEFAULT_PROJECT,
        ENVIRONMENT,
    );
    let wiring = WiringDocument::new(
        "store_label",
        1,
        "store",
        BTreeMap::from([(
            "store".into(),
            WiringNode {
                component: "blob-put".into(),
                interface_version: "0.1.0".into(),
                operation: OPERATION.into(),
                operation_dependency: None,
                params: BTreeMap::from([("binding".into(), json!("labels"))]),
                terminal: Some(WiringTerminal::Respond),
            },
        )]),
        Vec::new(),
    )
    .unwrap();
    let mut invocation = ConnectionInvocation {
        origin: ConnectionOrigin {
            package_id: PALETTE_PACKAGE_ID.into(),
            component_digest: DIGEST.into(),
            component: "blob-put".into(),
            interface_version: "0.1.0".into(),
            operation: OPERATION.into(),
        },
        entry: InvocationEntry::Wiring(WiringPosition {
            package_id: PACKAGE.into(),
            wiring_id: "store_label".into(),
            wiring_version: 1,
            node_id: "store".into(),
            occurrence: 0,
            configuration: Arc::new(wiring.nodes["store"].params.clone()),
            wiring_hash: wiring.wiring_hash().as_str().into(),
        }),
        package_id: PALETTE_PACKAGE_ID.into(),
        component_digest: DIGEST.into(),
        component: "blob-put".into(),
        operation: OPERATION.into(),
        closure: ConnectionExecutionClosure::Released,
        effects: None,
    };
    postgres
        .bind_invocation(COMPONENT_ID, invocation.clone())
        .unwrap();
    let snapshot = resolve(&postgres, &facts, &release, COMPONENT_ID, "labels")
        .await
        .unwrap();
    assert!(binding::resolve(&snapshot, BlobstoreProvider::S3).is_ok());
    assert!(
        resolve(&postgres, &facts, &release, COMPONENT_ID, "undeclared")
            .await
            .is_err()
    );
    let expected = facts.env_connections().await.unwrap().connections["labels-store"];
    assert_eq!(
        snapshot
            .instance_id
            .as_deref()
            .unwrap()
            .parse::<uuid::Uuid>()
            .unwrap(),
        expected
    );
    let pinned = CandidateConnectionBinding {
        read_only: snapshot.read_only,
        component_digest: DIGEST.into(),
        store_alias: "labels".into(),
        requirement_hash: snapshot.requirement_hash.unwrap(),
        instance_id: snapshot.instance_id.unwrap(),
        instance_revision: snapshot.instance_revision.unwrap(),
        requirement_type: snapshot.requirement_type.unwrap(),
        contract: snapshot.contract.unwrap(),
        validation_hash: snapshot.validation_hash.unwrap(),
        generation: snapshot.generation.unwrap(),
        definition_hash: snapshot.definition_hash.unwrap(),
        credential_set_handle: snapshot.credential_handle,
    };
    for (accepted, pin) in [
        (true, pinned.clone()),
        (
            false,
            CandidateConnectionBinding {
                instance_id: uuid::Uuid::new_v4().to_string(),
                ..pinned.clone()
            },
        ),
        (
            false,
            CandidateConnectionBinding {
                generation: pinned.generation + 1,
                ..pinned
            },
        ),
    ] {
        invocation.closure = ConnectionExecutionClosure::Candidate {
            manifest_digest: digest.clone(),
            environment: ENVIRONMENT.into(),
            wiring_hash: invocation.entry.wiring().unwrap().wiring_hash.clone(),
            component: invocation.component.clone(),
            interface_version: invocation.origin.interface_version.clone(),
            binding_world: Arc::new(
                CandidateBindingWorld::from_json(serde_json::to_value([pin]).unwrap()).unwrap(),
            ),
        };
        postgres.revoke_invocation(COMPONENT_ID);
        postgres
            .bind_invocation(COMPONENT_ID, invocation.clone())
            .unwrap();
        assert_eq!(
            resolve(&postgres, &facts, &release, COMPONENT_ID, "labels")
                .await
                .is_ok(),
            accepted
        );
    }
    invocation.closure = ConnectionExecutionClosure::Released;
    postgres.revoke_invocation(COMPONENT_ID);
    postgres.bind_invocation(COMPONENT_ID, invocation).unwrap();
    let conflicting = facts.env_connections().await.unwrap().connections["http-read"];
    sqlx::query("UPDATE catalog.connection_binding SET connection_id=$4 WHERE project_env_key=$1 AND manifest_digest=$2 AND component_digest=$3 AND package_id='nested_origin' AND name='labels'")
        .bind(PROJECT_ENV_KEY).bind(&digest).bind(DIGEST).bind(conflicting).execute(&mut project).await.unwrap();
    assert!(
        resolve(&postgres, &facts, &release, COMPONENT_ID, "labels")
            .await
            .is_err()
    );
}
