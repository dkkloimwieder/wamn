use sqlx::Row as _;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::super::pool::Object;
use sqlx::postgres::PgPoolOptions;
use tracing::Instrument as _;

use wamn_identity_protocol::AuthorityClass;

use super::super::pool::{
    CheckoutProbe, PoolKey, PoolLifecycle, ProjectPool, ResolvedCredential, destroy_connection,
};
use super::super::{DEFAULT_PROJECT, PgError};
use super::WamnPostgres;

impl WamnPostgres {
    /// Build one SQLx pool for the selected login and database.
    pub(super) async fn build_pool(
        cfg: &ResolvedCredential,
        class: AuthorityClass,
        _project: &str,
    ) -> anyhow::Result<sqlx::PgPool> {
        let options = wamn_postgres::connect_options(&cfg.database_url).await?;
        let username = options.get_username().to_owned();
        let timeout = cfg.statement_timeout_ms.to_string();
        let size = u32::try_from(PoolLifecycle::for_class(class).max_size(cfg))?;
        Ok(PgPoolOptions::new().max_connections(size)
            .acquire_timeout(std::time::Duration::from_millis(cfg.wait_timeout_ms))
            .after_connect(move |connection, _| {
                let username = username.clone();
                let timeout = timeout.clone();
                Box::pin(async move {
                    let row = sqlx::query("SELECT current_user::text, pg_has_role(current_user, $1, 'MEMBER'), current_setting('standard_conforming_strings'), NOT rolsuper AND NOT rolbypassrls FROM pg_roles WHERE rolname=current_user")
                        .bind(class.acl_role()).fetch_one(&mut *connection).await?;
                    if row.get::<String, _>(0) != username || !row.get::<bool, _>(1) || row.get::<String, _>(2) != "on" || !row.get::<bool, _>(3) {
                        return Err(sqlx::Error::Protocol("database login does not hold its configured authority".into()));
                    }
                    sqlx::query("SELECT set_config('statement_timeout', $1, false)")
                        .bind(timeout).execute(connection).await?;
                    Ok(())
                })
            }).connect_lazy_with(options))
    }

    /// Resolve + lazily build (memoized) the pool for a project. Unknown project
    /// or a build/resolution failure ⇒ `connection-unavailable`.
    fn pools(
        &self,
        lifecycle: PoolLifecycle,
    ) -> &std::sync::RwLock<HashMap<PoolKey, Arc<ProjectPool>>> {
        match lifecycle {
            PoolLifecycle::Guest => &self.guest_pools,
            PoolLifecycle::Platform => &self.platform_pools,
        }
    }

    pub(super) async fn ensure_pool(
        &self,
        class: AuthorityClass,
        project: &str,
        project_env_key: Option<&str>,
    ) -> Result<Arc<ProjectPool>, PgError> {
        let lifecycle = PoolLifecycle::for_class(class);
        let pools = self.pools(lifecycle);
        let key = PoolKey::new(project, class, project_env_key);
        if let Some(pool) = pools.read().expect("pools lock poisoned").get(&key) {
            return Ok(Arc::clone(pool));
        }
        let cfg = match self.provider.resolve(project, class, project_env_key) {
            Ok(Some(c)) => c,
            Ok(None) => {
                tracing::warn!(
                    project,
                    class = class.as_str(),
                    lifecycle = lifecycle.label(),
                    "wamn:postgres: no credentials for project"
                );
                return Err(PgError::ConnectionUnavailable);
            }
            Err(e) => {
                tracing::warn!(
                    project,
                    class = class.as_str(),
                    lifecycle = lifecycle.label(),
                    error = %e,
                    "wamn:postgres: credential resolution failed"
                );
                return Err(PgError::ConnectionUnavailable);
            }
        };
        let pp = match Self::build_pool(&cfg, class, project).await {
            Ok(pool) => Arc::new(ProjectPool {
                pool,
                statement_timeout_ms: cfg.statement_timeout_ms,
                row_limit: cfg.row_limit,
            }),
            Err(e) => {
                tracing::warn!(
                    project,
                    class = class.as_str(),
                    lifecycle = lifecycle.label(),
                    error = %e,
                    "wamn:postgres: pool build failed"
                );
                return Err(PgError::ConnectionUnavailable);
            }
        };
        let mut w = pools.write().expect("pools lock poisoned");
        Ok(Arc::clone(w.entry(key).or_insert(pp)))
    }

    /// Connections destroyed instead of repooled since startup.
    pub fn destroyed_connections(&self) -> u64 {
        self.destroyed.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(super) fn pool_status_all_by_lifecycle(
        &self,
    ) -> Vec<(PoolLifecycle, String, (usize, usize, usize))> {
        let mut statuses = Vec::new();
        for (lifecycle, pools) in [
            (PoolLifecycle::Guest, &self.guest_pools),
            (PoolLifecycle::Platform, &self.platform_pools),
        ] {
            statuses.extend(
                pools
                    .read()
                    .expect("pools lock poisoned")
                    .iter()
                    .map(|(key, pp)| {
                        (
                            lifecycle,
                            key.project().to_string(),
                            (pp.pool.size() as usize, pp.pool.num_idle(), 0),
                        )
                    }),
            );
        }
        statuses
    }

    /// acquire a raw connection from the default project and report its state
    /// *before* any identity injection. Condition verification only.
    pub async fn probe_checkout(&self, project_env_key: &str) -> anyhow::Result<CheckoutProbe> {
        self.probe_checkout_of(DEFAULT_PROJECT, project_env_key)
            .await
    }

    /// acquire a raw connection from a project's (lazily built) pool and
    /// report its state *before* any identity injection. Condition verification only —
    /// not reachable from guests and not a platform work path. It deliberately
    /// observes the guest lifecycle that the conformance acceptance test is proving.
    pub async fn probe_checkout_of(
        &self,
        project: &str,
        project_env_key: &str,
    ) -> anyhow::Result<CheckoutProbe> {
        let pp = self
            .ensure_pool(AuthorityClass::GuestSql, project, Some(project_env_key))
            .await
            .map_err(|_| anyhow::anyhow!("no pool for project {project:?}"))?;
        let mut conn = pp.pool.acquire().await?;
        let row = sqlx::query("SELECT pg_backend_pid(), current_setting('app.project_env_key', true), pg_current_xact_id_if_assigned()::text")
            .fetch_one(&mut *conn).await?;
        Ok(CheckoutProbe {
            backend_pid: row.try_get(0)?,
            project_env_key_identity: row.try_get(1)?,
            xact_id: row.try_get(2)?,
        })
    }

    pub(in super::super) fn destroy(&self, obj: Object) {
        destroy_connection(obj, &self.destroyed);
    }

    async fn checkout_class(
        &self,
        class: AuthorityClass,
        project: &str,
        project_env_key: Option<&str>,
    ) -> Result<(Object, Arc<ProjectPool>), PgError> {
        async {
            let pp = self.ensure_pool(class, project, project_env_key).await?;
            let obj = pp.pool.acquire().await.map_err(|e| {
                tracing::warn!(
                    project,
                    class = class.as_str(),
                    lifecycle = PoolLifecycle::for_class(class).label(),
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
        project: &str,
        project_env_key: &str,
    ) -> Result<(Object, Arc<ProjectPool>), PgError> {
        self.checkout_class(AuthorityClass::DynamicSql, project, Some(project_env_key))
            .await
    }

    pub(in super::super) async fn checkout_guest(
        &self,
        project: &str,
        project_env_key: &str,
    ) -> Result<(Object, Arc<ProjectPool>), PgError> {
        self.checkout_class(AuthorityClass::GuestSql, project, Some(project_env_key))
            .await
    }

    /// acquire the credential selected by the host-owned workload binding.
    /// Absence is exactly the existing guest path; only the closed
    /// event-materializer binding selects a platform credential.
    pub(in super::super) async fn checkout_workload(
        &self,
        component_id: &str,
        project: &str,
        project_env_key: &str,
    ) -> Result<(Object, Arc<ProjectPool>, AuthorityClass), PgError> {
        let class = self.workload_authority_for(component_id);
        let (connection, pool) = match class {
            AuthorityClass::GuestSql => self.checkout_guest(project, project_env_key).await?,
            AuthorityClass::EventMaterializer => self.checkout_platform(project, class).await?,
            AuthorityClass::DynamicSql
            | AuthorityClass::ExecutorPlatform
            | AuthorityClass::CallableHttp
            | AuthorityClass::Administration => {
                unreachable!("the closed workload binding admits only EventMaterializer")
            }
        };
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
        project: &str,
        class: AuthorityClass,
    ) -> Result<(Object, Arc<ProjectPool>), PgError> {
        // A HARD refusal, not a debug_assert: a debug_assert compiles out of
        // release, so the one build where this matters would be the build
        // without the test. Guest-sql is not a platform authority, and a
        // caller asking for platform work under it is a bug that must fail
        // closed rather than quietly draw a guest credential for host work.
        if matches!(class, AuthorityClass::GuestSql | AuthorityClass::DynamicSql) {
            tracing::error!(
                project,
                "wamn:postgres: guest-sql is not a platform authority; refusing the checkout"
            );
            return Err(PgError::ConnectionUnavailable);
        }
        self.checkout_class(class, project, None).await
    }
}
