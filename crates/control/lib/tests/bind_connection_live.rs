//! `[BIND-CONNECTION-LIVE]` — the connection-admin verb, shown by the round
//! trip and not by the rows.
//!
//! The ruling (wamn-362o.33): bind, then the PLUGIN resolves the binding from
//! a real snapshot. Existing rows do not show what the plugin accepts, so
//! the assertion here is `wamn_blobstore::binding::resolve` returning the
//! coordinates that were bound, over a snapshot the postgres plugin's own
//! `connection_effect_snapshot` loaded from the rows the verb wrote. Nothing
//! between the verb and the resolver is this file's: the fixture is a
//! package applied by the real verb, a component with the facts admission
//! produces, a wiring authored by the real author under a
//! green report, and a release row of the shape the publish writes.
//!
//! Two databases on the PostgreSQL server of the test process: a project
//! database with the tenant+platform floor and a control database with the
//! system floor. The test holds the process lock of that server.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use tokio_postgres::{Client, NoTls};
use wamn_catalog::{
    AdmittedComponent, AdmittedComponentOperation, ComponentPackageScope, ConnectionTypeDescriptor,
    RequirementType, WiringDocument, WiringNode, WiringTerminal,
};
use wamn_control::apply_package::{self, ApplyPackageRequest};
use wamn_control::bind_connection::{self, BindConnectionRequest};
use wamn_runtime::plugins::wamn_blobstore::binding::{self, BindingError};
use wamn_runtime::plugins::wamn_postgres::{
    CandidateConnectionBinding, ClassCredentials, ConnectionEffectLookup, DEFAULT_PROJECT,
    WamnPostgres, WamnPostgresConfig,
};
use wamn_schema_control::connections::{
    ComponentConnectionRequirement, activate_connection_generation_sql,
    insert_component_connection_requirement_sql,
};
use wamn_test_infrastructure::record_history::assert_stamp;

const ENV_KEY: &str = "bind-connection-tenant--fixture--dev";
const ENVIRONMENT: &str = "dev";
const PACKAGE: &str = "bind_connection_fixture";
const PACKAGE_VERSION: &str = "1.0.0";
const COMPONENT_ID: &str = "bind-connection-component";
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
    r#"{"descriptor":{"component":"blob-put","component-digest":"sha256:2222222222222222222222222222222222222222222222222222222222222222","connections":[{"requirement-type":"blobstore","store-alias":"labels"}],"effects":[],"imports":[],"imports-fingerprint":"sha256:6666666666666666666666666666666666666666666666666666666666666666","interface-version":"0.1.0","operations":{"wamn:node/handler@0.1.0":{"input-ports":[],"output-ports":[],"parameters":[]}}},"package-id":"bind_connection_fixture"},"#,
    r#"{"descriptor":{"component":"http-sidecar","component-digest":"sha256:3333333333333333333333333333333333333333333333333333333333333333","connections":[{"requirement-type":"http","store-alias":"erp"}],"effects":[],"imports":[],"imports-fingerprint":"sha256:6666666666666666666666666666666666666666666666666666666666666666","interface-version":"0.1.0","operations":{"wamn:node/handler@0.1.0":{"input-ports":[],"output-ports":[],"parameters":[]}}},"package-id":"bind_connection_fixture"},"#,
    r#"{"descriptor":{"component":"nested-origin","component-digest":"sha256:4444444444444444444444444444444444444444444444444444444444444444","connections":[{"requirement-type":"http","store-alias":"origin-http"}],"effects":[],"imports":[],"imports-fingerprint":"sha256:6666666666666666666666666666666666666666666666666666666666666666","interface-version":"0.1.0","operations":{"origin-call":{"input-ports":[],"output-ports":[],"parameters":[]}}},"package-id":"nested_origin"}],"#,
    r#""release":{"packages":[{"package-id":"bind_connection_fixture","package-version":"1.0.0"},"#,
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
    let canonical = String::from_utf8(requirement.canonical_bytes()).expect("canonical utf-8");
    project
        .execute(
            insert_component_connection_requirement_sql(),
            &[
                &ENV_KEY,
                &requirement.component_digest(),
                &requirement.store_alias(),
                &canonical,
                &requirement.requirement_hash(),
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
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bind_connection");
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
        org: "bind-connection-tenant".into(),
        project: "fixture".into(),
        environment: "dev".into(),
    })
    .await
    .expect("apply the bind-connection package fixture");
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

fn definition_file(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write a generation definition");
    path
}

fn args(
    project_url: &str,
    definition: std::path::PathBuf,
    store_alias: &str,
    digest: &str,
) -> BindConnectionRequest {
    BindConnectionRequest {
        database_url: project_url.to_owned(),
        org: "bind-connection-tenant".into(),
        project: "fixture".into(),
        environment: ENVIRONMENT.to_owned(),
        instance_id: "labels-store".to_owned(),
        requirement_type: RequirementType::Blobstore,
        definition: serde_json::from_slice(&std::fs::read(definition).expect("read a definition"))
            .expect("a JSON definition"),
        credential_handle: Some("labels-store".to_owned()),
        org_connection_name: None,
        manifest_digest: digest_of(RELEASE_BYTES),
        component_digest: digest.to_owned(),
        store_alias: store_alias.to_owned(),
    }
}

async fn rows(project: &Client) -> (i64, i64, i64) {
    let count = |sql: &'static str| async move {
        project
            .query_one(sql, &[&ENV_KEY])
            .await
            .expect("count rows")
            .get::<_, i64>(0)
    };
    (
        count("SELECT count(*) FROM catalog.connection_instances WHERE env_key = $1").await,
        count("SELECT count(*) FROM catalog.connection_generations WHERE env_key = $1").await,
        count("SELECT count(*) FROM catalog.connection_bindings WHERE env_key = $1").await,
    )
}

async fn assert_nested_effect_snapshot(
    project: &Client,
    postgres: &WamnPostgres,
    direct: ConnectionEffectLookup<'_>,
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
        .connection_effect_snapshot(COMPONENT_ID, DEFAULT_PROJECT, ENV_KEY, &nested)
        .await
        .expect("load the nested snapshot")
        .expect("the released wiring exists");
    assert!(snapshot.node_permitted);
    assert_eq!(snapshot.component.as_deref(), Some("blob-put"));
    assert_eq!(snapshot.operation.as_deref(), Some(direct.operation));
    let resolved = binding::resolve(&snapshot).expect("resolve the executor's bound alias");
    assert!(matches!(
        &resolved.provider,
        binding::BlobstoreProvider::S3 { credential_handle, .. } if credential_handle == "labels-rotated"
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
            .connection_effect_snapshot(COMPONENT_ID, DEFAULT_PROJECT, ENV_KEY, &refused)
            .await
            .expect("load the mismatched snapshot");
        assert!(
            snapshot.is_none_or(|snapshot| !snapshot.node_permitted),
            "a mismatched {changed} must not authorize the executor"
        );
    }
}

#[tokio::test]
async fn bind_connection_round_trips_through_the_plugins_own_resolution() {
    let _lock = wamn_test_postgres::lock();
    let project_database = wamn_project_state::test_database::tenant_platform();
    let project_url = project_database.url().to_owned();
    let (project, project_task) = connect(&project_url).await;
    provision_project(&project, &project_url).await;
    // No tempfile dev-dependency in this crate: a pid-named scratch directory,
    // removed at the end.
    let scratch_dir =
        std::env::temp_dir().join(format!("wamn-bind-connection-{}", std::process::id()));
    std::fs::create_dir_all(&scratch_dir).expect("scratch directory");
    let scratch = scratch_dir.as_path();

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
            .connection_effect_snapshot(COMPONENT_ID, DEFAULT_PROJECT, ENV_KEY, &lookup)
            .await
            .expect("the plugin loads a snapshot for a declared alias")
            .expect("the declared alias has a snapshot, bound or not")
    };

    // CONTROL FIRST: before the verb runs, the plugin sees the requirement and
    // refuses to resolve it as unbound. This is what makes the pass below the
    // verb's doing and not the fixture's.
    let before = snapshot_of().await;
    // An unbound alias fails the plugin's authorization, which the blobstore
    // resolver surfaces as Unauthorized -- the same refusal a host would give.
    assert!(
        matches!(binding::resolve(&before), Err(BindingError::Unauthorized)),
        "before bind-connection the plugin must refuse the unbound alias, got {:?}",
        binding::resolve(&before)
    );
    assert_eq!(rows(&project).await, (0, 0, 0));

    // REFUSALS LEAVE NO ROWS. A definition missing a coordinate is refused by
    // name before a connection is opened; a blobstore instance bound to an
    // alias declared as HTTP is refused naming both types.
    let lacking = definition_file(
        scratch,
        "lacking.json",
        r#"{"endpoint":"http://10.0.0.7:9000","container":"labels"}"#,
    );
    let error = bind_connection::bind(&args(&project_url, lacking.clone(), "labels", BLOB_PUT))
        .await
        .expect_err("a definition without prefix cannot be bound");
    assert!(format!("{error:#}").contains("lacks prefix"), "{error:#}");
    let good = definition_file(
        scratch,
        "good.json",
        r#"{"endpoint":"http://10.0.0.7:9000","container":"labels","prefix":"fixture/"}"#,
    );
    let error = bind_connection::bind(&args(&project_url, good.clone(), "erp", HTTP_SIDECAR))
        .await
        .expect_err("a blobstore instance cannot bind an HTTP alias");
    let text = format!("{error:#}");
    assert!(
        text.contains("declared erp as http/") && text.contains("not blobstore/"),
        "{text}"
    );
    let error = bind_connection::bind(&args(&project_url, good.clone(), "nothing", BLOB_PUT))
        .await
        .expect_err("an alias the component never declared cannot be bound");
    assert!(
        format!("{error:#}").contains("declares no connection requirement named nothing"),
        "{error:#}"
    );
    assert_eq!(rows(&project).await, (0, 0, 0), "a refusal writes nothing");

    // THE VERB.
    let bound = bind_connection::bind(&args(&project_url, good.clone(), "labels", BLOB_PUT))
        .await
        .expect("bind the labels store");
    assert_eq!(bound.instance_id, "labels-store");
    assert_eq!(bound.generation, 1);
    assert!(bound.definition_hash.starts_with("sha256:"));
    assert!(bound.validation_hash.starts_with("sha256:"));
    assert_ne!(bound.definition_hash, bound.validation_hash);
    assert_eq!(rows(&project).await, (1, 1, 1));
    for relation in [
        "catalog.connection_generations",
        "catalog.connection_bindings",
    ] {
        assert_stamp(
            &project,
            relation,
            &format!("env_key = '{ENV_KEY}'"),
            &wamn_control_provision::PlatformComponent::Provisioning
                .principal_id()
                .to_string(),
            &wamn_control_provision::PlatformComponent::Provisioning
                .principal_id()
                .to_string(),
        )
        .await;
    }

    // THE ROUND TRIP: the plugin loads the snapshot from the rows the verb
    // wrote and the blobstore resolver accepts it, returning what was bound.
    let after = snapshot_of().await;
    let resolved = binding::resolve(&after).expect("the plugin resolves the bound connection");
    assert_eq!(
        resolved.provider,
        binding::BlobstoreProvider::S3 {
            endpoint: "http://10.0.0.7:9000".to_owned(),
            credential_handle: "labels-store".to_owned(),
        }
    );
    assert_eq!(resolved.container, "labels");
    assert_eq!(resolved.prefix, "fixture/");
    assert_eq!(
        after.definition_hash.as_deref(),
        Some(bound.definition_hash.as_str())
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
        async move {
            postgres
                .connection_effect_snapshot(COMPONENT_ID, DEFAULT_PROJECT, ENV_KEY, &lookup)
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
    assert_eq!(
        after.validation_hash.as_deref(),
        Some(bound.validation_hash.as_str())
    );

    // Identical rebinding preserves every stored row.
    let again = definition_file(
        scratch,
        "again.json",
        r#"{"endpoint":"http://10.0.0.7:9000","container":"labels","prefix":"fixture/"}"#,
    );
    let rebound = bind_connection::bind(&args(&project_url, again, "labels", BLOB_PUT))
        .await
        .expect("identical rebinding is a no-op");
    assert_eq!(rebound.previous_generation, Some(1));
    assert_eq!(rebound.generation, 1);
    assert_eq!(rows(&project).await, (1, 1, 1));
    for relation in [
        "catalog.connection_generations",
        "catalog.connection_bindings",
    ] {
        assert_stamp(
            &project,
            relation,
            &format!("env_key = '{ENV_KEY}'"),
            &wamn_control_provision::PlatformComponent::Provisioning
                .principal_id()
                .to_string(),
            &wamn_control_provision::PlatformComponent::Provisioning
                .principal_id()
                .to_string(),
        )
        .await;
    }

    // A STALE ACTIVATION CHANGES NOTHING. The bind left generation 1 active at
    // revision 2. An activation that expects any other state matches no row,
    // and the instance row stays exactly as it was.
    let instance = || async {
        project
            .query_one(
                "SELECT active_generation, revision, row_to_json(instance)::text \
                   FROM catalog.connection_instances AS instance \
                  WHERE env_key = $1 AND environment = $2 AND instance_id = $3",
                &[&ENV_KEY, &ENVIRONMENT, &"labels-store"],
            )
            .await
            .map(|row| {
                (
                    row.get::<_, Option<i64>>(0),
                    row.get::<_, i64>(1),
                    row.get::<_, String>(2),
                )
            })
            .expect("read the bound instance")
    };
    let current = instance().await;
    assert_eq!((current.0, current.1), (Some(1), 2));
    for (expected_active_generation, expected_revision) in
        [(None::<i64>, 1_i64), (Some(1), 1), (None, 2)]
    {
        let activated = project
            .execute(
                activate_connection_generation_sql(),
                &[
                    &ENV_KEY,
                    &ENVIRONMENT,
                    &"labels-store",
                    &1_i64,
                    &expected_active_generation,
                    &expected_revision,
                ],
            )
            .await
            .expect("a stale activation is not an error");
        assert_eq!(
            activated, 0,
            "expecting ({expected_active_generation:?}, {expected_revision}) is stale"
        );
        assert_eq!(instance().await, current);
    }

    // Invalid configuration cannot disturb the already selected generation.
    bind_connection::bind(&args(&project_url, lacking, "labels", BLOB_PUT))
        .await
        .expect_err("invalid rotation refuses");
    assert_eq!(instance().await, current);
    assert_eq!(rows(&project).await, (1, 1, 1));
    for relation in [
        "catalog.connection_generations",
        "catalog.connection_bindings",
    ] {
        assert_stamp(
            &project,
            relation,
            &format!("env_key = '{ENV_KEY}'"),
            &wamn_control_provision::PlatformComponent::Provisioning
                .principal_id()
                .to_string(),
            &wamn_control_provision::PlatformComponent::Provisioning
                .principal_id()
                .to_string(),
        )
        .await;
    }

    project
        .execute(
            "INSERT INTO catalog.releases (env_key, manifest_digest, canonical_bytes) \
             VALUES ($1, $2, $3)",
            &[
                &ENV_KEY,
                &digest_of(OTHER_RELEASE_BYTES),
                &OTHER_RELEASE_BYTES.as_bytes(),
            ],
        )
        .await
        .unwrap();
    let mut wrong_scope = args(&project_url, good.clone(), "labels", BLOB_PUT);
    wrong_scope.manifest_digest = digest_of(OTHER_RELEASE_BYTES);
    bind_connection::bind(&wrong_scope)
        .await
        .expect_err("a release without this component refuses");
    assert_eq!(instance().await, current);
    assert_eq!(rows(&project).await, (1, 1, 1));
    for relation in [
        "catalog.connection_generations",
        "catalog.connection_bindings",
    ] {
        assert_stamp(
            &project,
            relation,
            &format!("env_key = '{ENV_KEY}'"),
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
        component_digest: BLOB_PUT.to_owned(),
        store_alias: "labels".to_owned(),
        requirement_hash: after.requirement_hash.clone().unwrap(),
        instance_id: after.instance_id.clone().unwrap(),
        instance_revision: after.instance_revision.unwrap(),
        requirement_type: after.requirement_type.clone().unwrap(),
        contract: after.contract.clone().unwrap(),
        validation_hash: after.validation_hash.clone().unwrap(),
        generation: 1,
        definition_hash: after.definition_hash.clone().unwrap(),
        credential_set_handle: after.credential_handle.clone(),
    };
    let changed = definition_file(
        scratch,
        "changed.json",
        r#"{"endpoint":"http://10.0.0.8:9000","container":"labels","prefix":"new/"}"#,
    );
    let mut changed_args = args(&project_url, changed.clone(), "labels", BLOB_PUT);
    changed_args.credential_handle = Some("labels-rotated".to_owned());
    let rotated = bind_connection::bind(&changed_args)
        .await
        .expect("rotate the instance");
    assert_eq!(rotated.previous_generation, Some(1));
    assert_eq!(rotated.generation, 2);
    assert_eq!(rows(&project).await, (1, 2, 1));
    for relation in [
        "catalog.connection_generations",
        "catalog.connection_bindings",
    ] {
        assert_stamp(
            &project,
            relation,
            &format!("env_key = '{ENV_KEY}'"),
            &wamn_control_provision::PlatformComponent::Provisioning
                .principal_id()
                .to_string(),
            &wamn_control_provision::PlatformComponent::Provisioning
                .principal_id()
                .to_string(),
        )
        .await;
    }
    let new_snapshot = snapshot_of().await;
    let new_binding = binding::resolve(&new_snapshot).expect("new selection resolves");
    assert_eq!(
        new_binding.provider,
        binding::BlobstoreProvider::S3 {
            endpoint: "http://10.0.0.8:9000".to_owned(),
            credential_handle: "labels-rotated".to_owned(),
        }
    );
    let old_snapshot = postgres
        .connection_effect_snapshot(
            COMPONENT_ID,
            DEFAULT_PROJECT,
            ENV_KEY,
            &ConnectionEffectLookup {
                candidate_binding: Some(&pinned),
                ..lookup
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old_snapshot.active_generation, Some(2));
    assert_eq!(old_snapshot.generation, Some(1));
    for candidate in [
        CandidateConnectionBinding {
            instance_id: "another-instance".to_owned(),
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
            )
            .await
            .expect("load a mismatched candidate snapshot")
            .expect("the released wiring still exists");
        assert!(
            binding::resolve(&snapshot).is_err(),
            "a candidate must not fall back to another instance or generation"
        );
    }
    let old_binding =
        binding::resolve(&old_snapshot).expect("the pinned generation remains usable");
    assert_eq!(
        old_binding.provider,
        binding::BlobstoreProvider::S3 {
            endpoint: "http://10.0.0.7:9000".to_owned(),
            credential_handle: "labels-store".to_owned(),
        }
    );

    // Hold the row after the competing bind reads it, then advance its revision.
    let (mut blocker, blocker_task) = connect(&project_url).await;
    let transaction = blocker.transaction().await.unwrap();
    transaction.query_one("SELECT instance_id FROM catalog.connection_instances WHERE env_key=$1 AND instance_id='labels-store' FOR UPDATE", &[&ENV_KEY]).await.unwrap();
    let stale_args = args(&project_url, good, "labels", BLOB_PUT);
    let stale_bind = tokio::spawn(async move { bind_connection::bind(&stale_args).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let activation_waits: bool = project.query_one(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'UPDATE catalog.connection_instances%')", &[])
                .await.unwrap().get(0);
            if activation_waits { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("the competing activation reaches its row lock");
    transaction.execute("UPDATE catalog.connection_instances SET revision=revision+1 WHERE env_key=$1 AND instance_id='labels-store'", &[&ENV_KEY]).await.unwrap();
    transaction.commit().await.unwrap();
    let error = stale_bind
        .await
        .unwrap()
        .expect_err("stale bind refuses atomically");
    assert!(
        error.to_string().contains("connection inputs changed"),
        "{error:#}"
    );
    assert_eq!(rows(&project).await, (1, 2, 1));
    for relation in [
        "catalog.connection_generations",
        "catalog.connection_bindings",
    ] {
        assert_stamp(
            &project,
            relation,
            &format!("env_key = '{ENV_KEY}'"),
            &wamn_control_provision::PlatformComponent::Provisioning
                .principal_id()
                .to_string(),
            &wamn_control_provision::PlatformComponent::Provisioning
                .principal_id()
                .to_string(),
        )
        .await;
    }
    assert_eq!(instance().await.0, Some(2));
    drop(blocker);
    blocker_task.abort();

    assert_nested_effect_snapshot(&project, &postgres, lookup).await;

    drop(project);
    project_task.abort();
    let _ = std::fs::remove_dir_all(&scratch_dir);
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
    use wamn_runtime::connection_credentials::ConnectionCredentials;
    use wamn_runtime::plugins::connection_http::{
        ConnectionExecutionClosure, ConnectionHttp, ConnectionInvocation, ConnectionOrigin,
        InvocationEntry, WiringPosition, transport::HttpTransport,
    };
    let _lock = wamn_test_postgres::lock();
    let database = wamn_project_state::test_database::tenant_platform();
    let (project, task) = connect(database.url()).await;
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
    let request = BindConnectionRequest {
        database_url: database.url().to_owned(),
        org: "bind-connection-tenant".into(),
        project: "fixture".into(),
        environment: ENVIRONMENT.into(),
        instance_id: "http-read".into(),
        requirement_type: RequirementType::Http,
        definition: serde_json::json!({"primary-authority":format!("http://{}",listener.local_addr().unwrap()), "tls-verification":"disabled"}),
        credential_handle: Some("unused".into()),
        org_connection_name: None,
        manifest_digest: manifest_digest.clone(),
        component_digest: HTTP_SIDECAR.into(),
        store_alias: "erp".into(),
    };
    bind_connection::bind_with_mode(
        &request,
        wamn_schema_generator::ConnectionMode::Read,
        PACKAGE,
    )
    .await
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
    postgres.set_env_key(COMPONENT_ID, ENV_KEY).unwrap();
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
        Arc::new(ConnectionCredentials::empty()),
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
                manifest_digest: &request.manifest_digest,
                environment: ENVIRONMENT,
                component_digest: HTTP_SIDECAR,
                store_alias: "erp",
                candidate_binding: None,
            },
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
        manifest_digest: request.manifest_digest,
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
        Arc::new(ConnectionCredentials::empty()),
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
