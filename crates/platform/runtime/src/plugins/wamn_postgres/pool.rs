//! Connection pooling + credential resolution for `wamn:postgres` (SR4 split,
//! wamn-cjv.18): the per-project config/policy, the `CredentialProvider` seam,
//! the live `ProjectPool`, the R18 connect-time assertion, and connection
//! teardown. The pool/claim METHODS live on `WamnPostgres` in `claims.rs`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context as _;
use deadpool_postgres::{Hook, HookError, Object, Pool};

#[derive(Clone, Debug)]
pub struct WamnPostgresConfig {
    /// `postgres://user:pass@host:port/db`. None = plugin registers but every
    /// call returns `connection-unavailable`.
    pub database_url: Option<String>,
    pub pool_max_size: usize,
    /// Max wait for a pool checkout before `connection-unavailable`.
    pub wait_timeout_ms: u64,
    /// Host-enforced `statement_timeout`, injected per transaction.
    pub statement_timeout_ms: u32,
    /// Host-enforced cap on rows returned by a single query.
    pub row_limit: u64,
}

impl WamnPostgresConfig {
    pub fn from_env() -> Self {
        fn num<T: std::str::FromStr>(key: &str, default: T) -> T {
            std::env::var(key)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        }
        Self {
            database_url: std::env::var("DATABASE_URL")
                .or_else(|_| std::env::var("WAMN_PG_URL"))
                .ok(),
            pool_max_size: num("WAMN_PG_POOL_MAX", 16),
            wait_timeout_ms: num("WAMN_PG_WAIT_TIMEOUT_MS", 2_000),
            statement_timeout_ms: num("WAMN_PG_STATEMENT_TIMEOUT_MS", 5_000),
            row_limit: num("WAMN_PG_ROW_LIMIT", 100_000),
        }
    }
}

// ---------------------------------------------------------------------------
// Credential resolution (per-project connection + policy)
// ---------------------------------------------------------------------------

/// Resolved connection + policy for one project's database. In production one
/// project = one database (plan 2.3); the pool, statement timeout, and row
/// limit are all per-project so one noisy project cannot starve or over-fetch
/// on behalf of another.
#[derive(Clone, Debug)]
pub struct ProjectConfig {
    pub database_url: String,
    pub pool_max_size: usize,
    pub wait_timeout_ms: u64,
    pub statement_timeout_ms: u32,
    pub row_limit: u64,
}

impl ProjectConfig {
    /// The default project's config, from the single-DB [`WamnPostgresConfig`].
    pub(super) fn from_global(url: String, cfg: &WamnPostgresConfig) -> Self {
        Self {
            database_url: url,
            pool_max_size: cfg.pool_max_size,
            wait_timeout_ms: cfg.wait_timeout_ms,
            statement_timeout_ms: cfg.statement_timeout_ms,
            row_limit: cfg.row_limit,
        }
    }
}

/// Resolves a project id to its database connection + policy. This is the seam
/// that separates *which project am I* (a host-injected claim, non-spoofable)
/// from *where does that project's data live* (a deployment/secret concern).
/// v0 ships [`StaticCredentialProvider`]; [`K8sSecretProvider`] (2.2b,
/// wamn-5x0.1) fills in live per-project Secret reads once 2.3 provisioning
/// fixes the layout.
pub trait CredentialProvider: Send + Sync {
    /// `Ok(Some)` = resolved; `Ok(None)` = unknown project (the caller returns
    /// `connection-unavailable`); `Err` = provider failure (also surfaced as
    /// `connection-unavailable`, logged).
    fn resolve(&self, project: &str) -> anyhow::Result<Option<ProjectConfig>>;
}

/// v0 provider: an in-memory project→config map plus an optional default used
/// for any unlisted project (so a single-DB deployment and the S2 bench work
/// with no map at all). The map is populated from `WAMN_PG_PROJECTS_FILE` (a
/// JSON object mounted like a Secret/ConfigMap) or constructed directly.
pub struct StaticCredentialProvider {
    projects: HashMap<String, ProjectConfig>,
    default: Option<ProjectConfig>,
}

impl StaticCredentialProvider {
    pub fn new(projects: HashMap<String, ProjectConfig>, default: Option<ProjectConfig>) -> Self {
        Self { projects, default }
    }

    /// Default-only provider (single database = the default project).
    pub(super) fn default_only(default: Option<ProjectConfig>) -> Self {
        Self {
            projects: HashMap::new(),
            default,
        }
    }

    /// Parse `{ "<project>": { "url": .., "row_limit"?: .., .. }, .. }`; unset
    /// per-project fields fall back to `base`. Mirrors a mounted projects
    /// Secret/ConfigMap. Public so the 2.3 `provisionbench` gate can feed the
    /// projects-file JSON that `provision-project` emits through the exact parse
    /// path production uses (`from_env`), proving a provisioned project resolves.
    pub fn projects_from_json(
        text: &str,
        base: &WamnPostgresConfig,
    ) -> anyhow::Result<HashMap<String, ProjectConfig>> {
        let v: serde_json::Value =
            serde_json::from_str(text).context("parse WAMN_PG_PROJECTS_FILE json")?;
        let obj = v
            .as_object()
            .context("WAMN_PG_PROJECTS_FILE must be a JSON object")?;
        let mut out = HashMap::new();
        for (name, entry) in obj {
            let url = entry
                .get("url")
                .and_then(|u| u.as_str())
                .with_context(|| format!("project {name:?} missing string \"url\""))?
                .to_string();
            let u64_or = |k: &str, d: u64| entry.get(k).and_then(|n| n.as_u64()).unwrap_or(d);
            out.insert(
                name.clone(),
                ProjectConfig {
                    database_url: url,
                    pool_max_size: u64_or("pool_max_size", base.pool_max_size as u64) as usize,
                    wait_timeout_ms: u64_or("wait_timeout_ms", base.wait_timeout_ms),
                    statement_timeout_ms: u64_or(
                        "statement_timeout_ms",
                        base.statement_timeout_ms as u64,
                    ) as u32,
                    row_limit: u64_or("row_limit", base.row_limit),
                },
            );
        }
        Ok(out)
    }
}

impl CredentialProvider for StaticCredentialProvider {
    fn resolve(&self, project: &str) -> anyhow::Result<Option<ProjectConfig>> {
        Ok(self
            .projects
            .get(project)
            .cloned()
            .or_else(|| self.default.clone()))
    }
}

/// Seam for 2.2b (wamn-5x0.1): resolve `wamn-db-<project>` Secrets from the
/// namespace via a K8s client. Deferred until 2.3 provisioning fixes the Secret
/// layout — defined so the [`CredentialProvider`] wiring is real, but not yet
/// functional (hence unconstructed in v0).
#[allow(dead_code)]
pub struct K8sSecretProvider {
    pub namespace: String,
}

impl CredentialProvider for K8sSecretProvider {
    fn resolve(&self, _project: &str) -> anyhow::Result<Option<ProjectConfig>> {
        anyhow::bail!(
            "K8sSecretProvider (namespace {:?}) is not implemented yet — see wamn-5x0.1 [2.2b]; use StaticCredentialProvider",
            self.namespace
        )
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// A project's live connection pool plus its host-enforced policy (statement
/// timeout + row limit travel with every call made against it).
pub(super) struct ProjectPool {
    pub(super) pool: Pool,
    pub(super) statement_timeout_ms: u32,
    pub(super) row_limit: u64,
}

/// Raw checkout state, observed before any claim injection. Gate probes use
/// this to assert a fresh checkout is transaction-free and claim-free.
#[derive(Debug)]
pub struct CheckoutProbe {
    pub backend_pid: i32,
    /// `current_setting('app.tenant', true)` — must be NULL on a clean conn.
    pub tenant_claim: Option<String>,
    /// `pg_current_xact_id_if_assigned()` — non-NULL means a leaked open
    /// transaction that performed writes.
    pub xact_id: Option<String>,
}

/// R18 (wamn-2jkm.21): this plugin's SQL quoting is only sound when the server
/// has `standard_conforming_strings = on` (the PG default since 9.1). With it
/// on, a backslash inside a `'…'` literal is a literal backslash, so a charset-
/// validated identifier quoted into a literal elsewhere cannot use `\'` to break
/// out. If a server had it OFF the assumption would silently fail, so the plugin
/// asserts it at connection establishment and fails CLOSED otherwise.
fn standard_conforming_strings_ok(setting: &str) -> bool {
    setting == "on"
}

/// The `post_create` deadpool hook that runs the R18 assertion once per new
/// physical connection — one cheap round trip. A server with
/// `standard_conforming_strings` off (or an unreadable setting) fails the
/// connection create, which surfaces to the guest as `connection-unavailable`.
pub(super) fn standard_conforming_strings_hook() -> Hook {
    Hook::async_fn(|client, _metrics| {
        Box::pin(async move {
            let setting: String = client
                .query_one("SHOW standard_conforming_strings", &[])
                .await
                .map_err(|e| {
                    HookError::message(format!("SHOW standard_conforming_strings failed: {e}"))
                })?
                .get(0);
            if standard_conforming_strings_ok(&setting) {
                Ok(())
            } else {
                Err(HookError::message(format!(
                    "standard_conforming_strings is {setting:?}, expected \"on\": \
                     wamn:postgres SQL quoting is unsafe otherwise (R18)"
                )))
            }
        })
    })
}

pub(super) fn destroy_connection(obj: Object, counter: &AtomicU64) {
    // Removes the connection from the pool accounting and closes the socket;
    // the server aborts any open transaction on disconnect. Never repooled.
    let client = Object::take(obj);
    drop(client);
    counter.fetch_add(1, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// HostPlugin
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // R18 — the connect-time check logic. A negative is hard to produce on stock
    // PG18 (the setting defaults on), so the fail-closed branch is asserted here.
    #[test]
    fn standard_conforming_strings_check() {
        assert!(standard_conforming_strings_ok("on"));
        assert!(!standard_conforming_strings_ok("off"));
        assert!(!standard_conforming_strings_ok(""));
        assert!(!standard_conforming_strings_ok("ON"));
    }
}
