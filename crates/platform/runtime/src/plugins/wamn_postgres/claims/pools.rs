use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use deadpool_postgres::{Manager, ManagerConfig, Object, Pool, RecyclingMethod, Runtime, Timeouts};
use tokio_postgres::NoTls;
use tracing::Instrument as _;

use wamn_run_state::AuthorityClass;

use super::super::pool::{
    CheckoutProbe, PoolKey, PoolLifecycle, ProjectPool, ResolvedCredential,
    credential_exactness_hook, credential_generation_role, destroy_connection,
    session_statement_timeout_hook, standard_conforming_strings_hook,
};
use super::super::{DEFAULT_PROJECT, PgError};
use super::WamnPostgres;

impl WamnPostgres {
    /// Build a deadpool pool for one resolved credential.
    pub(super) fn build_pool(
        cfg: &ResolvedCredential,
        class: AuthorityClass,
        project: &str,
    ) -> anyhow::Result<Pool> {
        let lifecycle = PoolLifecycle::for_class(class);
        let pg_config: tokio_postgres::Config = cfg
            .database_url
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid database url: {e}"))?;
        let manager_config = ManagerConfig {
            recycling_method: RecyclingMethod::Fast,
        };
        let mgr = Manager::from_config(pg_config, NoTls, manager_config);
        let timeout = std::time::Duration::from_millis(cfg.wait_timeout_ms);
        Ok(Pool::builder(mgr)
            .max_size(lifecycle.max_size(cfg))
            .timeouts(Timeouts {
                wait: Some(timeout),
                create: Some(timeout),
                recycle: Some(timeout),
            })
            // R18: assert standard_conforming_strings=on once per new connection.
            .post_create(standard_conforming_strings_hook())
            // wamn-0h0g.17.18: the project's statement_timeout is pool-uniform,
            // so it is applied here once instead of on every request.
            .post_create(session_statement_timeout_hook(cfg.statement_timeout_ms))
            // wamn-0h0g.22.8.4: and assert the connection IS the credential
            // this pool resolved. deadpool pushes hooks, so both run.
            .post_create(credential_exactness_hook(
                &cfg.database_url,
                class,
                project,
            )?)
            .runtime(Runtime::Tokio1)
            .build()?)
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

    /// Resolve FIRST, then look up.
    ///
    /// The key carries the credential generation, and the generation is only
    /// knowable from the resolved URL, so the lookup cannot precede resolution.
    /// That ordering is what makes rotation correct BY CONSTRUCTION: a rotated
    /// credential computes a different key, so the stale pool is never hit
    /// again instead of being hit until something notices. Resolution is a map
    /// lookup and a clone, which is nothing beside the awaited checkout it
    /// guards.
    pub(super) fn ensure_pool(
        &self,
        class: AuthorityClass,
        project: &str,
        tenant: Option<&str>,
    ) -> Result<Arc<ProjectPool>, PgError> {
        let lifecycle = PoolLifecycle::for_class(class);
        let pools = self.pools(lifecycle);
        let cfg = match self.provider.resolve(project, class, tenant) {
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
        let generation_role = match credential_generation_role(&cfg.database_url) {
            Ok(role) => role,
            Err(e) => {
                // Deliberately logs the ERROR and not the url: the url carries
                // the password.
                tracing::warn!(
                    project,
                    class = class.as_str(),
                    error = %e,
                    "wamn:postgres: resolved credential carries no generation identity"
                );
                return Err(PgError::ConnectionUnavailable);
            }
        };
        let key = PoolKey::new(project, class, &generation_role);
        if let Some(pp) = pools.read().expect("pools lock poisoned").get(&key) {
            return Ok(pp.clone());
        }
        let pp = match Self::build_pool(&cfg, class, project) {
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
        Ok(w.entry(key).or_insert(pp).clone())
    }

    /// Number of live (built) project/lifecycle pools — gate observability.
    pub fn project_pool_count(&self) -> usize {
        self.guest_pools
            .read()
            .expect("guest pools lock poisoned")
            .len()
            + self
                .platform_pools
                .read()
                .expect("platform pools lock poisoned")
                .len()
    }

    /// Connections destroyed instead of repooled since startup.
    pub fn destroyed_connections(&self) -> u64 {
        self.destroyed.load(Ordering::Relaxed)
    }

    pub(super) fn pool_status_all_by_lifecycle(
        &self,
    ) -> Vec<(PoolLifecycle, String, (usize, usize, usize))> {
        let mut statuses = Vec::new();
        for (lifecycle, pools) in [
            (PoolLifecycle::Guest, &self.guest_pools),
            (PoolLifecycle::Platform, &self.platform_pools),
        ] {
            // Aggregated BY PROJECT on purpose: the observable gauge labels are
            // a scraped surface, and wamn-0h0g.22.8.2 re-keys the cache without
            // renaming a metric. Splitting these by class is observability work
            // with its own owner.
            statuses.extend(
                pools
                    .read()
                    .expect("pools lock poisoned")
                    .iter()
                    .map(|(key, pp)| {
                        let status = pp.pool.status();
                        (
                            lifecycle,
                            key.project().to_string(),
                            (status.size, status.available, status.waiting),
                        )
                    }),
            );
        }
        statuses
    }

    /// Aggregate (size, available, waiting) across a project's built lifecycle pools.
    pub fn pool_status_of(&self, project: &str) -> Option<(usize, usize, usize)> {
        self.pool_status_all_by_lifecycle()
            .into_iter()
            .filter(|(_, candidate, _)| candidate == project)
            .map(|(_, _, status)| status)
            .reduce(|left, right| (left.0 + right.0, left.1 + right.1, left.2 + right.2))
    }

    /// Default-project pool status (single-DB benches).
    pub fn pool_status(&self) -> Option<(usize, usize, usize)> {
        self.pool_status_of(DEFAULT_PROJECT)
    }

    /// `(project, (size, available, waiting))` aggregated across every built
    /// lifecycle pool. The observable gauges use the unaggregated private view.
    pub fn pool_status_all(&self) -> Vec<(String, (usize, usize, usize))> {
        let mut aggregate = HashMap::<String, (usize, usize, usize)>::new();
        for (_, project, status) in self.pool_status_all_by_lifecycle() {
            let total = aggregate.entry(project).or_insert((0, 0, 0));
            total.0 += status.0;
            total.1 += status.1;
            total.2 += status.2;
        }
        aggregate.into_iter().collect()
    }

    /// [9.8] Register the `wamn.postgres.pool.{size,available,waiting}` observable
    /// gauges (deadpool `Pool::status()`), keyed by `wamn.project` and
    /// `wamn.pool.lifecycle`. The callbacks hold a `Weak` back to the plugin so
    /// registration never keeps it alive, and they observe every currently-built
    /// pool at export time. Call ONCE per process (observable instruments warn on
    /// duplicate registration); a no-op until the global meter provider is
    /// installed (`OTEL_*`).
    pub fn register_pool_metrics(self: &std::sync::Arc<Self>) {
        use opentelemetry::KeyValue;
        let meter = opentelemetry::global::meter("wamn-postgres");
        type PoolStatus = (usize, usize, usize);
        type MetricSpec = (&'static str, &'static str, fn(&PoolStatus) -> u64);
        let specs: [MetricSpec; 3] = [
            (
                "wamn.postgres.pool.size",
                "deadpool connections currently allocated for a project's pool",
                |s| s.0 as u64,
            ),
            (
                "wamn.postgres.pool.available",
                "deadpool connections idle + ready to check out",
                |s| s.1 as u64,
            ),
            (
                "wamn.postgres.pool.waiting",
                "tasks queued waiting for a pool checkout (saturation signal)",
                |s| s.2 as u64,
            ),
        ];
        for (name, desc, read) in specs {
            let weak = std::sync::Arc::downgrade(self);
            let _ = meter
                .u64_observable_gauge(name)
                .with_description(desc)
                .with_callback(move |o| {
                    if let Some(plugin) = weak.upgrade() {
                        for (lifecycle, project, status) in plugin.pool_status_all_by_lifecycle() {
                            o.observe(
                                read(&status),
                                &[
                                    KeyValue::new("wamn.project", project),
                                    KeyValue::new("wamn.pool.lifecycle", lifecycle.label()),
                                ],
                            );
                        }
                    }
                })
                .build();
        }
    }

    /// Check out a raw connection from the default project and report its state
    /// *before* any claim injection. Gate verification only.
    pub async fn probe_checkout(&self, tenant: &str) -> anyhow::Result<CheckoutProbe> {
        self.probe_checkout_of(DEFAULT_PROJECT, tenant).await
    }

    /// Check out a raw connection from a project's (lazily built) pool and
    /// report its state *before* any claim injection. Gate verification only —
    /// not reachable from guests and not a platform work path. It deliberately
    /// observes the guest lifecycle that the conformance gate is proving.
    pub async fn probe_checkout_of(
        &self,
        project: &str,
        tenant: &str,
    ) -> anyhow::Result<CheckoutProbe> {
        let pp = self
            .ensure_pool(AuthorityClass::GuestSql, project, Some(tenant))
            .map_err(|_| anyhow::anyhow!("no pool for project {project:?}"))?;
        let conn = pp.pool.get().await?;
        let row = conn
            .query_one(
                "SELECT pg_backend_pid(), current_setting('app.tenant', true), \
                 pg_current_xact_id_if_assigned()::text",
                &[],
            )
            .await?;
        Ok(CheckoutProbe {
            backend_pid: row.try_get(0)?,
            tenant_claim: row.try_get(1)?,
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
        tenant: Option<&str>,
    ) -> Result<(Object, Arc<ProjectPool>), PgError> {
        async {
            let pp = self.ensure_pool(class, project, tenant)?;
            let obj = pp.pool.get().await.map_err(|e| {
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

    /// Check out a connection reserved for guest-visible `wamn:postgres` calls.
    ///
    /// Takes no class parameter BY DESIGN: guest-visible work is
    /// [`AuthorityClass::GuestSql`] and nothing else, so there is no call site
    /// at which a guest checkout could name a platform authority.
    ///
    /// It DOES take the tenant, and that is the whole of `wamn-0h0g.22.6.7`:
    /// after the `wamn-0h0g.22.6` sweep the guest's tenant comes from
    /// `current_user`, so the credential IS the tenant authority and the
    /// connection cannot be selected without knowing which tenant it is for.
    pub(in super::super) async fn checkout_guest(
        &self,
        project: &str,
        tenant: &str,
    ) -> Result<(Object, Arc<ProjectPool>), PgError> {
        self.checkout_class(AuthorityClass::GuestSql, project, Some(tenant))
            .await
    }

    /// Check out the credential selected by the host-owned workload binding.
    /// Absence is exactly the existing guest path; only the closed
    /// event-materializer binding selects a platform credential.
    pub(in super::super) async fn checkout_workload(
        &self,
        component_id: &str,
        project: &str,
        tenant: &str,
    ) -> Result<(Object, Arc<ProjectPool>, AuthorityClass), PgError> {
        let class = self.workload_authority_for(component_id);
        let (connection, pool) = match class {
            AuthorityClass::GuestSql => self.checkout_guest(project, tenant).await?,
            AuthorityClass::EventMaterializer => self.checkout_platform(project, class).await?,
            AuthorityClass::ExecutorPlatform | AuthorityClass::CallableHttp => {
                unreachable!("the closed workload binding admits only EventMaterializer")
            }
        };
        Ok((connection, pool, class))
    }

    /// Check out a connection reserved for host-owned platform work.
    ///
    /// The class is REQUIRED because the platform lifecycle serves three
    /// distinct authorities (`wamn-0h0g.22.14`). Making the caller name which
    /// one is what stops executor-platform work and callable-HTTP admission
    /// sharing a pooled session.
    /// `tenant` is deliberately absent: platform credentials are scoped to the
    /// project-environment, not to a tenant, and the two relations that still
    /// carry a settable claim (`wamn_run.run_queue`,
    /// `wamn_run.operator_run_actions`) are exactly the ones the guest cannot
    /// reach — their claim is host-injected.
    pub(in super::super) async fn checkout_platform(
        &self,
        project: &str,
        class: AuthorityClass,
    ) -> Result<(Object, Arc<ProjectPool>), PgError> {
        // A HARD refusal, not a debug_assert: a debug_assert compiles out of
        // release, so the one build where this matters would be the build
        // without the check. Guest-sql is not a platform authority, and a
        // caller asking for platform work under it is a bug that must fail
        // closed rather than quietly draw a guest credential for host work.
        if matches!(class, AuthorityClass::GuestSql) {
            tracing::error!(
                project,
                "wamn:postgres: guest-sql is not a platform authority; refusing the checkout"
            );
            return Err(PgError::ConnectionUnavailable);
        }
        self.checkout_class(class, project, None).await
    }
}
