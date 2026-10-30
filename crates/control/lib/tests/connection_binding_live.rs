//! Release bindings, scope refusals, and native read authority on the environment catalog.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use serde_json::json;
use tokio_postgres::{Client, NoTls};
use wamn_catalog::{
    AdmittedComponent, AdmittedComponentOperation, ComponentPackageScope, ConnectionTypeDescriptor,
    RequirementType, WiringDocument, WiringNode, WiringTerminal,
};
use wamn_control::apply_package::{self, ApplyPackageRequest};
use wamn_control::environment::{bindings, document::EnvironmentDocument};
use wamn_platform_identity::connection_changes::ConnectionDrops;
use wamn_runtime::connection_credentials::ConnectionCredentials;
use wamn_runtime::connection_facts::ConnectionFacts;
use wamn_runtime::connection_memory::ConnectionMemory;
use wamn_runtime::plugins::wamn_blobstore::binding::{self, BindingError};
use wamn_runtime::plugins::wamn_postgres::{
    CandidateConnectionBinding, ClassCredentials, ConnectionEffectLookup, DEFAULT_PROJECT,
    WamnPostgres, WamnPostgresConfig,
};
use wamn_schema_control::connections::ComponentConnectionRequirement;
use wamn_test_infrastructure::record_history::assert_stamp;

const ENV_KEY: &str = "connection-binding-tenant--fixture--dev";
const ENVIRONMENT: &str = "dev";
const PACKAGE: &str = "connection_binding_fixture";
const PACKAGE_VERSION: &str = "1.0.0";
const COMPONENT_ID: &str = "connection-binding-component";
const BLOB_PUT: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
const HTTP_SIDECAR: &str =
    "sha256:3333333333333333333333333333333333333333333333333333333333333333";
const FACT_FINGERPRINT: &str =
    "sha256:6666666666666666666666666666666666666666666666666666666666666666";
/// The fixture release: its packages, nested ones included, the
/// components it serves, each with its admitted descriptor, which is where the
/// runtime reads component facts (wamn-vavs4.3), and the graph of each wiring,
/// which is where the runtime reads wirings (wamn-ps2tj.4).
const RELEASE_BYTES: &str = concat!(
    r#"{"components":["#,
    r#"{"descriptor":{"component":"blob-put","component-digest":"sha256:2222222222222222222222222222222222222222222222222222222222222222","connections":[{"requirement-type":"blobstore","store-alias":"labels"}],"effects":[],"imports":[],"imports-fingerprint":"sha256:6666666666666666666666666666666666666666666666666666666666666666","interface-version":"0.1.0","operations":{"wamn:node/handler@0.1.0":{"input-ports":[],"output-ports":[],"parameters":[]}}},"package-id":"connection_binding_fixture"},"#,
    r#"{"descriptor":{"component":"http-sidecar","component-digest":"sha256:3333333333333333333333333333333333333333333333333333333333333333","connections":[{"requirement-type":"http","store-alias":"erp"}],"effects":[],"imports":[],"imports-fingerprint":"sha256:6666666666666666666666666666666666666666666666666666666666666666","interface-version":"0.1.0","operations":{"wamn:node/handler@0.1.0":{"input-ports":[],"output-ports":[],"parameters":[]}}},"package-id":"connection_binding_fixture"},"#,
    r#"{"descriptor":{"component":"nested-origin","component-digest":"sha256:4444444444444444444444444444444444444444444444444444444444444444","connections":[{"requirement-type":"http","store-alias":"origin-http"}],"effects":[],"imports":[],"imports-fingerprint":"sha256:6666666666666666666666666666666666666666666666666666666666666666","interface-version":"0.1.0","operations":{"origin-call":{"input-ports":[],"output-ports":[],"parameters":[]}}},"package-id":"nested_origin"}],"#,
    r#""release":{"packages":[{"package-id":"connection_binding_fixture","package-version":"1.0.0"},"#,
    r#"{"package-id":"nested_origin","package-version":"1.0.0"},"#,
    r#"{"package-id":"nested_wiring","package-version":"1.0.0"}]},"#,
    r#""attachments":{}}"#
);
/// A release that serves none of the bound components.
const OTHER_RELEASE_BYTES: &str = r#"{"components":[]}"#;

/// `sha256:<hex>` of `bytes`, the digest `catalog.releases` keys them by.
fn digest_of(bytes: &str) -> String {
    format!(
        "sha256:{}",
        hex::encode(ring::digest::digest(
            &ring::digest::SHA256,
            bytes.as_bytes()
        ))
    )
}

async fn connect(url: &str) -> (Client, tokio::task::JoinHandle<()>) {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to a disposable database");
    let task = tokio::spawn(async move {
        let _ = connection.await;
    });
    (client, task)
}

fn wiring() -> WiringDocument {
    WiringDocument::new(
        "store_label",
        1,
        "store",
        BTreeMap::from([(
            "store".to_string(),
            WiringNode {
                component: "blob-put".to_string(),
                interface_version: "0.1.0".to_string(),
                operation: "wamn:node/handler@0.1.0".to_string(),
                operation_dependency: None,
                params: BTreeMap::new(),
                terminal: Some(WiringTerminal::Respond),
            },
        )]),
        Vec::new(),
        Vec::new(),
    )
    .expect("fixture wiring is structurally valid")
}

/// One admitted component with one declared connection. It records the
/// requirement row built by the control library from the platform's own
/// descriptor, and returns the
/// admitted facts, which authorship reads from the verified package artifact
/// in production (`wamn_package_artifact::load_component_facts`).
async fn admit_component(
    project: &Client,
    package_id: &str,
    component: &str,
    digest: &str,
    operation: &str,
    store_alias: &str,
    descriptor: ConnectionTypeDescriptor,
) -> AdmittedComponent {
    let admitted = AdmittedComponent {
        scope: ComponentPackageScope {
            env_key: ENV_KEY.to_owned(),
            package_id: package_id.to_owned(),
            package_version: PACKAGE_VERSION.to_owned(),
        },
        component: component.to_owned(),
        interface_version: "0.1.0".to_owned(),
        operations: BTreeMap::from([(
            operation.to_owned(),
            // These fixture operations register no package operation.
            AdmittedComponentOperation {
                committed_result_schema: None,
                registered_operation: None,
                host_transaction: false,
                input_ports: Vec::new(),
                output_ports: Vec::new(),
                parameters: Vec::new(),
                statements: BTreeMap::new(),
            },
        )]),
        component_digest: digest.to_owned(),
        imports: Vec::new(),
        imports_fingerprint: FACT_FINGERPRINT.to_owned(),
        effects: Vec::new(),
    };
    let requirement = ComponentConnectionRequirement::new(digest, store_alias, descriptor);
    project
        .execute(
            "INSERT INTO catalog.connection_requirement(env_key,component_digest,package_id,name,interfaces,mode) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING",
            &[
                &ENV_KEY,
                &requirement.component_digest(),
                &package_id,
                &requirement.store_alias(),
                &vec![requirement.requirement().contract.clone()],
                &if requirement.requirement().requirement_type=="http" {"read"} else {"write"},
            ],
        )
        .await
        .expect("record the fixture component's connection requirement");
    admitted
}

/// Provision the project and return the admitted components of the package.
async fn provision_project(project: &Client, project_url: &str) -> Vec<AdmittedComponent> {
    project.batch_execute(&format!(
        "SELECT set_config('app.user_id', '{}', false), set_config('app.operation', 'admin:seed-connection-fixture', false)",
        wamn_control_provision::PlatformComponent::Provisioning.principal_id()
    )).await.unwrap();
    // apply-package narrows migration authority to wamn_db_owner, a
    // CLUSTER-wide role. On a fresh server it does not exist; the sibling
    // fixture that omits this passes only where an earlier run left the role
    // behind -- a leftover healthy object masking an incomplete fixture.
    project
        .batch_execute(wamn_control_provision::sql::ensure_db_owner_role_sql())
        .await
        .expect("ensure the production package-owner role");
    project
        .batch_execute(
            "DO $grant$ BEGIN \
               EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()); \
             END $grant$;",
        )
        .await
        .expect("let the package owner create schemas in this database");
    // The plugin's platform checkout runs an R18 membership probe on every new
    // connection: the connecting role must be a member of the class's ACL role
    // (CallableHttp -> wamn_http_admitter). A superuser passes the probe once
    // the role EXISTS; on a fresh server it does not.
    project
        .batch_execute(&wamn_control_provision::sql::ensure_workload_acl_role_sql(
            wamn_control_provision::WorkloadRoleFamily::HttpAdmitter,
        ))
        .await
        .expect("ensure the CallableHttp class ACL role the pool probes for");
    project
        .batch_execute("CREATE SCHEMA IF NOT EXISTS wamn_run")
        .await
        .unwrap();
    project
        .batch_execute(&wamn_control_provision::sql::grant_administration_surface_sql("wamn_run"))
        .await
        .unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/connection_binding");
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
    let package = scratch.path().join("package");
    std::fs::create_dir_all(package.join("migrations")).unwrap();
    std::fs::copy(source.join("wamn.json"), package.join("wamn.json")).unwrap();
    for entry in std::fs::read_dir(source.join("migrations")).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(
            entry.path(),
            package.join("migrations").join(entry.file_name()),
        )
        .unwrap();
    }
    wamn_test_infrastructure::fixture_digest::write_receipt(&package);
    apply_package::apply_package(ApplyPackageRequest {
        component_source: wamn_control::apply_package::component_source_from_env(None).unwrap(),
        changes: wamn_platform_identity::auth_changes::AuthChanges::default(),
        package: wamn_control::apply_package::PackageRoot {
            path: package.clone(),
            artifact_digest: Some(
                wamn_test_infrastructure::fixture_digest::fixture_artifact_digest(&package)
                    .expect("read the fixture package root"),
            ),
        },
        database_url: project_url.to_string(),
        org: "connection-binding-tenant".into(),
        project: "fixture".into(),
        environment: "dev".into(),
    })
    .await
    .expect("apply the connection-binding package fixture");
    project
        .execute("SELECT set_config('app.env_key', $1, false)", &[&ENV_KEY])
        .await
        .expect("scope the project seed session");
    let blob_put = admit_component(
        project,
        PACKAGE,
        "blob-put",
        BLOB_PUT,
        "wamn:node/handler@0.1.0",
        "labels",
        ConnectionTypeDescriptor::blobstore_v1(),
    )
    .await;
    // A second alias of a DIFFERENT type, for the mismatch control.
    let http_sidecar = admit_component(
        project,
        PACKAGE,
        "http-sidecar",
        HTTP_SIDECAR,
        "wamn:node/handler@0.1.0",
        "erp",
        ConnectionTypeDescriptor::http_v1(),
    )
    .await;
    project
        .execute(
            "INSERT INTO catalog.releases (env_key, manifest_digest, canonical_bytes) \
             VALUES ($1, $2, $3)",
            &[
                &ENV_KEY,
                &digest_of(RELEASE_BYTES),
                &RELEASE_BYTES.as_bytes(),
            ],
        )
        .await
        .expect("publish the fixture release");
    for (application, document) in [
        (PACKAGE, wiring()),
        (
            "nested_wiring",
            WiringDocument::new(
                "store_label",
                1,
                "store",
                BTreeMap::from([(
                    "store".to_owned(),
                    WiringNode {
                        component: "nested-origin".to_owned(),
                        interface_version: "0.1.0".to_owned(),
                        operation: "origin-call".to_owned(),
                        operation_dependency: None,
                        params: BTreeMap::new(),
                        terminal: Some(WiringTerminal::Respond),
                    },
                )]),
                Vec::new(),
                Vec::new(),
            )
            .unwrap(),
        ),
    ] {
        project.execute("INSERT INTO catalog.workflows
            (env_key, environment, application, workflow_id, version, graph_json, wiring_hash, trigger, execution_type, created_by)
            VALUES ($1, $2, $3, $4, 1, $5::text::jsonb, $6, 'manual', 'invoker', $7::text::uuid)",
            &[&ENV_KEY, &ENVIRONMENT, &application, &document.wiring_id, &serde_json::to_string(&document).unwrap(),
              &document.wiring_hash().as_str(), &wamn_project_state::workflow_user_id().to_string()]
        ).await.expect("seed the workflow connection lookup");
    }
    vec![blob_put, http_sidecar]
}

struct Authority {
    database: wamn_test_postgres::Database,
    system: Client,
    _store: Arc<wamn_test_infrastructure::credentials::LocalCredentials>,
}
impl Authority {
    async fn new(http_origin: &str) -> Self {
        let database = wamn_control_provision::test_database::system();
        let (mut system, _) = connect(database.url()).await;
        system.batch_execute("SET ROLE wamn_system").await.unwrap();
        system
            .batch_execute(
                &wamn_control_provision::test_database::seed_org_project_env_sql(
                    "connection-binding-tenant",
                    DEFAULT_PROJECT,
                    "dev",
                ),
            )
            .await
            .unwrap();
        system.batch_execute(&format!("BEGIN; {} INSERT INTO registry.target(name,type,status) VALUES('cloud','cloud','active') ON CONFLICT DO NOTHING; UPDATE registry.meta SET platform_domain='example.invalid'; UPDATE registry.project_envs SET target_id=(SELECT id FROM registry.target WHERE name='cloud'); COMMIT;",wamn_control_provision::bind_platform_principal_sql(wamn_control_provision::PlatformComponent::Provisioning))).await.unwrap();
        let store =
            Arc::new(wamn_test_infrastructure::credentials::LocalCredentials::create().unwrap());
        let tx = system.transaction().await.unwrap();
        tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
        .await
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
            wamn_test_infrastructure::credentials::define(&tx,"connection-binding-tenant",&json!({"project":DEFAULT_PROJECT,"env":"dev","name":name,"integration_type":integration_type,"auth_type":auth_type,"parameters":parameters,"credential":credential.to_string()}),store.as_ref()).await.unwrap();
        }
        tx.commit().await.unwrap();
        Self {
            database,
            system,
            _store: store,
        }
    }
    fn document(digest: &str) -> EnvironmentDocument {
        serde_json::from_value(json!({"org":"connection-binding-tenant","project":DEFAULT_PROJECT,"env":"dev","target":"cloud","release":digest,"route_host":"fixture.example","policy":"dev", "integrations":{"labels-store":{"version":1},"http-read":{"version":1}},"applications":{PACKAGE:{"packages":[PACKAGE,"nested_origin","nested_wiring"],"connections":{"labels":"labels-store","erp":"http-read"}}}})).unwrap()
    }
    async fn apply(
        &mut self,
        project: &mut Client,
        url: &str,
        document: &EnvironmentDocument,
        packages: &[wamn_schema_generator::PackageManifest],
    ) {
        self.system
            .batch_execute("SET ROLE wamn_db_owner")
            .await
            .unwrap();
        let selected = bindings::analyse(&self.system, document, packages)
            .await
            .unwrap();
        let _ = bindings::project_authority(self.database.url(), url, document)
            .await
            .unwrap();
        let digest = match &document.release {
            wamn_control::environment::document::DeclaredRelease::Digest(digest) => digest.as_str(),
            wamn_control::environment::document::DeclaredRelease::None => unreachable!(),
        };
        // A write records the memory key of its release only, and a repeated
        // write that changes no row records nothing.
        for repeat in [false, true] {
            let mut drops = ConnectionDrops::default();
            let tx = project.transaction().await.unwrap();
            bindings::write(&tx, ENV_KEY, digest, document, &selected, &mut drops)
                .await
                .unwrap();
            tx.commit().await.unwrap();
            let mut expected = ConnectionDrops::default();
            if !repeat && !drops.is_empty() {
                expected.release(ENV_KEY, digest);
            }
            assert_eq!(drops, expected, "repeated write: {repeat}");
        }
    }
    async fn reader(&self) -> ConnectionCredentials {
        let role = format!("reader_{}", self.database.name());
        self.system
            .batch_execute(&format!(
                "RESET ROLE; BEGIN; {} {}",
                wamn_control_provision::bind_platform_principal_sql(
                    wamn_control_provision::PlatformComponent::Provisioning
                ),
                wamn_control_provision::sql::prepare_workload_generation_sql(
                    wamn_control_provision::WorkloadRoleFamily::ConnectionReader,
                    self.database.name(),
                    &role,
                    "fixture-password",
                    "2099-01-01T00:00:00Z"
                )
            ))
            .await
            .unwrap();
        self.system
            .execute(
                wamn_control_provision::sql::upsert_control_author_env_key_mapping_sql(),
                &[
                    &role,
                    &ENV_KEY,
                    &"connection-binding-tenant",
                    &DEFAULT_PROJECT,
                    &ENVIRONMENT,
                ],
            )
            .await
            .unwrap();
        self.system.batch_execute("COMMIT").await.unwrap();
        let mut url = url::Url::parse(self.database.url()).unwrap();
        url.set_username(&role).unwrap();
        url.set_password(Some("fixture-password")).unwrap();
        let (reader, _) = connect(url.as_str()).await;
        ConnectionCredentials::from_database(Arc::new(reader), "connection-binding-tenant")
    }
}
fn declarations(integration_type: &str, mode: &str) -> wamn_schema_generator::PackageManifest {
    let interfaces = vec!["wamn:connection/http@0.1.0"];
    let connections = if integration_type == "s3" {
        json!({"labels":{"interfaces":["wasmcloud:blobstore/blobstore@0.1.0"],"mode":"write"},"erp":{"interfaces":["wamn:connection/http@0.1.0"],"mode":"read"}})
    } else {
        json!({"erp":{"interfaces":interfaces,"mode":mode}})
    };
    serde_json::from_value(json!({"package":{"id":PACKAGE,"version":PACKAGE_VERSION},"required_platform_policy_contract":{"id":"fixture_access","state":"satisfied"},"models":{},"components":{},"connections":connections})).unwrap()
}
async fn assert_nested_effect_snapshot(
    project: &Client,
    postgres: &WamnPostgres,
    direct: ConnectionEffectLookup<'_>,
    facts: &ConnectionFacts,
) {
    const WIRING_PACKAGE: &str = "nested_wiring";
    const ORIGIN_PACKAGE: &str = "nested_origin";
    const ORIGIN_DIGEST: &str =
        "sha256:4444444444444444444444444444444444444444444444444444444444444444";
    for package_id in [WIRING_PACKAGE, ORIGIN_PACKAGE] {
        project
            .execute(
                "INSERT INTO catalog.packages \
                   (env_key, package_id, package_version, artifact_digest) \
                 VALUES ($1, $2, $3, $4)",
                &[&ENV_KEY, &package_id, &PACKAGE_VERSION, &FACT_FINGERPRINT],
            )
            .await
            .expect("seed a separate package coordinate for the snapshot test");
    }
    admit_component(
        project,
        ORIGIN_PACKAGE,
        "nested-origin",
        ORIGIN_DIGEST,
        "origin-call",
        "origin-http",
        ConnectionTypeDescriptor::http_v1(),
    )
    .await;

    // This shows the SQL snapshot only. The mounted-release helper owns the
    // separate test that the origin declares a dependency on this executor.
    let nested = ConnectionEffectLookup {
        entry: wamn_runtime::plugins::wamn_postgres::ConnectionEntryLookup {
            package_id: WIRING_PACKAGE,
            ..direct.entry
        },
        origin_package_id: ORIGIN_PACKAGE,
        origin_component_digest: ORIGIN_DIGEST,
        origin_component: "nested-origin",
        origin_operation: "origin-call",
        ..direct
    };
    let snapshot = postgres
        .connection_effect_snapshot(COMPONENT_ID, DEFAULT_PROJECT, ENV_KEY, &nested, facts)
        .await
        .expect("load the nested snapshot")
        .expect("the released wiring exists");
    assert!(snapshot.node_permitted);
    assert_eq!(snapshot.component.as_deref(), Some("blob-put"));
    assert_eq!(snapshot.operation.as_deref(), Some(direct.operation));
    let resolved = binding::resolve(&snapshot).expect("resolve the executor's bound alias");
    assert!(matches!(
        &resolved.provider,
        binding::BlobstoreProvider::S3 { credential_handle, .. } if credential_handle.starts_with("org:connection-binding-tenant:labels-store@")
    ));

    for (changed, refused) in [
        (
            "wiring package",
            ConnectionEffectLookup {
                entry: wamn_runtime::plugins::wamn_postgres::ConnectionEntryLookup {
                    package_id: ORIGIN_PACKAGE,
                    ..nested.entry
                },
                ..nested
            },
        ),
        (
            "origin package",
            ConnectionEffectLookup {
                origin_package_id: PACKAGE,
                ..nested
            },
        ),
        (
            "origin digest",
            ConnectionEffectLookup {
                origin_component_digest: BLOB_PUT,
                ..nested
            },
        ),
        (
            "origin component",
            ConnectionEffectLookup {
                origin_component: "blob-put",
                ..nested
            },
        ),
        (
            "origin interface",
            ConnectionEffectLookup {
                origin_interface_version: "wrong-interface",
                ..nested
            },
        ),
        (
            "origin operation",
            ConnectionEffectLookup {
                origin_operation: direct.operation,
                ..nested
            },
        ),
        (
            "origin node",
            ConnectionEffectLookup {
                entry: wamn_runtime::plugins::wamn_postgres::ConnectionEntryLookup {
                    wiring: nested.entry.wiring.map(|wiring| {
                        wamn_runtime::plugins::wamn_postgres::WiringLookup {
                            node_id: "missing-node",
                            ..wiring
                        }
                    }),
                    ..nested.entry
                },
                ..nested
            },
        ),
        (
            "executing package",
            ConnectionEffectLookup {
                package_id: ORIGIN_PACKAGE,
                ..nested
            },
        ),
        (
            "executing operation",
            ConnectionEffectLookup {
                operation: "origin-call",
                ..nested
            },
        ),
    ] {
        let snapshot = postgres
            .connection_effect_snapshot(COMPONENT_ID, DEFAULT_PROJECT, ENV_KEY, &refused, facts)
            .await
            .expect("load the mismatched snapshot");
        assert!(
            snapshot.is_none_or(|snapshot| !snapshot.node_permitted),
            "a mismatched {changed} must not authorize the executor"
        );
    }
}

async fn rows(project: &Client) -> serde_json::Value {
    project.query_one("SELECT jsonb_build_object('versions',(SELECT jsonb_agg(to_jsonb(r)||jsonb_build_object('xmin',r.xmin::text) ORDER BY r.connection_id) FROM catalog.release_connection r),'bindings',(SELECT jsonb_agg(to_jsonb(b)||jsonb_build_object('xmin',b.xmin::text) ORDER BY b.component_digest,b.name) FROM catalog.connection_binding b))",&[]).await.unwrap().get::<_,serde_json::Value>(0)
}

#[tokio::test]
async fn release_bindings_round_trip_through_the_plugins_own_resolution() {
    let _lock = wamn_test_postgres::lock();
    let project_database = wamn_project_state::test_database::tenant_platform();
    let project_url = project_database.url().to_owned();
    let (mut project, project_task) = connect(&project_url).await;
    provision_project(&project, &project_url).await;
    let mut authority = Authority::new("http://127.0.0.1:9/").await;
    // The plugin, constructed the way the host constructs it, scoped to the
    // component the way the host scopes it at bind time.
    let postgres = Arc::new(
        WamnPostgres::new(WamnPostgresConfig {
            credentials: Some(ClassCredentials::every_class(project_url.clone())),
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 2_000,
            statement_timeout_ms: 5_000,
            row_limit: 1_000,
        })
        .expect("construct the postgres plugin"),
    );
    postgres
        .set_env_key(COMPONENT_ID, ENV_KEY)
        .expect("register the component's tenant");
    let facts = Arc::new(ConnectionFacts::new(
        Arc::new(ConnectionMemory::default()),
        wamn_platform_identity::connection_changes::ConnectionChanges::default(),
        Arc::new(authority.reader().await),
        Arc::clone(&postgres),
        ENV_KEY,
        DEFAULT_PROJECT,
        ENVIRONMENT,
    ));
    let _reader = wamn_runtime::plugins::wamn_blobstore::plugin::WamnBlobstore::new(
        Arc::clone(&postgres),
        Arc::clone(&facts),
        ENV_KEY,
        DEFAULT_PROJECT,
        None,
    );
    let release: &'static str = Box::leak(digest_of(RELEASE_BYTES).into_boxed_str());
    let lookup = ConnectionEffectLookup {
        entry: wamn_runtime::plugins::wamn_postgres::ConnectionEntryLookup {
            package_id: PACKAGE,
            wiring: Some(wamn_runtime::plugins::wamn_postgres::WiringLookup {
                wiring_id: "store_label",
                wiring_version: 1,
                node_id: "store",
            }),
        },
        origin_package_id: PACKAGE,
        origin_component_digest: BLOB_PUT,
        origin_component: "blob-put",
        origin_interface_version: "0.1.0",
        origin_operation: "wamn:node/handler@0.1.0",
        package_id: PACKAGE,
        operation: "wamn:node/handler@0.1.0",
        manifest_digest: release,
        environment: ENVIRONMENT,
        component_digest: BLOB_PUT,
        store_alias: "labels",
        candidate_binding: None,
    };
    let snapshot_of = || async {
        postgres
            // WamnPostgres::new registers its one pool under DEFAULT_PROJECT;
            // the host names the project it serves, a fixture has only this.
            .connection_effect_snapshot(COMPONENT_ID, DEFAULT_PROJECT, ENV_KEY, &lookup, &facts)
            .await
            .expect("the plugin loads a snapshot for a declared alias")
            .expect("the declared alias has a snapshot, bound or not")
    };

    let before = snapshot_of().await;
    assert!(matches!(
        binding::resolve(&before),
        Err(BindingError::Unauthorized)
    ));
    let document = Authority::document(release);
    authority
        .apply(
            &mut project,
            &project_url,
            &document,
            &[declarations("s3", "write")],
        )
        .await;
    let before_reapply = rows(&project).await;
    authority
        .apply(
            &mut project,
            &project_url,
            &document,
            &[declarations("s3", "write")],
        )
        .await;
    assert_eq!(rows(&project).await, before_reapply);
    assert!(
        postgres
            .connection_effect_snapshot(COMPONENT_ID, DEFAULT_PROJECT, "other", &lookup, &facts)
            .await
            .is_err()
    );
    let after = snapshot_of().await;
    let resolved = binding::resolve(&after).unwrap();
    assert_eq!(resolved.container, "labels");
    assert_eq!(resolved.prefix, "fixture/");
    for relation in ["catalog.release_connection", "catalog.connection_binding"] {
        assert_stamp(
            &project,
            relation,
            &format!("env_key='{ENV_KEY}'"),
            &wamn_control_provision::PlatformComponent::Provisioning
                .principal_id()
                .to_string(),
            &wamn_control_provision::PlatformComponent::Provisioning
                .principal_id()
                .to_string(),
        )
        .await;
    }
    let pinned = CandidateConnectionBinding {
        read_only: false,
        component_digest: BLOB_PUT.into(),
        store_alias: "labels".into(),
        requirement_hash: after.requirement_hash.clone().unwrap(),
        instance_id: after.instance_id.clone().unwrap(),
        instance_revision: after.instance_revision.unwrap(),
        requirement_type: after.requirement_type.clone().unwrap(),
        contract: after.contract.clone().unwrap(),
        validation_hash: after.validation_hash.clone().unwrap(),
        generation: after.generation.unwrap(),
        definition_hash: after.definition_hash.clone().unwrap(),
        credential_set_handle: after.credential_handle.clone(),
    };
    for candidate in [
        CandidateConnectionBinding {
            instance_id: uuid::Uuid::new_v4().to_string(),
            ..pinned.clone()
        },
        CandidateConnectionBinding {
            generation: 99,
            ..pinned.clone()
        },
    ] {
        let snapshot = postgres
            .connection_effect_snapshot(
                COMPONENT_ID,
                DEFAULT_PROJECT,
                ENV_KEY,
                &ConnectionEffectLookup {
                    candidate_binding: Some(&candidate),
                    ..lookup
                },
                &facts,
            )
            .await
            .unwrap()
            .unwrap();
        assert!(binding::resolve(&snapshot).is_err());
    }
    for refused in [
        ConnectionEffectLookup {
            store_alias: "undeclared",
            ..lookup
        },
        ConnectionEffectLookup {
            manifest_digest: &digest_of(OTHER_RELEASE_BYTES),
            ..lookup
        },
    ] {
        let snapshot = postgres
            .connection_effect_snapshot(COMPONENT_ID, DEFAULT_PROJECT, ENV_KEY, &refused, &facts)
            .await
            .unwrap();
        assert!(snapshot.is_none_or(|snapshot| binding::resolve(&snapshot).is_err()));
    }
    let mut wrong = document.clone();
    wrong
        .applications
        .get_mut(PACKAGE)
        .unwrap()
        .connections
        .insert("erp".into(), "labels-store".into());
    authority
        .system
        .batch_execute("SET ROLE wamn_db_owner")
        .await
        .unwrap();
    assert_eq!(
        bindings::analyse(&authority.system, &wrong, &[declarations("s3", "write")])
            .await
            .unwrap_err()
            .code,
        "integration_import_unsatisfied"
    );
    // A ROUTE ENTRY: no wiring row participates. The same origin export
    // resolves the same bound connection, and the snapshot names no wiring.
    let route = ConnectionEffectLookup {
        entry: wamn_runtime::plugins::wamn_postgres::ConnectionEntryLookup {
            package_id: PACKAGE,
            wiring: None,
        },
        ..lookup
    };
    let route_snapshot = |lookup: ConnectionEffectLookup<'static>| {
        let postgres = Arc::clone(&postgres);
        let facts = Arc::clone(&facts);
        async move {
            postgres
                .connection_effect_snapshot(COMPONENT_ID, DEFAULT_PROJECT, ENV_KEY, &lookup, &facts)
                .await
                .expect("the plugin loads a route snapshot")
        }
    };
    let routed = route_snapshot(route)
        .await
        .expect("a route entry has a snapshot");
    assert_eq!(routed.wiring_hash, None);
    assert!(
        routed.node_permitted,
        "the origin exports the route operation"
    );
    assert_eq!(routed.definition_hash, after.definition_hash);
    let unexported = route_snapshot(ConnectionEffectLookup {
        origin_operation: "wamn:node/absent@0.1.0",
        ..route
    })
    .await;
    assert!(
        unexported.is_none_or(|snapshot| !snapshot.node_permitted),
        "a route whose origin does not export its operation must not authorize"
    );
    let foreign = route_snapshot(ConnectionEffectLookup {
        entry: wamn_runtime::plugins::wamn_postgres::ConnectionEntryLookup {
            package_id: "absent_package",
            wiring: None,
        },
        ..route
    })
    .await;
    assert!(
        foreign.is_none(),
        "a route in a package outside the release has no snapshot"
    );
    assert_nested_effect_snapshot(&project, &postgres, lookup, &facts).await;
    drop(project);
    project_task.abort();
}

fn provision_project_components_for_http_manifest()
-> std::collections::BTreeSet<wamn_catalog::ServingComponent> {
    let operation = "wamn:node/handler@0.1.0";
    let mut descriptor =
        wamn_catalog::ComponentDescriptor::named("http-sidecar", "0.1.0", HTTP_SIDECAR);
    descriptor.operations.insert(
        operation.into(),
        AdmittedComponentOperation {
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
        descriptor,
        operations: BTreeMap::from([(
            operation.into(),
            wamn_catalog::ServingComponentOperation {
                registered_operation: None,
                permissions: std::collections::BTreeSet::new(),
                statements: BTreeMap::new(),
                committed_result_schema: None,
            },
        )]),
        statement_instances: BTreeMap::new(),
    }]
    .into()
}

#[tokio::test]
async fn read_binding_refuses_post_and_forged_candidate_mode_before_dispatch() {
    use wamn_runtime::plugins::connection_http::{
        ConnectionExecutionClosure, ConnectionHttp, ConnectionInvocation, ConnectionOrigin,
        InvocationEntry, WiringPosition, transport::HttpTransport,
    };
    let _lock = wamn_test_postgres::lock();
    let database = wamn_project_state::test_database::tenant_platform();
    let (mut project, task) = connect(database.url()).await;
    provision_project(&project, database.url()).await;
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
        Vec::new(),
    )
    .unwrap();
    project.execute("INSERT INTO catalog.workflows (env_key,environment,application,workflow_id,version,graph_json,wiring_hash,trigger,execution_type,created_by) VALUES ($1,$2,$3,'http_read',1,$4::text::jsonb,$5,'manual','invoker',$6::text::uuid)", &[&ENV_KEY,&ENVIRONMENT,&PACKAGE,&serde_json::to_string(&document).unwrap(),&document.wiring_hash().as_str(),&wamn_project_state::workflow_user_id().to_string()]).await.unwrap();
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
    project.execute("INSERT INTO catalog.releases(env_key,manifest_digest,canonical_bytes) VALUES ($1,$2,$3)", &[&ENV_KEY,&manifest_digest,&manifest.canonical_bytes()]).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
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
    postgres.set_env_key(COMPONENT_ID, ENV_KEY).unwrap();
    let facts = Arc::new(ConnectionFacts::new(
        Arc::new(ConnectionMemory::default()),
        wamn_platform_identity::connection_changes::ConnectionChanges::default(),
        Arc::new(authority.reader().await),
        Arc::clone(&postgres),
        ENV_KEY,
        DEFAULT_PROJECT,
        ENVIRONMENT,
    ));
    let _reader = wamn_runtime::plugins::wamn_blobstore::plugin::WamnBlobstore::new(
        Arc::clone(&postgres),
        Arc::clone(&facts),
        ENV_KEY,
        DEFAULT_PROJECT,
        None,
    );
    let release = Arc::new(
        wamn_engine::release_manifest::LoadedRelease::load_canonical_bytes(
            &manifest.canonical_bytes(),
            "read binding fixture",
            wamn_engine::release_manifest::ReleaseScope::new(ENV_KEY, ENVIRONMENT),
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
            wiring_hash: document.wiring_hash().as_str().into(),
        }),
        package_id: PACKAGE.into(),
        component_digest: HTTP_SIDECAR.into(),
        component: "http-sidecar".into(),
        operation: "wamn:node/handler@0.1.0".into(),
        closure: ConnectionExecutionClosure::Released,
        effects: None,
    };
    let plugin = ConnectionHttp::new(
        Arc::clone(&postgres),
        Arc::new(HttpTransport::new().unwrap()),
        Arc::clone(&facts),
        ENV_KEY,
        DEFAULT_PROJECT,
        vec![
            format!("http://{}", listener.local_addr().unwrap())
                .parse()
                .unwrap(),
        ]
        .into(),
        Some(release),
    );
    plugin
        .bind_invocation(COMPONENT_ID, invocation.clone())
        .unwrap();
    assert_eq!(
        plugin
            .send_for_test(COMPONENT_ID, "erp", "POST")
            .await
            .unwrap_err(),
        "ConnectionError::AuthorityDenied"
    );
    assert_eq!(
        plugin
            .send_for_test(COMPONENT_ID, "erp", "GET")
            .await
            .unwrap_err(),
        "ConnectionError::CredentialUnavailable"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
    plugin.revoke_invocation(COMPONENT_ID);
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
            DEFAULT_PROJECT,
            ENV_KEY,
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
        )
        .await
        .unwrap()
        .unwrap();
    assert!(snapshot.read_only);
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
    let plugin = ConnectionHttp::new(
        postgres,
        Arc::new(HttpTransport::new().unwrap()),
        Arc::clone(&facts),
        ENV_KEY,
        DEFAULT_PROJECT,
        vec![
            format!("http://{}", listener.local_addr().unwrap())
                .parse()
                .unwrap(),
        ]
        .into(),
        None,
    );
    plugin.bind_invocation(COMPONENT_ID, invocation).unwrap();
    assert_eq!(
        plugin
            .send_for_test(COMPONENT_ID, "erp", "POST")
            .await
            .unwrap_err(),
        "ConnectionError::AttestationInvalid"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
    drop(project);
    task.abort();
}

fn blobstore(definition: &serde_json::Value) -> anyhow::Result<()> {
    RequirementType::Blobstore
        .check_definition(definition)
        .map_err(anyhow::Error::msg)
}
use wamn_runtime::connection_generation::{
    binding_validation_subject as validation_subject, definition_hash,
};
#[test]
fn a_blobstore_definition_needs_exactly_the_coordinates_the_plugin_reads() {
    blobstore(&serde_json::json!({
        "endpoint": "http://10.0.0.7:9000", "container": "labels", "prefix": "wms/"
    }))
    .expect("the three coordinates the plugin reads are a complete definition");
}

#[test]
fn a_missing_coordinate_is_refused_by_name() {
    let error = blobstore(&serde_json::json!({"endpoint": "http://x", "container": "c"}))
        .expect_err("prefix is read at resolve time");
    assert!(format!("{error:#}").contains("lacks prefix"), "{error:#}");
}

#[test]
fn an_empty_coordinate_is_refused_by_name() {
    let error = blobstore(&serde_json::json!({"endpoint": "", "container": "c", "prefix": "p"}))
        .expect_err("an empty endpoint is not an endpoint");
    assert!(
        format!("{error:#}").contains("endpoint is empty"),
        "{error:#}"
    );
}

#[test]
fn a_coordinate_nobody_reads_is_refused_by_name() {
    let error = blobstore(&serde_json::json!({
        "endpoint": "http://x", "container": "c", "prefix": "p", "region": "eu-3"
    }))
    .expect_err("a key the plugin never reads is a key nobody validates");
    assert!(format!("{error:#}").contains("carries region"), "{error:#}");
}

#[test]
fn the_http_shape_is_not_a_blobstore_definition() {
    let error = blobstore(&serde_json::json!({
        "primary-authority": "https://erp.example", "failover-authorities": [],
        "tls-policy": "verify-authority", "redirect-policy": "same-authority"
    }))
    .expect_err("an HTTP definition has none of blobstore's coordinates");
    assert!(format!("{error:#}").contains("lacks endpoint"), "{error:#}");
}

#[test]
fn a_non_object_definition_is_refused() {
    let error = blobstore(&serde_json::json!(["endpoint"])).expect_err("not an object");
    assert!(
        format!("{error:#}").contains("must be a JSON object"),
        "{error:#}"
    );
}

#[test]
fn the_validation_subject_names_everything_that_was_validated() {
    let descriptor = ConnectionTypeDescriptor::blobstore_v1();
    let subject = validation_subject(&descriptor, "sha256:req", "sha256:def");
    assert_eq!(subject["requirement-type"], "blobstore");
    assert_eq!(subject["contract"], "wasmcloud:blobstore/blobstore@0.1.0");
    assert_eq!(subject["requirement-hash"], "sha256:req");
    assert_eq!(subject["definition-hash"], "sha256:def");
    // One hasher: the runtime's. Changing any named input changes the hash.
    let baseline = definition_hash(&subject);
    let moved = definition_hash(&validation_subject(
        &descriptor,
        "sha256:req",
        "sha256:other",
    ));
    assert_ne!(baseline, moved);
    assert!(baseline.starts_with("sha256:") && baseline.len() == 71);
}
