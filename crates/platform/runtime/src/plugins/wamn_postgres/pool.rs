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

use wamn_run_state::AuthorityClass;

use super::DEFAULT_PROJECT;
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
            // wamn-0h0g.22.8.2: the ambient WAMN_PG_URL read is GONE. A
            // credential must arrive through an explicit source, because
            // `credential_exactness::AmbientCredentialState` already states the
            // contract that an explicit source plus ANY ambient source is a
            // CONFLICT. Reading the environment here made the runtime its own
            // second source, which is the acceptance-forbidden behaviour.
            database_url: None,
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
    ///
    /// `class` is the trusted caller's [`AuthorityClass`]. It never originates
    /// from a guest (`wamn-0h0g.22.8`), and it is part of the resolution rather
    /// than a post-hoc filter so a provider can hand different authorities
    /// different credentials for the same project.
    fn resolve(
        &self,
        project: &str,
        class: AuthorityClass,
    ) -> anyhow::Result<Option<ProjectConfig>>;
}

/// v0 provider: an in-memory project→config map populated from
/// `WAMN_PG_PROJECTS_FILE` (a JSON object mounted like a Secret/ConfigMap) or
/// constructed directly.
///
/// `wamn-0h0g.22.8.2`: THERE IS NO CATCH-ALL DEFAULT. A config supplied as the
/// "default" is registered under [`DEFAULT_PROJECT`] like any other project, so
/// a single-DB deployment and the S2 bench still resolve, while an UNLISTED
/// project now FAILS instead of silently borrowing another project's
/// credential. Silent fallback is the behaviour the parent acceptance forbids:
/// it made an unprovisioned project indistinguishable from a provisioned one.
pub struct StaticCredentialProvider {
    projects: HashMap<String, ProjectConfig>,
}

impl StaticCredentialProvider {
    /// `default`, when present, is registered under [`DEFAULT_PROJECT`] rather
    /// than acting as a fallback for every unlisted project.
    pub fn new(projects: HashMap<String, ProjectConfig>, default: Option<ProjectConfig>) -> Self {
        let mut projects = projects;
        if let Some(cfg) = default {
            projects.entry(DEFAULT_PROJECT.to_string()).or_insert(cfg);
        }
        Self { projects }
    }

    /// Default-only provider (single database = the default project).
    pub(super) fn default_only(default: Option<ProjectConfig>) -> Self {
        Self::new(HashMap::new(), default)
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
    fn resolve(
        &self,
        project: &str,
        _class: AuthorityClass,
    ) -> anyhow::Result<Option<ProjectConfig>> {
        // One credential per project in v0, so the class does not select here.
        // It still travels through the seam because it is part of the POOL KEY:
        // two authorities must never share a pooled session even when they
        // resolve to the same URL.
        Ok(self.projects.get(project).cloned())
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
    fn resolve(
        &self,
        _project: &str,
        _class: AuthorityClass,
    ) -> anyhow::Result<Option<ProjectConfig>> {
        anyhow::bail!(
            "K8sSecretProvider (namespace {:?}) is not implemented yet — see wamn-5x0.1 [2.2b]; use StaticCredentialProvider",
            self.namespace
        )
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// The identity a pooled session is cached under (`wamn-0h0g.22.8.2`).
///
/// Three components, all load-bearing:
///
/// * `project` — which database.
/// * `class` — which authority. Two authorities must never share a pooled
///   session even when they resolve to the same URL, because the session
///   carries the connected role.
/// * `generation_role` — WHICH CREDENTIAL GENERATION. See
///   [`credential_generation_role`]; a rotation produces a different role and
///   therefore a different key, so a stale principal is unreachable rather
///   than merely unlikely.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct PoolKey {
    project: Box<str>,
    class: AuthorityClass,
    generation_role: Box<str>,
}

impl PoolKey {
    pub(super) fn new(project: &str, class: AuthorityClass, generation_role: &str) -> Self {
        Self {
            project: project.into(),
            class,
            generation_role: generation_role.into(),
        }
    }

    pub(super) fn project(&self) -> &str {
        &self.project
    }
}

/// The credential generation identity a resolved URL carries.
///
/// The provisioner mints an A/B credential generation AS THE LOGIN ROLE
/// (`wamn_app_<scope-hash>_a` / `_b`, `wamn-0h0g.13.59`), so the generation
/// already travels inside the URL's user component. Deriving it here rather
/// than declaring a second field is deliberate: a declared generation could
/// disagree with the URL that actually authenticates, and then the key would
/// certify an identity the session does not have.
///
/// Note this is NOT `lease_generation`. That is the run-queue lease fence and
/// has nothing to do with credential rotation; conflating them in a pool key
/// would tie session reuse to queue state.
pub(super) fn credential_generation_role(database_url: &str) -> anyhow::Result<String> {
    let config: Config = database_url
        .parse()
        .context("parse the project database url")?;
    let user = config
        .get_user()
        .context("the project database url names no user, so it carries no credential identity")?;
    Ok(user.to_string())
}

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
    /// Which pool cache an authority class draws from.
    ///
    /// The class SUBSUMES the lifecycle, so a caller never pairs the two by
    /// hand and a guest-sql checkout against the platform cache is
    /// unrepresentable rather than merely unreviewed. Exhaustive on purpose: a
    /// new class is a compile error here, not a silent Platform default.
    pub(super) const fn for_class(class: AuthorityClass) -> Self {
        match class {
            AuthorityClass::GuestSql => Self::Guest,
            AuthorityClass::ExecutorPlatform
            | AuthorityClass::CallableHttp
            | AuthorityClass::EventMaterializer => Self::Platform,
        }
    }

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
    use super::*;

    /// Marks the re-exec'd child of the ambient-credential test below.
    const AMBIENT_PROBE_VAR: &str = "WAMN_POOL_AMBIENT_PROBE";

    fn config(url: &str) -> ProjectConfig {
        ProjectConfig {
            database_url: url.to_string(),
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 1,
            statement_timeout_ms: 1,
            row_limit: 1,
        }
    }

    /// The acceptance-forbidden silent fallback. An unprovisioned project used
    /// to be indistinguishable from a provisioned one, because it quietly
    /// borrowed whichever config was configured as the default.
    #[test]
    fn an_unlisted_project_does_not_borrow_another_projects_credential() {
        let mut projects = HashMap::new();
        projects.insert(
            "billing".to_string(),
            config("postgres://wamn_app_billing_a:pw@db/billing"),
        );
        let provider =
            StaticCredentialProvider::new(projects, Some(config("postgres://d_a:pw@db/d")));

        assert!(
            provider
                .resolve("billing", AuthorityClass::GuestSql)
                .expect("resolve billing")
                .is_some(),
            "a listed project must still resolve"
        );
        assert!(
            provider
                .resolve("not-provisioned", AuthorityClass::GuestSql)
                .expect("resolve unlisted")
                .is_none(),
            "an unlisted project must FAIL, not fall back onto the default              project's credential"
        );
    }

    /// The single-DB deployment and the S2 bench must keep working: the default
    /// config is registered UNDER the default project rather than deleted.
    #[test]
    fn the_default_config_is_registered_under_the_default_project() {
        let provider =
            StaticCredentialProvider::default_only(Some(config("postgres://wamn_app_d_a:pw@db/d")));
        assert!(
            provider
                .resolve(DEFAULT_PROJECT, AuthorityClass::GuestSql)
                .expect("resolve default")
                .is_some(),
            "the default project must resolve, or single-DB deployments break"
        );
        assert!(
            provider
                .resolve("somethingelse", AuthorityClass::GuestSql)
                .expect("resolve other")
                .is_none(),
            "registering the default must not resurrect the catch-all"
        );
    }

    /// The generation is the LOGIN ROLE, carried in the url's user component.
    #[test]
    fn the_credential_generation_is_the_login_role_the_url_carries() {
        assert_eq!(
            credential_generation_role("postgres://wamn_app_9f3c_a:pw@host:5432/db")
                .expect("parse generation role"),
            "wamn_app_9f3c_a"
        );
        assert_eq!(
            credential_generation_role("postgres://wamn_app_9f3c_b:pw@host:5432/db")
                .expect("parse generation role"),
            "wamn_app_9f3c_b",
            "the A and B generations are different roles, so they are different keys"
        );
        assert!(
            credential_generation_role("postgres://host:5432/db").is_err(),
            "a url naming no user carries no credential identity and must be refused              rather than pooled under an empty generation"
        );
    }

    /// Rotation must be unreachable, not merely unlikely.
    #[test]
    fn a_rotated_credential_is_a_different_pool_key() {
        let a = PoolKey::new("billing", AuthorityClass::GuestSql, "wamn_app_9f3c_a");
        let b = PoolKey::new("billing", AuthorityClass::GuestSql, "wamn_app_9f3c_b");
        assert_ne!(a, b, "an A->B rotation must not reuse the stale pool");
    }

    /// Two authorities must never share a pooled session even when they resolve
    /// to the same url, because the session carries the connected role.
    #[test]
    fn two_authorities_never_share_a_pool_key() {
        let mut seen = Vec::new();
        for class in AuthorityClass::ALL {
            let key = PoolKey::new("billing", class, "wamn_shared_9f3c_a");
            assert!(
                !seen.contains(&key),
                "{class} collided with an earlier authority on one pool key"
            );
            seen.push(key);
        }
        assert_eq!(seen.len(), 4);
    }

    /// The class subsumes the lifecycle, so a guest-sql checkout against the
    /// platform cache is unrepresentable rather than merely unreviewed.
    #[test]
    fn the_authority_class_selects_the_cache() {
        assert_eq!(
            PoolLifecycle::for_class(AuthorityClass::GuestSql).label(),
            "guest"
        );
        for class in [
            AuthorityClass::ExecutorPlatform,
            AuthorityClass::CallableHttp,
            AuthorityClass::EventMaterializer,
        ] {
            assert_eq!(
                PoolLifecycle::for_class(class).label(),
                "platform",
                "{class} is host-owned platform work"
            );
        }
    }

    /// The ambient credential source is gone.
    ///
    /// Proven in a CHILD PROCESS that actually has `WAMN_PG_URL` set, because
    /// asserting `is_none()` in a parent where the variable happens to be unset
    /// proves nothing, and `std::env::set_var` is `unsafe` in Rust 2024
    /// precisely because it races every concurrent reader. A re-exec gives a
    /// real positive condition with no data race.
    #[test]
    fn from_env_ignores_an_ambient_database_url() {
        if std::env::var(AMBIENT_PROBE_VAR).is_ok() {
            assert!(
                std::env::var("WAMN_PG_URL").is_ok(),
                "the child must run WITH the ambient url set, or it proves nothing"
            );
            assert!(
                WamnPostgresConfig::from_env().database_url.is_none(),
                "from_env read an ambient WAMN_PG_URL; the runtime must not be its                  own second credential source"
            );
            return;
        }

        let path = module_path!();
        let filter = path.split_once("::").map_or(path, |(_, rest)| rest);
        let exe = std::env::current_exe().expect("test binary path");
        let status = std::process::Command::new(exe)
            .args([
                "--exact",
                &format!("{filter}::from_env_ignores_an_ambient_database_url"),
            ])
            .env(AMBIENT_PROBE_VAR, "1")
            .env(
                "WAMN_PG_URL",
                "postgres://ambient_a:pw@ambient-host/ambient",
            )
            .status()
            .expect("re-exec this test binary with an ambient url");
        assert!(
            status.success(),
            "the child, running with WAMN_PG_URL set, saw from_env pick it up"
        );
    }

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
}
