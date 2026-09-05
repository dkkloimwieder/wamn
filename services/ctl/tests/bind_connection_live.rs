//! `[BIND-CONNECTION-LIVE]` — the connection-admin verb, proven by the round
//! trip and not by the rows.
//!
//! The ruling (wamn-362o.33): bind, then the PLUGIN resolves the binding from
//! a real snapshot. Rows existing proves nothing the plugin will accept, so
//! the assertion here is `wamn_blobstore::binding::resolve` returning the
//! coordinates that were bound, over a snapshot the postgres plugin's own
//! `connection_effect_snapshot` loaded from the rows the verb wrote. Nothing
//! between the verb and the resolver is this file's: the fixture is the real
//! WMS package applied by the real verb, a component admitted the way
//! push-component admits one, a wiring authored by the real author under a
//! green report, and a release row of the shape the mint writes.
//!
//! Two disposable PostgreSQL 18 databases, one server:
//!   WAMN_BIND_CONNECTION_PROJECT_PG_URL, WAMN_BIND_CONNECTION_CONTROL_PG_URL
//! Required, never self-skipped: the test `expect`s them. Ignored by default
//! because it needs the server; arm it with a container removed BY NAME.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use tokio_postgres::{Client, NoTls};
use wamn_catalog::{
    AdmittedComponent, AdmittedComponentOperation, ComponentPackageScope, ConnectionTypeDescriptor,
    DefinitionHash, WiringDocument, WiringNode, WiringTerminal,
};
use wamn_control_provision::CONTROL_BOOTSTRAP_SQL;
use wamn_ctl::apply_package::{self, ApplyPackageArgs};
use wamn_ctl::author_wiring::{AuthorWiringRequest, author_wiring};
use wamn_ctl::bind_connection::{self, BindConnectionArgs, RequirementType};
use wamn_ctl::push_component::admitted_projection_hash;
use wamn_runtime::plugins::wamn_blobstore::binding::{self, BindingError};
use wamn_runtime::plugins::wamn_postgres::{
    ClassCredentials, ConnectionEffectLookup, DEFAULT_PROJECT, WamnPostgres, WamnPostgresConfig,
};
use wamn_schema_control::connections::{
    ComponentConnectionRequirement, insert_component_connection_requirement_sql,
};

const TENANT: &str = "bind-connection-tenant";
const ENVIRONMENT: &str = "dev";
const PACKAGE: &str = "wamn_wms";
const PACKAGE_VERSION: &str = "1.0.0";
const COMPONENT_ID: &str = "bind-connection-component";
const BLOB_PUT: &str = "sha256:2222222222222222222222222222222222222222222222222222222222222222";
const HTTP_SIDECAR: &str = "sha256:3333333333333333333333333333333333333333333333333333333333333333";
const FACT_FINGERPRINT: &str =
    "sha256:6666666666666666666666666666666666666666666666666666666666666666";
const RELEASE_ID: i32 = 1;
const CATALOG_SCHEMA_SQL: &str = include_str!("../../../deploy/sql/catalog-schema.sql");
const APP_SCHEMA_SQL: &str = include_str!("../../../deploy/sql/app-schema.sql");

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

/// One admitted component with one declared connection, recorded the way
/// push-component records it: the library row, then the requirement row built
/// by the control library from the platform's own descriptor.
async fn admit_component(
    project: &Client,
    component: &str,
    digest: &str,
    store_alias: &str,
    descriptor: ConnectionTypeDescriptor,
) {
    let admitted = AdmittedComponent {
        scope: ComponentPackageScope {
            tenant_id: TENANT.to_owned(),
            package_id: PACKAGE.to_owned(),
            package_version: PACKAGE_VERSION.to_owned(),
        },
        component: component.to_owned(),
        interface_version: "0.1.0".to_owned(),
        operations: BTreeMap::from([(
            "wamn:node/handler@0.1.0".to_owned(),
            // A palette node registers no package operation: its handler
            // is the platform's wamn:node contract, not a wamn_wms one.
            AdmittedComponentOperation {
                registered_operation: None,
                dependencies: Vec::new(),
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
    let projection_hash =
        admitted_projection_hash(&admitted, &[]).expect("hash the admitted fixture projection");
    project
        .execute(
            "INSERT INTO catalog.component_library \
                   (tenant_id, package_id, package_version, component, interface_version, \
                    operations, component_digest, projection_hash, imports, \
                    imports_fingerprint, effects) \
             VALUES ($1, $2, $3, $4, '0.1.0', \
                     '{\"wamn:node/handler@0.1.0\":{\"registered-operation\":null,\"input-ports\":[],\"output-ports\":[],\"parameters\":[]}}'::jsonb, \
                     $5, $6, '[]'::jsonb, $7, '[]'::jsonb)",
            &[
                &TENANT,
                &PACKAGE,
                &PACKAGE_VERSION,
                &component,
                &digest,
                &projection_hash,
                &FACT_FINGERPRINT,
            ],
        )
        .await
        .expect("admit the fixture component");
    let requirement = ComponentConnectionRequirement::new(digest, store_alias, descriptor);
    let canonical = String::from_utf8(requirement.canonical_bytes()).expect("canonical utf-8");
    project
        .execute(
            insert_component_connection_requirement_sql(),
            &[
                &TENANT,
                &requirement.component_digest(),
                &requirement.store_alias(),
                &canonical,
                &requirement.requirement_hash(),
            ],
        )
        .await
        .expect("record the fixture component's connection requirement");
}

async fn provision_project(project: &Client, project_url: &str) {
    project
        .batch_execute(
            "DROP SCHEMA IF EXISTS wms CASCADE; \
             DROP SCHEMA IF EXISTS app_system CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DO $$ DECLARE role_name text; BEGIN \
               PERFORM pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtext('wamn_role_bootstrap')); \
               FOREACH role_name IN ARRAY ARRAY['wamn_app', 'wamn_scenario_author'] LOOP \
                 IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = role_name) THEN \
                   EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                                   NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); \
                 END IF; \
               END LOOP; \
             END $$;",
        )
        .await
        .expect("reset the project catalog schema and prerequisite roles");
    project
        .batch_execute(&format!("{CATALOG_SCHEMA_SQL}\n{APP_SCHEMA_SQL}"))
        .await
        .expect("install the production package and application schemas");
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
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("ctl crate lives under services/ctl");
    apply_package::run(ApplyPackageArgs {
        package: repository.join("packages/wms"),
        database_url: project_url.to_string(),
        tenant: TENANT.to_string(),
    })
    .await
    .expect("apply the real WMS package");
    project
        .execute("SELECT set_config('app.tenant', $1, false)", &[&TENANT])
        .await
        .expect("scope the project seed session");
    admit_component(project, "blob-put", BLOB_PUT, "labels", ConnectionTypeDescriptor::blobstore_v1())
        .await;
    // A second alias of a DIFFERENT type, for the mismatch control.
    admit_component(project, "http-sidecar", HTTP_SIDECAR, "erp", ConnectionTypeDescriptor::http_v1())
        .await;
    project
        .execute(
            "INSERT INTO catalog.effective_releases \
               (tenant_id, effective_release_id, environment, verified_publisher_principal) \
             VALUES ($1, $2, $3, 'bind-connection-live')",
            &[&TENANT, &RELEASE_ID, &ENVIRONMENT],
        )
        .await
        .expect("mint the fixture release");
    project
        .execute(
            "INSERT INTO catalog.effective_release_packages \
               (tenant_id, effective_release_id, package_id, package_version) \
             VALUES ($1, $2, $3, $4)",
            &[&TENANT, &RELEASE_ID, &PACKAGE, &PACKAGE_VERSION],
        )
        .await
        .expect("scope the fixture release to the package");
}

async fn provision_control(control: &Client) {
    control
        .batch_execute(
            "DROP SCHEMA IF EXISTS wamn_run CASCADE; \
             DROP SCHEMA IF EXISTS wamn_authority CASCADE; \
             DROP SCHEMA IF EXISTS catalog CASCADE; \
             DROP SCHEMA IF EXISTS registry CASCADE; \
             DROP SCHEMA IF EXISTS provisioning CASCADE; \
             DROP SCHEMA IF EXISTS identity CASCADE; \
             DO $$ DECLARE role_name text; BEGIN \
               PERFORM pg_catalog.pg_advisory_xact_lock(pg_catalog.hashtext('wamn_role_bootstrap')); \
               FOREACH role_name IN ARRAY ARRAY['wamn_system', 'wamn_control_author', 'wamn_app'] \
               LOOP \
                 IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = role_name) THEN \
                   EXECUTE format('CREATE ROLE %I NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                                   NOINHERIT NOREPLICATION NOBYPASSRLS', role_name); \
                 END IF; \
               END LOOP; \
             END $$;",
        )
        .await
        .expect("reset the control store and its prerequisite roles");
    for stage in CONTROL_BOOTSTRAP_SQL {
        control
            .batch_execute(stage)
            .await
            .expect("install the production control bootstrap");
    }
    control
        .execute("SELECT set_config('app.tenant', $1, false)", &[&TENANT])
        .await
        .expect("scope the control seed session");
}

async fn record_green_report(control: &Client, wiring_hash: &DefinitionHash) {
    control
        .execute(
            "INSERT INTO wamn_run.gate_reports (tenant_id, wiring_hash, passed, summary) \
             VALUES ($1, $2, true, '{}'::jsonb)",
            &[&TENANT, &wiring_hash.as_str()],
        )
        .await
        .expect("record a green gate report at the wiring's own hash");
}

async fn author(control: &Client, project: &mut Client, document: &WiringDocument) {
    let transaction = project
        .transaction()
        .await
        .expect("open the wiring authorship transaction");
    author_wiring(
        control,
        &transaction,
        &AuthorWiringRequest {
            tenant_id: TENANT,
            package_id: PACKAGE,
            package_version: PACKAGE_VERSION,
            document,
        },
    )
    .await
    .expect("author the fixture wiring under its green report");
    transaction
        .commit()
        .await
        .expect("commit the wiring authorship");
}

fn definition_file(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write a generation definition");
    path
}

fn args(project_url: &str, definition: std::path::PathBuf, store_alias: &str, digest: &str) -> BindConnectionArgs {
    BindConnectionArgs {
        database_url: project_url.to_owned(),
        tenant: TENANT.to_owned(),
        environment: ENVIRONMENT.to_owned(),
        instance_id: "labels-store".to_owned(),
        requirement_type: RequirementType::Blobstore,
        definition,
        credential_handle: "labels-store".to_owned(),
        effective_release_id: RELEASE_ID as u32,
        component_digest: digest.to_owned(),
        store_alias: store_alias.to_owned(),
    }
}

async fn rows(project: &Client) -> (i64, i64, i64) {
    let count = |sql: &'static str| async move {
        project
            .query_one(sql, &[&TENANT])
            .await
            .expect("count rows")
            .get::<_, i64>(0)
    };
    (
        count("SELECT count(*) FROM catalog.connection_instances WHERE tenant_id = $1").await,
        count("SELECT count(*) FROM catalog.connection_generations WHERE tenant_id = $1").await,
        count("SELECT count(*) FROM catalog.connection_bindings WHERE tenant_id = $1").await,
    )
}

#[tokio::test]
#[ignore = "requires two disposable PostgreSQL 18 databases named by WAMN_BIND_CONNECTION_{PROJECT,CONTROL}_PG_URL"]
async fn bind_connection_round_trips_through_the_plugins_own_resolution() {
    let project_url = std::env::var("WAMN_BIND_CONNECTION_PROJECT_PG_URL")
        .expect("WAMN_BIND_CONNECTION_PROJECT_PG_URL names a disposable PostgreSQL 18 database");
    let control_url = std::env::var("WAMN_BIND_CONNECTION_CONTROL_PG_URL")
        .expect("WAMN_BIND_CONNECTION_CONTROL_PG_URL names a disposable PostgreSQL 18 database");
    let (mut project, project_task) = connect(&project_url).await;
    let (control, control_task) = connect(&control_url).await;
    provision_project(&project, &project_url).await;
    provision_control(&control).await;
    let document = wiring();
    record_green_report(&control, &document.wiring_hash()).await;
    author(&control, &mut project, &document).await;
    // No tempfile dev-dependency in this crate: a pid-named scratch directory,
    // removed at the end.
    let scratch_dir = std::env::temp_dir().join(format!("wamn-bind-connection-{}", std::process::id()));
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
        .set_tenant(COMPONENT_ID, TENANT)
        .expect("register the component's tenant");
    let lookup = ConnectionEffectLookup {
        package_id: PACKAGE,
        effective_release_id: RELEASE_ID,
        environment: ENVIRONMENT,
        wiring_id: "store_label",
        wiring_version: 1,
        node_id: "store",
        component_digest: BLOB_PUT,
        store_alias: "labels",
        candidate_binding: None,
    };
    let snapshot_of = || async {
        postgres
            // WamnPostgres::new registers its one pool under DEFAULT_PROJECT;
            // the host names the project it serves, a fixture has only this.
            .connection_effect_snapshot(COMPONENT_ID, DEFAULT_PROJECT, TENANT, &lookup)
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
    let lacking = definition_file(scratch, "lacking.json", r#"{"endpoint":"http://10.0.0.7:9000","container":"labels"}"#);
    let error = bind_connection::bind(&args(&project_url, lacking, "labels", BLOB_PUT))
        .await
        .expect_err("a definition without prefix cannot be bound");
    assert!(format!("{error:#}").contains("lacks prefix"), "{error:#}");
    let good = definition_file(scratch, "good.json", r#"{"endpoint":"http://10.0.0.7:9000","container":"labels","prefix":"wms/"}"#);
    let error = bind_connection::bind(&args(&project_url, good.clone(), "erp", HTTP_SIDECAR))
        .await
        .expect_err("a blobstore instance cannot bind an HTTP alias");
    let text = format!("{error:#}");
    assert!(text.contains("declared erp as http/") && text.contains("not blobstore/"), "{text}");
    let error = bind_connection::bind(&args(&project_url, good.clone(), "nothing", BLOB_PUT))
        .await
        .expect_err("an alias the component never declared cannot be bound");
    assert!(format!("{error:#}").contains("declares no connection requirement named nothing"), "{error:#}");
    assert_eq!(rows(&project).await, (0, 0, 0), "a refusal writes nothing");

    // THE VERB.
    let bound = bind_connection::bind(&args(&project_url, good, "labels", BLOB_PUT))
        .await
        .expect("bind the labels store");
    assert_eq!(bound.instance_id, "labels-store");
    assert_eq!(bound.generation, 1);
    assert!(bound.definition_hash.starts_with("sha256:"));
    assert!(bound.validation_hash.starts_with("sha256:"));
    assert_ne!(bound.definition_hash, bound.validation_hash);
    assert_eq!(rows(&project).await, (1, 1, 1));

    // THE ROUND TRIP: the plugin loads the snapshot from the rows the verb
    // wrote and the blobstore resolver accepts it, returning what was bound.
    let after = snapshot_of().await;
    let resolved = binding::resolve(&after).expect("the plugin resolves the bound connection");
    assert_eq!(resolved.endpoint, "http://10.0.0.7:9000");
    assert_eq!(resolved.container, "labels");
    assert_eq!(resolved.prefix, "wms/");
    assert_eq!(resolved.credential_handle, "labels-store");
    assert_eq!(after.definition_hash.as_deref(), Some(bound.definition_hash.as_str()));
    assert_eq!(after.validation_hash.as_deref(), Some(bound.validation_hash.as_str()));

    // Binding the same alias twice is a primary-key refusal, not a silent
    // second generation: an amendment is a different verb.
    let again = definition_file(scratch, "again.json", r#"{"endpoint":"http://10.0.0.7:9000","container":"labels","prefix":"wms/"}"#);
    bind_connection::bind(&args(&project_url, again, "labels", BLOB_PUT))
        .await
        .expect_err("a second bind of the same instance and alias is refused");
    assert_eq!(rows(&project).await, (1, 1, 1));

    drop(project);
    drop(control);
    project_task.abort();
    control_task.abort();
    let _ = std::fs::remove_dir_all(&scratch_dir);
}
