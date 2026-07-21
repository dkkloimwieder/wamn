//! The claim boundary as ONE reviewable security unit (SR4, wamn-cjv.18): the
//! `WamnPostgres` plugin state (the correlated claim maps), the identity-format
//! validators it imports, the in-band claim/causation-mutation guard, and the
//! `set_config()`-bound claim injection (`begin_with_claims`). This is the exact
//! surface the injection review (R2/R16/R16b/cjv.2/l5i9.12.2) reasons about.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context as _;
use deadpool_postgres::{Manager, ManagerConfig, Object, Pool, RecyclingMethod, Runtime, Timeouts};
use tokio_postgres::NoTls;
use tokio_postgres::types::ToSql;

use wamn_event_wire::Causation;

use wamn_registry::identifiers::{valid_project, valid_runner, valid_schema, valid_tenant};

use super::pool::{
    CheckoutProbe, CredentialProvider, ProjectConfig, ProjectPool, StaticCredentialProvider,
    WamnPostgresConfig, destroy_connection, standard_conforming_strings_hook,
};
use super::resources::{run_execute, run_query};
use super::types::map_pg_error;
use super::{DEFAULT_PROJECT, PgError, RowSet, SqlValue};

pub struct WamnPostgres {
    /// Resolves a project id → its database connection + policy.
    provider: Arc<dyn CredentialProvider>,
    /// project id → live pool + policy, built lazily on first use and memoized
    /// for the plugin's lifetime. Strict per-host caps (D5 hybrid v0/P1); a
    /// pgBouncer tier, when added, sits under this map transparently.
    pools: std::sync::RwLock<HashMap<String, Arc<ProjectPool>>>,
    /// component id → tenant claim.
    tenants: std::sync::RwLock<HashMap<String, String>>,
    /// component id → project id (which database). Absent ⇒ the default project.
    projects: std::sync::RwLock<HashMap<String, String>>,
    /// component id → `search_path` schema. Empty (the default) leaves the
    /// server's search_path alone — so S2/pgbench behaviour is unchanged. When
    /// set, the plugin injects `SET LOCAL search_path` alongside the tenant
    /// claim, so unqualified table names resolve to a host-chosen schema (S6:
    /// prod = the shared fixture schema, test = a per-run ephemeral schema).
    schemas: std::sync::RwLock<HashMap<String, String>>,
    /// component id → durable-queue lease owner (fqg.4). Absent (the default)
    /// leaves `app.runner` unset — so every non-claiming path (S2..S6, the
    /// gateway) is byte-unchanged. When set, the plugin injects
    /// `SET LOCAL app.runner` so a flowrunner replica reads its owner identity to
    /// claim/renew queue rows under.
    runners: std::sync::RwLock<HashMap<String, String>>,
    /// component id → the causation context {run, root, depth} of the run the
    /// trusted flow-runner is currently driving (l5i9.12.2). Declared through
    /// the `wamn:runner/causation` channel ([`add_runner_causation_to_linker`]),
    /// cleared (removed) between runs. Absent (the default) ⇒ no causation is
    /// stamped — so every non-run path (S2..S6, the gateway, benches without a
    /// declaration) is byte-unchanged. When set, [`begin_with_claims`] appends a
    /// TRANSACTIONAL `wamn.causation` logical message to every transaction the
    /// plugin opens for that component, which the CDC reader (l5i9.12.1)
    /// stitches onto the txn's row events.
    current_run: std::sync::RwLock<HashMap<String, Causation>>,
    /// Connections destroyed instead of repooled (chaos-gate observability).
    pub(super) destroyed: Arc<AtomicU64>,
}

/// Reject guest SQL that would set or reset a session variable or role in-band.
///
/// A guest on the transaction / one-shot / cursor API must not be able to
/// rewrite its host-injected `app.tenant` claim (or switch roles) and defeat
/// RLS tenant isolation (wamn-cjv.2 / review C4-1). RLS keys on the settable
/// GUC `current_setting('app.tenant', …)`, and the `wamn_app` login role
/// (`NOSUPERUSER NOBYPASSRLS`) may freely `SET` it; a later
/// `SET app.tenant = 'victim'` overrides the BEGIN-time `SET LOCAL`.
///
/// The extended-query protocol forbids statement chaining, so a claim override
/// can only arrive as a *standalone* `SET` / `RESET` / `set_config(…)`
/// statement — which this catches. It is a defense-in-depth **blocklist**, not
/// a structural close: raw dynamic SQL (`DO` / `EXECUTE`) can still build a
/// claim mutation at runtime. The structural close re-keys RLS onto a
/// non-settable identity (per-tenant role + `current_user`) and is a
/// prerequisite for enabling the raw-SQL node (wamn-1nd).
pub(super) fn reject_claim_mutation(sql: &str) -> Result<(), PgError> {
    if statement_mutates_session(sql) {
        tracing::warn!(
            target: "wamn::security",
            "rejected an in-band claim/role mutation on the guest SQL surface"
        );
        return Err(PgError::QueryError((
            "WAMN0".to_string(),
            "in-band claim or role mutation is not permitted".to_string(),
        )));
    }
    if statement_forges_causation(sql) {
        tracing::warn!(
            target: "wamn::security",
            "rejected a guest wamn.* logical-message emit on the guest SQL surface"
        );
        return Err(PgError::QueryError((
            "WAMN0".to_string(),
            "emitting a wamn.* logical message is not permitted".to_string(),
        )));
    }
    Ok(())
}

/// True if `sql` calls `pg_logical_emit_message` (either overload) AND names the
/// reserved `wamn.` message-prefix namespace (l5i9.12.2). Only the plugin's own
/// [`begin_with_claims`] emit — which runs through `batch_execute`, NOT this
/// guest surface — may write a `wamn.causation` frame; a guest forging one over
/// the parameterized query/execute/cursor surface would ride its own txn's
/// commit and the reader (l5i9.12.1) would stitch it, misattributing causation.
/// This is a defense-in-depth **blocklist** (the AR1 theme, like
/// [`reject_claim_mutation`]), not a structural close: matching is
/// case-insensitive and comment-stripped, and over-rejects the rare statement
/// that merely names both tokens in a literal — fail-closed, acceptable on this
/// (flag-OFF) raw surface. A guest's own non-`wamn.` logical messages are left
/// alone (the reader ignores them).
fn statement_forges_causation(sql: &str) -> bool {
    let lower = sql.to_ascii_lowercase();
    lower.contains("pg_logical_emit_message") && lower.contains("wamn.")
}

/// The transactional `wamn.causation` logical-message emit appended to a
/// run-owned transaction's BEGIN batch (l5i9.12.2). The [`Causation`] is
/// serialized canonically (`{"run":..,"root":..,"depth":..}` — the reader
/// deserializes with `deny_unknown_fields`) and SQL-escaped (single quotes
/// doubled) for safe literal embedding in the simple-query batch, which takes
/// no bind params. `transactional = true` so the message rides the txn's commit
/// at its own LSN; the reader (l5i9.12.1) buffers the whole txn and stamps this
/// onto every row event regardless of frame order.
fn causation_emit_sql(c: &Causation) -> String {
    let json = serde_json::to_string(c).expect("Causation serializes to JSON");
    let escaped = json.replace('\'', "''");
    format!(" SELECT pg_logical_emit_message(true, 'wamn.causation', '{escaped}');")
}

/// The fully-bound claim statement run inside the plugin-managed transaction
/// (R2/R16). Every claim value travels as a bind parameter (`$1..$4`) — there is
/// NO string-interpolation path, so an injection-shaped tenant / schema / runner
/// is *unrepresentable* as SQL, not merely rejected by validation. `set_config`
/// with `is_local => true` is the exact `SET LOCAL` equivalent (scoped to the
/// current transaction). Parameter order:
///
/// - `$1` `app.tenant` — the RLS claim (always present).
/// - `$2` `statement_timeout` — as TEXT (a bare-integer string = milliseconds).
/// - `$3` `search_path` — `COALESCE($3, current_setting('search_path'))`, so a
///   NULL bind (absent schema) preserves the server's default search_path; the
///   S2/pgbench path is byte-unchanged.
/// - `$4` `app.runner` — `COALESCE($4, current_setting('app.runner', true))`, so
///   a NULL bind (absent runner) re-asserts the current value (a no-op), exactly
///   like the pre-fqg.4 "no `app.runner` statement" path.
///
/// The `wamn.causation` emit (l5i9.12.2) is NOT part of this statement — it is a
/// separate, already-escaped simple-query emit appended by [`begin_with_claims`]
/// only for a run-owned transaction.
const CLAIM_SQL: &str = "SELECT \
     set_config('app.tenant', $1, true), \
     set_config('statement_timeout', $2, true), \
     set_config('search_path', COALESCE($3, current_setting('search_path')), true), \
     set_config('app.runner', COALESCE($4, current_setting('app.runner', true)), true)";

/// Reject a malformed claim identity before it is bound (R16). Since R2 these
/// validators are NO LONGER the injection boundary — every claim value binds as a
/// parameter into [`CLAIM_SQL`], so a `'`/`;`/`--` value is inert data — but a
/// malformed identity still fails closed: they define what a *legal* id is (and
/// the no-hyphen `valid_schema` rule still matters where a schema name is quoted
/// into DDL elsewhere).
fn validate_claims(
    tenant: &str,
    schema: Option<&str>,
    runner: Option<&str>,
) -> Result<(), PgError> {
    if !valid_tenant(tenant) {
        return Err(PgError::QueryError((
            "WAMN0".to_string(),
            "invalid tenant identity".to_string(),
        )));
    }
    if let Some(schema) = schema
        && !valid_schema(schema)
    {
        return Err(PgError::QueryError((
            "WAMN0".to_string(),
            "invalid search_path schema".to_string(),
        )));
    }
    if let Some(runner) = runner
        && !valid_runner(runner)
    {
        return Err(PgError::QueryError((
            "WAMN0".to_string(),
            "invalid runner owner".to_string(),
        )));
    }
    Ok(())
}

/// True if `sql`'s first keyword is `SET` (covers `SET LOCAL` / `SET SESSION` /
/// `SET ROLE` / `SET SESSION AUTHORIZATION`) or `RESET`, or if it calls
/// `set_config` anywhere (CTE, sub-select, target list). `current_setting`
/// (a *read* of a GUC) is deliberately allowed. Matching is case-insensitive;
/// leading whitespace and SQL comments are stripped so a comment prefix cannot
/// hide the keyword.
fn statement_mutates_session(sql: &str) -> bool {
    let lower = sql.to_ascii_lowercase();
    let head = strip_leading_noise(&lower);
    if starts_with_keyword(head, "set") || starts_with_keyword(head, "reset") {
        return true;
    }
    // `set_config` is the only GUC-*write* function; `current_setting` reads and
    // is not matched by this substring. Over-rejects the rare statement that
    // merely names `set_config` in a literal/identifier — fail-closed, which is
    // acceptable on this (flag-OFF) raw surface.
    lower.contains("set_config")
}

/// Strip leading whitespace and SQL comments (`--` line, `/* … */` block) so
/// the first real token can be inspected. Best-effort: an unterminated block
/// comment stops stripping and the statement is inspected from there, which
/// only makes the guard *more* likely to reject (fail-closed).
fn strip_leading_noise(sql: &str) -> &str {
    let mut s = sql.trim_start();
    loop {
        if let Some(rest) = s.strip_prefix("--") {
            match rest.find('\n') {
                Some(i) => s = rest[i + 1..].trim_start(),
                None => return "",
            }
        } else if let Some(rest) = s.strip_prefix("/*") {
            match rest.find("*/") {
                Some(i) => s = rest[i + 2..].trim_start(),
                None => return s,
            }
        } else {
            return s;
        }
    }
}

/// True if `head` (already lowercased and comment-stripped) begins with `kw` as
/// a whole keyword — followed by whitespace or end-of-input, so `set` matches
/// `set …` but not `settings`.
fn starts_with_keyword(head: &str, kw: &str) -> bool {
    match head.strip_prefix(kw) {
        Some(rest) => rest.is_empty() || rest.starts_with(|c: char| c.is_whitespace()),
        None => false,
    }
}

impl WamnPostgres {
    /// Plugin over a single default database (the [`WamnPostgresConfig`] URL).
    /// Pools are built lazily; `database_url: None` ⇒ every call returns
    /// `connection-unavailable`.
    pub fn new(cfg: WamnPostgresConfig) -> anyhow::Result<Self> {
        let default = cfg
            .database_url
            .clone()
            .map(|url| ProjectConfig::from_global(url, &cfg));
        Ok(Self::with_provider(Arc::new(
            StaticCredentialProvider::default_only(default),
        )))
    }

    /// Plugin over an explicit [`CredentialProvider`] (multi-project / tests).
    pub fn with_provider(provider: Arc<dyn CredentialProvider>) -> Self {
        Self {
            provider,
            pools: std::sync::RwLock::new(HashMap::new()),
            tenants: std::sync::RwLock::new(HashMap::new()),
            projects: std::sync::RwLock::new(HashMap::new()),
            schemas: std::sync::RwLock::new(HashMap::new()),
            runners: std::sync::RwLock::new(HashMap::new()),
            current_run: std::sync::RwLock::new(HashMap::new()),
            destroyed: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Build from the environment: the default project from
    /// `DATABASE_URL`/`WAMN_PG_URL`, plus any explicit projects listed in the
    /// JSON at `WAMN_PG_PROJECTS_FILE` (mounted like a Secret/ConfigMap).
    pub fn from_env() -> anyhow::Result<Self> {
        let cfg = WamnPostgresConfig::from_env();
        let default = cfg
            .database_url
            .clone()
            .map(|url| ProjectConfig::from_global(url, &cfg));
        let mut projects = HashMap::new();
        if let Ok(path) = std::env::var("WAMN_PG_PROJECTS_FILE") {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("read WAMN_PG_PROJECTS_FILE {path}"))?;
            projects = StaticCredentialProvider::projects_from_json(&text, &cfg)?;
        }
        Ok(Self::with_provider(Arc::new(
            StaticCredentialProvider::new(projects, default),
        )))
    }

    /// Build a deadpool pool for one project's connection config.
    fn build_pool(cfg: &ProjectConfig) -> anyhow::Result<Pool> {
        let pg_config: tokio_postgres::Config = cfg
            .database_url
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid database url: {e}"))?;
        let mgr = Manager::from_config(
            pg_config,
            NoTls,
            ManagerConfig {
                recycling_method: RecyclingMethod::Fast,
            },
        );
        let timeout = std::time::Duration::from_millis(cfg.wait_timeout_ms);
        Ok(Pool::builder(mgr)
            .max_size(cfg.pool_max_size)
            .timeouts(Timeouts {
                wait: Some(timeout),
                create: Some(timeout),
                recycle: Some(timeout),
            })
            // R18: assert standard_conforming_strings=on once per new connection.
            .post_create(standard_conforming_strings_hook())
            .runtime(Runtime::Tokio1)
            .build()?)
    }

    /// Resolve + lazily build (memoized) the pool for a project. Unknown project
    /// or a build/resolution failure ⇒ `connection-unavailable`.
    fn ensure_pool(&self, project: &str) -> Result<Arc<ProjectPool>, PgError> {
        if let Some(pp) = self.pools.read().expect("pools lock poisoned").get(project) {
            return Ok(pp.clone());
        }
        let cfg = match self.provider.resolve(project) {
            Ok(Some(c)) => c,
            Ok(None) => {
                tracing::warn!(project, "wamn:postgres: no credentials for project");
                return Err(PgError::ConnectionUnavailable);
            }
            Err(e) => {
                tracing::warn!(project, error = %e, "wamn:postgres: credential resolution failed");
                return Err(PgError::ConnectionUnavailable);
            }
        };
        let pp = match Self::build_pool(&cfg) {
            Ok(pool) => Arc::new(ProjectPool {
                pool,
                statement_timeout_ms: cfg.statement_timeout_ms,
                row_limit: cfg.row_limit,
            }),
            Err(e) => {
                tracing::warn!(project, error = %e, "wamn:postgres: pool build failed");
                return Err(PgError::ConnectionUnavailable);
            }
        };
        let mut w = self.pools.write().expect("pools lock poisoned");
        Ok(w.entry(project.to_string()).or_insert(pp).clone())
    }

    /// Register the tenant claim for a component id. The bench harness calls
    /// this directly; the host path feeds it from workload bind.
    pub fn set_tenant(&self, component_id: &str, tenant: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            valid_tenant(tenant),
            "invalid tenant {tenant:?}: 1-64 chars of [A-Za-z0-9_-] required"
        );
        self.tenants
            .write()
            .expect("tenants lock poisoned")
            .insert(component_id.to_string(), tenant.to_string());
        Ok(())
    }

    pub(super) fn tenant_for(&self, component_id: &str) -> Option<String> {
        self.tenants
            .read()
            .expect("tenants lock poisoned")
            .get(component_id)
            .cloned()
    }

    /// Register which project's database a component uses. The bench harness
    /// calls this directly; the host path feeds it from the `wamn.project`
    /// workload config. Absent ⇒ the default project.
    pub fn set_project(&self, component_id: &str, project: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            valid_project(project),
            "invalid project {project:?}: 1-64 chars of [A-Za-z0-9_-] required"
        );
        self.projects
            .write()
            .expect("projects lock poisoned")
            .insert(component_id.to_string(), project.to_string());
        Ok(())
    }

    pub(super) fn project_for(&self, component_id: &str) -> String {
        self.projects
            .read()
            .expect("projects lock poisoned")
            .get(component_id)
            .cloned()
            .unwrap_or_else(|| DEFAULT_PROJECT.to_string())
    }

    /// Number of live (built) project pools — gate observability.
    pub fn project_pool_count(&self) -> usize {
        self.pools.read().expect("pools lock poisoned").len()
    }

    /// Register the `search_path` schema for a component id. When set, every
    /// transaction the plugin opens for that component also runs
    /// `SET LOCAL search_path`, so the guest's unqualified table names resolve
    /// to a host-chosen schema. The bench harness calls this directly; the host
    /// path feeds it from the `wamn.schema` workload config.
    pub fn set_schema(&self, component_id: &str, schema: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            valid_schema(schema),
            "invalid schema {schema:?}: 1-63 chars of [A-Za-z0-9_] starting with a letter/underscore required"
        );
        self.schemas
            .write()
            .expect("schemas lock poisoned")
            .insert(component_id.to_string(), schema.to_string());
        Ok(())
    }

    pub(super) fn schema_for(&self, component_id: &str) -> Option<String> {
        self.schemas
            .read()
            .expect("schemas lock poisoned")
            .get(component_id)
            .cloned()
    }

    /// Register the durable-queue lease owner for a component id (fqg.4). When
    /// set, every transaction the plugin opens for that component also runs
    /// `SET LOCAL app.runner`, so a flowrunner replica reads a stable owner to
    /// claim/renew queue rows under. The bench harness calls this directly (a
    /// distinct owner per replica); the host path feeds it from the
    /// `wamn.runner` workload config.
    pub fn set_runner(&self, component_id: &str, runner: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            valid_runner(runner),
            "invalid runner owner {runner:?}: 1-128 chars of [A-Za-z0-9_-] required"
        );
        self.runners
            .write()
            .expect("runners lock poisoned")
            .insert(component_id.to_string(), runner.to_string());
        Ok(())
    }

    pub(super) fn runner_for(&self, component_id: &str) -> Option<String> {
        self.runners
            .read()
            .expect("runners lock poisoned")
            .get(component_id)
            .cloned()
    }

    /// Declare (`Some`) or clear (`None`) the causation context of the run a
    /// component is driving (l5i9.12.2). The trusted flow-runner feeds this
    /// through the `wamn:runner/causation` channel; while set, every
    /// transaction the plugin opens for the component carries a `wamn.causation`
    /// message. The bench harness / live tests call this directly, exactly like
    /// [`set_tenant`](Self::set_tenant) / [`set_runner`](Self::set_runner).
    pub fn set_current_run(&self, component_id: &str, ctx: Option<Causation>) {
        let mut w = self.current_run.write().expect("current_run lock poisoned");
        match ctx {
            Some(c) => {
                w.insert(component_id.to_string(), c);
            }
            None => {
                w.remove(component_id);
            }
        }
    }

    pub(super) fn current_run_for(&self, component_id: &str) -> Option<Causation> {
        self.current_run
            .read()
            .expect("current_run lock poisoned")
            .get(component_id)
            .cloned()
    }

    /// Reap EVERY per-component-id claim registry this plugin keeps for a workload
    /// on teardown (R31): tenant, project, search_path schema, runner lease-owner,
    /// and the causation run context — all set at workload bind (or via the
    /// runner channel) and keyed by component id. Without this a stale claim
    /// survives unbind, the maps grow across workload churn, and a rebound
    /// component id inherits the prior claim. The `pools` map is deliberately NOT
    /// touched: it is keyed by PROJECT (shared, memoized for the plugin's
    /// lifetime), not by component id. Keyed like the fork's builtin postgres
    /// plugin — a workload's component ids are prefixed by the workload id — so
    /// everything NOT under it is retained; an unknown workload id is a no-op.
    pub(super) fn clear_component_claims(&self, workload_id: &str) {
        let retain = |c: &String| !c.starts_with(workload_id);
        self.tenants
            .write()
            .expect("tenants lock poisoned")
            .retain(|c, _| retain(c));
        self.projects
            .write()
            .expect("projects lock poisoned")
            .retain(|c, _| retain(c));
        self.schemas
            .write()
            .expect("schemas lock poisoned")
            .retain(|c, _| retain(c));
        self.runners
            .write()
            .expect("runners lock poisoned")
            .retain(|c, _| retain(c));
        self.current_run
            .write()
            .expect("current_run lock poisoned")
            .retain(|c, _| retain(c));
    }

    /// Connections destroyed instead of repooled since startup.
    pub fn destroyed_connections(&self) -> u64 {
        self.destroyed.load(Ordering::Relaxed)
    }

    /// (size, available, waiting) of a project's pool, if it has been built.
    pub fn pool_status_of(&self, project: &str) -> Option<(usize, usize, usize)> {
        self.pools
            .read()
            .expect("pools lock poisoned")
            .get(project)
            .map(|pp| {
                let s = pp.pool.status();
                (s.size, s.available, s.waiting)
            })
    }

    /// Default-project pool status (single-DB benches).
    pub fn pool_status(&self) -> Option<(usize, usize, usize)> {
        self.pool_status_of(DEFAULT_PROJECT)
    }

    /// Check out a raw connection from the default project and report its state
    /// *before* any claim injection. Gate verification only.
    pub async fn probe_checkout(&self) -> anyhow::Result<CheckoutProbe> {
        self.probe_checkout_of(DEFAULT_PROJECT).await
    }

    /// Check out a raw connection from a project's (lazily built) pool and
    /// report its state *before* any claim injection. Gate verification only —
    /// not reachable from guests.
    pub async fn probe_checkout_of(&self, project: &str) -> anyhow::Result<CheckoutProbe> {
        let pp = self
            .ensure_pool(project)
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

    pub(super) fn destroy(&self, obj: Object) {
        destroy_connection(obj, &self.destroyed);
    }

    /// Check out a connection from a project's (lazily built) pool, returning
    /// the pool handle too so its statement-timeout/row-limit policy travels
    /// with the call.
    pub(super) async fn checkout(
        &self,
        project: &str,
    ) -> Result<(Object, Arc<ProjectPool>), PgError> {
        let pp = self.ensure_pool(project)?;
        let obj = pp.pool.get().await.map_err(|e| {
            tracing::warn!(project, error = %e, "wamn:postgres pool checkout failed");
            PgError::ConnectionUnavailable
        })?;
        Ok((obj, pp))
    }

    /// `BEGIN` + claim/limit injection. The claims are injected by ONE fully
    /// bound statement ([`CLAIM_SQL`]) whose every value travels as a bind
    /// parameter — there is no interpolation path (R2/R16). `tenant` is always
    /// present; `schema`/`runner` bind NULL when absent (COALESCE-to-current
    /// preserves the server default / prior value — the S2/pgbench path is
    /// byte-unchanged). A run-owned transaction also appends the transactional
    /// `wamn.causation` emit (l5i9.12.2).
    ///
    /// Cost: `BEGIN` and the bound claim statement are pipelined (issued without
    /// an await between them; tokio-postgres preserves FIFO order so `BEGIN`
    /// opens the txn before the transaction-LOCAL `set_config`s apply), and the
    /// claim statement is `prepare_cached`, so the steady-state round-trip count
    /// on a pooled connection matches the pre-R2 single batch.
    pub(super) async fn begin_with_claims(
        &self,
        conn: &Object,
        tenant: &str,
        schema: Option<&str>,
        runner: Option<&str>,
        run: Option<&Causation>,
        statement_timeout_ms: u32,
    ) -> Result<(), PgError> {
        validate_claims(tenant, schema, runner)?;
        let stmt = conn
            .prepare_cached(CLAIM_SQL)
            .await
            .map_err(|e| map_pg_error(&e))?;
        // statement_timeout binds as TEXT (a bare-integer string = ms).
        let timeout = statement_timeout_ms.to_string();
        let params: [&(dyn ToSql + Sync); 4] = [&tenant, &timeout, &schema, &runner];
        // Pipeline BEGIN ahead of the bound claim statement: both requests are
        // enqueued in `join!` poll order (BEGIN first) and travel in one flight;
        // tokio-postgres processes them FIFO, so the txn is open before the
        // transaction-LOCAL `set_config`s run.
        let (begin, claims) =
            tokio::join!(conn.batch_execute("BEGIN"), conn.execute(&stmt, &params));
        begin.map_err(|e| map_pg_error(&e))?;
        claims.map_err(|e| map_pg_error(&e))?;
        if let Some(run) = run {
            // l5i9.12.2: stamp the run's causation onto this txn. The
            // TRANSACTIONAL emit rides the commit; a rolled-back txn emits
            // nothing and the reader (l5i9.12.1) stitches it onto the txn's row
            // events. It carries no bind params, so the already-escaped
            // simple-query emit is unchanged by R2.
            conn.batch_execute(&causation_emit_sql(run))
                .await
                .map_err(|e| map_pg_error(&e))?;
        }
        Ok(())
    }

    pub(super) fn require_tenant(&self, component_id: &str) -> Result<String, PgError> {
        self.tenant_for(component_id).ok_or_else(|| {
            tracing::warn!(
                component_id,
                "wamn:postgres call from component with no tenant identity"
            );
            PgError::QueryError((
                "WAMN0".to_string(),
                "no tenant identity configured for this component".to_string(),
            ))
        })
    }

    /// Single statement in an implicit transaction: claims injected,
    /// committed on success, rolled back on statement failure.
    pub(super) async fn one_shot(
        &self,
        component_id: &str,
        sql: &str,
        params: &[SqlValue],
        want_rows: bool,
    ) -> Result<OneShotResult, PgError> {
        let tenant = self.require_tenant(component_id)?;
        let project = self.project_for(component_id);
        let schema = self.schema_for(component_id);
        let runner = self.runner_for(component_id);
        let run = self.current_run_for(component_id);
        let (conn, pp) = self.checkout(&project).await?;
        if let Err(e) = self
            .begin_with_claims(
                &conn,
                &tenant,
                schema.as_deref(),
                runner.as_deref(),
                run.as_ref(),
                pp.statement_timeout_ms,
            )
            .await
        {
            // Claim injection failed: connection state is unknown — destroy.
            self.destroy(conn);
            return Err(e);
        }
        let result = if want_rows {
            run_query(&conn, sql, params, pp.row_limit)
                .await
                .map(OneShotResult::Rows)
        } else {
            run_execute(&conn, sql, params)
                .await
                .map(OneShotResult::Count)
        };
        match result {
            Ok(v) => match conn.batch_execute("COMMIT").await {
                Ok(()) => Ok(v),
                Err(e) => {
                    self.destroy(conn);
                    Err(map_pg_error(&e))
                }
            },
            Err(pg_err) => {
                // Statement failed; roll the implicit transaction back and
                // repool. If even ROLLBACK fails the connection is toast.
                if let Err(e) = conn.batch_execute("ROLLBACK").await {
                    tracing::warn!(error = %e, "rollback after failed statement also failed; destroying connection");
                    self.destroy(conn);
                }
                Err(pg_err)
            }
        }
    }
}

pub(super) enum OneShotResult {
    Rows(RowSet),
    Count(u64),
}

#[cfg(test)]
mod tests {
    use super::*;

    // wamn-cjv.2 — the in-band claim/role mutation guard.
    #[test]
    fn guard_rejects_set_and_reset_variants() {
        for s in [
            "SET app.tenant = 'victim'",
            "set local app.tenant = 'victim'",
            "SET SESSION app.tenant TO 'victim'",
            "SET ROLE postgres",
            "set session authorization postgres",
            "RESET app.tenant",
            "RESET ALL",
            "   \n\t SET app.tenant='victim'",
            "/* sneaky */ SET app.tenant='victim'",
            "-- lead\nSET app.tenant='victim'",
        ] {
            assert!(statement_mutates_session(s), "should reject: {s:?}");
            assert!(reject_claim_mutation(s).is_err(), "should reject: {s:?}");
        }
    }

    #[test]
    fn guard_rejects_set_config_anywhere() {
        for s in [
            "SELECT set_config('app.tenant','victim',false)",
            "WITH t AS (SELECT set_config('app.tenant','victim',true)) SELECT 1",
            "select pg_catalog.set_config('app.tenant','victim',false)",
            "SELECT SET_CONFIG('app.tenant','victim',false)",
        ] {
            assert!(statement_mutates_session(s), "should reject: {s:?}");
        }
    }

    #[test]
    fn guard_allows_normal_statements_and_current_setting() {
        for s in [
            "SELECT count(*) FROM s2.rls_secrets WHERE secret LIKE $1",
            "INSERT INTO t (tenant_id, k) VALUES (current_setting('app.tenant', true), $1)",
            "UPDATE t SET a = 1 WHERE id = $1",
            "SELECT current_setting('app.tenant', true)",
            "SELECT * FROM settings",
            "DELETE FROM assets WHERE id = $1",
        ] {
            assert!(!statement_mutates_session(s), "should allow: {s:?}");
            assert!(reject_claim_mutation(s).is_ok(), "should allow: {s:?}");
        }
    }

    // l5i9.12.2 — the guest wamn.* logical-message forgery guard.
    #[test]
    fn guard_rejects_guest_causation_forgery() {
        for s in [
            "SELECT pg_logical_emit_message(true,'wamn.causation','{}')",
            "select PG_LOGICAL_EMIT_MESSAGE(true, 'wamn.causation', $1)",
            "SELECT pg_logical_emit_message_bytea(true,'wamn.anything','\\x00')",
            "/* hide */ SELECT pg_logical_emit_message(false,'wamn.x','y')",
            "WITH t AS (SELECT pg_logical_emit_message(true,'wamn.causation','z')) SELECT 1",
        ] {
            assert!(
                statement_forges_causation(s),
                "should detect forgery: {s:?}"
            );
            assert!(reject_claim_mutation(s).is_err(), "should reject: {s:?}");
        }
    }

    #[test]
    fn guard_allows_non_wamn_logical_messages_and_normal_sql() {
        for s in [
            // a guest's OWN (non-reserved) logical message is fine — the reader
            // only stitches `wamn.causation`.
            "SELECT pg_logical_emit_message(true,'app.audit','{}')",
            "SELECT count(*) FROM wamn_things WHERE id = $1",
            "INSERT INTO t (k) VALUES ($1)",
        ] {
            assert!(!statement_forges_causation(s), "should allow: {s:?}");
            assert!(reject_claim_mutation(s).is_ok(), "should allow: {s:?}");
        }
    }

    // l5i9.12.2 — the emit bytes are the load-bearing contract with the reader
    // (l5i9.12.1 parses `wamn.causation` via serde `deny_unknown_fields`), so pin
    // them exactly. A builder mutation that drops the message, flips
    // `transactional`, or reshapes the JSON must fail this.
    #[test]
    fn causation_emit_sql_pins_the_transactional_wamn_message() {
        let c = Causation {
            run: "r-1".into(),
            root: "r-1".into(),
            depth: 0,
        };
        assert_eq!(
            causation_emit_sql(&c),
            " SELECT pg_logical_emit_message(true, 'wamn.causation', '{\"run\":\"r-1\",\"root\":\"r-1\",\"depth\":0}');"
        );
    }

    #[test]
    fn causation_emit_sql_escapes_single_quotes_in_the_run_id() {
        // A run id with a single quote must not break the SQL literal: quotes are
        // doubled (injection-safe), the JSON itself is unchanged.
        let c = Causation {
            run: "o'brien".into(),
            root: "o'brien".into(),
            depth: 2,
        };
        assert_eq!(
            causation_emit_sql(&c),
            " SELECT pg_logical_emit_message(true, 'wamn.causation', '{\"run\":\"o''brien\",\"root\":\"o''brien\",\"depth\":2}');"
        );
    }

    // R2/R16 — the claim statement is a FIXED, fully-bound SELECT: every value is
    // a `$n` bind, there is no interpolation path. Pin its shape so a regression
    // that reintroduces `SET LOCAL` string-building or drops a claim fails here
    // (the unit-level twin of the "no `format!` with `SET LOCAL`" grep-gate).
    #[test]
    fn claim_sql_is_fully_bound_with_no_interpolation() {
        assert!(
            !CLAIM_SQL.to_ascii_uppercase().contains("SET LOCAL"),
            "CLAIM_SQL must not use SET LOCAL"
        );
        for frag in [
            "set_config('app.tenant', $1, true)",
            "set_config('statement_timeout', $2, true)",
            "set_config('search_path', COALESCE($3, current_setting('search_path')), true)",
            "set_config('app.runner', COALESCE($4, current_setting('app.runner', true)), true)",
        ] {
            assert!(CLAIM_SQL.contains(frag), "CLAIM_SQL missing {frag:?}");
        }
    }

    // R16 — the validators stay as the identity-format contract (demoted from the
    // injection boundary by R2): a malformed identity fails closed even though
    // the value would bind as inert data.
    #[test]
    fn validate_claims_rejects_malformed_identities() {
        assert!(validate_claims("acme", Some("public"), Some("owner-1")).is_ok());
        assert!(validate_claims("acme", None, None).is_ok());
        assert!(validate_claims("bad'tenant", None, None).is_err());
        assert!(validate_claims("acme", Some("has-hyphen"), None).is_err());
        assert!(validate_claims("acme", None, Some("bad;runner")).is_err());
    }

    #[test]
    fn set_and_clear_current_run_is_per_component() {
        let pg =
            WamnPostgres::with_provider(Arc::new(StaticCredentialProvider::default_only(None)));
        assert!(pg.current_run_for("c1").is_none());
        pg.set_current_run(
            "c1",
            Some(Causation {
                run: "r1".into(),
                root: "r1".into(),
                depth: 0,
            }),
        );
        assert_eq!(pg.current_run_for("c1").unwrap().run, "r1");
        // a second component is independent.
        assert!(pg.current_run_for("c2").is_none());
        // None clears it.
        pg.set_current_run("c1", None);
        assert!(pg.current_run_for("c1").is_none());
    }

    // R31 — unbind reaps ALL FIVE per-component claim registries for a workload
    // (tenant/project/schema/runner/causation) while leaving another workload's
    // component untouched; the project-keyed `pools` map is never touched here.
    // Keyed by the workload-id prefix (the fork's builtin convention). An unknown
    // workload id is a no-op.
    #[test]
    fn clear_component_claims_reaps_all_registries_for_the_workload() {
        let pg =
            WamnPostgres::with_provider(Arc::new(StaticCredentialProvider::default_only(None)));
        // Two components under workload "wl-a", one under "wl-b".
        for c in ["wl-a-component-0", "wl-b-component-0"] {
            pg.set_tenant(c, "acme").unwrap();
            pg.set_project(c, "proj").unwrap();
            pg.set_schema(c, "s_run").unwrap();
            pg.set_runner(c, "owner-1").unwrap();
            pg.set_current_run(
                c,
                Some(Causation {
                    run: "r1".into(),
                    root: "r1".into(),
                    depth: 0,
                }),
            );
        }

        // Unbinding an UNKNOWN workload clears nothing.
        pg.clear_component_claims("wl-unknown");
        assert_eq!(pg.tenant_for("wl-a-component-0").as_deref(), Some("acme"));

        pg.clear_component_claims("wl-a");

        // Every registry emptied for the unbound workload's component.
        assert_eq!(pg.tenant_for("wl-a-component-0"), None);
        // project_for falls back to DEFAULT_PROJECT once the claim is gone.
        assert_eq!(pg.project_for("wl-a-component-0"), DEFAULT_PROJECT);
        assert_eq!(pg.schema_for("wl-a-component-0"), None);
        assert_eq!(pg.runner_for("wl-a-component-0"), None);
        assert!(pg.current_run_for("wl-a-component-0").is_none());

        // The other workload's component is untouched across the board.
        assert_eq!(pg.tenant_for("wl-b-component-0").as_deref(), Some("acme"));
        assert_eq!(pg.project_for("wl-b-component-0"), "proj");
        assert_eq!(pg.schema_for("wl-b-component-0").as_deref(), Some("s_run"));
        assert_eq!(
            pg.runner_for("wl-b-component-0").as_deref(),
            Some("owner-1")
        );
        assert_eq!(pg.current_run_for("wl-b-component-0").unwrap().run, "r1");
    }

    // ------------------------------------------------------------------
    // Live-PG checks (hermetic; skipped cleanly when no test URL is set).
    // Set WAMN_PG_TEST_URL (or WAMN_PG_URL / DATABASE_URL) to a throwaway
    // Postgres. Each test creates + drops its own objects.
    // ------------------------------------------------------------------

    fn test_pg_url() -> Option<String> {
        std::env::var("WAMN_PG_TEST_URL")
            .or_else(|_| std::env::var("WAMN_PG_URL"))
            .or_else(|_| std::env::var("DATABASE_URL"))
            .ok()
    }

    async fn connect_raw(url: &str) -> tokio_postgres::Client {
        let (client, conn) = tokio_postgres::connect(url, NoTls).await.unwrap();
        tokio::spawn(async move {
            let _ = conn.await;
        });
        client
    }

    // R2/R16 — the ACTUAL bound claim statement makes injection-shaped and
    // unicode values INERT DATA: bound as `$n`, none takes statement-level effect
    // (a marker table a spliced `DROP`/`DELETE` would destroy survives).
    // `valid_*` would reject these values, but the point is the BIND is safe
    // regardless of validation.
    #[tokio::test]
    async fn live_bound_claims_are_injection_inert_and_txn_local() {
        let Some(url) = test_pg_url() else {
            return;
        };
        let client = connect_raw(&url).await;
        let marker = format!("wave2_marker_{}", std::process::id());
        client
            .batch_execute(&format!(
                "DROP TABLE IF EXISTS public.{marker}; \
                 CREATE TABLE public.{marker}(id int); \
                 INSERT INTO public.{marker} VALUES (1);"
            ))
            .await
            .unwrap();
        let stmt = client.prepare(CLAIM_SQL).await.unwrap();
        let timeout = "5000";

        // (1) app.tenant / app.runner are free-form custom GUCs: injection-shaped
        //     + unicode values bind as DATA and round-trip VERBATIM; the absent
        //     schema ($3 NULL) leaves the server-default search_path untouched.
        let default_sp: Option<String> = client
            .query_one("SELECT current_setting('search_path', true)", &[])
            .await
            .unwrap()
            .get(0);
        let evil_tenant = format!("x'; DROP TABLE public.{marker}; -- 😀Ω");
        let evil_runner = format!("r'; DELETE FROM public.{marker}; --");
        let no_schema: Option<&str> = None;
        client.batch_execute("BEGIN").await.unwrap();
        let params: [&(dyn ToSql + Sync); 4] = [&evil_tenant, &timeout, &no_schema, &evil_runner];
        client.execute(&stmt, &params).await.unwrap();

        let got_tenant: Option<String> = client
            .query_one("SELECT current_setting('app.tenant', true)", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(got_tenant.as_deref(), Some(evil_tenant.as_str()));
        let got_runner: Option<String> = client
            .query_one("SELECT current_setting('app.runner', true)", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(got_runner.as_deref(), Some(evil_runner.as_str()));
        let got_sp: Option<String> = client
            .query_one("SELECT current_setting('search_path', true)", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(
            got_sp, default_sp,
            "absent schema must preserve the default"
        );

        // marker survived — no spliced statement ran.
        let n: i64 = client
            .query_one(&format!("SELECT count(*) FROM public.{marker}"), &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(n, 1);
        client.batch_execute("COMMIT").await.unwrap();

        // SET LOCAL equivalence: after COMMIT the txn-local claim is gone. Per the
        // custom-GUC gotcha a touched GUC reverts to '' (NOT NULL) — the value the
        // RLS floor NULLIFs.
        let after: Option<String> = client
            .query_one("SELECT current_setting('app.tenant', true)", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(after.as_deref(), Some(""));

        // (2) The $3 (search_path) bind is a VALUE, not SQL: an injection-shaped
        //     schema is rejected by search_path's own list-check hook (22023) —
        //     parsed as data, never executed — and the marker still stands.
        client.batch_execute("BEGIN").await.unwrap();
        let evil_schema: Option<&str> = Some("s'; DROP TABLE public.foo; --");
        let params2: [&(dyn ToSql + Sync); 4] =
            [&evil_tenant, &timeout, &evil_schema, &evil_runner];
        let err = client.execute(&stmt, &params2).await.unwrap_err();
        assert_eq!(
            err.as_db_error().map(|db| db.code().code()),
            Some("22023"),
            "malformed search_path must fail as an invalid VALUE, not execute"
        );
        client.batch_execute("ROLLBACK").await.unwrap();
        let n2: i64 = client
            .query_one(&format!("SELECT count(*) FROM public.{marker}"), &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(n2, 1);

        client
            .batch_execute(&format!("DROP TABLE public.{marker}"))
            .await
            .unwrap();
    }

    // R2/R16 — the REAL plugin path: begin_with_claims injects all four claims via
    // the bound statement, they are visible in-txn, and revert after the txn.
    #[tokio::test]
    async fn live_begin_with_claims_sets_all_four_and_reverts() {
        let Some(url) = test_pg_url() else {
            return;
        };
        let pg = WamnPostgres::new(WamnPostgresConfig {
            database_url: Some(url),
            pool_max_size: 2,
            wait_timeout_ms: 2_000,
            statement_timeout_ms: 5_000,
            row_limit: 1_000,
        })
        .unwrap();
        let (conn, _pp) = pg.checkout(DEFAULT_PROJECT).await.unwrap();
        pg.begin_with_claims(&conn, "acme", Some("public"), Some("owner-1"), None, 4321)
            .await
            .unwrap();
        let row = conn
            .query_one(
                "SELECT current_setting('app.tenant', true), \
                 current_setting('statement_timeout', true), \
                 current_setting('search_path', true), \
                 current_setting('app.runner', true)",
                &[],
            )
            .await
            .unwrap();
        let tenant: Option<String> = row.get(0);
        let timeout: Option<String> = row.get(1);
        let sp: Option<String> = row.get(2);
        let runner: Option<String> = row.get(3);
        assert_eq!(tenant.as_deref(), Some("acme"));
        assert_eq!(timeout.as_deref(), Some("4321ms"));
        assert_eq!(sp.as_deref(), Some("public"));
        assert_eq!(runner.as_deref(), Some("owner-1"));

        // COMMIT (the one_shot success path): a `set_config(is_local => true)`
        // claim reverts even across a commit — proving it is truly LOCAL, not a
        // session-level leak.
        conn.batch_execute("COMMIT").await.unwrap();
        let after: Option<String> = conn
            .query_one("SELECT current_setting('app.tenant', true)", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(after.as_deref(), Some(""));
    }

    // R18 — the post_create hook runs on connect; a successful checkout from the
    // pool proves the assertion passed on this server (stock PG18 = on).
    #[tokio::test]
    async fn live_connect_asserts_standard_conforming_strings() {
        let Some(url) = test_pg_url() else {
            return;
        };
        let pg = WamnPostgres::new(WamnPostgresConfig {
            database_url: Some(url),
            pool_max_size: 1,
            wait_timeout_ms: 2_000,
            statement_timeout_ms: 5_000,
            row_limit: 1_000,
        })
        .unwrap();
        // The checkout builds the pool (with the R18 hook) and creates a physical
        // connection; the hook must pass for this to be Ok.
        let (conn, _pp) = pg
            .checkout(DEFAULT_PROJECT)
            .await
            .expect("checkout ok (scs=on)");
        let scs: String = conn
            .query_one("SHOW standard_conforming_strings", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(scs, "on");
    }
}
