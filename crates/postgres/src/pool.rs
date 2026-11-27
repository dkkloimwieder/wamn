//! Connection pools keyed by project and login, credential resolution, and teardown.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context as _;
use sqlx::PgPool;
pub type Object = sqlx::pool::PoolConnection<sqlx::Postgres>;

use wamn_identity_protocol::AuthorityClass;
use wamn_registry::DEFAULT_PROJECT;

use wamn_app::MAX_HOST_CALL_DURATION;

const DEFAULT_GUEST_POOL_MAX_SIZE: usize = 14;
const DEFAULT_PLATFORM_POOL_MAX_SIZE: usize = 2;

/// The host call ceiling in milliseconds. The constant is well under an hour,
/// so it fits every width these bounds convert to.
fn max_host_call_ms() -> u64 {
    u64::try_from(MAX_HOST_CALL_DURATION.as_millis()).unwrap_or(u64::MAX)
}

pub(super) fn bounded_wait_timeout_ms(value: u64) -> u64 {
    value.clamp(1, max_host_call_ms())
}

pub(super) fn bounded_statement_timeout_ms(value: u64) -> u32 {
    u32::try_from(value.clamp(1, max_host_call_ms())).unwrap_or(u32::MAX)
}

#[derive(Clone, Debug)]
pub struct WamnPostgresConfig {
    /// The default project's per-class credentials. None = plugin registers but
    /// every call returns `connection-unavailable`.
    pub credentials: Option<ClassCredentials>,
    /// Connections reserved for guest-visible `wamn:postgres` calls.
    pub guest_pool_max_size: usize,
    /// Connections reserved for host-owned identity, authorization, and plan-supply work.
    pub platform_pool_max_size: usize,
    /// Max wait for a pool checkout before `connection-unavailable`.
    pub wait_timeout_ms: u64,
    /// Host-enforced `statement_timeout`, injected per transaction.
    pub statement_timeout_ms: u32,
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
            // platform operation plus capacity against guest starvation.
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
    /// Name one class's own credential, replacing any previous entry.
    #[must_use]
    pub fn with_class(mut self, class: AuthorityClass, url: impl Into<String>) -> Self {
        self.urls.insert(class, url.into());
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
/// (plan 2.3); the pool and statement timeout are per-project so
/// one noisy project cannot starve another. The
/// CREDENTIAL is per authority class, because two authorities in one database
/// are two different logins.
#[derive(Clone, Debug)]
pub struct ProjectConfig {
    pub credentials: ClassCredentials,
    pub guest_pool_max_size: usize,
    pub platform_pool_max_size: usize,
    pub wait_timeout_ms: u64,
    pub statement_timeout_ms: u32,
}

impl ProjectConfig {
    /// The default project's config, consuming the single-DB
    /// [`WamnPostgresConfig`]. `None` when it names no credential.
    pub(super) fn from_global_config(cfg: WamnPostgresConfig) -> Option<Self> {
        let WamnPostgresConfig {
            credentials,
            guest_pool_max_size,
            platform_pool_max_size,
            wait_timeout_ms,
            statement_timeout_ms,
        } = cfg;
        Some(Self {
            credentials: credentials?,
            guest_pool_max_size,
            platform_pool_max_size,
            wait_timeout_ms,
            statement_timeout_ms,
        })
    }

    /// The default project's config, from the single-DB [`WamnPostgresConfig`].
    pub(super) fn from_global(credentials: ClassCredentials, cfg: &WamnPostgresConfig) -> Self {
        Self {
            credentials,
            guest_pool_max_size: cfg.guest_pool_max_size,
            platform_pool_max_size: cfg.platform_pool_max_size,
            wait_timeout_ms: cfg.wait_timeout_ms,
            statement_timeout_ms: cfg.statement_timeout_ms,
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
}

/// Resolves a project id to its database connection + policy. This is the seam
/// that separates *which project am I* (a host-injected identity, non-spoofable)
/// from *where does that project's data live* (a deployment/secret concern).
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
    /// `project_env_key` is `Some` for guest-SQL and `None` for host-owned platform
    /// work. After `wamn-0h0g.22.6` the guest's project comes from
    /// `current_user`, so for that class THE CREDENTIAL IS THE PROJECT
    /// AUTHORITY: resolution must refuse rather than hand back a credential
    /// issued for someone else.
    fn resolve(
        &self,
        project: &wamn_registry::ProjectId,
        class: AuthorityClass,
        project_env_key: Option<&wamn_registry::ProjectEnvKey>,
    ) -> anyhow::Result<Option<ResolvedCredential>>;
}

/// An in-memory project→config map built by the composition root or at host bind.
///
/// `wamn-0h0g.22.8.2`: THERE IS NO CATCH-ALL DEFAULT. A config supplied as the
/// "default" is registered under [`DEFAULT_PROJECT`] like any other project, so
/// a single-DB deployment and the S2 bench still resolve, while an UNLISTED
/// project now FAILS instead of silently borrowing another project's
/// credential. Silent fallback is the behaviour the parent acceptance forbids:
/// it made an unprovisioned project indistinguishable from a provisioned one.
#[derive(Debug, Default)]
pub struct StaticCredentialProvider {
    projects: HashMap<wamn_registry::ProjectId, ProjectConfig>,
}

impl StaticCredentialProvider {
    /// `default`, when present, is registered under [`DEFAULT_PROJECT`] rather
    /// than acting as a fallback for every unlisted project.
    pub fn new(
        projects: HashMap<wamn_registry::ProjectId, ProjectConfig>,
        default: Option<ProjectConfig>,
    ) -> Self {
        let mut projects = projects;
        if let Some(cfg) = default {
            projects.entry((*DEFAULT_PROJECT).clone()).or_insert(cfg);
        }
        Self { projects }
    }

    pub(super) fn project_credentials(
        &self,
        project: &wamn_registry::ProjectId,
    ) -> Option<&ClassCredentials> {
        self.projects.get(project).map(|config| &config.credentials)
    }

    /// Register a project's explicit credentials without changing other projects.
    pub fn insert(&mut self, project: wamn_registry::ProjectId, config: ProjectConfig) {
        self.projects.insert(project, config);
    }

    /// Default-only provider (single database = the default project).
    pub(super) fn default_only(default: Option<ProjectConfig>) -> Self {
        Self::new(HashMap::new(), default)
    }
}

impl CredentialProvider for StaticCredentialProvider {
    /// THE CLASS SELECTS THE CREDENTIAL (`wamn-0h0g.22.16`). A project names one
    /// login per authority class; a class it does not name is REFUSED here, so
    /// an authority that was never provisioned cannot authenticate as one that
    /// was. The class remains part of the POOL KEY as well: two authorities must
    /// never share a pooled session even when they resolve to the same URL.
    ///
    /// Guest SQL credentials must match the selected login class and the
    /// endpoint of the requested project database generation.
    fn resolve(
        &self,
        project: &wamn_registry::ProjectId,
        class: AuthorityClass,
        project_env_key: Option<&wamn_registry::ProjectEnvKey>,
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
        if matches!(class, AuthorityClass::GuestSql | AuthorityClass::DynamicSql) {
            let key = project_env_key.context("guest SQL requires a project coordinate")?;
            let triple = key.triple();
            let expected = if class == AuthorityClass::GuestSql {
                wamn_role::APP_LOGIN
            } else {
                wamn_role::QUERY_LOGIN
            };
            let options: sqlx::postgres::PgConnectOptions =
                crate::configuration(&resolved.database_url)?.parse()?;
            let database = options
                .get_database()
                .context("project database is absent")?;
            anyhow::ensure!(
                &triple.project == project
                    && options.get_username() == expected
                    && wamn_registry::name::project_database_key(database).as_ref() == Some(key)
                    && options.get_host()
                        == wamn_registry::name::project_database_host(
                            database,
                            &wamn_registry::name::env_key(&triple.org, &triple.env),
                        )
                    && options.get_port() == 5432
                    && options.get_socket().is_none(),
                "the {class} credential does not match project {key} and its database endpoint"
            );
        }
        Ok(Some(resolved))
    }
}

// ---------------------------------------------------------------------------
// Pools
// ---------------------------------------------------------------------------

/// A pool belongs to one project and authenticated login.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct PoolKey {
    pub(super) project: wamn_registry::ProjectId,
    login: wamn_role::LoginName,
}

impl PoolKey {
    pub(super) fn new(project: &wamn_registry::ProjectId, login: wamn_role::LoginName) -> Self {
        Self {
            project: project.clone(),
            login,
        }
    }
}

/// An open project pool and the statement timeout applied to its transactions.
#[derive(Debug)]
pub struct ProjectPool {
    pub(super) pool: PgPool,
    pub statement_timeout_ms: u32,
}

/// Discard a connection whose transaction outcome or session state is unknown.
pub fn destroy_connection(obj: Object, counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
    drop(obj.detach());
}

/// Names the shape only: the credential provider and the live pools carry
/// connection material that must not reach a log.
impl std::fmt::Debug for WamnPostgres {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WamnPostgres")
            .finish_non_exhaustive()
    }
}

/// Owns the configured project logins and their lazily opened pools.
pub struct WamnPostgres {
    /// Resolves a project id → its database connection + policy.
    pub(super) provider: Arc<dyn CredentialProvider>,
    pub(super) host_projects:
        std::collections::BTreeMap<wamn_registry::ProjectId, crate::host_project::HostProject>,
    pub(super) host_credentials: Option<Arc<crate::host_project::HostCredentials>>,
    pub(super) host_project_bind: tokio::sync::Mutex<()>,
    /// Pools indexed by project and authenticated login.
    pub(super) pools: std::sync::RwLock<HashMap<PoolKey, Arc<ProjectPool>>>,
    /// Connections destroyed instead of repooled (chaos-acceptance-test observability).
    destroyed: Arc<AtomicU64>,
}

impl WamnPostgres {
    /// Pools for a single default database (the [`WamnPostgresConfig`]
    /// credentials). Pools are built lazily; `credentials: None` ⇒ every call
    /// returns `connection-unavailable`.
    pub fn new(cfg: WamnPostgresConfig) -> anyhow::Result<Self> {
        Ok(Self::with_provider(Arc::new(
            StaticCredentialProvider::default_only(ProjectConfig::from_global_config(cfg)),
        )))
    }

    /// Pools for an explicit [`CredentialProvider`] (multi-project / tests).
    pub fn with_provider(provider: Arc<dyn CredentialProvider>) -> Self {
        Self {
            provider,
            host_projects: std::collections::BTreeMap::new(),
            host_credentials: None,
            host_project_bind: tokio::sync::Mutex::new(()),
            pools: std::sync::RwLock::new(HashMap::new()),
            destroyed: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Read the pool policy from the environment and use the supplied credentials.
    pub fn from_env(credentials: Option<ClassCredentials>) -> anyhow::Result<Self> {
        let cfg = WamnPostgresConfig::from_env();
        let default = credentials.map(|credentials| ProjectConfig::from_global(credentials, &cfg));
        Ok(Self::with_provider(Arc::new(
            StaticCredentialProvider::default_only(default),
        )))
    }

    /// Projects configured with host-owned database logins.
    pub fn host_projects(&self) -> impl Iterator<Item = &wamn_registry::ProjectId> {
        self.host_projects.keys()
    }

    /// Share the counter with transaction guards that discard uncertain sessions.
    pub fn destroyed_connections(&self) -> &Arc<AtomicU64> {
        &self.destroyed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guest-sql must never be usable as a platform authority, and the refusal
    /// must survive `--release`, where a `debug_assert` would not exist.
    #[tokio::test]
    async fn a_platform_checkout_refuses_the_guest_authority() {
        let postgres =
            WamnPostgres::from_env(Some(credentials("acme--dev--default"))).expect("compose");
        let refused = postgres
            .checkout_platform(&DEFAULT_PROJECT, AuthorityClass::GuestSql)
            .await;
        assert!(
            refused.is_err(),
            "platform work under the guest authority must fail closed"
        );
    }

    /// REGRESSION GUARD (`wamn-0h0g.22.8.3`).
    ///
    /// Removing the ambient read in split B silently cut the HOST's default
    /// project, because `from_env` derived it from the config field that split
    /// had just forced to `None`. Nothing in the run exercised that path, so
    /// the only thing standing between that and a deployed host with no
    /// database is this test. It asserts the composed credential actually
    /// reaches resolution, and that composing without one resolves NOTHING
    /// rather than falling back.
    /// A guest credential for `project_env_key`, named the way a host binds one:
    /// the application login on the primary of the project's database.
    fn guest_url(project_env_key: &str) -> String {
        let key: wamn_registry::ProjectEnvKey = project_env_key.parse().unwrap();
        let triple = key.triple();
        let database = wamn_registry::name::project_env_database_name(
            &triple.org,
            &triple.project,
            &triple.env,
            &"a1b2c3d4".parse().unwrap(),
        );
        format!(
            "postgres://{}:pw@{}:5432/{database}",
            wamn_role::APP_LOGIN,
            wamn_registry::name::project_database_host(
                &database,
                &wamn_registry::name::env_key(&triple.org, &triple.env),
            )
        )
    }

    /// The guest and platform credentials that a host binds for one project.
    fn credentials(project_env_key: &str) -> ClassCredentials {
        ClassCredentials::default()
            .with_class(AuthorityClass::GuestSql, guest_url(project_env_key))
            .with_class(
                AuthorityClass::Platform,
                format!("postgres://{}:pw@db/project", wamn_role::PLATFORM_LOGIN),
            )
    }

    /// *** THE REFUSAL THAT MAKES THE CREDENTIAL THE AUTHORITY. ***
    ///
    /// After `wamn-0h0g.22.6` a guest's project_env_key is its LOGIN, so handing back a
    /// credential issued for another project_env_key is not a mis-selection, it is a
    /// cross-project_env_key read. Resolution refuses instead, and refuses again when the
    /// caller names no project_env_key at all.
    #[test]
    fn a_guest_credential_resolves_for_its_own_project_env_key_and_no_other() {
        let host = WamnPostgres::from_env(Some(credentials("test--dev--default")))
            .expect("compose with a credential");
        assert!(
            host.provider
                .resolve(
                    &DEFAULT_PROJECT,
                    AuthorityClass::GuestSql,
                    Some(&"test--dev--default".parse().unwrap())
                )
                .expect("the credential's own project_env_key resolves")
                .is_some()
        );
        assert!(
            host.provider
                .resolve(
                    &DEFAULT_PROJECT,
                    AuthorityClass::GuestSql,
                    Some(&"evil--dev--default".parse().unwrap())
                )
                .is_err(),
            "a credential issued for another project_env_key must be REFUSED, not borrowed"
        );
        assert!(
            host.provider
                .resolve(&DEFAULT_PROJECT, AuthorityClass::GuestSql, None)
                .is_err(),
            "guest resolution without an project_env_key has no authority to validate"
        );
        // Platform classes are project-environment scoped, so the project_env_key is not
        // part of their binding and its absence is not an error.
        assert!(
            host.provider
                .resolve(&DEFAULT_PROJECT, AuthorityClass::Platform, None)
                .expect("platform resolution needs no project_env_key")
                .is_some()
        );
    }

    #[test]
    fn the_composed_credential_becomes_the_default_project() {
        let composed = WamnPostgres::from_env(Some(credentials("test--dev--default")))
            .expect("compose with a credential");
        assert!(
            composed
                .provider
                .resolve(
                    &DEFAULT_PROJECT,
                    AuthorityClass::GuestSql,
                    Some(&"test--dev--default".parse().unwrap())
                )
                .expect("resolve default")
                .is_some(),
            "a host composed WITH a credential must resolve the default project; \
             deploy/platform injects it via secretKeyRef and a host that cannot \
             resolve it has no database at all"
        );

        let bare = WamnPostgres::from_env(None).expect("compose without a credential");
        assert!(
            bare.provider
                .resolve(
                    &DEFAULT_PROJECT,
                    AuthorityClass::GuestSql,
                    Some(&"test--dev--default".parse().unwrap())
                )
                .expect("resolve default")
                .is_none(),
            "composing without a credential must resolve nothing, not reach for \
             an ambient one"
        );
    }

    // The identity-time record must match `runs_release_record_check` exactly, or a
    // identity that would otherwise succeed dies on a CHECK inside the lease grant.
    // The hand-rolled shape test this used to exercise is retired: the invariant
    // now rides `wamn_digest::Digest`, so what is pinned here is that the TYPE admits
    // exactly what the run-plane CHECK admits. Same coupling, one owner.
    // wamn-cjv.2 — the in-band identity/role mutation guard.

    /// Marks the re-exec'd child of the ambient-credential test below.
    const AMBIENT_PROBE_VAR: &str = "WAMN_POOL_AMBIENT_PROBE";

    fn config(url: &str) -> ProjectConfig {
        project_config(ClassCredentials::default().with_class(AuthorityClass::Platform, url))
    }

    fn project_config(credentials: ClassCredentials) -> ProjectConfig {
        ProjectConfig {
            credentials,
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 1,
            statement_timeout_ms: 1,
        }
    }

    /// *** THE `wamn-0h0g.22.6.7` REFUSAL, ON THE SELECTED CREDENTIAL. ***
    ///
    /// The test must read the credential the GUEST would actually connect with,
    /// not whatever url the project happens to carry for some other authority.
    /// This project's platform login carries `acme`'s project key and its
    /// guest login carries `evil`'s, so a test that read the wrong credential
    /// would ADMIT a cross-project guest read. Both directions are asserted.
    #[test]
    fn the_guest_project_env_key_requirement_reads_the_credential_the_guest_would_connect_with() {
        let credentials = ClassCredentials::default()
            .with_class(AuthorityClass::Platform, guest_url("acme--dev--billing"))
            .with_class(AuthorityClass::GuestSql, guest_url("evil--dev--billing"));
        let mut projects = HashMap::new();
        projects.insert("billing".parse().unwrap(), project_config(credentials));
        let provider = StaticCredentialProvider::new(projects, None);

        assert!(
            provider
                .resolve(
                    &"billing".parse().unwrap(),
                    AuthorityClass::GuestSql,
                    Some(&"acme--dev--billing".parse().unwrap())
                )
                .is_err(),
            "the guest credential is issued for another project and must be REFUSED, \
             not borrowed — reading the platform credential here would admit a \
             cross-project read"
        );
        assert!(
            provider
                .resolve(
                    &"billing".parse().unwrap(),
                    AuthorityClass::GuestSql,
                    Some(&"evil--dev--billing".parse().unwrap())
                )
                .expect("the guest credential's own project resolves")
                .is_some()
        );
        assert!(
            provider
                .resolve(&"billing".parse().unwrap(), AuthorityClass::GuestSql, None)
                .is_err(),
            "guest resolution without a project has no authority to validate"
        );
        // The platform class is restricted to a project-environment: no project binding.
        assert!(
            provider
                .resolve(&"billing".parse().unwrap(), AuthorityClass::Platform, None)
                .expect("platform resolution needs no project")
                .is_some()
        );
    }

    /// The acceptance-forbidden silent fallback. An unprovisioned project used
    /// to be indistinguishable from a provisioned one, because it quietly
    /// borrowed whichever config was configured as the default.
    ///
    /// Resolved as a PLATFORM class deliberately: the subject here is the
    /// project MAP, and guest resolution additionally verifies the credential's
    /// project binding (`wamn-0h0g.22.6.7`), which has its own test. Mixing the
    /// two would make a project-selection failure and a project-binding failure
    /// indistinguishable.
    #[test]
    fn an_unlisted_project_does_not_borrow_another_projects_credential() {
        let mut projects = HashMap::new();
        projects.insert(
            "billing".parse().unwrap(),
            config("postgres://wamn_app_billing:pw@db/billing"),
        );
        let provider =
            StaticCredentialProvider::new(projects, Some(config("postgres://d_a:pw@db/d")));

        assert!(
            provider
                .resolve(&"billing".parse().unwrap(), AuthorityClass::Platform, None)
                .expect("resolve billing")
                .is_some(),
            "a listed project must still resolve"
        );
        assert!(
            provider
                .resolve(
                    &"not-provisioned".parse().unwrap(),
                    AuthorityClass::Platform,
                    None
                )
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
            StaticCredentialProvider::default_only(Some(config("postgres://wamn_app_d:pw@db/d")));
        assert!(
            provider
                .resolve(&DEFAULT_PROJECT, AuthorityClass::Platform, None)
                .expect("resolve default")
                .is_some(),
            "the default project must resolve, or single-DB deployments break"
        );
        assert!(
            provider
                .resolve(
                    &"somethingelse".parse().unwrap(),
                    AuthorityClass::Platform,
                    None
                )
                .expect("resolve other")
                .is_none(),
            "registering the default must not resurrect the catch-all"
        );
    }

    /// A url that names no credential cannot produce a probe: there would be
    /// nothing to compare the server's answer against.
    #[test]
    fn from_env_ignores_an_ambient_database_url() {
        if std::env::var(AMBIENT_PROBE_VAR).is_ok() {
            assert!(
                std::env::var("WAMN_PG_URL").is_ok(),
                "the child must run WITH the ambient url set to test its refusal"
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
}
