//! Shared input document for the Receiving and WMS test runs.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const JOURNEY_DOCUMENT_ENV: &str = "WAMN_JOURNEY_DOCUMENT";

/// Sole field authority for the cluster journey's input document.
///
/// An environment variable carries a process setting; data crosses a boundary
/// as a declared, schema'd artifact. This document replaced thirteen
/// `WAMN_*` environment variables that had grown one name at a time, each
/// encoding whatever its author was thinking about -- the application, the
/// test, the database -- until two PG18 URLs sat one segment apart in a flat
/// namespace with nothing to say they were different things. As fields they
/// are `system_pg_url` and, in the materializer phase, `project_pg_url`, and
/// the question does not arise.
///
/// The shell writes it once, with `jq`, from the values it owns; this crate
/// reads it strictly. `deny_unknown_fields` is what makes it a contract: a key
/// the writer invents and the reader does not know fails here, not forty
/// minutes into a cluster run as an empty string.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JourneyDocument {
    pub system_pg_url: String,
    pub component_directory: PathBuf,
    pub compilation_cache_directory: PathBuf,
    pub flow_http_wasm: PathBuf,
    pub component_artifact_base: String,
    pub release_artifact_base: String,
    pub route_host: String,
    pub registry_auth_file: PathBuf,
    pub host_secret_directory: PathBuf,
    pub host_secret_namespace: String,
    pub route_caller_secret_output: PathBuf,
    /// Copied package sources for the dedicated fresh-only proof.
    /// The initial phase creates this directory before any package admission.
    pub fresh_only_packages: Option<PathBuf>,
    /// Fresh-install proof with unchanged overlay artifacts.
    pub overlay_compatibility: Option<CompatibilityPhase>,
    /// Released materializer replay and retry proof.
    pub postcommit: Option<PostcommitPhase>,
    /// Known only after the route phase has provisioned the project
    /// environment and the materializer trigger has produced a receipt. The
    /// shell amends the document with it then; before that it is absent, and
    /// the materializer test refuses to run rather than read an empty string.
    pub materializer: Option<MaterializerPhase>,
    /// Known only once the released route is reachable from this machine and
    /// the fixture rows exist (wamn-362o.27). The shell amends it in; the
    /// runtime assertions refuse to run without it rather than guess an
    /// endpoint or a pallet.
    pub runtime: Option<RuntimePhase>,
}

/// The runtime-assertion phase: where the released route answers from this
/// machine, and the fixture the journey seeded, declared ONCE there and handed
/// over here so the test carries no second copy of it.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuntimePhase {
    /// The route's origin as reachable from the test -- a temporary NodePort
    /// on a kind node's docker-network address. The Host header still names
    /// the released route host.
    pub route_endpoint: String,
    /// The fixture pallet the contention moves.
    pub pallet_id: String,
    /// The fixture location it moves to.
    pub to_location_id: String,
}

/// The materializer phase's inputs: the project-environment database the
/// route phase provisioned, the event stream it subscribes to, and the receipt
/// the trigger produced. The NATS URL is known from the start, but the only
/// reader that needs it is this phase's, so it rides here rather than being a
/// required top-level field a route-only run would have to invent.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MaterializerPhase {
    pub project_pg_url: String,
    pub nats_url: String,
    pub receipt_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BaseCandidate {
    Baseline,
    Additive,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CompatibilityPhase {
    pub base: BaseCandidate,
    pub package_directory: PathBuf,
    pub evidence_file: PathBuf,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PostcommitPhase {
    pub route_endpoint: String,
    pub evidence_file: PathBuf,
    pub kubeconfig: PathBuf,
    pub context: String,
    pub namespace: String,
    pub materializer_workload: String,
    pub source_commit: String,
    pub statement_timeout_ms: u64,
}

impl JourneyDocument {
    pub fn required() -> anyhow::Result<Self> {
        let path = std::env::var(JOURNEY_DOCUMENT_ENV)
            .ok()
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .with_context(|| format!("set {JOURNEY_DOCUMENT_ENV} to the test input document"))?;
        let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        parse_journey_document(&bytes)
            .with_context(|| format!("{} is not a valid journey document", path.display()))
    }

    /// Every scalar the document carries, named, so emptiness is refused with
    /// the field's name rather than surfacing as a path that does not exist.
    pub fn scalars(&self) -> [(&'static str, &str); 11] {
        fn path(value: &Path) -> &str {
            value.to_str().unwrap_or("")
        }
        [
            ("system_pg_url", &self.system_pg_url),
            ("component_directory", path(&self.component_directory)),
            (
                "compilation_cache_directory",
                path(&self.compilation_cache_directory),
            ),
            ("flow_http_wasm", path(&self.flow_http_wasm)),
            ("component_artifact_base", &self.component_artifact_base),
            ("release_artifact_base", &self.release_artifact_base),
            ("route_host", &self.route_host),
            ("registry_auth_file", path(&self.registry_auth_file)),
            ("host_secret_directory", path(&self.host_secret_directory)),
            ("host_secret_namespace", &self.host_secret_namespace),
            (
                "route_caller_secret_output",
                path(&self.route_caller_secret_output),
            ),
        ]
    }
}

/// Parse one strict journey document. Unknown keys, missing keys and empty
/// values are all refusals, each naming the field.
pub fn parse_journey_document(bytes: &[u8]) -> anyhow::Result<JourneyDocument> {
    let document: JourneyDocument = serde_json::from_slice(bytes)
        .context("journey document disagrees with its generated schema")?;
    for (field, value) in document.scalars() {
        anyhow::ensure!(!value.is_empty(), "journey document field {field} is empty");
    }
    if let Some(root) = &document.fresh_only_packages {
        anyhow::ensure!(
            root.is_absolute()
                && root.file_name().is_some()
                && root.parent() == document.host_secret_directory.parent()
                && root != &document.host_secret_directory,
            "fresh_only_packages must name a separate directory beside the private host secrets"
        );
    }
    if let Some(materializer) = &document.materializer {
        for (field, value) in [
            ("materializer.project_pg_url", &materializer.project_pg_url),
            ("materializer.nats_url", &materializer.nats_url),
            ("materializer.receipt_id", &materializer.receipt_id),
        ] {
            anyhow::ensure!(!value.is_empty(), "journey document field {field} is empty");
        }
    }
    if let Some(runtime) = &document.runtime {
        for (field, value) in [
            ("runtime.route_endpoint", &runtime.route_endpoint),
            ("runtime.pallet_id", &runtime.pallet_id),
            ("runtime.to_location_id", &runtime.to_location_id),
        ] {
            anyhow::ensure!(!value.is_empty(), "journey document field {field} is empty");
        }
    }
    Ok(document)
}

/// Byte-stable pretty JSON Schema generated from the strict document type.
pub fn journey_document_schema_bytes() -> Vec<u8> {
    let schema = serde_json::to_value(schemars::schema_for!(JourneyDocument))
        .expect("journey document schema serializes");
    let mut bytes = serde_json::to_vec_pretty(&schema).expect("journey document schema serializes");
    bytes.push(b'\n');
    bytes
}
