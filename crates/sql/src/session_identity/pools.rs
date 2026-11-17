use sqlx::Row as _;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::super::pool::Object;
use sqlx::postgres::PgPoolOptions;
use tracing::Instrument as _;

use wamn_identity_protocol::AuthorityClass;

use super::super::pool::{
    CheckoutProbe, PoolKey, ProjectPool, ResolvedCredential, destroy_connection,
};
use super::super::{DEFAULT_PROJECT, PgError};
use super::WamnPostgres;

impl WamnPostgres {
    /// Build one SQLx pool for the selected login and database.
    pub(super) async fn build_pool(
        cfg: &ResolvedCredential,
        class: AuthorityClass,
        _project: &wamn_registry::ProjectId,
    ) -> anyhow::Result<sqlx::PgPool> {
        let options = wamn_postgres::connect_options(&cfg.database_url).await?;
        let timeout = cfg.statement_timeout_ms.to_string();
        let size = u32::try_from(match class {
            AuthorityClass::GuestSql | AuthorityClass::DynamicSql => cfg.guest_pool_max_size,
            AuthorityClass::Platform => cfg.platform_pool_max_size,
        })?;
        Ok(PgPoolOptions::new()
            .max_connections(size)
            .acquire_timeout(std::time::Duration::from_millis(cfg.wait_timeout_ms))
            .after_connect(move |connection, _| {
                let timeout = timeout.clone();
                Box::pin(async move {
                    sqlx::query("SELECT set_config('statement_timeout', $1, false)")
                        .bind(timeout)
                        .execute(connection)
                        .await?;
                    Ok(())
                })
            })
            .connect_lazy_with(options))
    }

    pub(crate) async fn ensure_pool(
        &self,
        class: AuthorityClass,
        project: &wamn_registry::ProjectId,
        project_env_key: Option<&wamn_registry::ProjectEnvKey>,
    ) -> Result<Arc<ProjectPool>, PgError> {
        let cfg = match self.provider.resolve(project, class, project_env_key) {
            Ok(Some(c)) => c,
            Ok(None) => {
                tracing::warn!(
                    project = %project,
                    class = class.as_str(),
                    "wamn:postgres: no credentials for project"
                );
                return Err(PgError::ConnectionUnavailable);
            }
            Err(e) => {
                tracing::warn!(
                    project = %project,
                    class = class.as_str(),
                    error = %e,
                    "wamn:postgres: credential resolution failed"
                );
                return Err(PgError::ConnectionUnavailable);
            }
        };
        let (login, _) = wamn_postgres::identity(&cfg.database_url)
            .map_err(|_| PgError::ConnectionUnavailable)?;
        let key = PoolKey::new(project, login);
        if let Some(pool) = self.pools.read().expect("pools lock poisoned").get(&key) {
            return Ok(Arc::clone(pool));
        }
        let pp = match Self::build_pool(&cfg, class, project).await {
            Ok(pool) => Arc::new(ProjectPool {
                pool,
                statement_timeout_ms: cfg.statement_timeout_ms,
            }),
            Err(e) => {
                tracing::warn!(
                    project = %project,
                    class = class.as_str(),
                    error = %e,
                    "wamn:postgres: pool build failed"
                );
                return Err(PgError::ConnectionUnavailable);
            }
        };
        let mut w = self.pools.write().expect("pools lock poisoned");
        Ok(Arc::clone(w.entry(key).or_insert(pp)))
    }

    /// Connections destroyed instead of repooled since startup.
    pub fn destroyed_connections(&self) -> u64 {
        self.destroyed.load(Ordering::Relaxed)
    }

    /// acquire a raw connection from the default project and report its state
    /// *before* any identity injection. Condition verification only.
    pub async fn probe_checkout(
        &self,
        project_env_key: &wamn_registry::ProjectEnvKey,
    ) -> anyhow::Result<CheckoutProbe> {
        self.probe_checkout_of(&DEFAULT_PROJECT, project_env_key)
            .await
    }

    /// acquire a raw connection from a project's (lazily built) pool and
    /// report its state *before* any identity injection. Condition verification only —
    /// not reachable from guests and not a platform work path. It deliberately
    /// observes the guest lifecycle that the conformance acceptance test is proving.
    pub async fn probe_checkout_of(
        &self,
        project: &wamn_registry::ProjectId,
        project_env_key: &wamn_registry::ProjectEnvKey,
    ) -> anyhow::Result<CheckoutProbe> {
        let pp = self
            .ensure_pool(AuthorityClass::GuestSql, project, Some(project_env_key))
            .await
            .map_err(|_| anyhow::anyhow!("no pool for project {project:?}"))?;
        let mut conn = pp.pool.acquire().await?;
        let row = sqlx::query("SELECT pg_backend_pid(), pg_current_xact_id_if_assigned()::text")
            .fetch_one(&mut *conn)
            .await?;
        Ok(CheckoutProbe {
            backend_pid: row.try_get(0)?,
            xact_id: row.try_get(1)?,
        })
    }

    pub(in super::super) fn destroy(&self, obj: Object) {
        destroy_connection(obj, &self.destroyed);
    }

    async fn checkout_class(
        &self,
        class: AuthorityClass,
        project: &wamn_registry::ProjectId,
        project_env_key: Option<&wamn_registry::ProjectEnvKey>,
    ) -> Result<(Object, Arc<ProjectPool>), PgError> {
        async {
            let pp = self.ensure_pool(class, project, project_env_key).await?;
            let obj = pp.pool.acquire().await.map_err(|e| {
                tracing::warn!(
                    project = %project,
                    class = class.as_str(),
                    error = %e,
                    "wamn:postgres pool checkout failed"
                );
                PgError::ConnectionUnavailable
            })?;
            Ok((obj, pp))
        }
        .instrument(tracing::info_span!(
            "wamn.postgres.acquire",
            wamn.authority_class = class.as_str(),
        ))
        .await
    }

    /// acquire a connection reserved for guest-visible `wamn:postgres` calls.
    ///
    /// Takes no class parameter BY DESIGN: guest-visible work is
    /// [`AuthorityClass::GuestSql`] and nothing else, so there is no call site
    /// at which a guest checkout could name a platform authority.
    ///
    /// It DOES take the project, and that is the whole of `wamn-0h0g.22.6.7`:
    /// after the `wamn-0h0g.22.6` sweep the guest's project comes from
    /// `current_user`, so the credential IS the project authority and the
    /// connection cannot be selected without knowing which project it is for.
    pub(in super::super) async fn checkout_dynamic(
        &self,
        project: &wamn_registry::ProjectId,
        project_env_key: &wamn_registry::ProjectEnvKey,
    ) -> Result<(Object, Arc<ProjectPool>), PgError> {
        self.checkout_class(AuthorityClass::DynamicSql, project, Some(project_env_key))
            .await
    }

    pub(in super::super) async fn checkout_guest(
        &self,
        project: &wamn_registry::ProjectId,
        project_env_key: &wamn_registry::ProjectEnvKey,
    ) -> Result<(Object, Arc<ProjectPool>), PgError> {
        self.checkout_class(AuthorityClass::GuestSql, project, Some(project_env_key))
            .await
    }

    /// acquire the credential selected by the host-owned workload binding.
    /// Absence is exactly the existing guest path; only the closed
    /// event-materializer binding selects a platform credential.
    pub(in super::super) async fn checkout_workload(
        &self,
        _component_id: &str,
        project: &wamn_registry::ProjectId,
        project_env_key: &wamn_registry::ProjectEnvKey,
    ) -> Result<(Object, Arc<ProjectPool>, AuthorityClass), PgError> {
        let class = AuthorityClass::GuestSql;
        let (connection, pool) = self.checkout_guest(project, project_env_key).await?;
        Ok((connection, pool, class))
    }

    /// acquire a connection reserved for host-owned platform work.
    ///
    /// The class is REQUIRED because the platform lifecycle serves three
    /// distinct authorities (`wamn-0h0g.22.14`). Making the caller name which
    /// one is what stops executor-platform work and callable-HTTP admission
    /// sharing a pooled session.
    /// `project_env_key` is deliberately absent: platform credentials are bound to the
    /// project environment, not to a project. The remaining relation with a
    /// settable identity is `wamn_run.run_queue`. The host injects that identity,
    /// and the guest cannot reach the relation.
    pub(in super::super) async fn checkout_platform(
        &self,
        project: &wamn_registry::ProjectId,
        class: AuthorityClass,
    ) -> Result<(Object, Arc<ProjectPool>), PgError> {
        // A HARD refusal, not a debug_assert: a debug_assert compiles out of
        // release, so the one build where this matters would be the build
        // without the test. Guest-sql is not a platform authority, and a
        // caller asking for platform work under it is a bug that must fail
        // closed rather than quietly draw a guest credential for host work.
        if matches!(class, AuthorityClass::GuestSql | AuthorityClass::DynamicSql) {
            tracing::error!(
                project = %project,
                "wamn:postgres: guest-sql is not a platform authority; refusing the checkout"
            );
            return Err(PgError::ConnectionUnavailable);
        }
        self.checkout_class(class, project, None).await
    }
}
