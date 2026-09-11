//! One router-invoked `wamn:node` guest performing a real trusted HTTP effect.
//!
//! Shared proof support: it seeds exactly the facts
//! `ConnectionHttp::send` reads — a component-grain
//! `catalog.connection_requirements` row, an `active`/`valid`
//! `catalog.connection_bindings` row over an `enabled`
//! `catalog.connection_instances` whose `active_generation` matches the
//! `catalog.connection_generations` row carrying the credential handle — and
//! builds the production `RouterDriver` over them.
//!
//! Two beads need this same closure. `wamn-0h0g.11.8` drives it to witness
//! trace propagation at the wire; `wamn-0h0g.11.3` needs it to prove HTTP
//! connection confinement refusals. The test module also proves native socket
//! reuse across real guest stores, with live lifecycle and generation changes.
//!
//! It needs three throwaway resources, all named by the caller: a superuser
//! PostgreSQL database (the `catalog` schema is DROPped and reinstalled), an
//! insecure OCI registry, and an upstream HTTP origin. Nothing here is stubbed —
//! the wiring resolves through `ACTIVE_WIRING_SQL`, the component bytes come
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
use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, workload_generation_role,
};
use wamn_ctl::push_component::admitted_projection_hash;
use wamn_execution_host::{RouterDriver, RouterDriverConfig, WiringCacheCapacity};
use wamn_run_state::AuthorityClass;
use wamn_runtime::component_admission::{ComponentAdmissionRequest, validate_component_admission};
use wamn_runtime::component_artifact::{
    component_artifact_config_bytes, component_artifact_layout, component_artifact_reference,
};
use wamn_runtime::component_artifact_source::{
    ComponentArtifactSource, ComponentArtifactSourceConfig,
};
use wamn_runtime::engine::build_engine;
use wamn_runtime::plugins::connection_http::transport::HttpTransport;
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_logging::{WamnLogging, WamnLoggingConfig};
use wamn_runtime::plugins::wamn_postgres::{ClassCredentials, WamnPostgres, WamnPostgresConfig};
use wamn_runtime::release_manifest::ReleaseManifestWeld;
use wamn_schema_control::connections::ComponentConnectionRequirement;

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
pub const OPERATION: &str = "wamn:node/handler@0.1.0";
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
    /// is dropped and reinstalled from `deploy/sql/catalog-schema.sql`.
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

/// One live driver over the seeded closure, plus the identities a proof needs
/// to address it.
pub struct TrustedHttpRoute {
    pub driver: Arc<RouterDriver>,
    /// The same welded release the driver authorizes. Callers that exercise a
    /// release-owned ingress plugin must share this exact weld rather than mint
    /// a second view of the closure.
    pub(crate) release: Arc<ReleaseManifestWeld>,
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
        ReleaseManifestWeld::load_canonical_bytes(
            &wamn_execution_contract::canonical_json_bytes(&release_manifest(
                &admitted,
                &wiring_hash,
            )),
            "trusted-http-route fixture",
        )
        .context("weld the fixture serving manifest")?,
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
    );

    let driver = Arc::new(
        RouterDriver::new(
            engine,
            postgres,
            Arc::new(HttpTransport::new().context("build the process HTTP transport")?),
            Arc::new(credentials),
            Arc::new(WamnLogging::new(WamnLoggingConfig::default()).context("build wamn:logging")?),
            // The upstream is a loopback origin the test owns; the cluster
            // ceiling is Kubernetes' job, not this fixture's.
            Arc::from(vec!["*".parse().context("parse the allowed-host policy")?]),
            Arc::clone(&release),
            source,
            RouterDriverConfig {
                owner_prefix: "trusted-http-route".to_owned(),
                project: PROJECT.to_owned(),
                schema: None,
                cache_capacity: WiringCacheCapacity::default(),
            },
        )
        .context("build the router driver")?,
    );

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

/// The one manifest the weld carries. Both membership checks the released
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
                    committed_result_schema: None,
                    registered_operation: operation.registered_operation.clone(),
                    fresh_only: operation.fresh_only,
                    dependencies: operation.dependencies.clone(),
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
        "registrations": {},
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
    release: &ReleaseManifestWeld,
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
    release: &ReleaseManifestWeld,
    additional_wiring: Option<(&AdmittedComponent, &[WiringDocument])>,
) -> anyhow::Result<ClassCredentials> {
    // `catalog-schema.sql` applies whole only on a fresh install, and its
    // migration blocks take an ACCESS EXCLUSIVE lock — so it must arrive as ONE
    // implicit transaction, which `batch_execute` gives it and psql without
    // `--single-transaction` does not.
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let schema = std::fs::read_to_string(format!("{root}/deploy/sql/catalog-schema.sql"))
        .context("read the catalog DDL")?;
    let app_schema = std::fs::read_to_string(format!("{root}/deploy/sql/app-schema.sql"))
        .context("read the application authorization DDL")?;
    let run_state = std::fs::read_to_string(format!("{root}/deploy/sql/run-state.sql"))
        .context("read the run-state DDL")?;
    let run_queue = std::fs::read_to_string(format!("{root}/deploy/sql/run-queue.sql"))
        .context("read the run-queue DDL")?;
    let role_bootstrap = format!(
        "{} {} {}",
        wamn_control_provision::sql::ensure_app_acl_role_sql(),
        wamn_schema_control::ensure_scenario_author_role_sql(),
        wamn_control_provision::sql::ensure_effect_writer_acl_role_sql(),
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
        "connections": {},
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
        .batch_execute(&format!("{executor_sql} {http_sql}"))
        .await
        .context("mint the production platform credential generations")?;

    Ok(ClassCredentials::default()
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
            "models": {}, "custom_operations": {}, "queries": {}, "connections": {}, "components": {}});
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
    use std::collections::{BTreeMap, BTreeSet, HashMap};
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::Context as _;
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_sdk::trace::{InMemorySpanExporterBuilder, SdkTracerProvider};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::mpsc;
    use tokio::task::{JoinHandle, JoinSet};
    use tokio_postgres::{Client, NoTls};
    use tracing_subscriber::layer::SubscriberExt as _;
    use wamn_catalog::{ComponentDeclaration, ComponentOperationDependency};
    use wamn_control_provision::{
        CredentialGeneration, SystemReader, WorkloadRoleFamily, system_reader_generation_role,
    };
    use wamn_execution_host::{
        CandidateCaseRequest, CandidateExecutionRefusal, CandidateExecutionRefusalKind,
        CandidateWiringTarget, RouterDelivery, RouterDriver, RouterDriverConfig,
        RouterDriverRequest, WiringCacheCapacity, WiringResolution,
    };
    use wamn_platform_identity::{
        assign_project_role, create_service, issue_pat, route_caller_subject,
    };
    use wamn_runtime::component_admission::{
        ComponentAdmissionRequest, validate_component_admission,
    };
    use wamn_runtime::component_artifact_source::{
        ComponentArtifactSource, ComponentArtifactSourceConfig,
    };
    use wamn_runtime::engine::build_engine;
    use wamn_runtime::plugins::connection_http::transport::HttpTransport;
    use wamn_runtime::plugins::flow_http_routing::{
        AuthenticatedCaller, FlowHttpRouting, RouteAuthentication, RouteInFlightLimit,
    };
    use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
    use wamn_runtime::plugins::wamn_logging::{WamnLogging, WamnLoggingConfig};
    use wamn_runtime::plugins::wamn_postgres::{
        CANDIDATE_WIRING_SQL, CandidateBindingWorld, WamnPostgres, WamnPostgresConfig,
    };
    use wamn_runtime::release_manifest::ReleaseManifestWeld;
    use wasm_encoder::reencode::{Reencode, ReencodeComponent};

    use super::{
        ATTACHMENT_ID, CREDENTIAL_HANDLE, EFFECTIVE_RELEASE_ID, ENVIRONMENT, GENERATION_PASSWORD,
        GENERATION_VALID_UNTIL, INSTANCE_ID, NODE_ID, OPERATION, PACKAGE, PACKAGE_VERSION, PROJECT,
        REGISTRY_IO_TIMEOUT, RouteOptions, TENANT, TrustedHttpRoute, WIRING_ID, WIRING_VERSION,
        build_with_credentials, credential_secret, declaration, generation_url, publish_component,
        release_manifest, seed_catalog, wiring_document,
    };

    const ROTATED_HANDLE: &str = "upstream-v2";
    const ROTATED_AUTHORIZATION: &str = "Bearer rotated-fixture-token";
    const CHILD_OPERATION: &str = "orders:http/send@1.0.0";
    const PARENT_COMPONENT: &str = "nested-http-parent";
    const PARENT_PACKAGE: &str = "http_adapter";
    const UNDECLARED_OPERATION: &str = "http-adapter:parent/undeclared@1.0.0";

    #[derive(Default)]
    struct RegisteredExport {
        depth: usize,
        renamed: usize,
    }

    impl Reencode for RegisteredExport {
        type Error = std::convert::Infallible;
    }

    impl ReencodeComponent for RegisteredExport {
        fn push_depth(&mut self) {
            self.depth += 1;
        }

        fn pop_depth(&mut self) {
            self.depth -= 1;
        }

        fn parse_component_export(
            &mut self,
            exports: &mut wasm_encoder::ComponentExportSection,
            mut export: wasmparser::ComponentExport<'_>,
        ) -> Result<(), wasm_encoder::reencode::Error<Self::Error>> {
            if self.depth == 0 && export.name.name == OPERATION {
                export.name.name = CHILD_OPERATION;
                self.renamed += 1;
            }
            wasm_encoder::reencode::component_utils::parse_component_export(self, exports, export)
        }
    }

    fn registered_http_guest(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
        // Only the top-level export gets a canonical application name. The
        // actual HTTP guest code and imports remain unchanged; normal byte
        // admission below owns the new digest and registered operation.
        let mut adapter = RegisteredExport::default();
        let mut component = wasm_encoder::Component::new();
        adapter.parse_component(&mut component, wasmparser::Parser::new(0), bytes)?;
        anyhow::ensure!(
            adapter.renamed == 1,
            "expected one actual HTTP handler export"
        );
        Ok(component.finish())
    }

    fn parent_component() -> anyhow::Result<Vec<u8>> {
        // The real node ABI exceeds 16 flat parameters, so canon lower receives
        // one pointer to (context, input). Forward that aggregate unchanged.
        Ok(wat::parse_str(format!(
            r#"(component
          (import "wamn:node/types@0.1.0" (instance $node
            (type $json' string)
            (export "json" (type $json (eq $json')))
            (type $context' (record
              (field "wiring-id" string) (field "wiring-version" u32)
              (field "node-id" string) (field "delivery-id" string)
              (field "input-port" (option string)) (field "occurrence" u32)
              (field "traceparent" (option string)) (field "tracestate" (option string))
              (field "deadline-ms" (option u64)) (field "config" $json)))
            (export "node-context" (type $context (eq $context')))
            (type $detail' (record (field "message" string) (field "code" (option string))))
            (export "error-detail" (type $detail (eq $detail')))
            (type $rate' (record (field "detail" $detail) (field "retry-after-ms" (option u64))))
            (export "rate-limit-detail" (type $rate (eq $rate')))
            (type $error' (variant (case "retryable" $detail) (case "rate-limited" $rate)
              (case "terminal" $detail) (case "invalid-input" $detail) (case "cancelled")))
            (export "node-error" (type $error (eq $error')))
            (type $emission' (record (field "payload" $json) (field "port" (option string))))
            (export "emission" (type $emission (eq $emission')))))
          (alias export $node "json" (type $json))
          (alias export $node "node-context" (type $context))
          (alias export $node "node-error" (type $error))
          (alias export $node "emission" (type $emission))
          (import "{CHILD_OPERATION}" (instance $child
            (export "json" (type (eq $json)))
            (export "node-context" (type (eq $context)))
            (export "emission" (type (eq $emission)))
            (export "node-error" (type (eq $error)))
            (export "run" (func (param "ctx" $context) (param "input" $json)
              (result (result $emission (error $error)))))))
          (core module $memory
            (memory (export "memory") 16)
            (global $next (mut i32) (i32.const 1024))
            (func (export "realloc") (param $old i32) (param $old-size i32)
              (param $align i32) (param $size i32) (result i32) (local $new i32)
              global.get $next local.get $align i32.const 1 i32.sub i32.add
              i32.const 0 local.get $align i32.sub i32.and local.tee $new
              local.get $size i32.add global.set $next
              global.get $next i32.const 1048576 i32.gt_u if unreachable end
              local.get $old if
                local.get $new local.get $old local.get $old-size memory.copy
              end local.get $new))
          (core instance $memory (instantiate $memory))
          (core func $nested (canon lower (func $child "run")
            (memory $memory "memory") (realloc (func $memory "realloc"))))
          (core module $main
            (import "memory" "memory" (memory 16))
            (import "host" "nested" (func $nested (param i32 i32)))
            (func (export "run") (param $input i32) (result i32)
              local.get $input i32.const 832 call $nested i32.const 832))
          (core instance $main (instantiate $main (with "memory" (instance $memory))
            (with "host" (instance (export "nested" (func $nested))))))
          (func $run (param "ctx" $context) (param "input" $json)
            (result (result $emission (error $error)))
            (canon lift (core func $main "run") (memory $memory "memory")
              (realloc (func $memory "realloc"))))
          (instance $handler
            (export "json" (type $json)) (export "node-context" (type $context))
            (export "emission" (type $emission)) (export "node-error" (type $error))
            (export "run" (func $run)))
          (export "{OPERATION}" (instance $handler))
          (export "{UNDECLARED_OPERATION}" (instance $handler)))"#,
        ))?)
    }

    async fn nested_route(
        options: &RouteOptions,
    ) -> anyhow::Result<(TrustedHttpRoute, Arc<WamnPostgres>, String)> {
        let engine = Arc::new(build_engine(&[])?);
        let bytes = registered_http_guest(&std::fs::read(&options.component_wasm)?)?;
        let mut child_declaration = declaration()?;
        let mut operation = child_declaration
            .operations
            .remove(OPERATION)
            .context("actual HTTP declaration lacks its handler")?;
        operation.registered_operation = Some(CHILD_OPERATION.to_owned());
        child_declaration
            .operations
            .insert(CHILD_OPERATION.to_owned(), operation.clone());
        let child = validate_component_admission(
            &engine,
            &bytes,
            ComponentAdmissionRequest {
                declaration: child_declaration.clone(),
                admitted_platform_packages: BTreeSet::from([
                    "wamn:node".to_owned(),
                    "wamn:connection".to_owned(),
                ]),
                effect_free_operation_dependencies: BTreeSet::new(),
            },
        )?
        .component;
        publish_component(&options.artifact_base, &child, &bytes).await?;

        operation.registered_operation = None;
        operation.dependencies = vec![ComponentOperationDependency {
            package: PACKAGE.to_owned(),
            version: PACKAGE_VERSION.to_owned(),
            digest: child.component_digest.clone(),
            operation: CHILD_OPERATION.to_owned(),
        }];
        let mut undeclared_operation = operation.clone();
        undeclared_operation.dependencies.clear();
        let mut parent_scope = child_declaration.scope.clone();
        parent_scope.package_id = PARENT_PACKAGE.to_owned();
        let parent_bytes = parent_component()?;
        let parent = validate_component_admission(
            &engine,
            &parent_bytes,
            ComponentAdmissionRequest {
                declaration: ComponentDeclaration {
                    scope: parent_scope,
                    component: PARENT_COMPONENT.to_owned(),
                    connections: Vec::new(),
                    operations: BTreeMap::from([
                        (OPERATION.to_owned(), operation),
                        (UNDECLARED_OPERATION.to_owned(), undeclared_operation),
                    ]),
                    ..child_declaration
                },
                admitted_platform_packages: BTreeSet::from(["wamn:node".to_owned()]),
                effect_free_operation_dependencies: BTreeSet::new(),
            },
        )?
        .component;
        publish_component(&options.artifact_base, &parent, &parent_bytes).await?;

        let mut direct = wiring_document(&options.path_and_query);
        direct
            .nodes
            .get_mut(NODE_ID)
            .context("fixture node missing")?
            .operation = CHILD_OPERATION.to_owned();
        let direct_hash = direct.wiring_hash().as_str().to_owned();
        let mut nested = direct.clone();
        nested.version = 2;
        let node = nested
            .nodes
            .get_mut(NODE_ID)
            .context("fixture node missing")?;
        node.component = PARENT_COMPONENT.to_owned();
        node.operation = OPERATION.to_owned();
        let nested_hash = nested.wiring_hash().as_str().to_owned();
        let mut undeclared = nested.clone();
        undeclared.version = 3;
        undeclared
            .nodes
            .get_mut(NODE_ID)
            .context("fixture node missing")?
            .operation = UNDECLARED_OPERATION.to_owned();
        let mut manifest = release_manifest(&child, &direct_hash);
        manifest["release"]["packages"].as_array_mut().context("fixture packages missing")?
            .push(serde_json::json!({"package-id": PARENT_PACKAGE, "package-version": PACKAGE_VERSION}));
        let mut parent_manifest = release_manifest(&parent, &nested_hash)["components"][0].clone();
        parent_manifest["package-id"] = serde_json::json!(PARENT_PACKAGE);
        manifest["components"]
            .as_array_mut()
            .context("fixture components missing")?
            .push(parent_manifest);
        for document in [&nested, &undeclared] {
            manifest["wirings"]
                .as_array_mut()
                .context("fixture wirings missing")?
                .push(serde_json::json!({
                    "package-id": PACKAGE, "wiring-id": WIRING_ID,
                    "wiring-version": document.version,
                    "graph-hash": document.wiring_hash().as_str(),
                }));
        }
        manifest["attachments"][ATTACHMENT_ID]["auth-policy"] =
            serde_json::json!({"modes": ["pat"]});
        let manifest: wamn_catalog::ServingManifest = serde_json::from_value(manifest)?;
        let release = Arc::new(ReleaseManifestWeld::load_canonical_bytes(
            &manifest.canonical_bytes(),
            "nested HTTP authority fixture",
        )?);
        let credentials = seed_catalog(
            options,
            &child,
            &direct,
            &direct_hash,
            &release,
            Some((&parent, &[nested, undeclared])),
        )
        .await?;
        let (admin, connection) = tokio_postgres::connect(&options.database_url, NoTls).await?;
        let connection = tokio::spawn(connection);
        admin.execute("INSERT INTO app_system.roles (tenant_id, name, is_system) VALUES ($1, 'route-caller', false)", &[&TENANT]).await?;
        admin.execute("INSERT INTO app_system.permissions (tenant_id, role_name, permission) VALUES ($1, 'route-caller', $2)", &[&TENANT, &CHILD_OPERATION]).await?;
        drop(admin);
        connection.abort();

        let postgres = Arc::new(WamnPostgres::new(WamnPostgresConfig {
            credentials: Some(credentials),
            guest_pool_max_size: 4,
            platform_pool_max_size: 4,
            wait_timeout_ms: 5_000,
            statement_timeout_ms: 10_000,
            row_limit: 10_000,
        })?);
        let driver = Arc::new(RouterDriver::new(
            engine,
            Arc::clone(&postgres),
            Arc::new(HttpTransport::new()?),
            Arc::new(WamnCredentials::from_projects(HashMap::from([(
                PROJECT.to_owned(),
                HashMap::from([(CREDENTIAL_HANDLE.to_owned(), credential_secret())]),
            )]))),
            Arc::new(WamnLogging::new(WamnLoggingConfig::default())?),
            Arc::from(vec!["*".parse()?]),
            Arc::clone(&release),
            ComponentArtifactSource::new(ComponentArtifactSourceConfig::new(
                &options.artifact_base,
                true,
                REGISTRY_IO_TIMEOUT,
            )?),
            RouterDriverConfig {
                owner_prefix: "nested-http-authority".to_owned(),
                project: PROJECT.to_owned(),
                schema: None,
                cache_capacity: WiringCacheCapacity::default(),
            },
        )?);
        Ok((
            TrustedHttpRoute {
                driver,
                release,
                component_digest: child.component_digest,
                wiring_hash: direct_hash,
            },
            postgres,
            parent.component_digest,
        ))
    }

    async fn originating_caller(
        system_url: &str,
        route: &TrustedHttpRoute,
        postgres: Arc<WamnPostgres>,
    ) -> anyhow::Result<AuthenticatedCaller> {
        let config: tokio_postgres::Config = system_url.parse()?;
        anyhow::ensure!(
            config.get_dbname() == Some("wamnsystem"),
            "nested proof needs separate /wamnsystem"
        );
        let (system, task) = tokio_postgres::connect(system_url, NoTls).await?;
        let task = tokio::spawn(task);
        let version: String = system
            .query_one("SHOW server_version_num", &[])
            .await?
            .get(0);
        anyhow::ensure!(
            version.parse::<u32>()? / 10_000 == 18,
            "nested identity proof requires PostgreSQL 18"
        );
        wamn_ctl::dev::environment::reset_control_store(&system).await?;
        system.execute("INSERT INTO registry.orgs (id, placement_kind, pool_cluster) VALUES ($1, 'pooled', 'http-reuse-proof')", &[&TENANT]).await?;
        system
            .execute(
                "INSERT INTO registry.projects (org, id) VALUES ($1, $2)",
                &[&TENANT, &PROJECT],
            )
            .await?;
        let subject = route_caller_subject(TENANT, PROJECT, ENVIRONMENT)?;
        let principal = create_service(&system, &subject, "nested HTTP originating caller").await?;
        assign_project_role(&system, principal.id(), TENANT, PROJECT, "route-caller").await?;
        let pat = issue_pat(
            &system,
            principal.id(),
            "nested proof",
            Duration::from_secs(600),
        )
        .await?;
        let role = system_reader_generation_role(
            SystemReader::Identity,
            TENANT,
            PROJECT,
            ENVIRONMENT,
            "wamnsystem",
            CredentialGeneration::A,
        );
        system
            .batch_execute(
                &wamn_control_provision::sql::prepare_workload_generation_sql(
                    WorkloadRoleFamily::IdentityReader,
                    "wamnsystem",
                    &role,
                    GENERATION_PASSWORD,
                    GENERATION_VALID_UNTIL,
                ),
            )
            .await?;
        let (reader, reader_task) =
            tokio_postgres::connect(&generation_url(system_url, &role)?, NoTls).await?;
        let reader_task = tokio::spawn(reader_task);
        let routing = FlowHttpRouting::new(
            Some(Arc::clone(&route.release)),
            RouteInFlightLimit::default(),
        )
        .with_authentication(Arc::new(
            RouteAuthentication::new(Arc::new(reader), postgres, TENANT, PROJECT, subject).await?,
        ));
        let caller = routing
            .authenticate_authorization_for_test(
                ATTACHMENT_ID,
                Some(&format!("Bearer {}", pat.token())),
            )
            .await
            .map_err(|(status, code)| {
                anyhow::anyhow!("nested caller authentication refused: {status} {code}")
            })?
            .context("PAT route returned no originating caller")?;
        assert_eq!(caller.principal_id(), principal.id().as_str());
        assert!(
            caller.permits(CHILD_OPERATION),
            "caller lacks the exact child operation grant"
        );
        drop(routing);
        reader_task.abort();
        drop(system);
        task.abort();
        Ok(caller)
    }

    struct Receipt {
        connection_id: u64,
        request_id: u64,
        authorization: String,
        idempotency_key: String,
    }

    struct Origin {
        address: SocketAddr,
        receipts: mpsc::Receiver<Receipt>,
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
        let (sent, receipts) = mpsc::channel(16);
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
            receipts,
            task,
        })
    }

    async fn serve_socket(
        mut socket: TcpStream,
        connection_id: u64,
        sent: mpsc::Sender<Receipt>,
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
            let receipt = Receipt {
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
            // Emit before the response: a completed driver call cannot race an
            // unobserved receipt, including the denial checks below.
            sent.send(receipt)
                .await
                .map_err(|_| anyhow::anyhow!("receipt receiver closed"))?;
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
            resolution: WiringResolution::Active,
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
                ],
            )
            .await?;
        let json: String = row.try_get(10)?;
        Ok(Arc::new(CandidateBindingWorld::from_json(
            serde_json::from_str(&json)?,
        )?))
    }

    async fn receipt(
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
        let receipt = origin
            .receipts
            .recv()
            .await
            .context("upstream origin stopped")?;
        assert_eq!(receipt.request_id, request_id);
        assert!(
            receipt.authorization == authorization,
            "wrong generation credential reached the wire"
        );
        assert_eq!(
            receipt.idempotency_key,
            format!("reuse-{request_id}:{NODE_ID}:0")
        );
        assert_eq!(delivery.outcome.result["body"]["request-id"], request_id);
        assert_eq!(
            delivery.outcome.result["body"]["connection-id"],
            receipt.connection_id
        );
        Ok(receipt.connection_id)
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
    #[ignore = "requires fresh PostgreSQL, throwaway OCI registry, and actual http-request guest"]
    async fn real_http_guest_reuses_connections_without_reusing_authority() -> anyhow::Result<()> {
        anyhow::ensure!(
            std::env::var("WAMN_HTTP_REUSE_ALLOW_SCHEMA_RESET").as_deref() == Ok("1"),
            "set WAMN_HTTP_REUSE_ALLOW_SCHEMA_RESET=1 for the disposable database"
        );
        tokio::time::timeout(Duration::from_secs(180), pooling_proof())
            .await
            .context("real HTTP guest pooling proof exceeded 180 seconds")?
    }

    async fn pooling_proof() -> anyhow::Result<()> {
        let database_url = std::env::var("WAMN_HTTP_REUSE_PG_URL")
            .context("set WAMN_HTTP_REUSE_PG_URL to fresh disposable PostgreSQL 18")?;
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
            "proof requires PostgreSQL 18"
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
        let first = receipt(
            &mut origin,
            route.driver.execute(request(1)).await?,
            1,
            "Bearer fixture-token",
        )
        .await?;
        let second = receipt(
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
            origin.receipts.try_recv().is_err(),
            "denied effect reached the warm socket"
        );
        set_instance_status(&admin, "enabled").await?;

        let frozen = binding_world(&admin, &route).await?;
        let candidate_first = receipt(
            &mut origin,
            route
                .driver
                .execute_candidate(candidate(&route, &frozen, 4))
                .await?,
            4,
            "Bearer fixture-token",
        )
        .await?;
        let candidate_second = receipt(
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
        let Err(drift) = route
            .driver
            .execute_candidate(candidate(&route, &frozen, 6))
            .await
        else {
            anyhow::bail!("candidate reused stale frozen binding authority");
        };
        let refusal = drift
            .downcast_ref::<CandidateExecutionRefusal>()
            .context("candidate failed without the typed authority refusal")?;
        assert_eq!(refusal.kind(), CandidateExecutionRefusalKind::Binding);
        assert_eq!(refusal.refusal(), "candidate-binding-world-drift");
        assert!(
            origin.receipts.try_recv().is_err(),
            "drifted candidate reached the wire"
        );

        let generation_two = receipt(
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
        let current = receipt(
            &mut origin,
            route
                .driver
                .execute_candidate(candidate(&route, &frozen, 8))
                .await?,
            8,
            "Bearer fixture-token",
        )
        .await?;
        let current_again = receipt(
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
        let generation_three = receipt(
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
        let generation_three_again = receipt(
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
        assert!(
            origin.receipts.try_recv().is_err(),
            "unexpected extra upstream effect"
        );
        drop(admin);
        connection.abort();
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires fresh project PostgreSQL, separate fresh wamnsystem, OCI registry, and actual HTTP guest"]
    async fn nested_http_authorizes_child_and_preserves_original_caller() -> anyhow::Result<()> {
        anyhow::ensure!(
            std::env::var("WAMN_HTTP_REUSE_ALLOW_SCHEMA_RESET").as_deref() == Ok("1"),
            "set WAMN_HTTP_REUSE_ALLOW_SCHEMA_RESET=1 for both disposable databases"
        );
        tokio::time::timeout(Duration::from_secs(180), nested_authority_proof())
            .await
            .context("real nested HTTP authority proof exceeded 180 seconds")?
    }

    async fn nested_authority_proof() -> anyhow::Result<()> {
        let database_url = std::env::var("WAMN_HTTP_REUSE_PG_URL")
            .context("set WAMN_HTTP_REUSE_PG_URL to fresh disposable PostgreSQL 18")?;
        let system_url = std::env::var("WAMN_HTTP_REUSE_SYSTEM_PG_URL")
            .context("set WAMN_HTTP_REUSE_SYSTEM_PG_URL to a separate fresh /wamnsystem")?;
        let project_config: tokio_postgres::Config = database_url.parse()?;
        anyhow::ensure!(
            project_config
                .get_dbname()
                .is_some_and(|name| name != "wamnsystem"),
            "project proof must not install tenant schemas into wamnsystem"
        );
        let (admin, connection) = tokio_postgres::connect(&database_url, NoTls).await?;
        let connection = tokio::spawn(connection);
        let version: String = admin
            .query_one("SHOW server_version_num", &[])
            .await?
            .get(0);
        anyhow::ensure!(
            version.parse::<u32>()? / 10_000 == 18,
            "nested proof requires PostgreSQL 18"
        );
        let exporter = InMemorySpanExporterBuilder::new().build();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let subscriber = tracing_subscriber::registry().with(
            tracing_opentelemetry::layer().with_tracer(provider.tracer("nested-http-authority")),
        );
        let _guard = tracing::subscriber::set_default(subscriber);
        let mut origin = origin().await?;
        let (route, postgres, parent_digest) = nested_route(&RouteOptions {
            database_url,
            artifact_base: std::env::var("WAMN_HTTP_REUSE_ARTIFACT_BASE")
                .context("set WAMN_HTTP_REUSE_ARTIFACT_BASE to a throwaway OCI repository")?,
            component_wasm: std::env::var("WAMN_HTTP_REUSE_COMPONENT_WASM")
                .context("set WAMN_HTTP_REUSE_COMPONENT_WASM to the actual http_request.wasm")?
                .into(),
            upstream_base_url: format!("http://{}", origin.address),
            path_and_query: "/reuse".to_owned(),
        })
        .await?;
        let parent = route
            .release
            .manifest()
            .components
            .iter()
            .find(|component| component.digest.as_str() == parent_digest)
            .context("release lacks its exact parent component")?;
        let wiring = route
            .release
            .manifest()
            .wirings
            .iter()
            .find(|wiring| wiring.wiring_version == 2)
            .context("release lacks its exact nested wiring")?;
        assert_ne!(
            parent.package_id, wiring.package_id,
            "proof must distinguish the wiring owner from its root component package"
        );
        let caller = originating_caller(&system_url, &route, postgres).await?;
        let mut connection_id = None;
        for id in [21, 22] {
            let mut direct = request(id);
            direct.caller = Some(caller.clone());
            let accepted = receipt(
                &mut origin,
                route.driver.execute(direct).await?,
                id,
                "Bearer fixture-token",
            )
            .await?;
            if let Some(previous) = connection_id {
                assert_eq!(
                    accepted, previous,
                    "canonical child did not warm one reusable socket"
                );
            }
            connection_id = Some(accepted);
        }

        for id in [23, 24] {
            let mut nested = request(id);
            nested.wiring_version = 2;
            nested.resolution = WiringResolution::Frozen;
            nested.caller = Some(caller.clone());
            let accepted = receipt(
                &mut origin,
                route.driver.execute(nested).await?,
                id,
                "Bearer fixture-token",
            )
            .await?;
            assert_eq!(
                Some(accepted),
                connection_id,
                "the admitted child did not reuse its own warm transport"
            );
        }
        provider.force_flush()?;
        let spans = exporter.get_finished_spans()?;
        let attribute = |span: &opentelemetry_sdk::trace::SpanData, key: &str| {
            span.attributes
                .iter()
                .find(|attribute| attribute.key.as_str() == key)
                .map(|attribute| attribute.value.to_string())
        };
        let nested: Vec<_> = spans
            .iter()
            .filter(|span| {
                span.name == "wamn.component.invoke"
                    && attribute(span, "wamn.wiring_version").as_deref() == Some("2")
            })
            .collect();
        for (digest, operation) in [
            (&parent_digest, OPERATION),
            (&route.component_digest, CHILD_OPERATION),
        ] {
            let calls: Vec<_> = nested
                .iter()
                .filter(|span| {
                    attribute(span, "wamn.component_digest").as_deref() == Some(digest.as_str())
                        && attribute(span, "wamn.operation").as_deref() == Some(operation)
                })
                .collect();
            assert_eq!(
                calls.len(),
                2,
                "both real nested deliveries must invoke the exact admitted component"
            );
            for span in calls {
                assert_eq!(
                    attribute(span, "wamn.caller_principal_id").as_deref(),
                    Some(caller.principal_id())
                );
                assert_eq!(
                    attribute(span, "wamn.caller_credential_kind").as_deref(),
                    Some("pat")
                );
                assert_eq!(attribute(span, "wamn.node_id").as_deref(), Some(NODE_ID));
            }
        }
        // The same component bytes import the child, but this export does not
        // declare that dependency. Presence in the release grants no authority.
        let mut undeclared = request(25);
        undeclared.wiring_version = 3;
        undeclared.resolution = WiringResolution::Frozen;
        undeclared.caller = Some(caller.clone());
        let Err(refusal) = route.driver.execute(undeclared).await else {
            anyhow::bail!("the undeclared export acquired child authority");
        };
        assert!(
            refusal.chain().any(|cause| cause.to_string()
                == format!("nested-operation-not-declared-for-export: {CHILD_OPERATION}")),
            "wrong undeclared-export refusal: {refusal:#}"
        );
        assert!(
            origin.receipts.try_recv().is_err(),
            "undeclared child reached the wire"
        );

        set_instance_status(&admin, "disabled").await?;
        let mut nested = request(26);
        nested.wiring_version = 2;
        nested.resolution = WiringResolution::Frozen;
        nested.caller = Some(caller.clone());
        let failure = route
            .driver
            .execute(nested)
            .await?
            .outcome
            .failure
            .context("nested HTTP reused authority after the connection was disabled")?;
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
            origin.receipts.try_recv().is_err(),
            "disabled nested effect reached the wire"
        );
        set_instance_status(&admin, "enabled").await?;
        let mut nested = request(27);
        nested.wiring_version = 2;
        nested.resolution = WiringResolution::Frozen;
        nested.caller = Some(caller);
        let restored = receipt(
            &mut origin,
            route.driver.execute(nested).await?,
            27,
            "Bearer fixture-token",
        )
        .await?;
        assert_eq!(
            Some(restored),
            connection_id,
            "a refused request poisoned the reusable socket"
        );
        assert!(
            origin.receipts.try_recv().is_err(),
            "unexpected extra nested effect"
        );
        drop(admin);
        connection.abort();
        Ok(())
    }
}
