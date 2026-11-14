//! One router-invoked `wamn:node` guest performing a real trusted HTTP effect.
//!
//! Shared test support seeds the declared requirement, selected version, and
//! release binding in the environment database. The system database supplies
//! active parameters and credential references through its targeted reader.
//!
//! Two beads need this same closure. `wamn-0h0g.11.8` drives it to witness
//! trace propagation at the wire; `wamn-0h0g.11.3` needs it to test HTTP
//! connection confinement refusals. The test module also tests native socket
//! reuse across real guest stores, with live lifecycle and generation changes.
//!
//! It needs three throwaway resources, all named by the caller: a superuser
//! PostgreSQL database (the `catalog` schema is DROPped and reinstalled), an
//! insecure OCI registry, and an upstream HTTP origin. Nothing here is stubbed —
//! the workflow resolves from its catalog row, the component bytes come
//! back through the production OCI puller, and the effect leaves the process
//! over a real socket.

use sqlx::{Connection as _, Row as _};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use wamn_kubernetes::Store as _;

use anyhow::Context as _;
use oci_client::client::{ClientConfig, ClientProtocol, Config, ImageLayer};
use oci_client::manifest::OciImageManifest;
use oci_client::secrets::RegistryAuth;
use oci_client::{Client as OciClient, Reference};
use wamn_catalog::component_artifact_reference;
use wamn_catalog::{
    AdmittedComponent, ComponentConnection, ComponentConnectionType, ComponentDeclaration,
    ComponentDescriptor, SERVING_MANIFEST_FORMAT_VERSION, WiringDocument, WiringNode,
};
use wamn_component_artifact::component_artifact_source::{
    ComponentArtifactSource, ComponentArtifactSourceConfig,
};
use wamn_connections::connection_credentials::ConnectionCredentials;
use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleIdentity, workload_login};
use wamn_engine::component_admission::{ComponentAdmissionRequest, validate_component_admission};
use wamn_engine::component_artifact::{component_artifact_config_bytes, component_artifact_layout};
use wamn_engine::engine::build_engine;
use wamn_engine::release_manifest::LoadedRelease;
use wamn_execution_host::{OperationHost, OperationHostConfig};
use wamn_http_capability::transport::HttpTransport;
use wamn_identity_protocol::AuthorityClass;
use wamn_postgres_capability::{ClassCredentials, WamnPostgres, WamnPostgresConfig};
use wamn_workflow::RouterDriver;
use wash_runtime::plugin::wasi_logging::TracingLogger;

/// The one project every seeded row and every identity is bound to.
pub const PROJECT_ENV_KEY: &str = "org-a";
pub const PACKAGE: &str = "orders";
pub const ENVIRONMENT: &str = "prod";
pub const PACKAGE_VERSION: &str = "1.0.0";
pub const WIRING_ID: &str = "hot-route";
pub const WIRING_VERSION: u32 = 1;
pub const NODE_ID: &str = "call-upstream";
/// The alias the guest names in `Request.requirement`, resolved at the
/// component grain by the shared connection authority resolver.
pub const STORE_ALIAS: &str = "upstream";
pub const PROJECT: &str = "default";
pub const COMPONENT: &str = "http-request";
pub const INTERFACE_VERSION: &str = "0.1.0";
pub const OPERATION: &str = "wamn:node/async-handler@0.1.0";
pub const ATTACHMENT_ID: &str = "orders-http";
pub const ROUTE_AUTHORITY: &str = "trusted.example.test";
pub const ROUTE_PATH: &str = "/deliver";
const CREDENTIAL_HANDLE: &str = "org:acme:upstream-v1";
const CONTRACT: &str = "wamn:connection/http@0.1.0";
const REGISTRY_IO_TIMEOUT: Duration = Duration::from_secs(30);
const GENERATION_PASSWORD: &str = "trusted-http-route";

/// The throwaway resources this closure is built over.
#[derive(Debug, Clone)]
pub struct RouteOptions {
    /// Superuser URL of a throwaway PostgreSQL database. Its `catalog` schema
    /// is dropped and reinstalled from `wamn_catalog::CATALOG_SCHEMA_SQL`.
    pub database_url: String,
    /// `<registry>/<repository>` base of a throwaway plain-HTTP OCI registry.
    pub artifact_base: String,
    /// The built `http_request.wasm` for `wasm32-wasip2`.
    pub component_wasm: PathBuf,
    /// Absolute base URL of the upstream origin, e.g. `http://127.0.0.1:8080`.
    pub upstream_base_url: String,
    /// The relative absolute-path the node requests under that base.
    pub path_and_query: String,
}

/// One live driver over the seeded closure, plus the identities a test needs
/// to address it.
impl std::fmt::Debug for TrustedHttpRoute {
    /// Names the identity only: the driver and the loaded release carry live
    /// host state that must not reach a log.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TrustedHttpRoute")
            .field("component_digest", &self.component_digest)
            .field("authority_database", &self.credential_database.name())
            .finish_non_exhaustive()
    }
}

pub struct TrustedHttpRoute {
    pub driver: Arc<RouterDriver>,
    /// The same loaded release the driver authorizes. Callers that exercise a
    /// release-owned ingress plugin must share this exact loaded release rather than mint
    /// a second view of the closure.
    /// Read by the callers that share the exact loaded release.
    pub release: Arc<LoadedRelease>,
    /// The digest OCI serves the guest under and every seeded row keys on.
    pub component_digest: String,
    /// The immutable graph hash stored on the workflow row and frozen on each run.
    pub wiring_hash: String,
    credential_database: wamn_test_infrastructure::postgres::Database,
}

impl TrustedHttpRoute {
    /// Execute the request shape that the queue supplies for an environment workflow.
    pub async fn execute(&self, id: u64) -> anyhow::Result<wamn_workflow::Outcome> {
        self.driver
            .execute_workflow(wamn_workflow::WorkflowRunRequest {
                project_env_key: PROJECT_ENV_KEY.to_owned(),
                package_id: PACKAGE.to_owned(),
                environment: ENVIRONMENT.to_owned(),
                wiring_id: WIRING_ID.to_owned(),
                wiring_version: WIRING_VERSION,
                delivery_id: format!("reuse-{id}"),
                payload: serde_json::json!({"id": id}),
                wiring_hash: self.wiring_hash.clone(),
                node_digests: BTreeMap::from([(NODE_ID.to_owned(), self.component_digest.clone())]),
                identity: wamn_workflow::WorkflowIdentity {
                    principal_id: wamn_identity_protocol::workflow_user_id().to_string(),
                    role: "admin".to_owned(),
                },
                causation: None,
            })
            .await
            .0
    }
}

/// Seed the closure and build the production driver over it.
pub async fn build(options: &RouteOptions) -> anyhow::Result<TrustedHttpRoute> {
    let (credentials, database) = credential_fixture(
        &[(CREDENTIAL_HANDLE, credential_secret())],
        &options.upstream_base_url,
    )
    .await?;
    build_with_credentials(options, credentials, database).await
}

async fn credential_fixture(
    secrets: &[(&str, String)],
    upstream: &str,
) -> anyhow::Result<(
    ConnectionCredentials,
    wamn_test_infrastructure::postgres::Database,
)> {
    let database = wamn_control_provision::test_database::system();
    let mut admin = wamn_postgres::connect(database.url()).await?;

    sqlx::raw_sql("SET ROLE wamn_system")
        .execute(&mut admin)
        .await
        .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::test_database::seed_org_project_env_sql(
            "acme",
            PROJECT,
            ENVIRONMENT,
        ),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("BEGIN; {} INSERT INTO registry.target(name,type,status) VALUES('cloud','cloud','active') ON CONFLICT DO NOTHING; UPDATE registry.meta SET platform_domain='example.invalid'; UPDATE registry.environments SET target_id=(SELECT id FROM registry.target WHERE name='cloud'); COMMIT",wamn_control_provision::bind_platform_principal_sql(wamn_control_provision::PlatformComponent::Provisioning)))).execute(&mut admin).await.map(drop)?;
    let credential_store = Arc::new(wamn_test_infrastructure::credentials::MemoryStore::default());
    let mut tx = admin.begin().await?;
    sqlx::raw_sql("SET LOCAL ROLE wamn_control")
        .execute(&mut *tx)
        .await
        .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ),
    ))
    .execute(&mut *tx)
    .await
    .map(drop)?;
    for (handle, secret) in secrets {
        let (_, name) =
            wamn_catalog::parse_org_credential_handle(handle).map_err(anyhow::Error::msg)?;
        let stored = credential_store
            .put(
                &wamn_registry::Triple::new("acme", ENVIRONMENT, PROJECT),
                uuid::Uuid::new_v4(),
                secret.as_bytes(),
            )
            .await?;
        wamn_test_infrastructure::credentials::define(&mut tx,"acme",&serde_json::json!({"project":PROJECT,"env":ENVIRONMENT,"name":name,"integration_type":"http","auth_type":"http_basic","parameters":{"primary-authority":upstream,"tls-verification":"disabled"},"credential":secret}),&wamn_test_infrastructure::credentials::ReferencedCredentials(&stored)).await?;
    }
    tx.commit().await?;
    sqlx::raw_sql("RESET ROLE")
        .execute(&mut admin)
        .await
        .map(drop)?;
    let role = format!("trusted_http_{}", database.name());
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::test_database::login_sql(
            WorkloadRoleFamily::ConnectionReader,
            database.name(),
            &role,
            GENERATION_PASSWORD,
        ),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    let mut transaction =
        wamn_environment::provision_project_env::provisioning_transaction(&mut admin).await?;
    sqlx::query(sqlx::AssertSqlSafe(
        (wamn_control_provision::sql::upsert_control_author_project_env_key_mapping_sql())
            .to_string(),
    ))
    .bind(&role)
    .bind(PROJECT_ENV_KEY)
    .bind("acme")
    .bind(PROJECT)
    .bind(ENVIRONMENT)
    .execute(&mut *transaction)
    .await
    .map(|result| result.rows_affected())?;
    transaction.commit().await?;
    let _reader = wamn_postgres::connect(&generation_url(database.url(), &role)?).await?;

    Ok((
        ConnectionCredentials::from_database(
            wamn_postgres::pool(&generation_url(database.url(), &role)?, 4).await?,
            "acme",
        )
        .with_store(credential_store),
        database,
    ))
}

async fn build_with_credentials(
    options: &RouteOptions,
    credentials: ConnectionCredentials,
    credential_database: wamn_test_infrastructure::postgres::Database,
) -> anyhow::Result<TrustedHttpRoute> {
    let component_bytes = std::fs::read(&options.component_wasm).with_context(|| {
        format!(
            "read component bytes {}: build with cargo build --manifest-path apps/Cargo.toml \
             -p http-request --target wasm32-wasip2",
            options.component_wasm.display()
        )
    })?;

    let engine = build_engine(&[]).context("build the router engine")?;
    let engine = Arc::new(engine);

    let admitted = validate_component_admission(
        &engine,
        &component_bytes,
        ComponentAdmissionRequest {
            declaration: declaration()?,
            admitted_platform_packages: BTreeSet::from([
                "wamn:node".to_owned(),
                "wamn:connection".to_owned(),
            ]),
            base_digests: BTreeSet::new(),
        },
    )
    .context("admit the http-request guest")?
    .component;

    publish_component(&options.artifact_base, &admitted, &component_bytes)
        .await
        .context("publish the guest to the throwaway registry")?;

    let document = wiring_document(&options.path_and_query);
    let wiring_hash = document.wiring_hash().as_str().to_owned();
    let release = Arc::new(
        LoadedRelease::load_canonical_bytes(
            &wamn_digest::canonical_json_bytes(&release_manifest(&admitted, &document)),
            "trusted-http-route fixture",
            wamn_engine::release_manifest::ReleaseTarget::new(PROJECT_ENV_KEY, ENVIRONMENT)
                .with_applications(
                    [(
                        "orders".to_owned(),
                        wamn_catalog::ApplicationDefinition {
                            connections: std::collections::BTreeMap::new(),
                            packages: [PACKAGE.to_owned()].into(),
                        },
                    )]
                    .into(),
                ),
        )
        .context("load the fixture serving manifest")?,
    );
    let postgres_credentials = seed_catalog(
        options,
        &admitted,
        &document,
        &release,
        None,
        credential_database.url(),
    )
    .await
    .context("seed the catalog closure")?;

    let postgres = Arc::new(
        WamnPostgres::new(WamnPostgresConfig {
            // wamn-0h0g.22.16: one url, named for every class explicitly.
            credentials: Some(postgres_credentials),
            guest_pool_max_size: 4,
            platform_pool_max_size: 4,
            wait_timeout_ms: 5_000,
            statement_timeout_ms: 10_000,
            row_limit: 10_000,
        })
        .context("build the platform pool")?,
    );

    let source = ComponentArtifactSource::new(
        ComponentArtifactSourceConfig::new(&options.artifact_base, true, REGISTRY_IO_TIMEOUT)
            .context("configure the component puller")?,
    )
    .context("build the component puller registry client")?;

    let facts = Arc::new(wamn_connections::connection_facts::ConnectionFacts::new(
        Arc::new(wamn_connections::connection_memory::ConnectionMemory::default()),
        wamn_broker::Publisher::default(),
        Arc::new(credentials),
        &postgres,
        PROJECT_ENV_KEY,
        PROJECT,
        ENVIRONMENT,
    ));
    let driver = Arc::new(RouterDriver::new(Arc::new(
        OperationHost::new(
            engine,
            Arc::clone(&postgres),
            Arc::new(wamn_blobstore::plugin::WamnBlobstore::new(postgres)),
            Arc::new(HttpTransport::new().context("build the process HTTP transport")?),
            facts,
            Arc::new(TracingLogger::default()), // The upstream is a loopback origin the test owns; the cluster
            // ceiling is Kubernetes' job, not this fixture's.
            Arc::from(vec!["*".parse().context("parse the allowed-host policy")?]),
            Arc::clone(&release),
            Arc::new(source),
            OperationHostConfig {
                project: PROJECT.to_owned(),
                schema: None,
                owner_prefix: "trusted-http-route".to_owned(),
                warm_reuse: wamn_engine::warm_reuse::WarmReuse::default(),
            },
        )
        .context("build the router driver")?,
    )));

    Ok(TrustedHttpRoute {
        driver,
        release,
        component_digest: admitted.component_digest.clone(),
        wiring_hash,
        credential_database,
    })
}

/// The shipped guest declaration, rendered for this package fixture.
/// Catalog wiring compatibility owns validation of node parameters and ports.
fn declaration() -> anyhow::Result<ComponentDeclaration> {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let template = PathBuf::from(format!(
        "{root}/apps/platform/no-std/http-request/declaration.json.in"
    ));
    let target =
        wamn_catalog::ComponentPackageCoordinate::new(PROJECT_ENV_KEY, PACKAGE, PACKAGE_VERSION)?;
    let document = wamn_package_control::component_declaration::render_palette_declaration(
        &template,
        &target,
        Some(STORE_ALIAS),
    )?;
    serde_json::from_value(document).context("parse the rendered http-request declaration")
}

/// One node: the guest calls the upstream and responds with what it answered.
fn wiring_document(path_and_query: &str) -> WiringDocument {
    WiringDocument {
        response: None,
        format_version: wamn_catalog::WIRING_DOCUMENT_FORMAT_VERSION.to_owned(),
        wiring_id: WIRING_ID.to_owned(),
        version: WIRING_VERSION,
        entry: NODE_ID.to_owned(),
        nodes: BTreeMap::from([(
            NODE_ID.to_owned(),
            WiringNode {
                component: COMPONENT.to_owned(),
                interface_version: INTERFACE_VERSION.to_owned(),
                operation: OPERATION.to_owned(),
                operation_dependency: None,
                params: BTreeMap::from([
                    (
                        "requirement".to_owned(),
                        serde_json::Value::from(STORE_ALIAS),
                    ),
                    ("method".to_owned(), serde_json::Value::from("POST")),
                    (
                        "path-and-query".to_owned(),
                        serde_json::Value::from(path_and_query),
                    ),
                ]),
                terminal: None,
            },
        )]),
        edges: Vec::new(),
    }
}

/// The one manifest the loaded release carries. Both membership tests the released
/// closure makes — `manifest.components` in `invoke_node`, `manifest.wirings` in
/// `validate_wiring_closure` and again in `authorize_release_closure` — read
/// this document.
fn release_manifest(
    component: &AdmittedComponent,
    _document: &WiringDocument,
) -> serde_json::Value {
    serde_json::json!({
        "format-version": SERVING_MANIFEST_FORMAT_VERSION,
        "release": {
            "packages": [{
                "package-id": PACKAGE,
                "package-version": PACKAGE_VERSION,
            }],
        },
        "components": [{
            "package-id": PACKAGE,
            "descriptor": ComponentDescriptor::new(
                component.clone(),
                vec![ComponentConnection {
                    store_alias: STORE_ALIAS.to_owned().into(),
                    requirement_type: ComponentConnectionType::Http,
            mode: wamn_catalog::ConnectionMode::Write,
                }],
            ),
        }],
        "routes": [],
        "attachments": {},

    })
}

/// The fixture supplies the declared Basic authentication fields.
fn credential_secret() -> String {
    serde_json::json!({"username":"fixture","password":"fixture-token"}).to_string()
}

/// Publish the exact bytes the production puller will verify, in the layout
/// `wamn-ctl push-package` writes for each component.
async fn publish_component(
    artifact_base: &str,
    component: &AdmittedComponent,
    component_bytes: &[u8],
) -> anyhow::Result<()> {
    let artifact = component_artifact_reference(artifact_base, &component.component_digest)
        .context("derive the component artifact reference")?;
    let reference = Reference::with_tag(
        artifact.registry().to_owned(),
        artifact.repository().to_owned(),
        artifact.tag().to_owned(),
    );
    let config_bytes = component_artifact_config_bytes(component);
    let layout = component_artifact_layout(component_bytes, &config_bytes);
    let layer = ImageLayer::new(
        layout.component_bytes().to_vec(),
        layout.layer_media_type().to_owned(),
        None,
    );
    let config = Config::new(
        layout.config_bytes().to_vec(),
        layout.config_media_type().to_owned(),
        None,
    );
    let manifest = OciImageManifest::build(std::slice::from_ref(&layer), &config, None);
    OciClient::new(ClientConfig {
        protocol: ClientProtocol::HttpsExcept(vec![reference.resolve_registry().to_owned()]),
        read_timeout: Some(REGISTRY_IO_TIMEOUT),
        connect_timeout: Some(REGISTRY_IO_TIMEOUT),
        ..ClientConfig::default()
    })
    .push(
        &reference,
        std::slice::from_ref(&layer),
        config,
        &RegistryAuth::Anonymous,
        Some(manifest),
    )
    .await
    .with_context(|| format!("push {reference}"))?;
    Ok(())
}

/// Install the catalog DDL and every row the resolution and the effect read.
async fn seed_catalog(
    options: &RouteOptions,
    component: &AdmittedComponent,
    document: &WiringDocument,
    release: &LoadedRelease,
    additional_wiring: Option<(&AdmittedComponent, &[WiringDocument])>,
    system_url: &str,
) -> anyhow::Result<ClassCredentials> {
    let mut client = wamn_postgres::connect(&options.database_url)
        .await
        .context("connect the seeding session")?;

    let seeded = seed_with_client(
        &mut client,
        options,
        component,
        document,
        release,
        additional_wiring,
        system_url,
    )
    .await;
    drop(client);
    seeded
}

async fn seed_with_client(
    client: &mut sqlx::PgConnection,
    options: &RouteOptions,
    component: &AdmittedComponent,
    document: &WiringDocument,
    release: &LoadedRelease,
    additional_wiring: Option<(&AdmittedComponent, &[WiringDocument])>,
    system_url: &str,
) -> anyhow::Result<ClassCredentials> {
    // The complete catalog bootstrap includes its own transaction and applies
    // only to a fresh database. Submit that complete string as one batch.
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let schema = wamn_catalog::CATALOG_SCHEMA_SQL;
    let app_schema = std::fs::read_to_string(format!("{root}/deploy/sql/app-schema.sql"))
        .context("read the application authorization DDL")?;
    let run_state = std::fs::read_to_string(format!("{root}/deploy/sql/run-state.sql"))
        .context("read the run-state DDL")?;
    let run_queue = std::fs::read_to_string(format!("{root}/deploy/sql/run-queue.sql"))
        .context("read the run-queue DDL")?;
    let role_bootstrap = wamn_control_provision::sql::ensure_app_acl_role_sql();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "{role_bootstrap}\n\
             DROP SCHEMA IF EXISTS catalog CASCADE;\n\
             DROP SCHEMA IF EXISTS platform CASCADE;\n\
             DROP SCHEMA IF EXISTS wamn_run CASCADE;\n\
             {schema}\n\
             {app_schema}\n\
             {run_state}\n\
             {run_queue}"
    )))
    .execute(&mut *client)
    .await
    .map(drop)
    .context("install the catalog and run-plane DDL")?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::platform_principals_sql("http-fixture.invalid")?.clone(),
    ))
    .execute(&mut *client)
    .await
    .map(drop)?;

    sqlx::raw_sql("SELECT set_config('app.user_id','770df186-ac15-579e-b46b-c297cae2011b',false),set_config('app.operation','admin:seed-http-fixture',false)").execute(&mut *client).await.map(drop)?;
    sqlx::raw_sql("INSERT INTO platform.role (name) VALUES ('admin') ON CONFLICT DO NOTHING")
        .execute(&mut *client)
        .await
        .map(drop)?;
    sqlx::query("INSERT INTO platform.user_role (user_id, role_name) VALUES ($1::text::uuid, 'admin') ON CONFLICT DO NOTHING").bind(wamn_identity_protocol::workflow_user_id().to_string()).execute(&mut *client).await.map(|result| result.rows_affected())?;
    let wiring_version = i32::try_from(WIRING_VERSION).expect("fixture wiring version fits");
    let graph_json = serde_json::to_string(document).context("serialize the wiring document")?;
    let wiring_hash = document.wiring_hash().as_str().to_owned();
    // Every catalog table FORCEs RLS on `app.project_env_key`, and a superuser is not
    // exempt from FORCE — the identity is what makes these writes land at all.
    sqlx::query("SELECT set_config('app.project_env_key', $1, false)")
        .bind(PROJECT_ENV_KEY)
        .fetch_one(&mut *client)
        .await
        .context("bind the seeding project identity")?;

    sqlx::raw_sql("SELECT set_config('app.user_id', '770df186-ac15-579e-b46b-c297cae2011b', false), set_config('app.operation', 'admin:seed-catalog-fixture', false)").execute(&mut *client).await.map(drop)?;
    let package_manifest = serde_json::json!({
        "package": {"id": PACKAGE, "version": PACKAGE_VERSION},
        "models": {},
        "custom_operations": {},
        "queries": {},
        "connections": [],
        "components": {},
    });
    let package_artifact_digest = wamn_digest::canonical_json_sha256(&package_manifest);
    sqlx::query(
        "INSERT INTO catalog.packages \
                    (project_env_key, package_id, package_version, artifact_digest) \
             VALUES ($1, $2, $3, $4)",
    )
    .bind(PROJECT_ENV_KEY)
    .bind(PACKAGE)
    .bind(PACKAGE_VERSION)
    .bind(&package_artifact_digest)
    .execute(&mut *client)
    .await
    .map(|result| result.rows_affected())
    .context("seed the exact package coordinate")?;
    // The release is its manifest bytes under their digest; package and
    // component membership are read from them.
    let canonical_release = release.manifest().canonical_bytes();
    let manifest_digest = release.release().manifest_digest.as_str();
    sqlx::query(
        "INSERT INTO catalog.releases (project_env_key, manifest_digest, canonical_bytes) \
             VALUES ($1, $2, $3)",
    )
    .bind(PROJECT_ENV_KEY)
    .bind(manifest_digest)
    .bind(&canonical_release)
    .execute(&mut *client)
    .await
    .map(|result| result.rows_affected())
    .context("record the release")?;
    sqlx::query(
        "INSERT INTO catalog.workflows (project_env_key, environment, application, workflow_id, \
                    version, graph_json, wiring_hash, trigger, execution_type, created_by) \
             VALUES ($1, $2, $3, $4, $5, $6::text::jsonb, $7, 'manual', 'invoker', $8::text::uuid)",
    )
    .bind(PROJECT_ENV_KEY)
    .bind(ENVIRONMENT)
    .bind(PACKAGE)
    .bind(WIRING_ID)
    .bind(wiring_version)
    .bind(&graph_json)
    .bind(&wiring_hash)
    .bind(wamn_identity_protocol::workflow_user_id().to_string())
    .execute(&mut *client)
    .await
    .map(|result| result.rows_affected())
    .context("seed the wiring version")?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_control_provision::sql::grant_administration_surface_sql("wamn_run"),
    ))
    .execute(&mut *client)
    .await
    .map(drop)?;
    let authority: wamn_environment::environment::document::EnvironmentDocument =
        serde_json::from_value(
            serde_json::json!({"org":"acme","project":PROJECT,"env":ENVIRONMENT,"target":"cloud","policy":"dev","revocation_seconds":60,
            "applications": {}}),
        )?;
    let _ = wamn_environment::environment::bindings::project_authority(
        system_url,
        &options.database_url,
        &authority,
    )
    .await?;
    let connection: String=sqlx::query("SELECT c.id::text FROM platform.connection c JOIN platform.integration i ON i.id=c.integration_id WHERE i.name='upstream-v1'").fetch_one(&mut *client).await?.get(0);
    sqlx::query("INSERT INTO catalog.connection_requirement(project_env_key,component_digest,package_id,name,interfaces,mode) VALUES($1,$2,$3,$4,$5,'write')").bind(PROJECT_ENV_KEY).bind(&component.component_digest).bind(PACKAGE).bind(STORE_ALIAS).bind(vec![CONTRACT]).execute(&mut *client).await.map(|result| result.rows_affected())?;
    sqlx::query("INSERT INTO catalog.release_connection(project_env_key,manifest_digest,connection_id,version) VALUES($1,$2,$3::text::uuid,1)").bind(PROJECT_ENV_KEY).bind(manifest_digest).bind(&connection).execute(&mut *client).await.map(|result| result.rows_affected())?;
    sqlx::query("INSERT INTO catalog.connection_binding(project_env_key,manifest_digest,component_digest,package_id,name,connection_id) VALUES($1,$2,$3,$4,$5,$6::text::uuid)").bind(PROJECT_ENV_KEY).bind(manifest_digest).bind(&component.component_digest).bind(PACKAGE).bind(STORE_ALIAS).bind(&connection).execute(&mut *client).await.map(|result| result.rows_affected())?;

    if let Some((component, documents)) = additional_wiring {
        seed_additional_wirings(client, component, documents).await?;
    }

    let config: sqlx::postgres::PgConnectOptions = options
        .database_url
        .parse()
        .context("parse the disposable admin URL")?;
    let database = config
        .get_database()
        .context("the disposable admin URL names no database")?;
    let target = WorkloadRoleIdentity::ProjectEnvironment {
        org: PROJECT_ENV_KEY,
        project: PROJECT,
        environment: ENVIRONMENT,
        database,
    };
    let guest_role = workload_login(
        WorkloadRoleFamily::App,
        WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key: PROJECT_ENV_KEY,
            database,
        },
    )?;
    let guest_sql = wamn_control_provision::test_database::login_sql(
        WorkloadRoleFamily::App,
        database,
        &guest_role,
        GENERATION_PASSWORD,
    );
    let executor_role = workload_login(WorkloadRoleFamily::ExecutorPlatform, target)
        .context("derive the executor-platform generation")?;
    let http_role = workload_login(WorkloadRoleFamily::HttpAdmitter, target)
        .context("derive the callable-HTTP generation")?;
    let executor_sql = wamn_control_provision::test_database::login_sql(
        WorkloadRoleFamily::ExecutorPlatform,
        database,
        &executor_role,
        GENERATION_PASSWORD,
    );
    let http_sql = wamn_control_provision::test_database::login_sql(
        WorkloadRoleFamily::HttpAdmitter,
        database,
        &http_role,
        GENERATION_PASSWORD,
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "{guest_sql} {executor_sql} {http_sql}"
    )))
    .execute(&mut *client)
    .await
    .map(drop)
    .context("mint the production platform credential generations")?;

    Ok(ClassCredentials::default()
        .with_class(
            AuthorityClass::GuestSql,
            generation_url(&options.database_url, &guest_role)?,
        )
        .with_class(
            AuthorityClass::ExecutorPlatform,
            generation_url(&options.database_url, &executor_role)?,
        )
        .with_class(
            AuthorityClass::CallableHttp,
            generation_url(&options.database_url, &http_role)?,
        ))
}

async fn seed_additional_wirings(
    client: &mut sqlx::PgConnection,
    component: &AdmittedComponent,
    documents: &[WiringDocument],
) -> anyhow::Result<()> {
    let package = &component.target.package_id;
    let package_version = &component.target.package_version;
    if package != PACKAGE {
        let manifest = serde_json::json!({"package": {"id": package, "version": package_version},
            "models": {}, "custom_operations": {}, "queries": {}, "connections": {}, "components": {}});
        sqlx::query("INSERT INTO catalog.packages (project_env_key, package_id, package_version, artifact_digest) \
             VALUES ($1, $2, $3, $4)").bind(PROJECT_ENV_KEY).bind(package).bind(package_version).bind(wamn_digest::canonical_json_sha256(&manifest)).execute(&mut *client).await.map(|result| result.rows_affected())?;
    }
    for document in documents {
        let version = i32::try_from(document.version)?;
        let wiring_hash = document.wiring_hash().as_str().to_owned();
        sqlx::query("INSERT INTO catalog.workflows (project_env_key, environment, application, workflow_id, \
             version, graph_json, wiring_hash, trigger, execution_type, created_by) VALUES ($1, $2, $3, $4, $5, $6::text::jsonb, $7, 'manual', 'invoker', $8::text::uuid)").bind(PROJECT_ENV_KEY).bind(ENVIRONMENT).bind(PACKAGE).bind(&document.wiring_id).bind(version).bind(&serde_json::to_string(document)?).bind(&wiring_hash).bind(wamn_identity_protocol::workflow_user_id().to_string()).execute(&mut *client).await.map(|result| result.rows_affected())
            .context("seed the additional immutable wiring")?;
    }
    Ok(())
}

fn generation_url(admin_url: &str, role: &str) -> anyhow::Result<String> {
    let mut url = reqwest::Url::parse(admin_url).context("parse the disposable admin URL")?;
    url.set_username(role)
        .map_err(|()| anyhow::anyhow!("set the production generation username"))?;
    url.set_password(Some(GENERATION_PASSWORD))
        .map_err(|()| anyhow::anyhow!("set the production generation password"))?;
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use sqlx::Row as _;
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::time::Duration;

    use anyhow::Context as _;

    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::mpsc;
    use tokio::task::{JoinHandle, JoinSet};

    use wamn_workflow::Outcome;

    use super::{
        CREDENTIAL_HANDLE, ENVIRONMENT, NODE_ID, PROJECT, RouteOptions, build_with_credentials,
        credential_fixture, credential_secret,
    };

    const ROTATED_HANDLE: &str = "org:acme:upstream-v2";
    const ROTATED_AUTHORIZATION: &str = "Basic Zml4dHVyZTpyb3RhdGVkLWZpeHR1cmUtdG9rZW4=";

    struct ObservedRequest {
        connection_id: u64,
        request_id: u64,
        authorization: String,
        idempotency_key: String,
    }

    struct Origin {
        address: SocketAddr,
        requests: mpsc::Receiver<ObservedRequest>,
        task: JoinHandle<()>,
    }

    impl Drop for Origin {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn origin() -> anyhow::Result<Origin> {
        let interface = std::net::UdpSocket::bind("0.0.0.0:0")?;
        interface.connect("192.0.2.1:9")?;
        let listener = TcpListener::bind((interface.local_addr()?.ip(), 0)).await?;
        let address = listener.local_addr()?;
        let (sent, requests) = mpsc::channel(16);
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            let mut connection_id = 0;
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (socket, _) = accepted.expect("accept the actual HTTP peer");
                        connection_id += 1;
                        connections.spawn(serve_socket(socket, connection_id, sent.clone()));
                    }
                    finished = connections.join_next(), if !connections.is_empty() => {
                        finished.expect("one origin connection finished")
                            .expect("origin connection task did not panic")
                            .expect("origin connection completed without protocol errors");
                    }
                }
            }
        });
        Ok(Origin {
            address,
            requests,
            task,
        })
    }

    async fn serve_socket(
        mut socket: TcpStream,
        connection_id: u64,
        sent: mpsc::Sender<ObservedRequest>,
    ) -> anyhow::Result<()> {
        loop {
            let mut head = Vec::new();
            let mut byte = [0_u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                if socket.read(&mut byte).await? == 0 {
                    anyhow::ensure!(head.is_empty(), "HTTP peer truncated its request head");
                    return Ok(());
                }
                head.push(byte[0]);
                anyhow::ensure!(head.len() <= 16_384, "fixture request head exceeds 16 KiB");
            }
            let head = std::str::from_utf8(&head)?;
            anyhow::ensure!(
                head.lines().next() == Some("POST /reuse HTTP/1.1"),
                "fixture received the wrong target or protocol"
            );
            let headers: HashMap<_, _> = head
                .lines()
                .skip(1)
                .filter_map(|line| line.split_once(':'))
                .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
                .collect();
            let length: usize = headers
                .get("content-length")
                .context("fixture request has no content length")?
                .parse()?;
            anyhow::ensure!(length <= 65_536, "fixture body exceeds 64 KiB");
            let mut body = vec![0_u8; length];
            socket.read_exact(&mut body).await?;
            let body: serde_json::Value = serde_json::from_slice(&body)?;
            let request_id = body["id"].as_u64().context("fixture request has no id")?;
            let observed_request = ObservedRequest {
                connection_id,
                request_id,
                authorization: headers
                    .get("authorization")
                    .context("effect omitted its credential header")?
                    .clone(),
                idempotency_key: headers
                    .get("idempotency-key")
                    .context("effect omitted its delivery idempotency key")?
                    .clone(),
            };
            // Record the request before sending the response.
            // A completed driver call always exposes its request to the assertions below.
            sent.send(observed_request)
                .await
                .map_err(|_| anyhow::anyhow!("request receiver closed"))?;
            let body = serde_json::json!({
                "connection-id": connection_id,
                "request-id": request_id,
            })
            .to_string();
            socket.write_all(format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                body.len(),
            ).as_bytes()).await?;
            socket.flush().await?;
        }
    }

    async fn assert_observed_request(
        origin: &mut Origin,
        delivery: Outcome,
        request_id: u64,
        authorization: &str,
    ) -> anyhow::Result<u64> {
        assert!(
            delivery.failure.is_none(),
            "real HTTP guest failed: {:?}",
            delivery.failure
        );
        assert_eq!(delivery.result["status"], 200);
        let observed_request = origin
            .requests
            .recv()
            .await
            .context("upstream origin stopped")?;
        assert_eq!(observed_request.request_id, request_id);
        assert!(
            observed_request.authorization == authorization,
            "wrong generation credential reached the wire"
        );
        assert_eq!(
            observed_request.idempotency_key,
            format!("reuse-{request_id}:{NODE_ID}:0")
        );
        assert_eq!(delivery.result["body"]["request-id"], request_id);
        assert_eq!(
            delivery.result["body"]["connection-id"],
            observed_request.connection_id
        );
        Ok(observed_request.connection_id)
    }

    async fn set_instance_status(system_url: &str, status: &str) -> anyhow::Result<()> {
        let mut client = wamn_postgres::connect(system_url).await?;

        let mut tx =
            wamn_environment::provision_project_env::provisioning_transaction(&mut client).await?;
        sqlx::raw_sql("SET LOCAL ROLE wamn_control")
            .execute(&mut *tx)
            .await
            .map(drop)?;
        wamn_platform_identity::connections::administration::execute(
            &mut tx,
            "acme",
            if status == "disabled" {
                "connection.disable"
            } else {
                "connection.enable"
            },
            &serde_json::json!({"project":PROJECT,"env":ENVIRONMENT,"name":"upstream-v1"})
                .to_string(),
            &mut wamn_broker::auth_changes::AuthDrops::default(),
            &mut wamn_broker::connection_changes::ConnectionDrops::default(),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn rotate(system_url: &str, generation: i64, handle: &str) -> anyhow::Result<()> {
        let mut client = wamn_postgres::connect(system_url).await?;

        let (_, name) =
            wamn_catalog::parse_org_credential_handle(handle).map_err(anyhow::Error::msg)?;
        let mut tx =
            wamn_environment::provision_project_env::provisioning_transaction(&mut client).await?;
        let row=sqlx::query("SELECT g.credential_id::text,k.store,k.ref,k.version FROM registry.connection c JOIN registry.integration i ON i.id=c.integration_id JOIN registry.connection_generation g ON(g.connection_id,g.generation)=(c.id,c.active_generation) JOIN identity.credential k ON k.id=g.credential_id WHERE i.org='acme' AND i.project=$1 AND i.name=$2").bind(&PROJECT).bind(&name).fetch_one(&mut *tx).await?;
        let stored = wamn_kubernetes::StoreVersion {
            store: "k8s",
            reference: row.get(2),
            version: row.get(3),
        };
        sqlx::raw_sql("SET LOCAL ROLE wamn_control")
            .execute(&mut *tx)
            .await
            .map(drop)?;
        wamn_platform_identity::connections::administration::execute_with_store(&mut tx,"acme","connection.set_credential",&serde_json::json!({"project":PROJECT,"env":ENVIRONMENT,"name":"upstream-v1","credential":if handle==CREDENTIAL_HANDLE {credential_secret()} else {serde_json::json!({"username":"fixture","password":"rotated-fixture-token"}).to_string()}}).to_string(),&mut wamn_broker::auth_changes::AuthDrops::default(), &mut wamn_broker::connection_changes::ConnectionDrops::default(),Some(&wamn_test_infrastructure::credentials::ReferencedCredentials(&stored))).await?;
        tx.commit().await?;
        let actual: i64=sqlx::query("SELECT c.active_generation FROM registry.connection c JOIN registry.integration i ON i.id=c.integration_id WHERE i.project=$1 AND i.name='upstream-v1'").bind(&PROJECT).fetch_one(&mut client).await?.get(0);
        assert_eq!(actual, generation);
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires: env:WAMN_HTTP_REUSE_ARTIFACT_BASE, env:WAMN_HTTP_REUSE_COMPONENT_WASM"]
    async fn real_http_guest_reuses_connections_without_reusing_authority() -> anyhow::Result<()> {
        wamn_test_postgres::require_ignored_prerequisites(
            "requires: env:WAMN_HTTP_REUSE_ARTIFACT_BASE, env:WAMN_HTTP_REUSE_COMPONENT_WASM",
        );
        Box::pin(tokio::time::timeout(
            Duration::from_secs(180),
            test_connection_reuse(),
        ))
        .await
        .context("real HTTP guest connection reuse test exceeded 180 seconds")?
    }

    async fn test_connection_reuse() -> anyhow::Result<()> {
        // The route seed creates shared roles.
        let _lock = wamn_test_infrastructure::postgres::lock();
        let database = wamn_test_infrastructure::postgres::database();
        let database_url = database.url().to_owned();
        let artifact_base = std::env::var("WAMN_HTTP_REUSE_ARTIFACT_BASE")
            .context("set WAMN_HTTP_REUSE_ARTIFACT_BASE to a throwaway OCI repository")?;
        let component_wasm = std::env::var("WAMN_HTTP_REUSE_COMPONENT_WASM")
            .context("set WAMN_HTTP_REUSE_COMPONENT_WASM to the actual http_request.wasm")?;
        let mut admin = wamn_postgres::connect(&database_url).await?;

        let version: String = sqlx::query("SHOW server_version_num")
            .fetch_one(&mut admin)
            .await?
            .get(0);
        anyhow::ensure!(
            version.parse::<u32>()? / 10_000 == 18,
            "test requires PostgreSQL 18"
        );
        let mut origin = origin().await?;
        let (credentials, credential_database) = credential_fixture(
            &[
                (CREDENTIAL_HANDLE, credential_secret()),
                (
                    ROTATED_HANDLE,
                    serde_json::json!({"username":"fixture","password":"rotated-fixture-token"})
                        .to_string(),
                ),
            ],
            &format!("http://{}", origin.address),
        )
        .await?;
        let route = build_with_credentials(
            &RouteOptions {
                database_url: database_url.clone(),
                artifact_base,
                component_wasm: component_wasm.into(),
                upstream_base_url: format!("http://{}", origin.address),
                path_and_query: "/reuse".to_owned(),
            },
            credentials,
            credential_database,
        )
        .await?;
        // RouterDriver instantiates and drops a distinct Store for each call.
        // The peer identity comes from accept(), not a transport cache counter.
        let first = assert_observed_request(
            &mut origin,
            route.execute(1).await?,
            1,
            "Basic Zml4dHVyZTpmaXh0dXJlLXRva2Vu",
        )
        .await?;
        let second = assert_observed_request(
            &mut origin,
            route.execute(2).await?,
            2,
            "Basic Zml4dHVyZTpmaXh0dXJlLXRva2Vu",
        )
        .await?;
        assert_eq!(
            first, second,
            "fresh guest stores did not reuse the accepted connection"
        );

        set_instance_status(route.credential_database.url(), "disabled").await?;
        let denied = route.execute(3).await?;
        let failure = denied
            .failure
            .context("warm transport bypassed the disabled connection instance")?;
        assert_eq!(failure.node, NODE_ID);
        assert_eq!(
            failure.detail.code.as_deref(),
            Some("connection-unavailable")
        );
        assert_eq!(
            failure.detail.message,
            "connection failed: ConnectionError::CredentialUnavailable"
        );
        assert!(
            origin.requests.try_recv().is_err(),
            "denied effect reached the warm socket"
        );
        set_instance_status(route.credential_database.url(), "enabled").await?;

        rotate(route.credential_database.url(), 2, CREDENTIAL_HANDLE).await?;
        let generation_two = assert_observed_request(
            &mut origin,
            route.execute(7).await?,
            7,
            "Basic Zml4dHVyZTpmaXh0dXJlLXRva2Vu",
        )
        .await?;
        assert_ne!(
            generation_two, first,
            "new generation reused the old generation socket"
        );
        rotate(route.credential_database.url(), 3, ROTATED_HANDLE).await?;
        let generation_three = assert_observed_request(
            &mut origin,
            route.execute(10).await?,
            10,
            ROTATED_AUTHORIZATION,
        )
        .await?;
        assert_ne!(
            generation_three, generation_two,
            "rotated credential reused the previous socket"
        );
        let generation_three_again = assert_observed_request(
            &mut origin,
            route.execute(11).await?,
            11,
            ROTATED_AUTHORIZATION,
        )
        .await?;
        assert_eq!(
            generation_three, generation_three_again,
            "rotated generation did not become reusable"
        );
        assert_observed_request(
            &mut origin,
            route.execute(13).await?,
            13,
            ROTATED_AUTHORIZATION,
        )
        .await?;

        assert!(
            origin.requests.try_recv().is_err(),
            "unexpected extra upstream effect"
        );
        drop(admin);
        Ok(())
    }
}
