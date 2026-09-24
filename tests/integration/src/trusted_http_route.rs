//! One router-invoked `wamn:node` guest performing a real trusted HTTP effect.
//!
//! Shared test support: it seeds exactly the facts
//! `ConnectionHttp::send` reads — a component-grain
//! `catalog.connection_requirements` row, an `active`/`valid`
//! `catalog.connection_bindings` row over an `enabled`
//! `catalog.connection_instances` whose `active_generation` matches the
//! `catalog.connection_generations` row carrying the credential handle — and
//! builds the production `RouterDriver` over them.
//!
//! Two beads need this same closure. `wamn-0h0g.11.8` drives it to witness
//! trace propagation at the wire; `wamn-0h0g.11.3` needs it to test HTTP
//! connection confinement refusals. The test module also tests native socket
//! reuse across real guest stores, with live lifecycle and generation changes.
//!
//! It needs three throwaway resources, all named by the caller: a superuser
//! PostgreSQL database (the `catalog` schema is DROPped and reinstalled), an
//! insecure OCI registry, and an upstream HTTP origin. Nothing here is stubbed —
//! the wiring resolves through `RELEASE_WIRING_SQL`, the component bytes come
//! back through the production OCI puller, and the effect leaves the process
//! over a real socket.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use oci_client::client::{ClientConfig, ClientProtocol, Config, ImageLayer};
use oci_client::manifest::OciImageManifest;
use oci_client::secrets::RegistryAuth;
use oci_client::{Client as OciClient, Reference};
use tokio_postgres::NoTls;
use wamn_catalog::{
    AdmittedComponent, ComponentDeclaration, ConnectionTypeDescriptor,
    SERVING_MANIFEST_FORMAT_VERSION, ServingComponentOperation, WiringDocument, WiringNode,
    WiringTerminal, flip_activation,
};
use wamn_control::push_component::admitted_projection_hash;
use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, workload_generation_role,
};
use wamn_engine::component_admission::{ComponentAdmissionRequest, validate_component_admission};
use wamn_engine::component_artifact::{
    component_artifact_config_bytes, component_artifact_layout, component_artifact_reference,
};
use wamn_engine::engine::build_engine;
use wamn_engine::release_manifest::LoadedRelease;
use wamn_execution_host::{OperationHost, OperationScope};
use wamn_run_state::AuthorityClass;
use wamn_runtime::component_artifact_source::{
    ComponentArtifactSource, ComponentArtifactSourceConfig,
};
use wamn_runtime::plugins::connection_http::transport::HttpTransport;
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_logging::{WamnLogging, WamnLoggingConfig};
use wamn_runtime::plugins::wamn_postgres::{ClassCredentials, WamnPostgres, WamnPostgresConfig};
use wamn_schema_control::connections::ComponentConnectionRequirement;
use wamn_workflow::{RouterDriver, RouterDriverConfig, WiringCacheCapacity};

/// The one tenant every seeded row and every claim is scoped to.
pub const TENANT: &str = "tenant-a";
pub const PACKAGE: &str = "orders";
pub const ENVIRONMENT: &str = "prod";
pub const PACKAGE_VERSION: &str = "1.0.0";
pub const EFFECTIVE_RELEASE_ID: i32 = 1;
pub const WIRING_ID: &str = "hot-route";
pub const WIRING_VERSION: u32 = 1;
pub const NODE_ID: &str = "call-upstream";
/// The alias the guest names in `Request.requirement`, resolved at the
/// component grain by `CONNECTION_EFFECT_SNAPSHOT_SQL`.
pub const STORE_ALIAS: &str = "upstream";
pub const PROJECT: &str = "default";
pub const COMPONENT: &str = "http-request";
pub const INTERFACE_VERSION: &str = "0.1.0";
pub const OPERATION: &str = "wamn:node/async-handler@0.1.0";
pub const ATTACHMENT_ID: &str = "orders-http";
pub const ROUTE_AUTHORITY: &str = "tap.example.test";
pub const ROUTE_PATH: &str = "/deliver";
const INSTANCE_ID: &str = "upstream-instance";
const CREDENTIAL_HANDLE: &str = "upstream-v1";
const GENERATION: i64 = 1;
const CONTRACT: &str = "wamn:connection/http@0.1.0";
const REGISTRY_IO_TIMEOUT: Duration = Duration::from_secs(30);
const GENERATION_PASSWORD: &str = "router-tap-live";
const GENERATION_VALID_UNTIL: &str = "2099-01-01T00:00:00Z";

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
    /// The wiring document's own canonical hash — `catalog.wirings.wiring_hash`,
    /// the activation pointer's `confirmed_definition_hash`, and the release
    /// manifest's `graph-hash`, which must all agree.
    pub wiring_hash: String,
}

/// Seed the closure and build the production driver over it.
pub async fn build(options: &RouteOptions) -> anyhow::Result<TrustedHttpRoute> {
    build_with_credentials(
        options,
        WamnCredentials::from_projects(HashMap::from([(
            PROJECT.to_owned(),
            HashMap::from([(CREDENTIAL_HANDLE.to_owned(), credential_secret())]),
        )])),
    )
    .await
}

async fn build_with_credentials(
    options: &RouteOptions,
    credentials: WamnCredentials,
) -> anyhow::Result<TrustedHttpRoute> {
    let component_bytes = std::fs::read(&options.component_wasm).with_context(|| {
        format!(
            "read component bytes {} — build it with a SEPARATE cargo invocation \
             (`cargo build -p http-request --target wasm32-wasip2` inside \
             apps/platform/no-std/, which is a separate workspace because sharing one \
             invocation with flow-http/materializer unifies serde_json/std into the \
             no_std guest and fails with E0152 — wamn-0h0g.11.56)",
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
            effect_free_operation_dependencies: BTreeSet::new(),
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
            &wamn_execution_contract::canonical_json_bytes(&release_manifest(
                &admitted,
                &wiring_hash,
            )),
            "trusted-http-route fixture",
        )
        .context("load the fixture serving manifest")?,
    );
    let postgres_credentials =
        seed_catalog(options, &admitted, &document, &wiring_hash, &release, None)
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

    let driver = Arc::new(RouterDriver::new(
        Arc::new(
            OperationHost::new(
                engine,
                postgres,
                Arc::new(HttpTransport::new().context("build the process HTTP transport")?),
                Arc::new(credentials),
                Arc::new(
                    WamnLogging::new(&WamnLoggingConfig::default())
                        .context("build wamn:logging")?,
                ), // The upstream is a loopback origin the test owns; the cluster
                // ceiling is Kubernetes' job, not this fixture's.
                Arc::from(vec!["*".parse().context("parse the allowed-host policy")?]),
                Arc::clone(&release),
                Arc::new(source),
                OperationScope {
                    project: PROJECT.to_owned(),
                    schema: None,
                    owner_prefix: "trusted-http-route".to_owned(),
                    warm_reuse: wamn_engine::warm_reuse::WarmReuse::default(),
                },
            )
            .context("build the router driver")?,
        ),
        RouterDriverConfig {
            cache_capacity: WiringCacheCapacity::default(),
        },
    ));

    Ok(TrustedHttpRoute {
        driver,
        release,
        component_digest: admitted.component_digest.clone(),
        wiring_hash,
    })
}

/// The SHIPPED palette declaration, rendered the way
/// `apps/platform/no-std/publish.sh` renders it.
///
/// Read from the template rather than restated here: a copy would drift from
/// the guest's real parameter contract, and `validate_parameters` in
/// `wiring_lowering` checks this node's params against exactly these
/// declarations.
fn declaration() -> anyhow::Result<ComponentDeclaration> {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let template = std::fs::read_to_string(format!(
        "{root}/apps/platform/no-std/http-request/declaration.json.in"
    ))
    .context("read the http-request declaration template")?;
    let rendered = template
        .replace("__TENANT_ID__", TENANT)
        .replace("__PACKAGE_ID__", PACKAGE)
        .replace("__PACKAGE_VERSION__", PACKAGE_VERSION);
    serde_json::from_str(&rendered).context("parse the rendered http-request declaration")
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
                terminal: Some(WiringTerminal::Respond),
            },
        )]),
        edges: Vec::new(),
        cases: Vec::new(),
    }
}

/// The one manifest the loaded release carries. Both membership checks the released
/// closure makes — `manifest.components` in `invoke_node`, `manifest.wirings` in
/// `validate_wiring_closure` and again in `authorize_release_closure` — read
/// this document.
fn release_manifest(component: &AdmittedComponent, wiring_hash: &str) -> serde_json::Value {
    let attachment_definition = serde_json::json!({
        "id": ATTACHMENT_ID,
        "kind": "http",
        "source-id": "public",
        "route": {
            "host": ROUTE_AUTHORITY,
            "path": ROUTE_PATH,
            "method": "POST"
        }
    });
    let attachment_definition_hash =
        wamn_execution_contract::canonical_json_sha256(&attachment_definition);
    let operations: BTreeMap<_, _> = component
        .operations
        .iter()
        .map(|(name, operation)| {
            (
                name.clone(),
                ServingComponentOperation {
                    pre_commit: operation.pre_commit.clone(),
                    committed_result_schema: None,
                    registered_operation: operation.registered_operation.clone(),
                    fresh_only: operation.fresh_only,
                    permissions: operation.registered_operation.iter().cloned().collect(),
                    participant: None,
                    statements: operation.statements.clone(),
                },
            )
        })
        .collect();
    serde_json::json!({
        "format-version": SERVING_MANIFEST_FORMAT_VERSION,
        "release": {
            "tenant-id": TENANT,
            "effective-release-id": EFFECTIVE_RELEASE_ID,
            "environment": ENVIRONMENT,
            "packages": [{
                "package-id": PACKAGE,
                "package-version": PACKAGE_VERSION,
            }],
        },
        "components": [{
            "package-id": PACKAGE,
            "component": component.component,
            "interface-version": component.interface_version,
            "digest": component.component_digest,
            "operations": operations,
        }],
        "routes": [],
        "attachments": {},
        "workflow": {
            "wirings": [{
                "package-id": PACKAGE,
                "wiring-id": WIRING_ID,
                "wiring-version": WIRING_VERSION,
                "graph-hash": wiring_hash,
            }],
            "attachments": {
                (ATTACHMENT_ID): {
                    "kind": "http",
                    "package-id": PACKAGE,
                    "wiring-id": WIRING_ID,
                    "wiring-version": WIRING_VERSION,
                    "definition-hash": attachment_definition_hash,
                    "definition": attachment_definition,
                    "auth-policy": {"modes": ["none"]}
                }
            },
        },
    })
}

/// `credential_headers` admits exactly `{"headers": {..}}` and nothing else.
fn credential_secret() -> String {
    serde_json::json!({"headers": {"authorization": "Bearer fixture-token"}}).to_string()
}

/// Publish the exact bytes the production puller will verify, in the layout
/// `wamn-ctl push-component` writes.
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
    wiring_hash: &str,
    release: &LoadedRelease,
    additional_wiring: Option<(&AdmittedComponent, &[WiringDocument])>,
) -> anyhow::Result<ClassCredentials> {
    let (client, connection) = tokio_postgres::connect(&options.database_url, NoTls)
        .await
        .context("connect the seeding session")?;
    let driver = tokio::spawn(connection);
    let seeded = seed_with_client(
        &client,
        options,
        component,
        document,
        wiring_hash,
        release,
        additional_wiring,
    )
    .await;
    drop(client);
    driver.abort();
    seeded
}

async fn seed_with_client(
    client: &tokio_postgres::Client,
    options: &RouteOptions,
    component: &AdmittedComponent,
    document: &WiringDocument,
    wiring_hash: &str,
    release: &LoadedRelease,
    additional_wiring: Option<(&AdmittedComponent, &[WiringDocument])>,
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
    let role_bootstrap = format!(
        "{} {}",
        wamn_control_provision::sql::ensure_app_acl_role_sql(),
        wamn_schema_control::ensure_scenario_author_role_sql(),
    );
    client
        .batch_execute(&format!(
            "{role_bootstrap}\n\
             DROP SCHEMA IF EXISTS catalog CASCADE;\n\
             DROP SCHEMA IF EXISTS app_system CASCADE;\n\
             DROP SCHEMA IF EXISTS wamn_run CASCADE;\n\
             {schema}\n\
             {app_schema}\n\
             {run_state}\n\
             {run_queue}"
        ))
        .await
        .context("install the catalog and run-plane DDL")?;
    client
        .batch_execute(&wamn_control_provision::platform_principals_sql(
            TENANT,
            "http-fixture.invalid",
        )?)
        .await?;

    let wiring_version = i32::try_from(WIRING_VERSION).expect("fixture wiring version fits");
    let graph_json = serde_json::to_string(document).context("serialize the wiring document")?;
    let imports = serde_json::to_string(&component.imports).context("serialize imports")?;
    let operations =
        serde_json::to_string(&component.operations).context("serialize operations")?;
    let effects = serde_json::to_string(&component.effects).context("serialize effects")?;
    // The whole portable record component admission mints, not just its
    // descriptor: `requirement_hash` is the SHA-256 of exactly these bytes.
    let requirement = ComponentConnectionRequirement::new(
        &component.component_digest,
        STORE_ALIAS,
        ConnectionTypeDescriptor::http_v1(),
    );
    let requirement_json = String::from_utf8(requirement.canonical_bytes())
        .context("portable requirement bytes are UTF-8")?;
    let requirement_hash = requirement.requirement_hash();
    let projection_hash = admitted_projection_hash(component, std::slice::from_ref(&requirement))
        .context("hash the complete admitted projection")?;
    // `require_direct_transport` demands an EXPLICIT null proxy-transport: an
    // absent key is a refusal, not a default.
    let definition_json = serde_json::json!({
        "primary-authority": options.upstream_base_url,
        "tls-verification": "disabled",
        "proxy-transport": serde_json::Value::Null,
    })
    .to_string();

    // Every catalog table FORCEs RLS on `app.tenant`, and a superuser is not
    // exempt from FORCE — the claim is what makes these writes land at all.
    client
        .query_one("SELECT set_config('app.tenant', $1, false)", &[&TENANT])
        .await
        .context("claim the seeding tenant")?;

    let package_manifest = serde_json::json!({
        "package": {"id": PACKAGE, "version": PACKAGE_VERSION},
        "models": {},
        "custom_operations": {},
        "queries": {},
        "connections": [],
        "components": {},
    });
    let package_manifest_sha256 = wamn_execution_contract::canonical_json_sha256(&package_manifest);
    client
        .execute(
            "INSERT INTO catalog.packages \
                    (tenant_id, package_id, package_version, manifest_sha256) \
             VALUES ($1, $2, $3, $4)",
            &[
                &TENANT,
                &PACKAGE,
                &PACKAGE_VERSION,
                &package_manifest_sha256,
            ],
        )
        .await
        .context("seed the exact package coordinate")?;
    client
        .execute(
            "INSERT INTO catalog.effective_releases \
                    (tenant_id, effective_release_id, environment, verified_publisher_principal) \
             VALUES ($1, $2, $3, 'trusted-http-route')",
            &[&TENANT, &EFFECTIVE_RELEASE_ID, &ENVIRONMENT],
        )
        .await
        .context("seed the effective release")?;
    client
        .execute(
            "INSERT INTO catalog.effective_release_packages \
                    (tenant_id, effective_release_id, package_id, package_version) \
             VALUES ($1, $2, $3, $4)",
            &[&TENANT, &EFFECTIVE_RELEASE_ID, &PACKAGE, &PACKAGE_VERSION],
        )
        .await
        .context("pin the package in the effective release")?;
    client
        .execute(
            "INSERT INTO catalog.wirings (tenant_id, package_id, package_version, wiring_id, \
                    version, graph_json, wiring_hash) \
             VALUES ($1, $2, $3, $4, $5, $6::text::jsonb, $7)",
            &[
                &TENANT,
                &PACKAGE,
                &PACKAGE_VERSION,
                &WIRING_ID,
                &wiring_version,
                &graph_json,
                &wiring_hash,
            ],
        )
        .await
        .context("seed the wiring version")?;
    client
        .execute(
            "INSERT INTO catalog.component_library (\
                 tenant_id, package_id, package_version, component, interface_version, operations, \
                 component_digest, projection_hash, imports, imports_fingerprint, effects\
             ) VALUES ($1, $2, $3, $4, $5, $6::text::jsonb, $7, $8, $9::text::jsonb, $10, \
                 $11::text::jsonb)",
            &[
                &TENANT,
                &PACKAGE,
                &PACKAGE_VERSION,
                &component.component,
                &component.interface_version,
                &operations,
                &component.component_digest,
                &projection_hash,
                &imports,
                &component.imports_fingerprint,
                &effects,
            ],
        )
        .await
        .context("seed the admitted component fact")?;

    client
        .execute(
            "INSERT INTO catalog.release_components (\
                 tenant_id, effective_release_id, wiring_package_id, \
                 wiring_package_version, wiring_id, wiring_version, node_id, package_id, \
                 package_version, component_digest\
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            &[
                &TENANT,
                &EFFECTIVE_RELEASE_ID,
                &PACKAGE,
                &PACKAGE_VERSION,
                &WIRING_ID,
                &wiring_version,
                &NODE_ID,
                &PACKAGE,
                &PACKAGE_VERSION,
                &component.component_digest,
            ],
        )
        .await
        .context("seed the released component membership")?;
    client
        .execute(
            "INSERT INTO catalog.connection_requirements (\
                 tenant_id, component_digest, store_alias, requirement_json, requirement_hash\
             ) VALUES ($1, $2, $3, $4::text::jsonb, $5)",
            &[
                &TENANT,
                &component.component_digest,
                &STORE_ALIAS,
                &requirement_json,
                &requirement_hash,
            ],
        )
        .await
        .context("seed the component-grain connection requirement")?;
    client
        .execute(
            "INSERT INTO catalog.connection_instances (\
                 tenant_id, environment, instance_id, requirement_type, contract, \
                 lifecycle_status\
             ) VALUES ($1, $2, $3, 'http', $4, 'enabled')",
            &[&TENANT, &ENVIRONMENT, &INSTANCE_ID, &CONTRACT],
        )
        .await
        .context("seed the connection instance")?;
    client
        .execute(
            "INSERT INTO catalog.connection_generations (\
                 tenant_id, environment, instance_id, generation, definition_json, \
                 definition_hash, credential_set_handle\
             ) VALUES ($1, $2, $3, $4, $5::text::jsonb, $6, $7)",
            &[
                &TENANT,
                &ENVIRONMENT,
                &INSTANCE_ID,
                &GENERATION,
                &definition_json,
                &format!("sha256:{}", "c".repeat(64)),
                &CREDENTIAL_HANDLE,
            ],
        )
        .await
        .context("seed the connection generation")?;
    // `catalog.guard_connection_instance_update` demands revision+1 and a
    // STRICTLY later `updated_at`, so the pointer cannot be moved by a bare
    // UPDATE — and `now()` is the transaction timestamp, which can tie.
    client
        .execute(
            "UPDATE catalog.connection_instances \
                SET active_generation = $4, revision = revision + 1, \
                    updated_at = clock_timestamp() + interval '1 second' \
              WHERE tenant_id = $1 AND environment = $2 AND instance_id = $3",
            &[&TENANT, &ENVIRONMENT, &INSTANCE_ID, &GENERATION],
        )
        .await
        .context("point the instance at its active generation")?;
    client
        .execute(
            "INSERT INTO catalog.connection_bindings (\
                 tenant_id, effective_release_id, component_digest, store_alias, \
                 environment, instance_id, binding_status, validation_status, validation_hash\
             ) VALUES ($1, $2, $3, $4, $5, $6, 'active', 'valid', $7)",
            &[
                &TENANT,
                &EFFECTIVE_RELEASE_ID,
                &component.component_digest,
                &STORE_ALIAS,
                &ENVIRONMENT,
                &INSTANCE_ID,
                &format!("sha256:{}", "e".repeat(64)),
            ],
        )
        .await
        .context("bind the requirement to the instance")?;

    if let Some((component, documents)) = additional_wiring {
        seed_additional_wirings(client, component, documents).await?;
    }
    // All component memberships and connection facts exist before the single
    // immutable snapshot seals this release. No post-seal membership writes.
    let canonical_release = release.manifest().canonical_bytes();
    let manifest_digest = release.release().manifest_digest.as_str();
    client
        .execute(
            "INSERT INTO catalog.release_manifest_v3_snapshots (\
                 tenant_id, effective_release_id, manifest_digest, canonical_bytes\
             ) VALUES ($1, $2, $3, $4)",
            &[
                &TENANT,
                &EFFECTIVE_RELEASE_ID,
                &manifest_digest,
                &canonical_release,
            ],
        )
        .await
        .context("seal the complete serving-manifest snapshot")?;
    client
        .execute(
            "INSERT INTO catalog.effective_release_heads \
                    (tenant_id, environment, effective_release_id) \
             VALUES ($1, $2, $3)",
            &[&TENANT, &ENVIRONMENT, &EFFECTIVE_RELEASE_ID],
        )
        .await
        .context("select the effective release")?;
    client
        .execute(
            flip_activation(),
            &[&PACKAGE, &ENVIRONMENT, &WIRING_ID, &wiring_hash, &true],
        )
        .await
        .context("activate the wiring")?;

    let config: tokio_postgres::Config = options
        .database_url
        .parse()
        .context("parse the disposable admin URL")?;
    let database = config
        .get_dbname()
        .context("the disposable admin URL names no database")?;
    let scope = WorkloadRoleScope::ProjectEnvironment {
        org: TENANT,
        project: PROJECT,
        environment: ENVIRONMENT,
        database,
    };
    let guest_role = workload_generation_role(
        WorkloadRoleFamily::App,
        WorkloadRoleScope::Tenant {
            tenant: TENANT,
            database,
        },
        CredentialGeneration::A,
    )?;
    let guest_sql = wamn_control_provision::sql::prepare_workload_generation_sql(
        WorkloadRoleFamily::App,
        database,
        &guest_role,
        GENERATION_PASSWORD,
        GENERATION_VALID_UNTIL,
    );
    let executor_role = workload_generation_role(
        WorkloadRoleFamily::ExecutorPlatform,
        scope,
        CredentialGeneration::A,
    )
    .context("derive the executor-platform generation")?;
    let http_role = workload_generation_role(
        WorkloadRoleFamily::HttpAdmitter,
        scope,
        CredentialGeneration::A,
    )
    .context("derive the callable-HTTP generation")?;
    let executor_sql = wamn_control_provision::sql::prepare_workload_generation_sql(
        WorkloadRoleFamily::ExecutorPlatform,
        database,
        &executor_role,
        GENERATION_PASSWORD,
        GENERATION_VALID_UNTIL,
    );
    let http_sql = wamn_control_provision::sql::prepare_workload_generation_sql(
        WorkloadRoleFamily::HttpAdmitter,
        database,
        &http_role,
        GENERATION_PASSWORD,
        GENERATION_VALID_UNTIL,
    );
    client
        .batch_execute(&format!("{guest_sql} {executor_sql} {http_sql}"))
        .await
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
    client: &tokio_postgres::Client,
    component: &AdmittedComponent,
    documents: &[WiringDocument],
) -> anyhow::Result<()> {
    let package = &component.scope.package_id;
    let package_version = &component.scope.package_version;
    if package != PACKAGE {
        let manifest = serde_json::json!({"package": {"id": package, "version": package_version},
            "models": {}, "custom_operations": {}, "queries": {}, "connections": [], "components": {}});
        client.execute(
            "INSERT INTO catalog.packages (tenant_id, package_id, package_version, manifest_sha256) \
             VALUES ($1, $2, $3, $4)",
            &[&TENANT, package, package_version,
              &wamn_execution_contract::canonical_json_sha256(&manifest)],
        ).await?;
        client.execute(
            "INSERT INTO catalog.effective_release_packages \
             (tenant_id, effective_release_id, package_id, package_version) VALUES ($1, $2, $3, $4)",
            &[&TENANT, &EFFECTIVE_RELEASE_ID, package, package_version],
        ).await?;
    }
    let projection_hash = admitted_projection_hash(component, &[])?;
    client.execute(
        "INSERT INTO catalog.component_library (tenant_id, package_id, package_version, \
             component, interface_version, operations, component_digest, projection_hash, \
             imports, imports_fingerprint, effects) \
         VALUES ($1, $2, $3, $4, $5, $6::text::jsonb, $7, $8, $9::text::jsonb, $10, $11::text::jsonb)",
        &[&TENANT, package, package_version, &component.component, &component.interface_version,
          &serde_json::to_string(&component.operations)?, &component.component_digest, &projection_hash,
          &serde_json::to_string(&component.imports)?, &component.imports_fingerprint,
          &serde_json::to_string(&component.effects)?],
    ).await.context("seed the additional admitted component before sealing")?;
    for document in documents {
        let version = i32::try_from(document.version)?;
        let wiring_hash = document.wiring_hash().as_str().to_owned();
        client
            .execute(
                "INSERT INTO catalog.wirings (tenant_id, package_id, package_version, wiring_id, \
             version, graph_json, wiring_hash) VALUES ($1, $2, $3, $4, $5, $6::text::jsonb, $7)",
                &[
                    &TENANT,
                    &PACKAGE,
                    &PACKAGE_VERSION,
                    &document.wiring_id,
                    &version,
                    &serde_json::to_string(document)?,
                    &wiring_hash,
                ],
            )
            .await
            .context("seed the additional immutable wiring")?;
        client
            .execute(
                "INSERT INTO catalog.release_components (tenant_id, effective_release_id, \
             wiring_package_id, wiring_package_version, wiring_id, wiring_version, node_id, \
             package_id, package_version, component_digest) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
                &[
                    &TENANT,
                    &EFFECTIVE_RELEASE_ID,
                    &PACKAGE,
                    &PACKAGE_VERSION,
                    &document.wiring_id,
                    &version,
                    &NODE_ID,
                    package,
                    package_version,
                    &component.component_digest,
                ],
            )
            .await
            .context("seed the additional component membership before sealing")?;
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
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::Context as _;

    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::mpsc;
    use tokio::task::{JoinHandle, JoinSet};
    use tokio_postgres::{Client, NoTls};

    use wamn_workflow::{
        CandidateCaseRequest, CandidateWiringTarget, RouterDelivery, RouterDriverRequest,
    };

    use wamn_runtime::plugins::wamn_credentials::WamnCredentials;

    use wamn_runtime::plugins::wamn_postgres::{CANDIDATE_WIRING_SQL, CandidateBindingWorld};

    use super::{
        CREDENTIAL_HANDLE, EFFECTIVE_RELEASE_ID, ENVIRONMENT, INSTANCE_ID, NODE_ID, PACKAGE,
        PROJECT, RouteOptions, TENANT, TrustedHttpRoute, WIRING_ID, WIRING_VERSION,
        build_with_credentials, credential_secret,
    };

    const ROTATED_HANDLE: &str = "upstream-v2";
    const ROTATED_AUTHORIZATION: &str = "Bearer rotated-fixture-token";

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
        let listener = TcpListener::bind("127.0.0.1:0").await?;
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

    fn request(id: u64) -> RouterDriverRequest {
        RouterDriverRequest {
            tenant_id: TENANT.to_owned(),
            package_id: PACKAGE.to_owned(),
            environment: ENVIRONMENT.to_owned(),
            wiring_id: WIRING_ID.to_owned(),
            wiring_version: WIRING_VERSION,
            delivery_id: format!("reuse-{id}"),
            payload: serde_json::json!({"id": id}),
            caller_attached: true,
            caller: None,
            traceparent: None,
            tracestate: None,
        }
    }

    fn candidate(
        route: &TrustedHttpRoute,
        binding_world: &Arc<CandidateBindingWorld>,
        id: u64,
    ) -> CandidateCaseRequest {
        let request = request(id);
        CandidateCaseRequest {
            target: CandidateWiringTarget {
                tenant_id: request.tenant_id,
                package_id: request.package_id,
                environment: request.environment,
                effective_release_id: EFFECTIVE_RELEASE_ID
                    .try_into()
                    .expect("positive fixture release"),
                wiring_id: request.wiring_id,
                wiring_version: request.wiring_version,
                wiring_hash: route.wiring_hash.clone(),
            },
            binding_world: Arc::clone(binding_world),
            delivery_id: request.delivery_id,
            payload: request.payload,
            traceparent: None,
            tracestate: None,
        }
    }

    async fn binding_world(
        client: &Client,
        route: &TrustedHttpRoute,
    ) -> anyhow::Result<Arc<CandidateBindingWorld>> {
        // Capture the same complete DB projection candidate admission reads,
        // not a hand-authored approximation of the frozen authority facts.
        let wiring_version = i32::try_from(WIRING_VERSION)?;
        let row = client
            .query_one(
                CANDIDATE_WIRING_SQL,
                &[
                    &TENANT,
                    &PACKAGE,
                    &ENVIRONMENT,
                    &WIRING_ID,
                    &wiring_version,
                    &EFFECTIVE_RELEASE_ID,
                    &route.wiring_hash,
                    &None::<String>,
                ],
            )
            .await?;
        let json: String = row.try_get(10)?;
        Ok(Arc::new(CandidateBindingWorld::from_json(
            serde_json::from_str(&json)?,
        )?))
    }

    async fn assert_observed_request(
        origin: &mut Origin,
        delivery: RouterDelivery,
        request_id: u64,
        authorization: &str,
    ) -> anyhow::Result<u64> {
        assert!(
            delivery.outcome.failure.is_none(),
            "real HTTP guest failed: {:?}",
            delivery.outcome.failure
        );
        assert_eq!(delivery.outcome.result["status"], 200);
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
        assert_eq!(delivery.outcome.result["body"]["request-id"], request_id);
        assert_eq!(
            delivery.outcome.result["body"]["connection-id"],
            observed_request.connection_id
        );
        Ok(observed_request.connection_id)
    }

    async fn set_instance_status(client: &Client, status: &str) -> anyhow::Result<()> {
        assert_eq!(
            client
                .execute(
                    "UPDATE catalog.connection_instances \
                        SET lifecycle_status = $4, revision = revision + 1 \
                      WHERE tenant_id = $1 AND environment = $2 AND instance_id = $3",
                    &[&TENANT, &ENVIRONMENT, &INSTANCE_ID, &status],
                )
                .await?,
            1
        );
        Ok(())
    }

    async fn rotate(client: &Client, generation: i64, handle: &str) -> anyhow::Result<()> {
        // Keep definition bytes and hash identical. The gen-2 check therefore
        // fails if the pool key omits the generation number.
        assert_eq!(
            client
                .execute(
                    "INSERT INTO catalog.connection_generations (tenant_id, environment, \
                 instance_id, generation, definition_json, definition_hash, credential_set_handle) \
             SELECT tenant_id, environment, instance_id, $4, definition_json, definition_hash, $5 \
               FROM catalog.connection_generations \
              WHERE tenant_id = $1 AND environment = $2 AND instance_id = $3 AND generation = 1",
                    &[&TENANT, &ENVIRONMENT, &INSTANCE_ID, &generation, &handle],
                )
                .await?,
            1
        );
        assert_eq!(
            client
                .execute(
                    "UPDATE catalog.connection_instances \
                SET active_generation = $4, revision = revision + 1 \
              WHERE tenant_id = $1 AND environment = $2 AND instance_id = $3",
                    &[&TENANT, &ENVIRONMENT, &INSTANCE_ID, &generation],
                )
                .await?,
            1
        );
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires: WAMN_HTTP_REUSE_ARTIFACT_BASE, WAMN_HTTP_REUSE_COMPONENT_WASM"]
    async fn real_http_guest_reuses_connections_without_reusing_authority() -> anyhow::Result<()> {
        wamn_test_postgres::require_prerequisites(&[
            "WAMN_HTTP_REUSE_ARTIFACT_BASE",
            "WAMN_HTTP_REUSE_COMPONENT_WASM",
        ]);
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
        let (admin, connection) = tokio_postgres::connect(&database_url, NoTls).await?;
        let connection = tokio::spawn(connection);
        let version: String = admin
            .query_one("SHOW server_version_num", &[])
            .await?
            .get(0);
        anyhow::ensure!(
            version.parse::<u32>()? / 10_000 == 18,
            "test requires PostgreSQL 18"
        );
        let mut origin = origin().await?;
        let credentials = WamnCredentials::from_projects(HashMap::from([(
            PROJECT.to_owned(),
            HashMap::from([
                (CREDENTIAL_HANDLE.to_owned(), credential_secret()),
                (
                    ROTATED_HANDLE.to_owned(),
                    serde_json::json!({
                        "headers": {"authorization": ROTATED_AUTHORIZATION},
                    })
                    .to_string(),
                ),
            ]),
        )]));
        let route = build_with_credentials(
            &RouteOptions {
                database_url: database_url.clone(),
                artifact_base,
                component_wasm: component_wasm.into(),
                upstream_base_url: format!("http://{}", origin.address),
                path_and_query: "/reuse".to_owned(),
            },
            credentials,
        )
        .await?;
        // RouterDriver instantiates and drops a distinct Store for each call.
        // The peer identity comes from accept(), not a transport cache counter.
        let first = assert_observed_request(
            &mut origin,
            route.driver.execute(request(1)).await?,
            1,
            "Bearer fixture-token",
        )
        .await?;
        let second = assert_observed_request(
            &mut origin,
            route.driver.execute(request(2)).await?,
            2,
            "Bearer fixture-token",
        )
        .await?;
        assert_eq!(
            first, second,
            "fresh guest stores did not reuse the accepted connection"
        );

        set_instance_status(&admin, "disabled").await?;
        let denied = route.driver.execute(request(3)).await?;
        let failure = denied
            .outcome
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
        set_instance_status(&admin, "enabled").await?;

        let frozen = binding_world(&admin, &route).await?;
        let candidate_first = assert_observed_request(
            &mut origin,
            route
                .driver
                .execute_candidate(candidate(&route, &frozen, 4))
                .await?,
            4,
            "Bearer fixture-token",
        )
        .await?;
        let candidate_second = assert_observed_request(
            &mut origin,
            route
                .driver
                .execute_candidate(candidate(&route, &frozen, 5))
                .await?,
            5,
            "Bearer fixture-token",
        )
        .await?;
        assert_eq!(
            candidate_first, candidate_second,
            "fresh candidate stores did not reuse transport"
        );

        rotate(&admin, 2, CREDENTIAL_HANDLE).await?;
        let pinned_after_switch = assert_observed_request(
            &mut origin,
            route
                .driver
                .execute_candidate(candidate(&route, &frozen, 6))
                .await?,
            6,
            "Bearer fixture-token",
        )
        .await?;
        assert_eq!(
            pinned_after_switch, candidate_first,
            "an admitted candidate must retain its generation after activation"
        );

        let generation_two = assert_observed_request(
            &mut origin,
            route.driver.execute(request(7)).await?,
            7,
            "Bearer fixture-token",
        )
        .await?;
        assert_ne!(
            generation_two, first,
            "new generation reused the old generation socket"
        );
        assert_ne!(
            generation_two, candidate_first,
            "new generation reused a candidate's old socket"
        );
        let frozen = binding_world(&admin, &route).await?;
        let current = assert_observed_request(
            &mut origin,
            route
                .driver
                .execute_candidate(candidate(&route, &frozen, 8))
                .await?,
            8,
            "Bearer fixture-token",
        )
        .await?;
        let current_again = assert_observed_request(
            &mut origin,
            route
                .driver
                .execute_candidate(candidate(&route, &frozen, 9))
                .await?,
            9,
            "Bearer fixture-token",
        )
        .await?;
        assert_eq!(
            current, current_again,
            "current candidate world did not reuse its generation"
        );

        rotate(&admin, 3, ROTATED_HANDLE).await?;
        let generation_three = assert_observed_request(
            &mut origin,
            route.driver.execute(request(10)).await?,
            10,
            ROTATED_AUTHORIZATION,
        )
        .await?;
        assert_ne!(
            generation_three, generation_two,
            "rotated credential reused the previous socket"
        );
        assert_ne!(
            generation_three, current,
            "rotated credential reused the candidate socket"
        );
        let generation_three_again = assert_observed_request(
            &mut origin,
            route.driver.execute(request(11)).await?,
            11,
            ROTATED_AUTHORIZATION,
        )
        .await?;
        assert_eq!(
            generation_three, generation_three_again,
            "rotated generation did not become reusable"
        );
        // A missing pinned credential cannot fall forward to a usable generation.
        rotate(&admin, 4, "missing-fixture-credential").await?;
        let missing = binding_world(&admin, &route).await?;
        rotate(&admin, 5, ROTATED_HANDLE).await?;
        let refused = route
            .driver
            .execute_candidate(candidate(&route, &missing, 12))
            .await?;
        let failure = refused
            .outcome
            .failure
            .context("missing pinned credential was substituted")?;
        assert_eq!(
            failure.detail.code.as_deref(),
            Some("connection-unavailable")
        );
        assert!(
            origin.requests.try_recv().is_err(),
            "missing credential reached the wire"
        );
        assert_observed_request(
            &mut origin,
            route.driver.execute(request(13)).await?,
            13,
            ROTATED_AUTHORIZATION,
        )
        .await?;

        assert!(
            origin.requests.try_recv().is_err(),
            "unexpected extra upstream effect"
        );
        drop(admin);
        connection.abort();
        Ok(())
    }
}
