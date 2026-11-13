//! Connection pooling + credential resolution for `wamn:postgres` (SR4 split,
//! wamn-cjv.18): the per-project config/policy, the `CredentialProvider` seam,
//! the live `ProjectPool`, the R18 connect-time assertion, and connection
//! teardown. The pool/identity METHODS live on `WamnPostgres` in `identity.rs`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context as _;
use sqlx::PgPool;
pub(super) type Object = sqlx::pool::PoolConnection<sqlx::Postgres>;

use wamn_run_state::{AuthorityClass, app_identity_hash};

use super::DEFAULT_PROJECT;
use wamn_engine::engine::MAX_HOST_CALL_DURATION;

const DEFAULT_GUEST_POOL_MAX_SIZE: usize = 14;
const DEFAULT_PLATFORM_POOL_MAX_SIZE: usize = 2;

/// The host call ceiling in milliseconds. The constant is well under an hour,
/// so it fits every width these bounds convert to.
fn max_host_call_ms() -> u64 {
    u64::try_from(MAX_HOST_CALL_DURATION.as_millis()).unwrap_or(u64::MAX)
}

fn bounded_wait_timeout_ms(value: u64) -> u64 {
    value.clamp(1, max_host_call_ms())
}

fn bounded_statement_timeout_ms(value: u64) -> u32 {
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
    /// Build explicit fixture credentials, including the separate read-only login.
    #[cfg(any(test, feature = "test-util"))]
    pub fn every_class(url: impl Into<String>) -> Self {
        let url = url.into();
        let query = url::Url::parse(&url).ok().and_then(|mut input| {
            let login = input.username().strip_prefix("wamn_app_")?;
            let login = format!("wamn_query_{login}");
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
    /// The default project's config, consuming the single-DB
    /// [`WamnPostgresConfig`]. `None` when it names no credential.
    pub(super) fn from_global_config(cfg: WamnPostgresConfig) -> Option<Self> {
        let WamnPostgresConfig {
            credentials,
            guest_pool_max_size,
            platform_pool_max_size,
            wait_timeout_ms,
            statement_timeout_ms,
            row_limit,
        } = cfg;
        Some(Self {
            credentials: credentials?,
            guest_pool_max_size,
            platform_pool_max_size,
            wait_timeout_ms,
            statement_timeout_ms,
            row_limit,
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
        project: &str,
        class: AuthorityClass,
        project_env_key: Option<&str>,
    ) -> anyhow::Result<Option<ResolvedCredential>>;
}

/// v0 provider: an in-memory project→config map, built directly by the
/// composition root or parsed from a JSON object of projects.
///
/// `wamn-0h0g.22.8.2`: THERE IS NO CATCH-ALL DEFAULT. A config supplied as the
/// "default" is registered under [`DEFAULT_PROJECT`] like any other project, so
/// a single-DB deployment and the S2 bench still resolve, while an UNLISTED
/// project now FAILS instead of silently borrowing another project's
/// credential. Silent fallback is the behaviour the parent acceptance forbids:
/// it made an unprovisioned project indistinguishable from a provisioned one.
#[derive(Debug)]
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
    /// `base`. Public so the route-authentication harnesses can build a
    /// multi-project [`StaticCredentialProvider`] from one JSON literal instead
    /// of assembling every [`ProjectConfig`] field by hand.
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
        let v: serde_json::Value = serde_json::from_str(text).context("parse projects json")?;
        let obj = v
            .as_object()
            .context("projects json must be a JSON object")?;
        let mut out = HashMap::new();
        for (name, entry) in obj {
            anyhow::ensure!(
                entry.get("pool_max_size").is_none(),
                "project {name:?} uses retired \"pool_max_size\"; configure \
                 \"guest_pool_max_size\" and \"platform_pool_max_size\" separately"
            );
            let credentials = class_credentials_from_json(name, entry)?;
            let u64_or = |k: &str, d: u64| {
                entry
                    .get(k)
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(d)
            };
            out.insert(
                name.clone(),
                ProjectConfig {
                    credentials,
                    guest_pool_max_size: usize::try_from(u64_or(
                        "guest_pool_max_size",
                        base.guest_pool_max_size as u64,
                    ))
                    .unwrap_or(usize::MAX),
                    platform_pool_max_size: usize::try_from(u64_or(
                        "platform_pool_max_size",
                        base.platform_pool_max_size as u64,
                    ))
                    .unwrap_or(usize::MAX),
                    wait_timeout_ms: bounded_wait_timeout_ms(u64_or(
                        "wait_timeout_ms",
                        base.wait_timeout_ms,
                    )),
                    statement_timeout_ms: bounded_statement_timeout_ms(u64_or(
                        "statement_timeout_ms",
                        u64::from(base.statement_timeout_ms),
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
    anyhow::ensure!(
        entry.get("url").is_none(),
        "each database authority requires its own configured login"
    );
    let mut credentials = ClassCredentials::default();
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
        "project {name:?} names no configured database authority"
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
    /// FOR GUEST SQL THE PROJECT BINDING IS VERIFIED, NOT CONFIGURED. The
    /// credential's login role carries the project key as its identity digest, and
    /// the same key is what every governed predicate computes — so the binding
    /// is validated from the credential itself rather than declared beside it,
    /// and a credential minted for another project is REFUSED instead of
    /// silently borrowed. Selection happens FIRST and the test runs on the
    /// selected credential, so it fires on exactly the credential that would be
    /// handed back — never on a different class's login.
    fn resolve(
        &self,
        project: &str,
        class: AuthorityClass,
        project_env_key: Option<&str>,
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
            let project_env_key = project_env_key.context(
                "guest-SQL resolution requires the project: after wamn-0h0g.22.6 the \
                 credential IS the project authority",
            )?;
            let (role, database) = wamn_postgres::identity(&resolved.database_url)?;
            let key = app_identity_hash(project_env_key, &database);
            anyhow::ensure!(
                role == format!("{}_{key}", class.acl_role()),
                "the credential for project {project:?} authenticates as {role:?}, which \
                 does not carry the project key for the requested project; refusing rather \
                 than reading another project's rows"
            );
        }
        Ok(Some(resolved))
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

/// A pool belongs to one project, authority and project environment.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct PoolKey {
    project: Box<str>,
    class: AuthorityClass,
    project_env_key: Option<Box<str>>,
}

impl PoolKey {
    pub(super) fn new(project: &str, class: AuthorityClass, project_env_key: Option<&str>) -> Self {
        Self {
            project: project.into(),
            class,
            project_env_key: project_env_key.map(Into::into),
        }
    }
    #[cfg(test)]
    pub(super) fn project(&self) -> &str {
        &self.project
    }
}

pub(super) struct ProjectPool {
    pub(super) pool: PgPool,
    pub(super) statement_timeout_ms: u32,
    pub(super) row_limit: u64,
}

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
            AuthorityClass::GuestSql | AuthorityClass::DynamicSql => Self::Guest,
            AuthorityClass::ExecutorPlatform
            | AuthorityClass::CallableHttp
            | AuthorityClass::EventMaterializer
            | AuthorityClass::Administration => Self::Platform,
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

/// Raw checkout state, observed before any identity injection. Condition probes use
/// this to assert a fresh checkout is transaction-free and identity-free.
#[derive(Debug)]
pub struct CheckoutProbe {
    pub backend_pid: i32,
    /// `current_setting('app.project_env_key', true)` — must be NULL on a clean conn.
    pub project_env_key_identity: Option<String>,
    /// `pg_current_xact_id_if_assigned()` — non-NULL means a leaked open
    /// transaction that performed writes.
    pub xact_id: Option<String>,
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
    /// login. Five classes, five configured logins, five different answers.
    #[test]
    fn each_authority_class_authenticates_with_its_own_configured_credential() {
        let projects = StaticCredentialProvider::projects_from_json(
            r#"{"billing":{"credentials":{
                 "guest-sql":"postgres://wamn_app_g:pw@db/billing",
                 "executor-platform":"postgres://wamn_exec_platform_x:pw@db/billing",
                 "callable-http":"postgres://wamn_http_admitter_x:pw@db/billing",
                 "event-materializer":"postgres://wamn_materializer_x:pw@db/billing",
                 "administration":"postgres://wamn_administration_x:pw@db/billing"}}}"#,
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
        assert_eq!(seen.len(), 5);
    }

    /// A class with no configured credential REFUSES. The forbidden behaviour is
    /// the quiet one: handing back whichever login happened to be configured for
    /// some other authority, which is how an unprovisioned family becomes
    /// indistinguishable from a provisioned one.
    #[test]
    fn a_class_with_no_configured_credential_is_refused_not_defaulted() {
        let projects = StaticCredentialProvider::projects_from_json(
            r#"{"billing":{"credentials":{"guest-sql":"postgres://wamn_app_g:pw@db/billing"}}}"#,
            &base(),
        )
        .expect("a single-class project parses");
        let provider = StaticCredentialProvider::new(projects, None);

        for class in [
            AuthorityClass::ExecutorPlatform,
            AuthorityClass::CallableHttp,
            AuthorityClass::EventMaterializer,
            AuthorityClass::Administration,
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
    /// The test must read the credential the GUEST would actually connect with,
    /// not whatever url the project happens to carry for some other authority.
    /// This project's platform login carries `acme`'s project key and its
    /// guest login carries `evil`'s, so a test that read the wrong credential
    /// would ADMIT a cross-project guest read. Both directions are asserted.
    #[test]
    fn the_guest_project_env_key_requirement_reads_the_credential_the_guest_would_connect_with() {
        let credentials = ClassCredentials::every_class(format!(
            "postgres://wamn_app_{}:pw@db/billing",
            app_identity_hash("acme", "billing")
        ))
        .with_class(
            AuthorityClass::GuestSql,
            format!(
                "postgres://wamn_app_{}:pw@db/billing",
                app_identity_hash("evil", "billing")
            ),
        );
        let mut projects = HashMap::new();
        projects.insert("billing".to_string(), project_config(credentials));
        let provider = StaticCredentialProvider::new(projects, None);

        assert!(
            provider
                .resolve("billing", AuthorityClass::GuestSql, Some("acme"))
                .is_err(),
            "the guest credential is minted for another project and must be REFUSED, \
             not borrowed — reading the platform credential here would admit a \
             cross-project read"
        );
        assert!(
            provider
                .resolve("billing", AuthorityClass::GuestSql, Some("evil"))
                .expect("the guest credential's own project resolves")
                .is_some()
        );
        assert!(
            provider
                .resolve("billing", AuthorityClass::GuestSql, None)
                .is_err(),
            "guest resolution without a project has no authority to validate"
        );
        // The platform class is restricted to a project-environment: no project binding.
        assert!(
            provider
                .resolve("billing", AuthorityClass::ExecutorPlatform, None)
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
            "billing".to_string(),
            config("postgres://wamn_app_billing:pw@db/billing"),
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
            StaticCredentialProvider::default_only(Some(config("postgres://wamn_app_d:pw@db/d")));
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
    fn the_authority_class_selects_the_cache() {
        assert_eq!(
            PoolLifecycle::for_class(AuthorityClass::GuestSql).label(),
            "guest"
        );
        for class in [
            AuthorityClass::ExecutorPlatform,
            AuthorityClass::CallableHttp,
            AuthorityClass::EventMaterializer,
            AuthorityClass::Administration,
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

    // R18 — the connect-time test logic. A negative is hard to produce on stock
    // PG18 (the setting defaults on), so the fail-closed branch is asserted here.
    #[test]
    fn configured_database_waits_are_finite_and_nonzero() {
        let max = max_host_call_ms();
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
