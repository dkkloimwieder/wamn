//! One router-invoked `wamn:node` guest performing a real trusted HTTP effect.
//!
//! Shared proof support, not a proof: it seeds exactly the facts
//! `ConnectionHttp::send` reads — a component-grain
//! `catalog.connection_requirements` row, an `active`/`valid`
//! `catalog.connection_bindings` row over an `enabled`
//! `catalog.connection_instances` whose `active_generation` matches the
//! `catalog.connection_generations` row carrying the credential handle — and
//! builds the production `RouterDriver` over them.
//!
//! Two beads need this same closure. `wamn-0h0g.11.8` drives it to witness
//! trace propagation at the wire; `wamn-0h0g.11.3` needs it to prove HTTP
//! connection confinement refusals. Nothing here asserts anything, so a refusal
//! proof can seed a deliberately wrong fact through [`RouteOptions`] and drive
//! the same driver.
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
    SERVING_MANIFEST_FORMAT_VERSION, WiringDocument, WiringNode, WiringTerminal, flip_activation,
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
    let component_bytes = std::fs::read(&options.component_wasm).with_context(|| {
        format!(
            "read component bytes {} — build it with a SEPARATE cargo invocation \
             (`cargo build -p http-request --target wasm32-wasip2` inside \
             components/no-std/, which is a separate workspace because sharing one \
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
    let postgres_credentials = seed_catalog(options, &admitted, &document, &wiring_hash, &release)
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
            Arc::new(WamnCredentials::from_projects(HashMap::from([(
                PROJECT.to_owned(),
                HashMap::from([(CREDENTIAL_HANDLE.to_owned(), credential_secret())]),
            )]))),
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
/// `components/no-std/publish.sh` renders it.
///
/// Read from the template rather than restated here: a copy would drift from
/// the guest's real parameter contract, and `validate_parameters` in
/// `wiring_lowering` checks this node's params against exactly these
/// declarations.
fn declaration() -> anyhow::Result<ComponentDeclaration> {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let template = std::fs::read_to_string(format!(
        "{root}/components/no-std/http-request/declaration.json.in"
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
                "auth-policy": {"mode": "none"}
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
) -> anyhow::Result<ClassCredentials> {
    let (client, connection) = tokio_postgres::connect(&options.database_url, NoTls)
        .await
        .context("connect the seeding session")?;
    let driver = tokio::spawn(connection);
    let seeded =
        seed_with_client(&client, options, component, document, wiring_hash, release).await;
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
        .context("seed the exact serving-manifest snapshot")?;
    client
        .execute(
            "INSERT INTO catalog.effective_release_heads \
                    (tenant_id, environment, effective_release_id) \
             VALUES ($1, $2, $3)",
            &[&TENANT, &ENVIRONMENT, &EFFECTIVE_RELEASE_ID],
        )
        .await
        .context("select the effective release")?;

    // The activation pointer, through the production statement itself — bound
    // directly, because the extended query protocol cannot pass parameters to a
    // server-side `EXECUTE`. Its tenant comes from the `app.tenant` claim, and
    // `ACTIVE_WIRING_SQL` joins it on
    // `confirmed_definition_hash = wirings.wiring_hash`.
    client
        .execute(
            flip_activation(),
            &[&PACKAGE, &ENVIRONMENT, &WIRING_ID, &wiring_hash, &true],
        )
        .await
        .context("activate the wiring")?;

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

fn generation_url(admin_url: &str, role: &str) -> anyhow::Result<String> {
    let mut url = reqwest::Url::parse(admin_url).context("parse the disposable admin URL")?;
    url.set_username(role)
        .map_err(|()| anyhow::anyhow!("set the production generation username"))?;
    url.set_password(Some(GENERATION_PASSWORD))
        .map_err(|()| anyhow::anyhow!("set the production generation password"))?;
    Ok(url.to_string())
}
