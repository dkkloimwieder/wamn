//! The `wamn:node/credentials` host plugin — the per-project credential VAULT
//! (5.9). Contract source of truth: docs/wamn-node.wit (`interface
//! credentials`, frozen 0.1 at 5.4; this is its first host implementation).
//!
//! Flows reference a credential BY NAME (`wamn-flow` `CredentialRef` +
//! `node.credential`, 5.1 — never inlined secret material); the node's
//! `get(handle)` resolves here lazily. Host-enforced invariants:
//!
//! - Resolution is PROJECT-SCOPED: the executing component's project is a
//!   host-injected claim (`set_project` / `wamn.project` config), never guest
//!   input — a component can only read its own project's credentials. An
//!   UNREGISTERED project fails CLOSED (`not-granted`, cjv.3) — never a
//!   fail-open default.
//! - Resolution is GRANT-SCOPED (cjv.3): `get(handle)` returns `not-granted`
//!   for any name NOT in the executing component's granted set, so the frozen
//!   contract's per-execution grant is enforced HOST-SIDE, not only by the
//!   guest-side `CapsCtx` facade a direct-import custom node could bypass. The
//!   granted set is registered per component:
//!     * the trusted, compiled-in flow-runner declares its per-RUN grant (the
//!       flow's declared `credentials`) via `wamn:runner/credentials`
//!       `set-granted` — a channel linked ONLY into its world; per-NODE
//!       scoping still rides `CapsCtx` (a node reads only its own declared
//!       name), so `get` is bounded by both;
//!     * a custom node (wamn-bd5) — a separate per-invocation component that
//!       imports `wamn:node/credentials` directly and never gets the trusted
//!       channel — is granted its exact declared name(s) host-side by the
//!       runner before invocation.
//! - Every `get` is AUDIT-LOGGED (component, project, handle, outcome) on the
//!   `wamn::credentials` target. The secret value itself is never logged.
//!   Run/node attribution rides the guest-side `node_runs` records (the host
//!   sees only the component on this path).
//! - The v1 SOURCE is a mounted static file (`WAMN_CREDENTIALS_FILE`, a JSON
//!   object `{project: {name: secret}}` mounted from a K8s Secret — the
//!   `WAMN_PG_PROJECTS_FILE` pattern). A live per-Secret K8s read is the
//!   follow-up sharing wamn-5x0.1's client.
//! - Error semantics: an ungranted name or unregistered project ⇒
//!   `not-granted` (terminal); a granted name with NO source configured at all
//!   ⇒ `unavailable` (retryable — a deployment being fixed); a granted name a
//!   present source lacks ⇒ `not-found` (config-shaped, terminal at the node).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::RwLock;

use anyhow::Context as _;
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::Linker;
use wash_runtime::wit::{WitInterface, WitWorld};

use super::wamn_postgres::PROJECT_CONFIG_KEY;

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "credentials-plugin",
        imports: { default: async | trappable | tracing },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wamn::node::credentials::{self, CredentialError};
use bindings::wamn::runner::credentials as runner_credentials;

pub const WAMN_CREDENTIALS_ID: &str = "wamn-credentials";

/// Wire the `wamn:node/credentials` `get` host function into a linker directly
/// (the runner / gate-harness path; the wash workload path uses
/// [`HostPlugin::on_workload_item_bind`]). Every component that resolves a
/// credential imports this.
pub fn add_to_linker(linker: &mut Linker<SharedCtx>) -> wash_runtime::wasmtime::Result<()> {
    credentials::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)
}

/// Wire the TRUSTED `wamn:runner/credentials` `set-granted` channel into a
/// linker (cjv.3). Call this ONLY for the trusted, compiled-in flow-runner —
/// the sole component allowed to declare its own per-run grant. A custom node
/// must NOT get this: its grant is registered host-side by the runner
/// ([`WamnCredentials::set_granted_credentials`]) before invocation.
pub fn add_runner_to_linker(linker: &mut Linker<SharedCtx>) -> wash_runtime::wasmtime::Result<()> {
    runner_credentials::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)
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
    /// component id → the credential names it may resolve (cjv.3). The trusted
    /// flow-runner declares its per-run set via `wamn:runner/credentials`; the
    /// runner registers a custom node's exact grant host-side. A component
    /// absent here (or a `get` for a name outside its set) is `not-granted` —
    /// fail-closed.
    grants: RwLock<HashMap<String, HashSet<String>>>,
}

impl WamnCredentials {
    /// A vault with NO backing source: a granted `get` is `unavailable`. The
    /// linker still needs the interface satisfied (the flowrunner imports it
    /// unconditionally), so gates and credential-less deployments use this.
    pub fn empty() -> Self {
        Self {
            has_source: false,
            projects: HashMap::new(),
            components: RwLock::new(HashMap::new()),
            grants: RwLock::new(HashMap::new()),
        }
    }

    /// A vault over an explicit per-project map (tests / the gate harness).
    pub fn from_projects(projects: HashMap<String, HashMap<String, String>>) -> Self {
        Self {
            has_source: true,
            projects,
            components: RwLock::new(HashMap::new()),
            grants: RwLock::new(HashMap::new()),
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

    /// Register (or replace) the credential names a component may resolve
    /// (cjv.3). The trusted flow-runner calls this per run via
    /// `wamn:runner/credentials`; the runner calls it host-side for a custom
    /// node before invoking it. A `get` for any name outside this set is
    /// `not-granted`.
    pub fn set_granted_credentials(
        &self,
        component_id: &str,
        names: impl IntoIterator<Item = String>,
    ) {
        self.grants
            .write()
            .expect("grants lock poisoned")
            .insert(component_id.to_string(), names.into_iter().collect());
    }

    /// Revoke a component's granted-credentials entry (cjv.3 / R31): the inverse
    /// of [`set_granted_credentials`]. The serve-node calls this after each
    /// invocation so a per-invocation grant never outlives its invocation (a
    /// later `get` — or a rebound node id — is `not-granted` again). Removing an
    /// absent entry is a no-op.
    pub fn clear_granted_credentials(&self, component_id: &str) {
        self.grants
            .write()
            .expect("grants lock poisoned")
            .remove(component_id);
    }

    /// The component's registered project, or `None` if unregistered (which
    /// `authorize` treats as fail-closed — cjv.3, no fail-open default).
    fn project_for(&self, component_id: &str) -> Option<String> {
        self.components
            .read()
            .expect("components lock poisoned")
            .get(component_id)
            .cloned()
    }

    /// Whether `component_id` was granted `name` (cjv.3). Fail-closed: an
    /// unregistered component grants nothing. `pub(crate)` so the serve-node's
    /// per-invocation grant/revoke tests can probe the vault directly (R31).
    pub(crate) fn is_granted(&self, component_id: &str, name: &str) -> bool {
        self.grants
            .read()
            .expect("grants lock poisoned")
            .get(component_id)
            .is_some_and(|set| set.contains(name))
    }

    /// The full guest-facing `get(handle)` decision (cjv.3): fail-closed
    /// project → fail-closed grant → project-scoped resolution. `not-granted`
    /// precedes any lookup, so an ungranted name never learns whether the
    /// secret exists.
    fn authorize(&self, component_id: &str, handle: &str) -> Result<String, CredentialError> {
        // Fail-closed identity: no registered project ⇒ nothing is granted.
        let project = self
            .project_for(component_id)
            .ok_or(CredentialError::NotGranted)?;
        // Fail-closed grant: the frozen per-execution grant, enforced host-side.
        if !self.is_granted(component_id, handle) {
            return Err(CredentialError::NotGranted);
        }
        self.resolve(&project, handle)
    }

    /// Resolve a raw `(project, name)` secret HOST-SIDE, bypassing the
    /// per-component grant/identity gate — for a host that needs the material
    /// itself, not on a guest's behalf. The serve-node reads its per-project-env
    /// HMAC signing key (wamn-fqg.22) this way at startup; a guest NEVER reaches
    /// this (a guest `get` always goes through [`authorize`]). `None` if the
    /// name is absent or no source is configured.
    pub fn lookup(&self, project: &str, name: &str) -> Option<String> {
        self.resolve(project, name).ok()
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

    /// Reap a workload's per-component registrations on teardown (R31): its
    /// project claim AND its granted-credentials entry. Without this a stale
    /// project claim + grant survive unbind, the maps grow across workload churn,
    /// and a REBOUND component id inherits the prior grant. Keyed like the fork's
    /// builtin postgres plugin — a workload's component ids are prefixed by the
    /// workload id — so everything NOT under it is retained. An unknown workload
    /// id clears nothing (no-op).
    async fn on_workload_unbind(
        &self,
        workload_id: &str,
        _interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        self.components
            .write()
            .expect("components lock poisoned")
            .retain(|component_id, _| !component_id.starts_with(workload_id));
        self.grants
            .write()
            .expect("grants lock poisoned")
            .retain(|component_id, _| !component_id.starts_with(workload_id));
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
        let out = plugin.authorize(&component, &handle);
        // The WIT-promised audit trail: every get, granted or refused, with
        // the executing component's host-known identity. NEVER the secret.
        // `project` may be absent (fail-closed identity); log what we know.
        let project = project.unwrap_or_default();
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

impl runner_credentials::Host for ActiveCtx<'_> {
    /// The trusted flow-runner declares the credential names granted to the
    /// run it is about to dispatch (cjv.3). Only components linked with
    /// [`add_runner_to_linker`] can call this — the compiled-in flow-runner,
    /// never a custom node.
    async fn set_granted(&mut self, names: Vec<String>) -> wash_runtime::wasmtime::Result<()> {
        let plugin = plugin_of(self)?;
        let component = self.component_id.to_string();
        plugin.set_granted_credentials(&component, names);
        Ok(())
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

        // The project registry is host-registered per component and fails
        // CLOSED — an unregistered component has no project identity (cjv.3).
        vault.set_project("comp-1", "proj-b").unwrap();
        assert_eq!(vault.project_for("comp-1").as_deref(), Some("proj-b"));
        assert_eq!(vault.project_for("comp-unregistered"), None);
    }

    /// The cjv.3 grant boundary end to end through `authorize` (the exact
    /// `get(handle)` decision): a granted name resolves; an ungranted name is
    /// `not-granted` BEFORE any lookup (never leaks whether the secret exists);
    /// an unregistered project or unregistered grant fails CLOSED.
    #[test]
    fn authorize_enforces_the_per_execution_grant() {
        let vault = WamnCredentials::from_projects(HashMap::from([(
            "proj-a".to_string(),
            HashMap::from([
                ("granted".to_string(), "sekret".to_string()),
                ("sibling".to_string(), "other".to_string()),
            ]),
        )]));
        vault.set_project("node", "proj-a").unwrap();
        vault.set_granted_credentials("node", ["granted".to_string()]);

        // Granted name in the project → the secret.
        assert_eq!(vault.authorize("node", "granted").unwrap(), "sekret");
        // A name that EXISTS in the project but was not granted → not-granted
        // (the C3-1 sibling-credential read, now closed host-side).
        assert!(matches!(
            vault.authorize("node", "sibling"),
            Err(CredentialError::NotGranted)
        ));
        // A name that does not exist and was not granted → not-granted (grant
        // is checked before existence, so no probing).
        assert!(matches!(
            vault.authorize("node", "absent"),
            Err(CredentialError::NotGranted)
        ));

        // Fail-closed project: a component with NO registered project, even one
        // granted the name, is not-granted (never fail-open to DEFAULT_PROJECT).
        vault.set_granted_credentials("no-project", ["granted".to_string()]);
        assert!(matches!(
            vault.authorize("no-project", "granted"),
            Err(CredentialError::NotGranted)
        ));

        // Fail-closed grant: a component with a project but NO granted set.
        vault.set_project("no-grant", "proj-a").unwrap();
        assert!(matches!(
            vault.authorize("no-grant", "granted"),
            Err(CredentialError::NotGranted)
        ));
    }

    /// Grant passes but resolution still governs existence/source: a GRANTED
    /// name absent from the source is `not-found`; a granted name with no
    /// source at all is `unavailable` (the retryable/terminal split survives
    /// the grant gate).
    #[test]
    fn a_granted_name_still_resolves_through_the_vault_semantics() {
        let vault =
            WamnCredentials::from_projects(HashMap::from([("proj-a".to_string(), HashMap::new())]));
        vault.set_project("node", "proj-a").unwrap();
        vault.set_granted_credentials("node", ["missing".to_string()]);
        assert!(matches!(
            vault.authorize("node", "missing"),
            Err(CredentialError::NotFound)
        ));

        let empty = WamnCredentials::empty();
        empty.set_project("node", "proj-a").unwrap();
        empty.set_granted_credentials("node", ["missing".to_string()]);
        assert!(matches!(
            empty.authorize("node", "missing"),
            Err(CredentialError::Unavailable)
        ));
    }

    /// R31 — unbind reaps a component's BOTH registries (project claim + grant)
    /// so nothing survives teardown and a rebound id starts clean. `on_workload_
    /// unbind` receives the WORKLOAD id; a component id is prefixed by it (the
    /// fork's builtin-plugin convention), so a bare-id single-component workload
    /// and a `<workload>-component-n` id both reap. This is the mutant-(a) witness:
    /// a no-op unbind leaves the maps populated and fails here.
    #[tokio::test]
    async fn unbind_reaps_project_and_grant_and_leaves_others() {
        let vault = one_project_vault();
        vault.set_project("proj-a-node", "proj-a").unwrap();
        vault.set_granted_credentials("proj-a-node", ["notify-token".to_string()]);
        // A second workload's component must survive the first's unbind.
        vault.set_project("other-node", "proj-a").unwrap();
        vault.set_granted_credentials("other-node", ["notify-token".to_string()]);

        assert_eq!(vault.project_for("proj-a-node").as_deref(), Some("proj-a"));
        assert!(vault.is_granted("proj-a-node", "notify-token"));

        let empty = HashSet::new();
        vault
            .on_workload_unbind("proj-a-node", WitInterfaces::new(&empty))
            .await
            .unwrap();

        // Both registries emptied for the unbound component.
        assert_eq!(vault.project_for("proj-a-node"), None);
        assert!(!vault.is_granted("proj-a-node", "notify-token"));
        // The other workload's registrations are untouched.
        assert_eq!(vault.project_for("other-node").as_deref(), Some("proj-a"));
        assert!(vault.is_granted("other-node", "notify-token"));
    }

    /// R31 — a REBOUND component id starts clean: after unbind, re-registering the
    /// same id with NO grant yields not-granted (it never inherits the prior
    /// grant). Unbinding an unknown id is a harmless no-op.
    #[tokio::test]
    async fn rebound_component_id_does_not_inherit_grant() {
        let vault = one_project_vault();
        vault.set_project("node", "proj-a").unwrap();
        vault.set_granted_credentials("node", ["notify-token".to_string()]);
        assert_eq!(vault.authorize("node", "notify-token").unwrap(), "s3cr3t");

        let empty = HashSet::new();
        // Unbind of an UNKNOWN id changes nothing (the grant still resolves).
        vault
            .on_workload_unbind("stranger", WitInterfaces::new(&empty))
            .await
            .unwrap();
        assert_eq!(vault.authorize("node", "notify-token").unwrap(), "s3cr3t");

        // Unbind the real id, then rebind ONLY the project (no grant) — the
        // rebound id is fail-closed, not carrying the prior grant.
        vault
            .on_workload_unbind("node", WitInterfaces::new(&empty))
            .await
            .unwrap();
        vault.set_project("node", "proj-a").unwrap();
        assert!(matches!(
            vault.authorize("node", "notify-token"),
            Err(CredentialError::NotGranted)
        ));
    }

    /// R31 — the explicit per-invocation revoke ([`clear_granted_credentials`])
    /// drops the grant while leaving the project claim intact (the serve-node
    /// keeps its host-owned project, set once, across invocations). Clearing an
    /// absent grant is a no-op.
    #[test]
    fn clear_granted_credentials_drops_grant_keeps_project() {
        let vault = one_project_vault();
        vault.set_project("node", "proj-a").unwrap();
        vault.set_granted_credentials("node", ["notify-token".to_string()]);
        assert!(vault.is_granted("node", "notify-token"));

        vault.clear_granted_credentials("node");
        assert!(!vault.is_granted("node", "notify-token"));
        // The project claim survives the grant revoke.
        assert_eq!(vault.project_for("node").as_deref(), Some("proj-a"));
        // Clearing an already-absent grant is a no-op.
        vault.clear_granted_credentials("node");
        assert!(!vault.is_granted("node", "notify-token"));
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
