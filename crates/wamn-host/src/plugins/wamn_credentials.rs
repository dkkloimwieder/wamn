//! The `wamn:node/credentials` host plugin — the per-project credential VAULT
//! (5.9). Contract source of truth: docs/wamn-node.wit (`interface
//! credentials`, frozen 0.1 at 5.4; this is its first host implementation).
//!
//! Flows reference a credential BY NAME (`wamn-flow` `CredentialRef` +
//! `node.credential`, 5.1 — never inlined secret material); the runner scopes
//! each dispatch to its declared name guest-side (`CapsCtx.credential`), and
//! the node's `get(handle)` resolves here lazily. Host-enforced invariants:
//!
//! - Resolution is PROJECT-SCOPED: the executing component's project is a
//!   host-injected claim (`set_project` / `wamn.project` config), never guest
//!   input — a component can only read its own project's credentials.
//! - Every `get` is AUDIT-LOGGED (component, project, handle, outcome) on the
//!   `wamn::credentials` target. The secret value itself is never logged.
//!   Run/node attribution rides the guest-side `node_runs` records (the host
//!   sees only the component on this path).
//! - The v1 SOURCE is a mounted static file (`WAMN_CREDENTIALS_FILE`, a JSON
//!   object `{project: {name: secret}}` mounted from a K8s Secret — the
//!   `WAMN_PG_PROJECTS_FILE` pattern). A live per-Secret K8s read is the
//!   follow-up sharing wamn-5x0.1's client.
//! - Error semantics: NO SOURCE configured at all ⇒ `unavailable` (retryable —
//!   a deployment being fixed); a present source lacking the project or name ⇒
//!   `not-found` (config-shaped, terminal at the node).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::RwLock;

use anyhow::Context as _;
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::Linker;
use wash_runtime::wit::{WitInterface, WitWorld};

use super::wamn_postgres::{DEFAULT_PROJECT, PROJECT_CONFIG_KEY};

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "credentials-plugin",
        imports: { default: async | trappable | tracing },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wamn::node::credentials::{self, CredentialError};

pub const WAMN_CREDENTIALS_ID: &str = "wamn-credentials";

/// Wire the `wamn:node/credentials` host function into a linker directly (the
/// runner / gate-harness path; the wash workload path uses
/// [`HostPlugin::on_workload_item_bind`]).
pub fn add_to_linker(linker: &mut Linker<SharedCtx>) -> wash_runtime::wasmtime::Result<()> {
    credentials::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)
}

/// The vault: an in-memory per-project credential map plus the component →
/// project claim registry (host-injected, non-spoofable — the wamn:postgres
/// tenant/project pattern).
pub struct WamnCredentials {
    /// Whether ANY backing source was configured. Distinguishes `unavailable`
    /// (no source — retryable) from `not-found` (source present, name absent —
    /// config-shaped).
    has_source: bool,
    /// project → { credential name → secret material }.
    projects: HashMap<String, HashMap<String, String>>,
    /// component id → project. Registered host-side (`set_project` or the
    /// `wamn.project` workload config); a component can never choose it.
    components: RwLock<HashMap<String, String>>,
}

impl WamnCredentials {
    /// A vault with NO backing source: every `get` is `unavailable`. The
    /// linker still needs the interface satisfied (the flowrunner imports it
    /// unconditionally), so gates and credential-less deployments use this.
    pub fn empty() -> Self {
        Self {
            has_source: false,
            projects: HashMap::new(),
            components: RwLock::new(HashMap::new()),
        }
    }

    /// A vault over an explicit per-project map (tests / the gate harness).
    pub fn from_projects(projects: HashMap<String, HashMap<String, String>>) -> Self {
        Self {
            has_source: true,
            projects,
            components: RwLock::new(HashMap::new()),
        }
    }

    /// Parse the mounted credentials file: a JSON object
    /// `{ "<project>": { "<name>": "<secret>", ... }, ... }` (the
    /// `WAMN_PG_PROJECTS_FILE` shape, mounted from a K8s Secret).
    pub fn projects_from_json(
        text: &str,
    ) -> anyhow::Result<HashMap<String, HashMap<String, String>>> {
        let root: serde_json::Value =
            serde_json::from_str(text).context("credentials file is not valid JSON")?;
        let obj = root
            .as_object()
            .context("credentials file must be a JSON object of projects")?;
        let mut projects = HashMap::new();
        for (project, creds) in obj {
            let creds_obj = creds.as_object().with_context(|| {
                format!("credentials for project {project:?} must be an object of name: secret")
            })?;
            let mut map = HashMap::new();
            for (name, secret) in creds_obj {
                let secret = secret.as_str().with_context(|| {
                    format!("credential {project:?}/{name:?} must be a string secret")
                })?;
                map.insert(name.clone(), secret.to_string());
            }
            projects.insert(project.clone(), map);
        }
        Ok(projects)
    }

    /// A vault from the file at `path`. A MISSING file is a warn + empty
    /// vault (the deploy manifest mounts the Secret `optional`, so a
    /// credential-less project deploys cleanly); a present-but-malformed file
    /// is a hard error (a real misconfiguration must be loud).
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            tracing::warn!(
                path = %path.display(),
                "credentials file not found — the vault is empty (every get is unavailable)"
            );
            return Ok(Self::empty());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read credentials file {}", path.display()))?;
        Ok(Self::from_projects(Self::projects_from_json(&text)?))
    }

    /// Register which project a component belongs to (host-side only).
    pub fn set_project(&self, component_id: &str, project: &str) -> anyhow::Result<()> {
        self.components
            .write()
            .expect("components lock poisoned")
            .insert(component_id.to_string(), project.to_string());
        Ok(())
    }

    fn project_for(&self, component_id: &str) -> String {
        self.components
            .read()
            .expect("components lock poisoned")
            .get(component_id)
            .cloned()
            .unwrap_or_else(|| DEFAULT_PROJECT.to_string())
    }

    /// Resolve `name` within `project` — the vault semantics the WIT errors
    /// mirror (see the module docs for the unavailable/not-found split).
    fn resolve(&self, project: &str, name: &str) -> Result<String, CredentialError> {
        if !self.has_source {
            return Err(CredentialError::Unavailable);
        }
        self.projects
            .get(project)
            .and_then(|creds| creds.get(name))
            .cloned()
            .ok_or(CredentialError::NotFound)
    }
}

#[async_trait::async_trait]
impl HostPlugin for WamnCredentials {
    fn id(&self) -> &'static str {
        WAMN_CREDENTIALS_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from("wamn:node/credentials@0.1.0")]),
            exports: HashSet::new(),
        }
    }

    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        if !interfaces.contains("wamn", "node", &["credentials"]) {
            return Ok(());
        }
        // The SAME wamn.project key the wamn:postgres plugin scopes by — one
        // project identity per component, injected by the platform.
        if let Some(project) = item.local_resources().config.get(PROJECT_CONFIG_KEY) {
            let project = project.clone();
            self.set_project(item.id(), &project)?;
            tracing::debug!(
                component = item.id(),
                project,
                "wamn:node/credentials project registered"
            );
        }
        credentials::add_to_linker::<_, SharedCtx>(item.linker(), extract_active_ctx)?;
        Ok(())
    }
}

fn plugin_of(
    ctx: &ActiveCtx<'_>,
) -> wash_runtime::wasmtime::Result<std::sync::Arc<WamnCredentials>> {
    ctx.try_get_plugin::<WamnCredentials>(WAMN_CREDENTIALS_ID)
}

impl credentials::Host for ActiveCtx<'_> {
    async fn get(
        &mut self,
        handle: String,
    ) -> wash_runtime::wasmtime::Result<Result<String, CredentialError>> {
        let plugin = plugin_of(self)?;
        let component = self.component_id.to_string();
        let project = plugin.project_for(&component);
        let out = plugin.resolve(&project, &handle);
        // The WIT-promised audit trail: every get, granted or refused, with
        // the executing component's host-known identity. NEVER the secret.
        match &out {
            Ok(_) => tracing::info!(
                target: "wamn::credentials",
                component, project, handle,
                outcome = "granted",
                "credential get"
            ),
            Err(e) => tracing::warn!(
                target: "wamn::credentials",
                component, project, handle,
                outcome = ?e,
                "credential get refused"
            ),
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_project_vault() -> WamnCredentials {
        WamnCredentials::from_projects(HashMap::from([(
            "proj-a".to_string(),
            HashMap::from([("notify-token".to_string(), "s3cr3t".to_string())]),
        )]))
    }

    /// The unavailable/not-found split: no source at all is retryable
    /// infrastructure; a present source lacking the name is config-shaped.
    #[test]
    fn resolve_distinguishes_no_source_from_unknown_name() {
        let empty = WamnCredentials::empty();
        assert!(matches!(
            empty.resolve("proj-a", "notify-token"),
            Err(CredentialError::Unavailable)
        ));

        let vault = one_project_vault();
        assert_eq!(vault.resolve("proj-a", "notify-token").unwrap(), "s3cr3t");
        assert!(matches!(
            vault.resolve("proj-a", "unknown"),
            Err(CredentialError::NotFound)
        ));
    }

    /// PROJECT SCOPING is the host-enforced boundary: the same name in a
    /// different project resolves that project's secret (or nothing) — a
    /// component can never read across projects.
    #[test]
    fn resolution_is_project_scoped() {
        let vault = WamnCredentials::from_projects(HashMap::from([
            (
                "proj-a".to_string(),
                HashMap::from([("token".to_string(), "secret-a".to_string())]),
            ),
            (
                "proj-b".to_string(),
                HashMap::from([("token".to_string(), "secret-b".to_string())]),
            ),
        ]));
        assert_eq!(vault.resolve("proj-a", "token").unwrap(), "secret-a");
        assert_eq!(vault.resolve("proj-b", "token").unwrap(), "secret-b");
        assert!(matches!(
            vault.resolve("proj-c", "token"),
            Err(CredentialError::NotFound)
        ));

        // The claim registry defaults to the default project and is
        // host-registered per component.
        vault.set_project("comp-1", "proj-b").unwrap();
        assert_eq!(vault.project_for("comp-1"), "proj-b");
        assert_eq!(vault.project_for("comp-unregistered"), DEFAULT_PROJECT);
    }

    /// The mounted-file shape is the WAMN_PG_PROJECTS_FILE pattern:
    /// `{project: {name: secret}}`, strings only, malformed = loud.
    #[test]
    fn credentials_file_parses_the_nested_project_shape() {
        let projects = WamnCredentials::projects_from_json(
            r#"{"default": {"notify-token": "tok-1", "other": "tok-2"}, "p2": {}}"#,
        )
        .unwrap();
        assert_eq!(projects["default"]["notify-token"], "tok-1");
        assert_eq!(projects["default"]["other"], "tok-2");
        assert!(projects["p2"].is_empty());

        assert!(WamnCredentials::projects_from_json("[]").is_err());
        assert!(WamnCredentials::projects_from_json(r#"{"p": "flat"}"#).is_err());
        assert!(WamnCredentials::projects_from_json(r#"{"p": {"n": 7}}"#).is_err());
    }
}
