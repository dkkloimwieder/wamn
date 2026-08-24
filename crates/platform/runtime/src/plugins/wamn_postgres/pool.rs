//! Connection pooling + credential resolution for `wamn:postgres` (SR4 split,
//! wamn-cjv.18): the per-project config/policy, the `CredentialProvider` seam,
//! the live `ProjectPool`, the R18 connect-time assertion, and connection
//! teardown. The pool/claim METHODS live on `WamnPostgres` in `claims.rs`.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context as _;
use deadpool_postgres::{Hook, HookError, Object, Pool};
use tokio::sync::mpsc;
use tokio_postgres::{AsyncMessage, Client, Config, Error, NoTls};

use crate::engine::MAX_HOST_CALL_DURATION;

/// Async traffic surfaced by connections created inside the existing platform
/// pool. The wiring doorbell consumes only the backend it checked out and put
/// into LISTEN; messages from ordinary platform checkouts are ignored by id.
#[derive(Debug)]
pub(crate) enum PlatformAsyncMessage {
    Notification {
        backend_pid: i32,
        channel: String,
        payload: String,
    },
    Disconnected {
        backend_pid: i32,
    },
}

/// The existing platform pool's connector, with async PostgreSQL messages kept
/// visible instead of discarded by the generic connection driver.
#[derive(Clone)]
pub(crate) struct PlatformConnect {
    messages: mpsc::UnboundedSender<PlatformAsyncMessage>,
}

impl PlatformConnect {
    pub(crate) fn new(messages: mpsc::UnboundedSender<PlatformAsyncMessage>) -> Self {
        Self { messages }
    }
}

impl deadpool_postgres::Connect for PlatformConnect {
    fn connect(
        &self,
        config: &Config,
    ) -> Pin<
        Box<dyn Future<Output = Result<(Client, tokio::task::JoinHandle<()>), Error>> + Send + '_>,
    > {
        let config = config.clone();
        let messages = self.messages.clone();
        Box::pin(async move {
            let (client, mut connection) = config.connect(NoTls).await?;
            let backend_pid = Arc::new(std::sync::atomic::AtomicI32::new(0));
            let driver_pid = Arc::clone(&backend_pid);
            let task = tokio::spawn(async move {
                while let Some(message) =
                    futures_util::future::poll_fn(|context| connection.poll_message(context)).await
                {
                    match message {
                        Ok(AsyncMessage::Notification(notification)) => {
                            let _ = messages.send(PlatformAsyncMessage::Notification {
                                // `Notification::process_id` names the backend
                                // that executed NOTIFY, not this connection.
                                // Routing belongs to the listener connection
                                // whose driver observed it.
                                backend_pid: driver_pid.load(Ordering::Acquire),
                                channel: notification.channel().to_owned(),
                                payload: notification.payload().to_owned(),
                            });
                        }
                        Ok(AsyncMessage::Notice(notice)) => {
                            tracing::debug!(message = %notice, "wamn:postgres server notice");
                        }
                        Err(error) => {
                            tracing::warn!(error = %error, "wamn:postgres connection failed");
                            break;
                        }
                        _ => {}
                    }
                }
                let _ = messages.send(PlatformAsyncMessage::Disconnected {
                    backend_pid: driver_pid.load(Ordering::Acquire),
                });
            });
            let pid: i32 = match client
                .query_one("SELECT pg_backend_pid()", &[])
                .await
                .and_then(|row| row.try_get(0))
            {
                Ok(pid) => pid,
                Err(error) => {
                    task.abort();
                    return Err(error);
                }
            };
            backend_pid.store(pid, Ordering::Release);
            Ok((client, task))
        })
    }
}

const DEFAULT_GUEST_POOL_MAX_SIZE: usize = 14;
const DEFAULT_PLATFORM_POOL_MAX_SIZE: usize = 2;

fn bounded_wait_timeout_ms(value: u64) -> u64 {
    value.clamp(1, MAX_HOST_CALL_DURATION.as_millis() as u64)
}

fn bounded_statement_timeout_ms(value: u64) -> u32 {
    value.clamp(1, MAX_HOST_CALL_DURATION.as_millis() as u64) as u32
}

#[derive(Clone, Debug)]
pub struct WamnPostgresConfig {
    /// `postgres://user:pass@host:port/db`. None = plugin registers but every
    /// call returns `connection-unavailable`.
    pub database_url: Option<String>,
    /// Connections reserved for guest-visible `wamn:postgres` calls.
    pub guest_pool_max_size: usize,
    /// Connections reserved for host-owned claim, authorization, and plan-supply work.
    pub platform_pool_max_size: usize,
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
            database_url: std::env::var("WAMN_PG_URL").ok(),
            // Preserve the former total of 16 while reserving one measured
            // platform operation plus one headroom slot against guest starvation.
            guest_pool_max_size: num("WAMN_PG_GUEST_POOL_MAX", DEFAULT_GUEST_POOL_MAX_SIZE),
            platform_pool_max_size: num(
                "WAMN_PG_PLATFORM_POOL_MAX",
                DEFAULT_PLATFORM_POOL_MAX_SIZE,
            ),
            wait_timeout_ms: bounded_wait_timeout_ms(num("WAMN_PG_WAIT_TIMEOUT_MS", 2_000)),
            statement_timeout_ms: bounded_statement_timeout_ms(num(
                "WAMN_PG_STATEMENT_TIMEOUT_MS",
                5_000,
            )),
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
    pub guest_pool_max_size: usize,
    pub platform_pool_max_size: usize,
    pub wait_timeout_ms: u64,
    pub statement_timeout_ms: u32,
    pub row_limit: u64,
}

impl ProjectConfig {
    /// The default project's config, from the single-DB [`WamnPostgresConfig`].
    pub(super) fn from_global(url: String, cfg: &WamnPostgresConfig) -> Self {
        Self {
            database_url: url,
            guest_pool_max_size: cfg.guest_pool_max_size,
            platform_pool_max_size: cfg.platform_pool_max_size,
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
            anyhow::ensure!(
                entry.get("pool_max_size").is_none(),
                "project {name:?} uses retired \"pool_max_size\"; configure \
                 \"guest_pool_max_size\" and \"platform_pool_max_size\" separately"
            );
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
                    guest_pool_max_size: u64_or(
                        "guest_pool_max_size",
                        base.guest_pool_max_size as u64,
                    ) as usize,
                    platform_pool_max_size: u64_or(
                        "platform_pool_max_size",
                        base.platform_pool_max_size as u64,
                    ) as usize,
                    wait_timeout_ms: bounded_wait_timeout_ms(u64_or(
                        "wait_timeout_ms",
                        base.wait_timeout_ms,
                    )),
                    statement_timeout_ms: bounded_statement_timeout_ms(u64_or(
                        "statement_timeout_ms",
                        base.statement_timeout_ms as u64,
                    )),
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

/// Connection lifecycle whose pool owns a checked-out PostgreSQL session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PoolLifecycle {
    Guest,
    Platform,
}

impl PoolLifecycle {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Guest => "guest",
            Self::Platform => "platform",
        }
    }

    pub(super) const fn max_size(self, config: &ProjectConfig) -> usize {
        match self {
            Self::Guest => config.guest_pool_max_size,
            Self::Platform => config.platform_pool_max_size,
        }
    }
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
    use super::super::production_half;
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

    #[test]
    fn configured_database_waits_are_finite_and_nonzero() {
        let max = MAX_HOST_CALL_DURATION.as_millis() as u64;
        assert_eq!(bounded_wait_timeout_ms(0), 1);
        assert_eq!(bounded_wait_timeout_ms(u64::MAX), max);
        assert_eq!(bounded_statement_timeout_ms(0), 1);
        assert_eq!(u64::from(bounded_statement_timeout_ms(u64::MAX)), max);
    }

    #[test]
    fn project_config_names_each_lifecycle_budget_and_refuses_the_retired_key() {
        let base = WamnPostgresConfig {
            database_url: None,
            guest_pool_max_size: 14,
            platform_pool_max_size: 2,
            wait_timeout_ms: 100,
            statement_timeout_ms: 100,
            row_limit: 10,
        };
        let projects = StaticCredentialProvider::projects_from_json(
            r#"{"p":{"url":"postgres://localhost/p","guest_pool_max_size":7,"platform_pool_max_size":3}}"#,
            &base,
        )
        .expect("separately named lifecycle budgets parse");
        let project = projects.get("p").expect("project p");
        assert_eq!(project.guest_pool_max_size, 7);
        assert_eq!(project.platform_pool_max_size, 3);

        let retired = StaticCredentialProvider::projects_from_json(
            r#"{"p":{"url":"postgres://localhost/p","pool_max_size":10}}"#,
            &base,
        )
        .expect_err("the ambiguous pre-split key must refuse");
        assert!(retired.to_string().contains("retired \"pool_max_size\""));
    }

    #[test]
    fn shipping_manifests_pin_the_split_budget_and_its_starvation_rationale() {
        assert_eq!(
            DEFAULT_GUEST_POOL_MAX_SIZE + DEFAULT_PLATFORM_POOL_MAX_SIZE,
            16
        );
        assert_eq!(DEFAULT_PLATFORM_POOL_MAX_SIZE, 2);

        // `deploy/platform/runner.yaml` was the other pinned carrier until
        // wamn-0h0g.26.7.2 (ea71c1c4) deleted it with the rest of the runner
        // deployment. Its successor `deploy/platform/executor.yaml` carries the
        // two budgets but not the starvation rationale, so it is not a
        // substitute for this pin without a deploy-side change this lane does
        // not own (wamn-0h0g.11.55.3).
        let manifest = include_str!("../../../../../../deploy/platform/values-host-default.yaml");
        assert!(manifest.contains("WAMN_PG_GUEST_POOL_MAX, value: \"14\""));
        assert!(manifest.contains("WAMN_PG_PLATFORM_POOL_MAX, value: \"2\""));
        assert!(manifest.contains("cannot starve"));

        // Implementation half only: a whole-file scan lets this assertion's own
        // spelling of the retired knob answer for the code it watches
        // (wamn-3o3a).
        let source = production_half(include_str!("pool.rs"), "pool.rs");
        assert!(!source.contains("num(\"WAMN_PG_POOL_MAX\""));
    }
}
