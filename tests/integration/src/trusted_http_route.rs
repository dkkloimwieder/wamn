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
    AdmittedComponent, ComponentDeclaration, SERVING_MANIFEST_FORMAT_VERSION, WiringDocument,
    WiringNode, WiringTerminal, flip_activation,
};
use wamn_execution_host::{RouterDriver, RouterDriverConfig, WiringCacheCapacity};
use wamn_runtime::component_admission::{ComponentAdmissionRequest, validate_component_admission};
use wamn_runtime::component_artifact::{
    component_artifact_config_bytes, component_artifact_layout, component_artifact_reference,
};
use wamn_runtime::component_artifact_source::{
    ComponentArtifactSource, ComponentArtifactSourceConfig,
};
use wamn_runtime::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_logging::{WamnLogging, WamnLoggingConfig};
use wamn_runtime::plugins::wamn_postgres::{WamnPostgres, WamnPostgresConfig};
use wamn_runtime::release_manifest::ReleaseManifestWeld;

/// The one tenant every seeded row and every claim is scoped to.
pub const TENANT: &str = "tenant-a";
pub const CATALOG: &str = "orders";
pub const ENVIRONMENT: &str = "prod";
pub const CATALOG_VERSION: u32 = 1;
pub const WIRING_ID: &str = "hot-route";
pub const WIRING_VERSION: u32 = 1;
pub const NODE_ID: &str = "call-upstream";
/// The alias the guest names in `Request.requirement`, resolved at the
/// component grain by `CONNECTION_EFFECT_SNAPSHOT_SQL`.
pub const STORE_ALIAS: &str = "upstream";
pub const PROJECT: &str = "default";
pub const COMPONENT: &str = "http-request";
pub const INTERFACE_VERSION: &str = "0.1.0";
pub const OPERATION: &str = "run";
const INSTANCE_ID: &str = "upstream-instance";
const CREDENTIAL_HANDLE: &str = "upstream-v1";
const GENERATION: i64 = 1;
const CONTRACT: &str = "wamn:connection/http@0.1.0";
const REGISTRY_IO_TIMEOUT: Duration = Duration::from_secs(30);

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
    /// The digest OCI serves the guest under and every seeded row keys on.
    pub component_digest: String,
    /// The wiring document's own canonical hash — `catalog.wirings.wiring_hash`,
    /// the activation pointer's `confirmed_definition_hash`, and the release
    /// manifest's `graph-hash`, which must all agree.
    pub wiring_hash: String,
    /// The engine's epoch ticker. Dropping it would stop deadlines advancing.
    _ticker: tokio::task::JoinHandle<()>,
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
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);
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
    seed_catalog(options, &admitted, &document, &wiring_hash)
        .await
        .context("seed the catalog closure")?;

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

    let postgres = Arc::new(
        WamnPostgres::new(WamnPostgresConfig {
            database_url: Some(options.database_url.clone()),
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
            release,
            source,
            RouterDriverConfig {
                owner_prefix: "trusted-http-route".to_owned(),
                project: PROJECT.to_owned(),
                schema: None,
                cache_capacity: WiringCacheCapacity::default(),
                epoch_tick: DEFAULT_EPOCH_TICK,
            },
        )
        .context("build the router driver")?,
    );

    Ok(TrustedHttpRoute {
        driver,
        component_digest: admitted.component_digest.clone(),
        wiring_hash,
        _ticker: ticker,
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
        .replace("__CATALOG_ID__", CATALOG)
        .replace("__CATALOG_VERSION__", &CATALOG_VERSION.to_string());
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
    serde_json::json!({
        "format-version": SERVING_MANIFEST_FORMAT_VERSION,
        "release": {
            "tenant-id": TENANT,
            "catalog-id": CATALOG,
            "catalog-version": CATALOG_VERSION,
            "environment": ENVIRONMENT,
        },
        "components": [{
            "component": component.component,
            "interface-version": component.interface_version,
            "digest": component.component_digest,
        }],
        "wirings": [{
            "wiring-id": WIRING_ID,
            "wiring-version": WIRING_VERSION,
            "graph-hash": wiring_hash,
        }],
        "attachments": {},
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
) -> anyhow::Result<()> {
    let (client, connection) = tokio_postgres::connect(&options.database_url, NoTls)
        .await
        .context("connect the seeding session")?;
    let driver = tokio::spawn(connection);
    let seeded = seed_with_client(&client, options, component, document, wiring_hash).await;
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
) -> anyhow::Result<()> {
    // `catalog-schema.sql` applies whole only on a fresh install, and its
    // migration blocks take an ACCESS EXCLUSIVE lock — so it must arrive as ONE
    // implicit transaction, which `batch_execute` gives it and psql without
    // `--single-transaction` does not.
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let schema = std::fs::read_to_string(format!("{root}/deploy/sql/catalog-schema.sql"))
        .context("read the catalog DDL")?;
    client
        .batch_execute(&format!(
            "DO $roles$ BEGIN \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                 CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' NOSUPERUSER NOCREATEDB \
                   NOBYPASSRLS; \
               END IF; \
               IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN \
                 CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOBYPASSRLS; \
               END IF; \
             END $roles$;\n\
             DROP SCHEMA IF EXISTS catalog CASCADE;\n\
             {schema}"
        ))
        .await
        .context("install the catalog DDL")?;

    let catalog_version = i32::try_from(CATALOG_VERSION).expect("fixture catalog version fits");
    let wiring_version = i32::try_from(WIRING_VERSION).expect("fixture wiring version fits");
    let graph_json = serde_json::to_string(document).context("serialize the wiring document")?;
    let imports = serde_json::to_string(&component.imports).context("serialize imports")?;
    let input_ports = serde_json::to_string(&component.input_ports).context("serialize inputs")?;
    let output_ports =
        serde_json::to_string(&component.output_ports).context("serialize outputs")?;
    let parameters = serde_json::to_string(&component.parameters).context("serialize params")?;
    let effects = serde_json::to_string(&component.effects).context("serialize effects")?;
    // The whole portable record component admission mints, not just its
    // descriptor: `requirement_hash` is the SHA-256 of exactly these bytes.
    let requirement_json = serde_json::json!({
        "component-digest": component.component_digest,
        "store-alias": STORE_ALIAS,
        "requirement": {
            "requirement-type": "http",
            "contract": CONTRACT,
        },
    })
    .to_string();
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

    client
        .execute(
            "INSERT INTO catalog.catalogs \
                    (tenant_id, catalog_id, version, environment, schema_version, state) \
             VALUES ($1, $2, $3, $4, '1', 'applied')",
            &[&TENANT, &CATALOG, &catalog_version, &ENVIRONMENT],
        )
        .await
        .context("seed the catalog header")?;
    client
        .execute(
            "INSERT INTO catalog.catalog_heads \
                    (tenant_id, catalog_id, environment, applied_catalog_version) \
             VALUES ($1, $2, $3, $4)",
            &[&TENANT, &CATALOG, &ENVIRONMENT, &catalog_version],
        )
        .await
        .context("seed the applied catalog head")?;
    // `catalog.connection_bindings` FKs this row.
    client
        .execute(
            "INSERT INTO catalog.releases (tenant_id, catalog_id, catalog_version) \
             VALUES ($1, $2, $3)",
            &[&TENANT, &CATALOG, &catalog_version],
        )
        .await
        .context("seed the release manifest row")?;
    client
        .execute(
            "INSERT INTO catalog.wirings (tenant_id, catalog_id, wiring_id, version, \
                    gated_catalog_version, graph_json, wiring_hash) \
             VALUES ($1, $2, $3, $4, $5, $6::text::jsonb, $7)",
            &[
                &TENANT,
                &CATALOG,
                &WIRING_ID,
                &wiring_version,
                &catalog_version,
                &graph_json,
                &wiring_hash,
            ],
        )
        .await
        .context("seed the wiring version")?;
    client
        .execute(
            "INSERT INTO catalog.component_library (\
                 tenant_id, catalog_id, catalog_version, component, interface_version, operation, \
                 component_digest, imports, imports_fingerprint, effects, input_ports, \
                 output_ports, parameters\
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8::text::jsonb, $9, $10::text::jsonb, \
                 $11::text::jsonb, $12::text::jsonb, $13::text::jsonb)",
            &[
                &TENANT,
                &CATALOG,
                &catalog_version,
                &component.component,
                &component.interface_version,
                &component.operation,
                &component.component_digest,
                &imports,
                &component.imports_fingerprint,
                &effects,
                &input_ports,
                &output_ports,
                &parameters,
            ],
        )
        .await
        .context("seed the admitted component fact")?;

    // The activation pointer, through the production statement itself — bound
    // directly, because the extended query protocol cannot pass parameters to a
    // server-side `EXECUTE`. Its tenant comes from the `app.tenant` claim, and
    // `ACTIVE_WIRING_SQL` joins it on
    // `confirmed_definition_hash = wirings.wiring_hash`.
    client
        .execute(
            flip_activation(),
            &[&CATALOG, &ENVIRONMENT, &WIRING_ID, &wiring_hash, &true],
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
                &format!("sha256:{}", "d".repeat(64)),
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
                 tenant_id, catalog_id, catalog_version, component_digest, store_alias, \
                 environment, instance_id, binding_status, validation_status, validation_hash\
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, 'active', 'valid', $8)",
            &[
                &TENANT,
                &CATALOG,
                &catalog_version,
                &component.component_digest,
                &STORE_ALIAS,
                &ENVIRONMENT,
                &INSTANCE_ID,
                &format!("sha256:{}", "e".repeat(64)),
            ],
        )
        .await
        .context("bind the requirement to the instance")?;
    Ok(())
}
