//! Connection pools keyed by project and login, credential resolution, and teardown.
//! Pool and identity methods live on `WamnPostgres` in `session_identity.rs`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context as _;
use sqlx::PgPool;
pub(super) type Object = sqlx::pool::PoolConnection<sqlx::Postgres>;

use wamn_identity_protocol::AuthorityClass;
#[cfg(test)]
use wamn_registry::app_identity_hash;

use super::DEFAULT_PROJECT;
use wamn_engine::engine::MAX_HOST_CALL_DURATION;

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
    /// Build explicit fixture credentials, including the separate read-only login.
    #[cfg(any(test, feature = "test-util"))]
    pub fn every_class(url: impl Into<String>) -> Self {
        let url = url.into();
        let query = url::Url::parse(&url).ok().and_then(|mut input| {
            let login = input.username().strip_prefix(&format!("{}_", wamn_role::APP_ROLE))?;
            let login = format!("{}_{login}", wamn_role::QUERY_ROLE);
            input.set_username(&login).ok()?;
            Some(String::from(input))
        });
        Self {
            urls: AuthorityClass::ALL
                .into_iter()
                .map(|class| {
                    (
                        class,
                        if class == AuthorityClass::DynamicSql {
                            query.as_ref().unwrap_or(&url).clone()
                        } else {
                            url.clone()
                        },
                    )
                })
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
    /// minted for someone else.
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
                wamn_postgres::configuration(&resolved.database_url)?.parse()?;
            let database = options.get_database().context("project database is absent")?;
            anyhow::ensure!(
                &triple.project == project
                    && options.get_username() == expected
                    && wamn_registry::name::project_database_key(database).as_ref() == Some(key)
                    && options.get_host() == wamn_registry::name::project_database_host(database, &triple.org)
                    && options.get_port() == 5432
                    && options.get_socket().is_none(),
                "the {class} credential does not match project {key} and its database endpoint"
            );
        }
        Ok(Some(resolved))
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// A pool belongs to one project and authenticated login.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct PoolKey {
    pub(super) project: wamn_registry::ProjectId,
    login: wamn_identity_protocol::LoginName,
}

impl PoolKey {
    pub(super) fn new(
        project: &wamn_registry::ProjectId,
        login: wamn_identity_protocol::LoginName,
    ) -> Self {
        Self {
            project: project.clone(),
            login,
        }
    }
}

pub(super) struct ProjectPool {
    pub(super) pool: PgPool,
    pub(super) statement_timeout_ms: u32,
}

/// Discard a connection whose transaction outcome or session state is unknown.
pub(super) fn destroy_connection(obj: Object, counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
    drop(obj.detach());
}

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
        let credentials = ClassCredentials::every_class(format!(
            "postgres://wamn_app_{}:pw@db/billing",
            app_identity_hash(&"acme--dev--project".parse().unwrap(), "billing")
        ))
        .with_class(
            AuthorityClass::GuestSql,
            format!(
                "postgres://wamn_app_{}:pw@db/billing",
                app_identity_hash(&"evil--dev--project".parse().unwrap(), "billing")
            ),
        );
        let mut projects = HashMap::new();
        projects.insert("billing".parse().unwrap(), project_config(credentials));
        let provider = StaticCredentialProvider::new(projects, None);

        assert!(
            provider
                .resolve(
                    &"billing".parse().unwrap(),
                    AuthorityClass::GuestSql,
                    Some(&"acme--dev--project".parse().unwrap())
                )
                .is_err(),
            "the guest credential is minted for another project and must be REFUSED, \
             not borrowed — reading the platform credential here would admit a \
             cross-project read"
        );
        assert!(
            provider
                .resolve(
                    &"billing".parse().unwrap(),
                    AuthorityClass::GuestSql,
                    Some(&"evil--dev--project".parse().unwrap())
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
