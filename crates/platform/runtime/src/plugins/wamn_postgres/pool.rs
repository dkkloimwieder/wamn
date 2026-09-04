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

use wamn_run_state::{AuthorityClass, app_scope_hash};

use super::DEFAULT_PROJECT;
use super::credential_exactness::{
    AmbientCredentialState, ExpectedCredentialIdentity, MembershipExpectation, MembershipMode,
    credential_exactness_probe, explicit_credential_source,
};
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
    /// The default project's per-class credentials. None = plugin registers but
    /// every call returns `connection-unavailable`.
    pub credentials: Option<ClassCredentials>,
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
            credentials: None,
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

/// The credential each [`AuthorityClass`] authenticates with for one project
/// (`wamn-0h0g.22.16`).
///
/// A class this map does not name has NO credential. That is the whole point:
/// resolution REFUSES for such a class rather than handing back whatever login
/// another class was configured with, so an un-provisioned authority is
/// distinguishable from a provisioned one instead of quietly borrowing a shared
/// principal.
///
/// There is deliberately no `From<&str>` for [`AuthorityClass`] here or
/// anywhere: `wamn-0h0g.22.14` ruled the class one-way, so a class is named in
/// code and matched against a configuration key, never parsed out of one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClassCredentials {
    urls: HashMap<AuthorityClass, String>,
}

impl ClassCredentials {
    /// Name ONE url as the credential of EVERY class, explicitly.
    ///
    /// This is the pre-cutover shape and it is not a fallback: the entries are
    /// written down, so `resolve` still selects rather than defaulting. Every
    /// family cutover REPLACES one entry with that family's own login
    /// ([`Self::with_class`]) and leaves the others alone; none of them
    /// reintroduces an implicit shared credential.
    pub fn every_class(url: impl Into<String>) -> Self {
        let url = url.into();
        Self {
            urls: AuthorityClass::ALL
                .into_iter()
                .map(|class| (class, url.clone()))
                .collect(),
        }
    }

    /// Name one class's own credential, replacing any previous entry.
    #[must_use]
    pub fn with_class(mut self, class: AuthorityClass, url: impl Into<String>) -> Self {
        self.urls.insert(class, url.into());
        self
    }

    /// UNNAME one class, so `resolve` REFUSES for it (`wamn-0h0g.22.31`).
    ///
    /// The other half of a family cutover, and it is not the same as leaving
    /// [`Self::every_class`]'s entry in place. Once a family authenticates as
    /// itself, that shared entry stops being a placeholder for it and becomes A
    /// LOGIN OF ANOTHER AUTHORITY that would still satisfy the map. A composer
    /// that cannot name this family's own credential must therefore ERASE the
    /// entry rather than keep the shared one, so an unprovisioned family is
    /// refused at checkout instead of quietly connecting as the guest.
    #[must_use]
    pub fn without_class(mut self, class: AuthorityClass) -> Self {
        self.urls.remove(&class);
        self
    }

    /// The url this class authenticates with, or `None` when it has none.
    pub fn url(&self, class: AuthorityClass) -> Option<&str> {
        self.urls.get(&class).map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.urls.is_empty()
    }
}

/// Per-project credentials + policy. In production one project = one database
/// (plan 2.3); the pool, statement timeout, and row limit are all per-project so
/// one noisy project cannot starve or over-fetch on behalf of another. The
/// CREDENTIAL is per authority class, because two authorities in one database
/// are two different logins.
#[derive(Clone, Debug)]
pub struct ProjectConfig {
    pub credentials: ClassCredentials,
    pub guest_pool_max_size: usize,
    pub platform_pool_max_size: usize,
    pub wait_timeout_ms: u64,
    pub statement_timeout_ms: u32,
    pub row_limit: u64,
}

impl ProjectConfig {
    /// The default project's config, from the single-DB [`WamnPostgresConfig`].
    pub(super) fn from_global(credentials: ClassCredentials, cfg: &WamnPostgresConfig) -> Self {
        Self {
            credentials,
            guest_pool_max_size: cfg.guest_pool_max_size,
            platform_pool_max_size: cfg.platform_pool_max_size,
            wait_timeout_ms: cfg.wait_timeout_ms,
            statement_timeout_ms: cfg.statement_timeout_ms,
            row_limit: cfg.row_limit,
        }
    }

    /// This class's credential, carrying the project's policy. `None` = the
    /// class has no configured credential and the caller must refuse.
    pub fn select(&self, class: AuthorityClass) -> Option<ResolvedCredential> {
        Some(ResolvedCredential {
            database_url: self.credentials.url(class)?.to_string(),
            guest_pool_max_size: self.guest_pool_max_size,
            platform_pool_max_size: self.platform_pool_max_size,
            wait_timeout_ms: self.wait_timeout_ms,
            statement_timeout_ms: self.statement_timeout_ms,
            row_limit: self.row_limit,
        })
    }
}

/// One authority class's resolved connection + the project policy that travels
/// with every call made against it.
#[derive(Clone, Debug)]
pub struct ResolvedCredential {
    pub database_url: String,
    pub guest_pool_max_size: usize,
    pub platform_pool_max_size: usize,
    pub wait_timeout_ms: u64,
    pub statement_timeout_ms: u32,
    pub row_limit: u64,
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
    /// from a guest (`wamn-0h0g.22.8`), and it SELECTS THE CREDENTIAL
    /// (`wamn-0h0g.22.16`): a project names one login per class, and a class it
    /// does not name resolves to an error rather than to another class's login.
    ///
    /// `tenant` is `Some` for guest-SQL and `None` for host-owned platform
    /// work. After `wamn-0h0g.22.6` the guest's tenant comes from
    /// `current_user`, so for that class THE CREDENTIAL IS THE TENANT
    /// AUTHORITY: resolution must refuse rather than hand back a credential
    /// minted for someone else.
    fn resolve(
        &self,
        project: &str,
        class: AuthorityClass,
        tenant: Option<&str>,
    ) -> anyhow::Result<Option<ResolvedCredential>>;
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

    /// Parse `{ "<project>": { "url"?: .., "credentials"?: { "<class>": .. },
    /// "row_limit"?: .., .. }, .. }`; unset per-project fields fall back to
    /// `base`. Mirrors a mounted projects Secret/ConfigMap. Public so the 2.3
    /// `provisionbench` gate can feed the projects-file JSON that
    /// `provision-project` emits through the exact parse path production uses
    /// (`from_env`), proving a provisioned project resolves.
    ///
    /// `"url"` names one login for EVERY class — written out, not defaulted —
    /// and `"credentials"` names one class's own login, overriding it. A class
    /// neither of them names has no credential and is refused at resolution
    /// (`wamn-0h0g.22.16`). An entry naming neither is refused here, because a
    /// project with no credential at all is a configuration error, not a
    /// project that resolves to nothing.
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
            let credentials = class_credentials_from_json(name, entry)?;
            let u64_or = |k: &str, d: u64| entry.get(k).and_then(|n| n.as_u64()).unwrap_or(d);
            out.insert(
                name.clone(),
                ProjectConfig {
                    credentials,
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

/// Read one project entry's per-class credentials.
///
/// The class is MATCHED, never parsed: `wamn-0h0g.22.14` ruled `AuthorityClass`
/// one-way and forbade a `FromStr`/`Deserialize` on it, so this walks the closed
/// class set and compares each label against the configured keys. An
/// unrecognised key is refused rather than ignored — silently dropping it would
/// leave the operator believing a class was configured when it was not.
fn class_credentials_from_json(
    name: &str,
    entry: &serde_json::Value,
) -> anyhow::Result<ClassCredentials> {
    let mut credentials = match entry.get("url") {
        Some(url) => ClassCredentials::every_class(
            url.as_str()
                .with_context(|| format!("project {name:?} \"url\" must be a string"))?,
        ),
        None => ClassCredentials::default(),
    };
    if let Some(per_class) = entry.get("credentials") {
        let per_class = per_class
            .as_object()
            .with_context(|| format!("project {name:?} \"credentials\" must be a JSON object"))?;
        for (label, url) in per_class {
            let class = AuthorityClass::ALL
                .into_iter()
                .find(|class| class.as_str() == label)
                .with_context(|| {
                    format!("project {name:?} names unknown authority class {label:?}")
                })?;
            let url = url.as_str().with_context(|| {
                format!("project {name:?} credential for {label:?} must be a string")
            })?;
            credentials = credentials.with_class(class, url);
        }
    }
    anyhow::ensure!(
        !credentials.is_empty(),
        "project {name:?} names no credential: give it \"url\" or a \"credentials\" entry"
    );
    Ok(credentials)
}

impl CredentialProvider for StaticCredentialProvider {
    /// THE CLASS SELECTS THE CREDENTIAL (`wamn-0h0g.22.16`). A project names one
    /// login per authority class; a class it does not name is REFUSED here, so
    /// an authority that was never provisioned cannot authenticate as one that
    /// was. The class remains part of the POOL KEY as well: two authorities must
    /// never share a pooled session even when they resolve to the same URL.
    ///
    /// FOR GUEST SQL THE TENANT BINDING IS VERIFIED, NOT CONFIGURED. The
    /// credential's login role carries the tenant key as its scope digest, and
    /// the same key is what every governed predicate computes — so the binding
    /// is proven from the credential itself rather than declared beside it,
    /// and a credential minted for another tenant is REFUSED instead of
    /// silently borrowed. Selection happens FIRST and the check runs on the
    /// selected credential, so it fires on exactly the credential that would be
    /// handed back — never on a different class's login.
    fn resolve(
        &self,
        project: &str,
        class: AuthorityClass,
        tenant: Option<&str>,
    ) -> anyhow::Result<Option<ResolvedCredential>> {
        let Some(cfg) = self.projects.get(project) else {
            return Ok(None);
        };
        let resolved = cfg.select(class).with_context(|| {
            format!(
                "project {project:?} names no credential for authority class {class}; refusing \
                 rather than authenticating as another authority's login"
            )
        })?;
        if class == AuthorityClass::GuestSql {
            let tenant = tenant.context(
                "guest-SQL resolution requires the tenant: after wamn-0h0g.22.6 the \
                 credential IS the tenant authority",
            )?;
            let database = credential_database(&resolved.database_url)?;
            let role = credential_generation_role(&resolved.database_url)?;
            let key = app_scope_hash(tenant, &database);
            anyhow::ensure!(
                role.contains(&key),
                "the credential for project {project:?} authenticates as {role:?}, which \
                 does not carry the tenant key for the requested tenant; refusing rather \
                 than reading another tenant's rows"
            );
        }
        Ok(Some(resolved))
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
        _tenant: Option<&str>,
    ) -> anyhow::Result<Option<ResolvedCredential>> {
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

/// The database a resolved URL names.
///
/// Read from the URL rather than configured beside it for the same reason the
/// generation role is: a declared database could disagree with the one that
/// actually connects, and the tenant key is computed over BOTH the tenant and
/// the database — so a disagreement would compute a key for a database the
/// session is not in.
pub(super) fn credential_database(database_url: &str) -> anyhow::Result<String> {
    let config: Config = database_url
        .parse()
        .context("parse the project database url")?;
    let database = config
        .get_dbname()
        .context("the project database url names no database")?;
    Ok(database.to_string())
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

    pub(super) const fn max_size(self, config: &ResolvedCredential) -> usize {
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
/// Refuse any newly-created physical connection whose identity is not exactly
/// the credential this pool resolved (`wamn-0h0g.22.8.4`).
///
/// Wired as a `post_create` hook, the same place the R18
/// `standard_conforming_strings` assertion lives, because that is where a NEW
/// PHYSICAL CONNECTION exists and has not yet been used. deadpool PUSHES
/// post-create hooks rather than replacing them, so both checks run.
///
/// This is what closes the loop that split B opened. B keys the pool on
/// (project, class, generation) taken from the resolved url; the url is a
/// CLAIM about who will connect. The probe is the server's own answer to
/// whether that claim is true: it compares `session_user`, `current_user` and
/// the database against the expectation, and asks the server whether the
/// principal really holds the class's ACL role. A generation mismatch
/// therefore fails the connection rather than serving under a stale principal.
///
/// `AmbientCredentialState::Absent` is asserted, not assumed: split B removed
/// the ambient `WAMN_PG_URL` read, so the url reaching here is the one named
/// explicit source. If a second source is ever reintroduced, this refuses.
/// The membership every physical connection of one class must satisfy.
///
/// A FUNCTION RATHER THAN THREE ARGUMENTS INLINE, because this expression IS the
/// per-family arm of the probe and it was UNTESTABLE inline
/// (`wamn-0h0g.22.31`). [`credential_exactness_hook`] returns a `Hook`, which
/// hides everything it was built from, so a mutant replacing `class.acl_role()`
/// with any fixed role — making every class probe for the guest's ACL role, and
/// so admitting the guest login onto the executor's platform pool — SURVIVED the
/// whole suite. `every_authority_probes_for_its_own_acl_role` asserted only that
/// each class produced a probe and that the four role names differ; it never
/// looked at what the probe expects. Extracted, the expectation is comparable.
fn expected_class_membership(class: AuthorityClass) -> MembershipExpectation {
    MembershipExpectation::new(class.acl_role(), MembershipMode::Member, true)
}

pub(super) fn credential_exactness_hook(
    database_url: &str,
    class: AuthorityClass,
    project: &str,
) -> anyhow::Result<Hook> {
    let source = explicit_credential_source(database_url, project, AmbientCredentialState::Absent)
        .map_err(|error| anyhow::anyhow!("explicit credential source refused: {error}"))?;
    let generation_role = credential_generation_role(database_url)?;
    let config: Config = database_url
        .parse()
        .context("parse the project database url")?;
    let database = config
        .get_dbname()
        .context("the project database url names no database")?
        .to_string();

    let expected = ExpectedCredentialIdentity::new(
        // Both users are the generation role: nothing issues SET ROLE between
        // connect and this hook, so a differing `current_user` means the
        // session is not the principal the url named.
        generation_role.clone(),
        generation_role,
        database,
        project,
        vec![expected_class_membership(class)],
        // ACL expectations are the per-family denial matrix and need a live
        // server to be meaningful; membership is the arm that is checkable on
        // every connection.
        Vec::new(),
    );
    let probe = Arc::new(
        credential_exactness_probe(source, expected)
            .map_err(|error| anyhow::anyhow!("credential exactness refused the source: {error}"))?,
    );

    Ok(Hook::async_fn(move |client, _metrics| {
        let probe = Arc::clone(&probe);
        Box::pin(async move {
            // deadpool hands a `ClientWrapper`; the probe wants the
            // `tokio_postgres::Client` it derefs to.
            probe.probe_pooled(&**client).await.map_err(|error| {
                // The error carries a predicate and a kind, never credential
                // material or server detail.
                HookError::message(format!(
                    "credential exactness refused the connection: {error}"
                ))
            })
        })
    }))
}

/// Apply the project's `statement_timeout` once, when the connection is created.
///
/// The value comes from [`ResolvedCredential`], which is resolved per
/// (project, class, tenant) -- exactly the pool key -- so every borrower of a
/// given connection wants the same number. Paying a `SET` round trip for it on
/// every request is waste: measured at 0.293 ms of each authenticated request
/// (`docs/perf/2026.09/2a-auth-instrument.md`).
///
/// SESSION scope, so a later transaction-LOCAL `set_config` still wins. That is
/// what keeps the guest paths unchanged: they set their own timeout alongside
/// `search_path`, which is per-component and therefore CANNOT be hoisted here.
pub(super) fn session_statement_timeout_hook(statement_timeout_ms: u32) -> Hook {
    let timeout = statement_timeout_ms.to_string();
    Hook::async_fn(move |client, _metrics| {
        let timeout = timeout.clone();
        Box::pin(async move {
            client
                .execute("SELECT set_config('statement_timeout', $1, false)", &[
                    &timeout,
                ])
                .await
                .map_err(|e| {
                    HookError::message(format!("set the session statement_timeout failed: {e}"))
                })?;
            Ok(())
        })
    })
}

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
        project_config(ClassCredentials::every_class(url))
    }

    fn project_config(credentials: ClassCredentials) -> ProjectConfig {
        ProjectConfig {
            credentials,
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 1,
            statement_timeout_ms: 1,
            row_limit: 1,
        }
    }

    fn base() -> WamnPostgresConfig {
        WamnPostgresConfig {
            credentials: None,
            guest_pool_max_size: 14,
            platform_pool_max_size: 2,
            wait_timeout_ms: 100,
            statement_timeout_ms: 100,
            row_limit: 10,
        }
    }

    /// *** THE CLASS SELECTS THE CREDENTIAL. ***
    ///
    /// Before `wamn-0h0g.22.16` `resolve` returned the same url whatever
    /// authority asked, so every family authenticated as the shared `wamn_app`
    /// login. Four classes, four configured logins, four different answers.
    #[test]
    fn each_authority_class_authenticates_with_its_own_configured_credential() {
        let projects = StaticCredentialProvider::projects_from_json(
            r#"{"billing":{"credentials":{
                 "guest-sql":"postgres://wamn_app_g_a:pw@db/billing",
                 "executor-platform":"postgres://wamn_exec_platform_x_a:pw@db/billing",
                 "callable-http":"postgres://wamn_http_admitter_x_a:pw@db/billing",
                 "event-materializer":"postgres://wamn_materializer_x_a:pw@db/billing"}}}"#,
            &base(),
        )
        .expect("per-class credentials parse");
        let cfg = projects.get("billing").expect("project billing");

        let mut seen = Vec::new();
        for class in AuthorityClass::ALL {
            let url = cfg
                .select(class)
                .unwrap_or_else(|| panic!("{class} has a configured credential"))
                .database_url;
            assert!(
                !seen.contains(&url),
                "{class} was handed a credential another authority already holds"
            );
            seen.push(url);
        }
        assert_eq!(seen.len(), 4);
    }

    /// A class with no configured credential REFUSES. The forbidden behaviour is
    /// the quiet one: handing back whichever login happened to be configured for
    /// some other authority, which is how an unprovisioned family becomes
    /// indistinguishable from a provisioned one.
    #[test]
    fn a_class_with_no_configured_credential_is_refused_not_defaulted() {
        let projects = StaticCredentialProvider::projects_from_json(
            r#"{"billing":{"credentials":{"guest-sql":"postgres://wamn_app_g_a:pw@db/billing"}}}"#,
            &base(),
        )
        .expect("a single-class project parses");
        let provider = StaticCredentialProvider::new(projects, None);

        for class in [
            AuthorityClass::ExecutorPlatform,
            AuthorityClass::CallableHttp,
            AuthorityClass::EventMaterializer,
        ] {
            let error = provider
                .resolve("billing", class, None)
                .expect_err("an unconfigured class must refuse");
            assert!(
                format!("{error:#}").contains("names no credential for authority class"),
                "{class} refused for the wrong reason: {error:#}"
            );
        }
    }

    /// The pre-cutover shape, stated rather than defaulted: one `"url"` names
    /// every class EXPLICITLY, so nothing regresses while no family is cut over
    /// and resolution still selects instead of falling back.
    #[test]
    fn one_configured_url_names_every_class_explicitly() {
        let projects = StaticCredentialProvider::projects_from_json(
            r#"{"billing":{"url":"postgres://wamn_app_g_a:pw@db/billing"}}"#,
            &base(),
        )
        .expect("a single-url project parses");
        let cfg = projects.get("billing").expect("project billing");
        for class in AuthorityClass::ALL {
            assert_eq!(
                cfg.select(class).map(|c| c.database_url).as_deref(),
                Some("postgres://wamn_app_g_a:pw@db/billing"),
                "{class} lost the project's only credential"
            );
        }
        // And a per-class entry OVERRIDES that url for its own class alone.
        let projects = StaticCredentialProvider::projects_from_json(
            r#"{"billing":{"url":"postgres://wamn_app_g_a:pw@db/billing",
                 "credentials":{"callable-http":"postgres://wamn_http_admitter_x_a:pw@db/billing"}}}"#,
            &base(),
        )
        .expect("a mixed project parses");
        let cfg = projects.get("billing").expect("project billing");
        assert_eq!(
            cfg.select(AuthorityClass::CallableHttp)
                .map(|c| c.database_url)
                .as_deref(),
            Some("postgres://wamn_http_admitter_x_a:pw@db/billing")
        );
        assert_eq!(
            cfg.select(AuthorityClass::EventMaterializer)
                .map(|c| c.database_url)
                .as_deref(),
            Some("postgres://wamn_app_g_a:pw@db/billing")
        );
    }

    #[test]
    fn a_credential_file_that_names_no_class_or_an_unknown_one_is_refused() {
        assert!(
            StaticCredentialProvider::projects_from_json(r#"{"billing":{}}"#, &base()).is_err(),
            "a project naming no credential at all must refuse"
        );
        let error = StaticCredentialProvider::projects_from_json(
            r#"{"billing":{"credentials":{"guest_sql":"postgres://u_a:pw@db/billing"}}}"#,
            &base(),
        )
        .expect_err("an unrecognised class key must refuse rather than be dropped");
        assert!(format!("{error:#}").contains("unknown authority class"));
    }

    /// *** THE `wamn-0h0g.22.6.7` REFUSAL, ON THE SELECTED CREDENTIAL. ***
    ///
    /// The check must read the credential the GUEST would actually connect with,
    /// not whatever url the project happens to carry for some other authority.
    /// This project's platform login carries `acme`'s tenant key and its
    /// guest login carries `evil`'s, so a check that read the wrong credential
    /// would ADMIT a cross-tenant guest read. Both directions are asserted.
    #[test]
    fn the_guest_tenant_check_reads_the_credential_the_guest_would_connect_with() {
        let credentials = ClassCredentials::every_class(format!(
            "postgres://wamn_app_{}_a:pw@db/billing",
            app_scope_hash("acme", "billing")
        ))
        .with_class(
            AuthorityClass::GuestSql,
            format!(
                "postgres://wamn_app_{}_a:pw@db/billing",
                app_scope_hash("evil", "billing")
            ),
        );
        let mut projects = HashMap::new();
        projects.insert("billing".to_string(), project_config(credentials));
        let provider = StaticCredentialProvider::new(projects, None);

        assert!(
            provider
                .resolve("billing", AuthorityClass::GuestSql, Some("acme"))
                .is_err(),
            "the guest credential is minted for another tenant and must be REFUSED, \
             not borrowed — reading the platform credential here would admit a \
             cross-tenant read"
        );
        assert!(
            provider
                .resolve("billing", AuthorityClass::GuestSql, Some("evil"))
                .expect("the guest credential's own tenant resolves")
                .is_some()
        );
        assert!(
            provider
                .resolve("billing", AuthorityClass::GuestSql, None)
                .is_err(),
            "guest resolution without a tenant has no authority to check"
        );
        // The platform class is project-environment scoped: no tenant binding.
        assert!(
            provider
                .resolve("billing", AuthorityClass::ExecutorPlatform, None)
                .expect("platform resolution needs no tenant")
                .is_some()
        );
    }

    /// The acceptance-forbidden silent fallback. An unprovisioned project used
    /// to be indistinguishable from a provisioned one, because it quietly
    /// borrowed whichever config was configured as the default.
    ///
    /// Resolved as a PLATFORM class deliberately: the subject here is the
    /// project MAP, and guest resolution additionally verifies the credential's
    /// tenant binding (`wamn-0h0g.22.6.7`), which has its own test. Mixing the
    /// two would make a project-selection failure and a tenant-binding failure
    /// indistinguishable.
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
                .resolve("billing", AuthorityClass::ExecutorPlatform, None)
                .expect("resolve billing")
                .is_some(),
            "a listed project must still resolve"
        );
        assert!(
            provider
                .resolve("not-provisioned", AuthorityClass::ExecutorPlatform, None)
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
                .resolve(DEFAULT_PROJECT, AuthorityClass::ExecutorPlatform, None)
                .expect("resolve default")
                .is_some(),
            "the default project must resolve, or single-DB deployments break"
        );
        assert!(
            provider
                .resolve("somethingelse", AuthorityClass::ExecutorPlatform, None)
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

    /// A url that names no credential cannot produce a probe: there would be
    /// nothing to compare the server's answer against.
    #[test]
    fn the_exactness_hook_refuses_a_url_that_names_no_credential() {
        assert!(
            credential_exactness_hook("postgres://host:5432/db", AuthorityClass::GuestSql, "acme")
                .is_err(),
            "a url with no user names no principal"
        );
    }

    /// The binding must name the project it was resolved for. An empty binding
    /// would make the probe agree with anything.
    #[test]
    fn the_exactness_hook_refuses_an_unbound_project() {
        assert!(
            credential_exactness_hook(
                "postgres://wamn_app_9f3c_a:pw@host:5432/db",
                AuthorityClass::GuestSql,
                "",
            )
            .is_err(),
            "an empty tenant binding must refuse"
        );
    }

    /// Every authority can be probed, and each expects ITS OWN acl role. The
    /// role strings are the arm that makes the probe a per-family check rather
    /// than a generic connection test.
    #[test]
    fn every_authority_probes_for_its_own_acl_role() {
        // THE ROLE NAMES ARE SPELLED OUT, NOT DERIVED (`wamn-0h0g.22.31`). This
        // arm previously asserted only that a probe was produced and that the
        // four `acl_role()` values differ — a claim about the enum, not about
        // the probe — so a hook that expected one fixed role for every class
        // passed it. The literals below are the second document the expectation
        // is checked against, and they are what makes the arm per-family.
        let expected: [(AuthorityClass, &str); 4] = [
            (AuthorityClass::GuestSql, "wamn_app"),
            (AuthorityClass::ExecutorPlatform, "wamn_executor_platform"),
            (AuthorityClass::CallableHttp, "wamn_http_admitter"),
            (AuthorityClass::EventMaterializer, "wamn_event_materializer"),
        ];
        let mut roles = Vec::new();
        for class in AuthorityClass::ALL {
            assert!(
                credential_exactness_hook(
                    "postgres://wamn_gen_9f3c_a:pw@host:5432/billingdb",
                    class,
                    "billing",
                )
                .is_ok(),
                "{class} must produce a probe"
            );
            let (_, role) = expected
                .iter()
                .find(|(named, _)| *named == class)
                .unwrap_or_else(|| panic!("{class} is unlisted; a new class needs a row here"));
            assert_eq!(
                expected_class_membership(class),
                MembershipExpectation::new(*role, MembershipMode::Member, true),
                "{class} must probe for {role} as an inherited member",
            );
            assert!(
                !roles.contains(&class.acl_role()),
                "{class} shares an acl role with an earlier authority; the \
                 per-family arm would not distinguish them"
            );
            roles.push(class.acl_role());
        }
        assert_eq!(roles.len(), 4);
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
                WamnPostgresConfig::from_env().credentials.is_none(),
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
        let base = base();
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
