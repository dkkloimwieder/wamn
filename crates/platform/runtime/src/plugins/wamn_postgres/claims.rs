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

use wamn_control_registry::identifiers::{valid_project, valid_runner, valid_schema, valid_tenant};

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

/// Host-only identity used to load one HTTP effect authorization snapshot.
#[derive(Debug, Clone, Copy)]
pub struct ConnectionEffectLookup<'a> {
    pub run_id: &'a str,
    pub root_plan_hash: &'a str,
    pub current_plan_hash: &'a str,
    pub frame_id: i64,
    pub local_node_id: &'a str,
    pub occurrence: i32,
    pub source_artifact_hash: &'a str,
    pub requirement_name: &'a str,
}

/// One transactionally consistent set of admitted HTTP effect facts.
#[derive(Debug, Clone)]
pub struct ConnectionEffectSnapshot {
    pub run_status: String,
    pub root_plan_matches: bool,
    pub resolution_matches: bool,
    pub attempt_matches: bool,
    pub requirement_json: Option<serde_json::Value>,
    pub node_permitted: bool,
    pub binding_active: bool,
    pub binding_valid: bool,
    pub instance_id: Option<String>,
    pub requirement_type: Option<String>,
    pub contract: Option<String>,
    pub instance_enabled: bool,
    pub active_generation: Option<i64>,
    pub generation: Option<i64>,
    pub definition: Option<serde_json::Value>,
    pub definition_hash: Option<String>,
    pub credential_handle: Option<String>,
    /// True for release runs; draft runs require an exact current unrevoked
    /// generation grant at this immediate pre-network snapshot.
    pub draft_generation_granted: bool,
}

/// Immutable identity rows for one claimed run's complete resolution map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunResolutionMetadata {
    pub tenant_id: String,
    pub root_flow_id: String,
    pub root_execution_bundle_hash: String,
    pub plans: Vec<ResolutionPlanMetadata>,
}

/// One immutable `run_flow_resolutions` row, without its cacheable bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionPlanMetadata {
    pub flow_id: String,
    pub execution_bundle_hash: String,
    pub source_artifact_hash: String,
}

/// Exact bytes loaded by immutable tenant-scoped execution-bundle identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolutionPlanBytes {
    pub execution_bundle_hash: String,
    pub exact_bytes: Vec<u8>,
}

const RUN_RESOLUTION_METADATA_SQL: &str = "\
SELECT run.tenant_id, run.flow_id, run.execution_bundle_hash, \
       resolution.flow_id, resolution.execution_bundle_hash, \
       resolution.source_artifact_hash \
  FROM runs AS run \
  JOIN run_flow_resolutions AS resolution \
    ON resolution.tenant_id = run.tenant_id \
   AND resolution.run_id = run.run_id \
 WHERE run.tenant_id = current_setting('app.tenant', true) \
   AND run.run_id = $1 \
 ORDER BY resolution.flow_id";

const RESOLUTION_PLAN_BYTES_SQL: &str = "\
SELECT bundle.execution_bundle_hash, bundle.exact_bytes \
  FROM catalog.execution_bundles AS bundle \
 WHERE bundle.tenant_id = current_setting('app.tenant', true) \
   AND bundle.execution_bundle_hash = ANY($1::text[]) \
 ORDER BY bundle.execution_bundle_hash";

const CONNECTION_EFFECT_SNAPSHOT_SQL: &str = "\
WITH authorized_plan AS MATERIALIZED ( \
    SELECT bundle.tenant_id, bundle.execution_bundle_hash, bundle.exact_bytes \
      FROM catalog.execution_bundles AS bundle \
     WHERE bundle.execution_bundle_hash = $3 \
       AND EXISTS ( \
           SELECT 1 \
             FROM run_flow_resolutions AS resolution \
            WHERE resolution.tenant_id = bundle.tenant_id \
              AND resolution.run_id = $1 \
              AND resolution.execution_bundle_hash = $3 \
              AND resolution.source_artifact_hash = $7 \
       ) \
) \
SELECT r.status, r.execution_bundle_hash = $2, \
       plan.execution_bundle_hash IS NOT NULL, attempt.attempt_id IS NOT NULL, \
       requirement.requirement_json::text, \
       COALESCE(plan_node.match_count = 1 AND plan_node.permitted, false), \
       binding.binding_status = 'active', binding.validation_status = 'valid', \
       instance.instance_id, instance.requirement_type, instance.contract, \
       instance.lifecycle_status = 'enabled', instance.active_generation, \
       generation.generation, generation.definition_json::text, generation.definition_hash, \
       generation.credential_set_handle, \
       CASE \
           WHEN r.trigger_source = 'scenario-draft' \
            AND r.invocation_context #>> '{source,producer}' = 'draft-scenario' \
           THEN grant_row.generation IS NOT NULL AND grant_row.revoked_at IS NULL \
           WHEN r.trigger_source IS DISTINCT FROM 'scenario-draft' \
            AND r.invocation_context #>> '{source,producer}' IS DISTINCT FROM 'draft-scenario' \
           THEN true \
           ELSE false \
       END \
  FROM runs AS r \
  LEFT JOIN authorized_plan AS plan ON plan.tenant_id = r.tenant_id \
  LEFT JOIN effect_attempts AS attempt \
    ON attempt.tenant_id = r.tenant_id \
   AND attempt.run_id = r.run_id \
   AND attempt.root_plan_hash = $2 \
   AND attempt.current_plan_hash = $3 \
   AND attempt.frame_id = $4 \
   AND attempt.local_node_id = $5 \
   AND attempt.occurrence = $6 \
   AND attempt.source_artifact_hash = $7 \
   AND attempt.requirement_name = $8 \
  LEFT JOIN catalog.connection_requirements AS requirement \
    ON requirement.tenant_id = r.tenant_id \
   AND requirement.artifact_hash = $7 \
   AND requirement.requirement_name = $8 \
  LEFT JOIN LATERAL ( \
      SELECT count(*) AS match_count, \
             COALESCE(bool_and( \
                 node.value ->> 'effect-policy' = 'effectful' \
                 AND node.value #>> '{source-connection-requirement,name}' = $8 \
                 AND node.value -> 'source-connection-requirement' -> 'descriptor' \
                     = requirement.requirement_json \
             ), false) AS permitted \
        FROM jsonb_array_elements( \
            convert_from(plan.exact_bytes, 'UTF8')::jsonb #> '{body,nodes}' \
        ) AS node(value) \
       WHERE node.value ->> 'local-node-id' = $5 \
  ) AS plan_node ON true \
  LEFT JOIN catalog.connection_bindings AS binding \
    ON binding.tenant_id = r.tenant_id AND binding.catalog_id = r.catalog_id \
   AND binding.catalog_version = r.catalog_version \
   AND binding.artifact_hash = $7 \
   AND binding.requirement_name = $8 \
   AND binding.environment = r.environment \
  LEFT JOIN catalog.connection_instances AS instance \
    ON instance.tenant_id = binding.tenant_id \
   AND instance.environment = binding.environment \
   AND instance.instance_id = binding.instance_id \
  LEFT JOIN catalog.connection_generations AS generation \
    ON generation.tenant_id = instance.tenant_id \
   AND generation.environment = instance.environment \
   AND generation.instance_id = instance.instance_id \
   AND generation.generation = instance.active_generation \
  LEFT JOIN catalog.draft_safe_connection_grants AS grant_row \
    ON grant_row.tenant_id = generation.tenant_id \
   AND grant_row.environment = generation.environment \
   AND grant_row.instance_id = generation.instance_id \
   AND grant_row.generation = generation.generation \
 WHERE r.run_id = $1";

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
    let literal = wamn_pg_core::quote_literal(&json);
    format!(" SELECT pg_logical_emit_message(true, 'wamn.causation', {literal});")
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

    /// `(project, (size, available, waiting))` for every built pool — the
    /// snapshot the [9.8] pool-saturation observable gauges fold over.
    pub fn pool_status_all(&self) -> Vec<(String, (usize, usize, usize))> {
        self.pools
            .read()
            .expect("pools lock poisoned")
            .iter()
            .map(|(project, pp)| {
                let s = pp.pool.status();
                (project.clone(), (s.size, s.available, s.waiting))
            })
            .collect()
    }

    /// [9.8] Register the `wamn.postgres.pool.{size,available,waiting}` observable
    /// gauges (deadpool `Pool::status()`), keyed by `wamn.project`. The callbacks
    /// hold a `Weak` back to the plugin so registration never keeps it alive, and
    /// they observe every currently-built pool at export time. Call ONCE per
    /// process (observable instruments warn on duplicate registration); a no-op
    /// until the global meter provider is installed (`OTEL_*`).
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
                        for (project, status) in plugin.pool_status_all() {
                            o.observe(read(&status), &[KeyValue::new("wamn.project", project)]);
                        }
                    }
                })
                .build();
        }
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

    /// Load every host-derived HTTP authorization input in one read-only
    /// transaction under the component's injected tenant and run-state schema.
    pub async fn connection_effect_snapshot(
        &self,
        component_id: &str,
        project: &str,
        tenant: &str,
        lookup: &ConnectionEffectLookup<'_>,
    ) -> anyhow::Result<Option<ConnectionEffectSnapshot>> {
        let schema = self.schema_for(component_id);
        let (conn, policy) = self
            .checkout(project)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if let Err(error) = self
            .begin_with_claims(
                &conn,
                tenant,
                schema.as_deref(),
                None,
                None,
                policy.statement_timeout_ms,
            )
            .await
        {
            self.destroy(conn);
            return Err(anyhow::anyhow!(error.to_string()));
        }
        let result: anyhow::Result<Option<ConnectionEffectSnapshot>> = async {
            let params: [&(dyn ToSql + Sync); 8] = [
                &lookup.run_id,
                &lookup.root_plan_hash,
                &lookup.current_plan_hash,
                &lookup.frame_id,
                &lookup.local_node_id,
                &lookup.occurrence,
                &lookup.source_artifact_hash,
                &lookup.requirement_name,
            ];
            let row = conn
                .query_opt(CONNECTION_EFFECT_SNAPSHOT_SQL, &params)
                .await
                .context("query HTTP effect authorization snapshot")?;
            let Some(row) = row else {
                return Ok(None);
            };
            let json = |index| -> anyhow::Result<Option<serde_json::Value>> {
                let value: Option<String> =
                    row.try_get(index).context("decode HTTP effect JSON fact")?;
                value
                    .map(|value| {
                        serde_json::from_str(&value).context("parse HTTP effect JSON fact")
                    })
                    .transpose()
            };
            let snapshot = ConnectionEffectSnapshot {
                run_status: row.try_get(0)?,
                root_plan_matches: row.try_get::<_, Option<bool>>(1)?.unwrap_or(false),
                resolution_matches: row.try_get::<_, Option<bool>>(2)?.unwrap_or(false),
                attempt_matches: row.try_get::<_, Option<bool>>(3)?.unwrap_or(false),
                requirement_json: json(4)?,
                node_permitted: row.try_get::<_, Option<bool>>(5)?.unwrap_or(false),
                binding_active: row.try_get::<_, Option<bool>>(6)?.unwrap_or(false),
                binding_valid: row.try_get::<_, Option<bool>>(7)?.unwrap_or(false),
                instance_id: row.try_get(8)?,
                requirement_type: row.try_get(9)?,
                contract: row.try_get(10)?,
                instance_enabled: row.try_get::<_, Option<bool>>(11)?.unwrap_or(false),
                active_generation: row.try_get(12)?,
                generation: row.try_get(13)?,
                definition: json(14)?,
                definition_hash: row.try_get(15)?,
                credential_handle: row.try_get(16)?,
                draft_generation_granted: row.try_get::<_, Option<bool>>(17)?.unwrap_or(false),
            };
            Ok(Some(snapshot))
        }
        .await;
        match result {
            Ok(snapshot) => {
                if let Err(error) = conn.batch_execute("COMMIT").await {
                    self.destroy(conn);
                    return Err(error).context("commit HTTP effect authorization snapshot");
                }
                Ok(snapshot)
            }
            Err(error) => {
                if conn.batch_execute("ROLLBACK").await.is_err() {
                    self.destroy(conn);
                }
                Err(error)
            }
        }
    }

    /// Load only the immutable resolution identities already materialized for a run.
    ///
    /// This never reads release membership or a catalog head. The component's
    /// host-injected tenant, project, and schema claims scope the read.
    pub async fn run_resolution_metadata(
        &self,
        component_id: &str,
        run_id: &str,
    ) -> anyhow::Result<Option<RunResolutionMetadata>> {
        let tenant = self
            .tenant_for(component_id)
            .context("plan supply has no host-injected tenant")?;
        let project = self.project_for(component_id);
        let schema = self.schema_for(component_id);
        let (conn, policy) = self
            .checkout(&project)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if let Err(error) = self
            .begin_with_claims(
                &conn,
                &tenant,
                schema.as_deref(),
                None,
                None,
                policy.statement_timeout_ms,
            )
            .await
        {
            self.destroy(conn);
            return Err(anyhow::anyhow!(error.to_string()));
        }
        let result: anyhow::Result<Option<RunResolutionMetadata>> = async {
            let rows = conn
                .query(RUN_RESOLUTION_METADATA_SQL, &[&run_id])
                .await
                .context("query immutable run resolution metadata")?;
            let Some(first) = rows.first() else {
                return Ok(None);
            };
            let tenant_id: String = first.try_get(0)?;
            let root_flow_id: String = first.try_get(1)?;
            let root_execution_bundle_hash: String = first.try_get(2)?;
            let plans = rows
                .into_iter()
                .map(|row| {
                    Ok(ResolutionPlanMetadata {
                        flow_id: row.try_get(3)?,
                        execution_bundle_hash: row.try_get(4)?,
                        source_artifact_hash: row.try_get(5)?,
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            Ok(Some(RunResolutionMetadata {
                tenant_id,
                root_flow_id,
                root_execution_bundle_hash,
                plans,
            }))
        }
        .await;
        match result {
            Ok(metadata) => {
                if let Err(error) = conn.batch_execute("COMMIT").await {
                    self.destroy(conn);
                    return Err(error).context("commit immutable resolution metadata read");
                }
                Ok(metadata)
            }
            Err(error) => {
                if conn.batch_execute("ROLLBACK").await.is_err() {
                    self.destroy(conn);
                }
                Err(error)
            }
        }
    }

    /// Load exact immutable plan bytes for cache misses under injected tenant RLS.
    pub async fn resolution_plan_bytes(
        &self,
        component_id: &str,
        execution_bundle_hashes: &[String],
    ) -> anyhow::Result<Vec<ResolutionPlanBytes>> {
        if execution_bundle_hashes.is_empty() {
            return Ok(Vec::new());
        }
        let tenant = self
            .tenant_for(component_id)
            .context("plan supply has no host-injected tenant")?;
        let project = self.project_for(component_id);
        let schema = self.schema_for(component_id);
        let (conn, policy) = self
            .checkout(&project)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if let Err(error) = self
            .begin_with_claims(
                &conn,
                &tenant,
                schema.as_deref(),
                None,
                None,
                policy.statement_timeout_ms,
            )
            .await
        {
            self.destroy(conn);
            return Err(anyhow::anyhow!(error.to_string()));
        }
        let hashes = execution_bundle_hashes.to_vec();
        let result: anyhow::Result<Vec<ResolutionPlanBytes>> = async {
            conn.query(RESOLUTION_PLAN_BYTES_SQL, &[&hashes])
                .await
                .context("query immutable resolution plan bytes")?
                .into_iter()
                .map(|row| {
                    Ok(ResolutionPlanBytes {
                        execution_bundle_hash: row.try_get(0)?,
                        exact_bytes: row.try_get(1)?,
                    })
                })
                .collect()
        }
        .await;
        match result {
            Ok(bytes) => {
                if let Err(error) = conn.batch_execute("COMMIT").await {
                    self.destroy(conn);
                    return Err(error).context("commit immutable plan-byte read");
                }
                Ok(bytes)
            }
            Err(error) => {
                if conn.batch_execute("ROLLBACK").await.is_err() {
                    self.destroy(conn);
                }
                Err(error)
            }
        }
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

    #[test]
    fn plan_supply_reads_only_immutable_run_map_and_bundle_identity() {
        assert!(RUN_RESOLUTION_METADATA_SQL.contains("JOIN run_flow_resolutions"));
        assert!(RUN_RESOLUTION_METADATA_SQL.contains("run.run_id = $1"));
        assert!(RUN_RESOLUTION_METADATA_SQL.contains("ORDER BY resolution.flow_id"));
        assert!(!RUN_RESOLUTION_METADATA_SQL.contains("release_flows"));
        assert!(!RUN_RESOLUTION_METADATA_SQL.contains("active"));
        assert!(RESOLUTION_PLAN_BYTES_SQL.contains("catalog.execution_bundles"));
        assert!(RESOLUTION_PLAN_BYTES_SQL.contains("ANY($1::text[])"));
        assert!(!RESOLUTION_PLAN_BYTES_SQL.contains("flow_id"));
    }

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

    #[test]
    fn effect_authority_uses_the_exact_attempt_and_resolved_current_plan() {
        for required in [
            "FROM runs AS r",
            "FROM run_flow_resolutions AS resolution",
            "resolution.run_id = $1",
            "resolution.execution_bundle_hash = $3",
            "resolution.source_artifact_hash = $7",
            "FROM catalog.execution_bundles AS bundle",
            "bundle.execution_bundle_hash = $3",
            "convert_from(plan.exact_bytes, 'UTF8')::jsonb #> '{body,nodes}'",
            "node.value ->> 'local-node-id' = $5",
            "node.value ->> 'effect-policy' = 'effectful'",
            "node.value #>> '{source-connection-requirement,name}' = $8",
            "node.value -> 'source-connection-requirement' -> 'descriptor'",
            "= requirement.requirement_json",
            "LEFT JOIN effect_attempts AS attempt",
            "attempt.run_id = r.run_id",
            "attempt.root_plan_hash = $2",
            "attempt.current_plan_hash = $3",
            "attempt.frame_id = $4",
            "attempt.local_node_id = $5",
            "attempt.occurrence = $6",
            "attempt.source_artifact_hash = $7",
            "attempt.requirement_name = $8",
        ] {
            assert!(
                CONNECTION_EFFECT_SNAPSHOT_SQL.contains(required),
                "effect authority snapshot omits {required:?}"
            );
        }
        assert!(CONNECTION_EFFECT_SNAPSHOT_SQL.contains("SELECT count(*) AS match_count"));
        assert!(CONNECTION_EFFECT_SNAPSHOT_SQL.contains("plan_node.match_count = 1"));
    }

    #[test]
    fn effect_authority_keeps_root_and_callee_plan_hashes_independent() {
        assert!(
            CONNECTION_EFFECT_SNAPSHOT_SQL.contains("r.execution_bundle_hash = $2"),
            "the root plan must bind independently from the active frame"
        );
        assert!(
            CONNECTION_EFFECT_SNAPSHOT_SQL.contains("bundle.execution_bundle_hash = $3"),
            "the current callee plan must select its own exact bytes"
        );
        assert!(CONNECTION_EFFECT_SNAPSHOT_SQL.contains("attempt.root_plan_hash = $2"));
        assert!(CONNECTION_EFFECT_SNAPSHOT_SQL.contains("attempt.current_plan_hash = $3"));
        assert!(!CONNECTION_EFFECT_SNAPSHOT_SQL.contains("$2 = $3"));
        assert!(!CONNECTION_EFFECT_SNAPSHOT_SQL.contains("$3 = $2"));
    }

    #[test]
    fn effect_authority_has_no_root_graph_or_mutable_run_fallback() {
        let sql = CONNECTION_EFFECT_SNAPSHOT_SQL.to_ascii_lowercase();
        for retired in [
            "graph_json",
            "flow_artifacts",
            "validated_flow_drafts",
            "node_runs",
            "recursive",
            "parent_frame_id",
            "call_site_id",
        ] {
            assert!(!sql.contains(retired), "effect authority retains {retired}");
        }
        for write in [" insert ", " update ", " delete "] {
            assert!(!sql.contains(write), "effect authority performs {write:?}");
        }
    }

    #[test]
    fn effect_authority_resolves_the_current_binding_and_draft_grant() {
        for required in [
            "requirement.artifact_hash = $7",
            "requirement.requirement_name = $8",
            "binding.catalog_id = r.catalog_id",
            "binding.catalog_version = r.catalog_version",
            "binding.artifact_hash = $7",
            "binding.requirement_name = $8",
            "binding.environment = r.environment",
            "generation.generation = instance.active_generation",
            "catalog.draft_safe_connection_grants AS grant_row",
            "grant_row.revoked_at IS NULL",
            "r.trigger_source = 'scenario-draft'",
            "#>> '{source,producer}' = 'draft-scenario'",
            "trigger_source IS DISTINCT FROM 'scenario-draft'",
            "#>> '{source,producer}' IS DISTINCT FROM 'draft-scenario'",
        ] {
            assert!(
                CONNECTION_EFFECT_SNAPSHOT_SQL.contains(required),
                "effect authority snapshot omits {required:?}"
            );
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

    fn database_url_for_role(database_url: &str, role: &str, password: &str) -> String {
        let mut url = url::Url::parse(database_url).expect("database URL is an absolute URI");
        url.set_username(role)
            .expect("PostgreSQL URL accepts a username");
        url.set_password(Some(password))
            .expect("PostgreSQL URL accepts a password");
        url.to_string()
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

    /// Full PostgreSQL proof for current-frame HTTP authority. The configured
    /// database is disposable: this test resets the canonical catalog and run
    /// schemas before applying both schema-of-record files.
    #[tokio::test]
    #[ignore = "requires WAMN_CONNECTION_EFFECT_PG_URL for a disposable PostgreSQL 18 superuser database"]
    async fn live_effect_authority_uses_callee_plan_and_exact_attempt() {
        use sha2::{Digest as _, Sha256};

        const CATALOG_SQL: &str = include_str!("../../../../../../deploy/sql/catalog-schema.sql");
        const RUN_STATE_SQL: &str = include_str!("../../../../../../deploy/sql/run-state.sql");
        const TENANT: &str = "effect-live";
        const COMPONENT: &str = "effect-live-runner";
        const RUN_ID: &str = "effect-live-run";
        const LOCAL_NODE_ID: &str = "send-notice";
        const REQUIREMENT_NAME: &str = "manager";
        const FRAME_ID: i64 = 7;
        const OCCURRENCE: i32 = 2;

        let Some(admin_url) = std::env::var("WAMN_CONNECTION_EFFECT_PG_URL").ok() else {
            eprintln!(
                "WAMN_CONNECTION_EFFECT_PG_URL unset — skipping the ignored PostgreSQL 18 \
                 current-frame HTTP authority proof"
            );
            return;
        };
        let mut admin = connect_raw(&admin_url).await;
        let server = admin
            .query_one(
                "SELECT current_setting('server_version_num')::int, \
                        (SELECT rolsuper FROM pg_roles WHERE rolname = current_user)",
                &[],
            )
            .await
            .expect("inspect disposable PostgreSQL server");
        let server_version: i32 = server.get(0);
        let is_superuser: bool = server.get(1);
        assert!(
            (180_000..190_000).contains(&server_version),
            "effect authority live proof requires PostgreSQL 18"
        );
        assert!(
            is_superuser,
            "WAMN_CONNECTION_EFFECT_PG_URL must name a disposable superuser database"
        );

        admin
            .batch_execute(
                "DO $$ BEGIN \
                   IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_app') THEN \
                     CREATE ROLE wamn_app LOGIN PASSWORD 'wamn_app' \
                       NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
                   END IF; \
                   IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_scenario_author') THEN \
                     CREATE ROLE wamn_scenario_author NOLOGIN \
                       NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
                   END IF; \
                 END $$; \
                 ALTER ROLE wamn_app WITH LOGIN PASSWORD 'wamn_app' \
                   NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
                 ALTER ROLE wamn_scenario_author WITH NOLOGIN \
                   NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS; \
                 DROP SCHEMA IF EXISTS wamn_run CASCADE; \
                 DROP SCHEMA IF EXISTS catalog CASCADE;",
            )
            .await
            .expect("reset disposable effect authority schemas and roles");
        admin
            .batch_execute(CATALOG_SQL)
            .await
            .expect("apply full catalog schema of record");
        admin
            .batch_execute(RUN_STATE_SQL)
            .await
            .expect("apply full run-state schema of record");

        let digest = |bytes: &[u8]| format!("sha256:{}", hex::encode(Sha256::digest(bytes)));
        let root_artifact_hash = digest(b"effect-live-root-artifact");
        let callee_artifact_hash = digest(b"effect-live-callee-artifact");
        let descriptor =
            serde_json::to_value(wamn_flow::node_contract::ConnectionTypeDescriptor::http_v1())
                .expect("serialize canonical HTTP descriptor");
        let flowrunner_revision = digest(b"effect-live-flowrunner");
        let effect_provider_revision = digest(b"effect-live-effect-provider");
        let callable_input_hash = wamn_flow::canonical_json_sha256(&serde_json::Value::Bool(true));
        let root_plan_bytes = serde_json::to_vec(&serde_json::json!({
            "header": {
                "format-version": "0.1",
                "plan-compiler-revision": "0.1",
                "runtime-revision": {
                    "flowrunner-component-digest": flowrunner_revision.clone(),
                    "effect-provider-revision": effect_provider_revision.clone(),
                    "host-effect-contract-version": "0.1"
                },
                "root-artifact-hash": root_artifact_hash.clone()
            },
            "body": {
                "entry-instruction": "root-entry",
                "nodes": [{
                    "local-node-id": "root-entry",
                    "source-node-id": "root-entry",
                    "type": "event",
                    "config": {},
                    "effect-policy": "pure"
                }],
                "edges": [],
                "root-terminal-behavior": {"kind": "frontier-exhaustion"},
                "entry-input-schema-guard": true,
                "callable-contract": null,
                "source-map": [{
                    "local-node-id": "root-entry",
                    "source-node-id": "root-entry"
                }]
            }
        }))
        .expect("encode root execution plan bytes");
        let callee_plan_bytes = serde_json::to_vec(&serde_json::json!({
            "header": {
                "format-version": "0.1",
                "plan-compiler-revision": "0.1",
                "runtime-revision": {
                    "flowrunner-component-digest": flowrunner_revision,
                    "effect-provider-revision": effect_provider_revision,
                    "host-effect-contract-version": "0.1"
                },
                "root-artifact-hash": callee_artifact_hash.clone()
            },
            "body": {
                "entry-instruction": "callee-entry",
                "nodes": [
                    {
                        "local-node-id": "callee-entry",
                        "source-node-id": "callee-entry",
                        "type": "request",
                        "config": {"input-schema": true},
                        "effect-policy": "pure"
                    },
                    {
                        "local-node-id": LOCAL_NODE_ID,
                        "source-node-id": LOCAL_NODE_ID,
                        "type": "http-call",
                        "config": {},
                        "effect-policy": "effectful",
                        "source-connection-requirement": {
                            "name": REQUIREMENT_NAME,
                            "descriptor": descriptor.clone()
                        }
                    },
                    {
                        "local-node-id": "callee-respond",
                        "source-node-id": "callee-respond",
                        "type": "respond",
                        "config": {"status": 200},
                        "effect-policy": "pure"
                    }
                ],
                "edges": [
                    {
                        "source": "callee-entry",
                        "source-port": "main",
                        "destination": LOCAL_NODE_ID,
                        "fan-out-ordinal": 0
                    },
                    {
                        "source": LOCAL_NODE_ID,
                        "source-port": "main",
                        "destination": "callee-respond",
                        "fan-out-ordinal": 0
                    }
                ],
                "root-terminal-behavior": {
                    "kind": "respond",
                    "responders": ["callee-respond"]
                },
                "entry-input-schema-guard": true,
                "callable-contract": {
                    "version": "0.1",
                    "input-schema-hash": callable_input_hash,
                    "return-contract": "untyped-json-body",
                    "effect-ceiling": "effectful"
                },
                "source-map": [
                    {
                        "local-node-id": "callee-entry",
                        "source-node-id": "callee-entry"
                    },
                    {
                        "local-node-id": LOCAL_NODE_ID,
                        "source-node-id": LOCAL_NODE_ID
                    },
                    {
                        "local-node-id": "callee-respond",
                        "source-node-id": "callee-respond"
                    }
                ]
            }
        }))
        .expect("encode callee execution plan bytes");
        let root_plan_hash = digest(&root_plan_bytes);
        let current_plan_hash = digest(&callee_plan_bytes);
        assert_ne!(
            root_plan_hash, current_plan_hash,
            "the proof must execute inside a distinct callee plan"
        );
        let root_plan_length =
            i32::try_from(root_plan_bytes.len()).expect("root plan fits PostgreSQL int");
        let callee_plan_length =
            i32::try_from(callee_plan_bytes.len()).expect("callee plan fits PostgreSQL int");
        let root_plan_slice = root_plan_bytes.as_slice();
        let callee_plan_slice = callee_plan_bytes.as_slice();
        let members_json = serde_json::to_string(&serde_json::json!([
            {
                "flow-id": "root-flow",
                "flow-version": 1,
                "artifact-hash": root_artifact_hash.clone()
            },
            {
                "flow-id": "callee-flow",
                "flow-version": 1,
                "artifact-hash": callee_artifact_hash.clone()
            }
        ]))
        .expect("encode release members");
        let descriptor_json =
            serde_json::to_string(&descriptor).expect("encode connection requirement");

        let seed = admin
            .transaction()
            .await
            .expect("begin effect authority seed transaction");
        seed.execute(
            "INSERT INTO catalog.catalogs \
               (tenant_id,catalog_id,version,environment,schema_version,state) \
             VALUES ($1,'effect-catalog',1,'prod','0.1','applied')",
            &[&TENANT],
        )
        .await
        .expect("seed release catalog");
        seed.execute(
            "INSERT INTO catalog.flow_artifacts \
               (tenant_id,flow_id,flow_version,schema_version,graph_json,graph_hash,artifact_hash) \
             VALUES \
               ($1,'root-flow',1,'0.1','{}'::jsonb,'root-graph',$2), \
               ($1,'callee-flow',1,'0.1','{}'::jsonb,'callee-graph',$3)",
            &[&TENANT, &root_artifact_hash, &callee_artifact_hash],
        )
        .await
        .expect("seed distinct source artifacts");
        seed.execute(
            "INSERT INTO catalog.execution_bundles \
               (tenant_id,execution_bundle_hash,format_version,exact_bytes,byte_length) \
             VALUES ($1,$2,'0.1',$3,$4), ($1,$5,'0.1',$6,$7)",
            &[
                &TENANT,
                &root_plan_hash,
                &root_plan_slice,
                &root_plan_length,
                &current_plan_hash,
                &callee_plan_slice,
                &callee_plan_length,
            ],
        )
        .await
        .expect("seed distinct exact execution bundles");
        seed.execute(
            "INSERT INTO catalog.release_manifests \
               (tenant_id,catalog_id,catalog_version,members_json) \
             VALUES ($1,'effect-catalog',1,$2::text::jsonb)",
            &[&TENANT, &members_json],
        )
        .await
        .expect("seed release manifest");
        seed.execute(
            "INSERT INTO catalog.release_flows \
               (tenant_id,catalog_id,catalog_version,flow_id,flow_version,execution_bundle_hash) \
             VALUES \
               ($1,'effect-catalog',1,'root-flow',1,$2), \
               ($1,'effect-catalog',1,'callee-flow',1,$3)",
            &[&TENANT, &root_plan_hash, &current_plan_hash],
        )
        .await
        .expect("seed release flow membership");
        seed.execute(
            "INSERT INTO catalog.connection_requirements \
               (tenant_id,artifact_hash,requirement_name,requirement_json,requirement_hash) \
             VALUES ($1,$2,$3,$4::text::jsonb,'effect-live-requirement')",
            &[
                &TENANT,
                &callee_artifact_hash,
                &REQUIREMENT_NAME,
                &descriptor_json,
            ],
        )
        .await
        .expect("seed callee source requirement");
        seed.execute(
            "INSERT INTO catalog.connection_instances \
               (tenant_id,environment,instance_id,requirement_type,contract) \
             VALUES ($1,'prod','manager-prod','http','wamn:connection/http@0.1.0')",
            &[&TENANT],
        )
        .await
        .expect("seed connection instance");
        seed.execute(
            "INSERT INTO catalog.connection_generations \
               (tenant_id,environment,instance_id,generation,definition_json,definition_hash,credential_set_handle) \
             VALUES ($1,'prod','manager-prod',1, \
                     '{\"primary-authority\":\"https://manager.example\",\"tls-verification\":\"verify-authority\"}'::jsonb, \
                     'effect-live-definition','effect-live-credential')",
            &[&TENANT],
        )
        .await
        .expect("seed active connection generation");
        seed.execute(
            "UPDATE catalog.connection_instances \
                SET active_generation = 1, revision = revision + 1, \
                    updated_at = updated_at + interval '1 second' \
              WHERE tenant_id = $1 AND environment = 'prod' \
                AND instance_id = 'manager-prod'",
            &[&TENANT],
        )
        .await
        .expect("activate connection generation");
        seed.execute(
            "INSERT INTO catalog.connection_bindings \
               (tenant_id,catalog_id,catalog_version,artifact_hash,requirement_name, \
                environment,instance_id,binding_status,validation_status,validation_hash) \
             VALUES ($1,'effect-catalog',1,$2,$3,'prod','manager-prod', \
                     'active','valid','effect-live-binding')",
            &[&TENANT, &callee_artifact_hash, &REQUIREMENT_NAME],
        )
        .await
        .expect("seed release-bound connection binding");
        seed.execute(
            "INSERT INTO wamn_run.runs \
               (tenant_id,run_id,flow_id,flow_version,catalog_id,catalog_version, \
                environment,execution_bundle_hash,status,trigger_source,invocation_context) \
             VALUES ($1,$2,'root-flow',1,'effect-catalog',1,'prod',$3, \
                     'running','manual','{\"source\":{\"producer\":\"manual\"}}'::jsonb)",
            &[&TENANT, &RUN_ID, &root_plan_hash],
        )
        .await
        .expect("seed root-pinned running run");
        seed.execute(
            "INSERT INTO wamn_run.run_flow_resolutions \
               (tenant_id,run_id,flow_id,execution_bundle_hash,source_artifact_hash) \
             VALUES \
               ($1,$2,'root-flow',$3,$4), \
               ($1,$2,'callee-flow',$5,$6)",
            &[
                &TENANT,
                &RUN_ID,
                &root_plan_hash,
                &root_artifact_hash,
                &current_plan_hash,
                &callee_artifact_hash,
            ],
        )
        .await
        .expect("seed complete root and callee resolution map");
        seed.execute(
            "INSERT INTO wamn_run.effect_attempts \
               (tenant_id,run_id,root_plan_hash,current_plan_hash,frame_id,parent_frame_id, \
                call_site_id,local_node_id,source_artifact_hash,requirement_name,occurrence, \
                seq,generation_fact_kind,connection_name,connection_generation, \
                credential_generation,attempt_started_at,attempt_deadline_at,attempt_input_ref) \
             VALUES ($1,$2,$3,$4,$5,3,'call-callee',$6,$7,$8,$9,0,'attested', \
                     'manager-prod','1','effect-live-credential', \
                     clock_timestamp(),clock_timestamp() + interval '1 minute', \
                     'sha256:effect-live-input')",
            &[
                &TENANT,
                &RUN_ID,
                &root_plan_hash,
                &current_plan_hash,
                &FRAME_ID,
                &LOCAL_NODE_ID,
                &callee_artifact_hash,
                &REQUIREMENT_NAME,
                &OCCURRENCE,
            ],
        )
        .await
        .expect("superuser seed exact immutable attempt attestation");
        seed.commit()
            .await
            .expect("commit effect authority fixture");

        let privileges_before = admin
            .query_one(
                "SELECT has_table_privilege('wamn_app','wamn_run.effect_attempts','SELECT'), \
                        has_table_privilege('wamn_app','wamn_run.effect_attempts','INSERT'), \
                        has_function_privilege( \
                          'wamn_app','wamn_run.guard_effect_fact_append()','EXECUTE')",
                &[],
            )
            .await
            .expect("inspect immutable attempt privileges");
        assert!(privileges_before.get::<_, bool>(0));
        assert!(!privileges_before.get::<_, bool>(1));
        assert!(!privileges_before.get::<_, bool>(2));

        let app_url = database_url_for_role(&admin_url, "wamn_app", "wamn_app");
        let postgres = WamnPostgres::new(WamnPostgresConfig {
            database_url: Some(app_url),
            pool_max_size: 1,
            wait_timeout_ms: 2_000,
            statement_timeout_ms: 5_000,
            row_limit: 1_000,
        })
        .expect("construct production PostgreSQL plugin");
        postgres
            .set_schema(COMPONENT, "wamn_run")
            .expect("set production run-state search path");
        let exact_lookup = ConnectionEffectLookup {
            run_id: RUN_ID,
            root_plan_hash: &root_plan_hash,
            current_plan_hash: &current_plan_hash,
            frame_id: FRAME_ID,
            local_node_id: LOCAL_NODE_ID,
            occurrence: OCCURRENCE,
            source_artifact_hash: &callee_artifact_hash,
            requirement_name: REQUIREMENT_NAME,
        };
        let exact = postgres
            .connection_effect_snapshot(COMPONENT, DEFAULT_PROJECT, TENANT, &exact_lookup)
            .await
            .expect("load exact production authority snapshot")
            .expect("root run is visible under tenant RLS");
        assert_eq!(exact.run_status, "running");
        assert!(exact.root_plan_matches);
        assert!(exact.resolution_matches);
        assert!(exact.attempt_matches);
        assert!(exact.node_permitted);
        assert_eq!(exact.requirement_json.as_ref(), Some(&descriptor));
        assert!(exact.binding_active);
        assert!(exact.binding_valid);
        assert_eq!(exact.instance_id.as_deref(), Some("manager-prod"));
        assert!(exact.instance_enabled);
        assert_eq!(exact.active_generation, Some(1));
        assert_eq!(exact.generation, Some(1));
        assert!(exact.draft_generation_granted);

        let wrong_occurrence = postgres
            .connection_effect_snapshot(
                COMPONENT,
                DEFAULT_PROJECT,
                TENANT,
                &ConnectionEffectLookup {
                    occurrence: OCCURRENCE + 1,
                    ..exact_lookup
                },
            )
            .await
            .expect("load wrong-occurrence snapshot")
            .expect("run remains visible for wrong occurrence");
        assert!(wrong_occurrence.root_plan_matches);
        assert!(wrong_occurrence.resolution_matches);
        assert!(wrong_occurrence.node_permitted);
        assert!(!wrong_occurrence.attempt_matches);

        let unknown_plan_hash = digest(b"effect-live-unknown-current-plan");
        let wrong_current = postgres
            .connection_effect_snapshot(
                COMPONENT,
                DEFAULT_PROJECT,
                TENANT,
                &ConnectionEffectLookup {
                    current_plan_hash: &unknown_plan_hash,
                    ..exact_lookup
                },
            )
            .await
            .expect("load wrong-current-plan snapshot")
            .expect("run remains visible for wrong current plan");
        assert!(wrong_current.root_plan_matches);
        assert!(!wrong_current.resolution_matches);
        assert!(!wrong_current.node_permitted);
        assert!(!wrong_current.attempt_matches);

        let unknown_source_hash = digest(b"effect-live-unknown-source-artifact");
        let wrong_source = postgres
            .connection_effect_snapshot(
                COMPONENT,
                DEFAULT_PROJECT,
                TENANT,
                &ConnectionEffectLookup {
                    source_artifact_hash: &unknown_source_hash,
                    ..exact_lookup
                },
            )
            .await
            .expect("load wrong-source snapshot")
            .expect("run remains visible for wrong source artifact");
        assert!(!wrong_source.resolution_matches);
        assert!(!wrong_source.attempt_matches);
        assert!(!wrong_source.node_permitted);
        assert!(wrong_source.requirement_json.is_none());

        let wrong_requirement = postgres
            .connection_effect_snapshot(
                COMPONENT,
                DEFAULT_PROJECT,
                TENANT,
                &ConnectionEffectLookup {
                    requirement_name: "other-manager",
                    ..exact_lookup
                },
            )
            .await
            .expect("load wrong-requirement snapshot")
            .expect("run remains visible for wrong requirement");
        assert!(wrong_requirement.resolution_matches);
        assert!(!wrong_requirement.attempt_matches);
        assert!(!wrong_requirement.node_permitted);
        assert!(wrong_requirement.requirement_json.is_none());

        let privileges_after = admin
            .query_one(
                "SELECT has_table_privilege('wamn_app','wamn_run.effect_attempts','SELECT'), \
                        has_table_privilege('wamn_app','wamn_run.effect_attempts','INSERT'), \
                        has_function_privilege( \
                          'wamn_app','wamn_run.guard_effect_fact_append()','EXECUTE')",
                &[],
            )
            .await
            .expect("recheck immutable attempt privileges");
        assert!(privileges_after.get::<_, bool>(0));
        assert!(!privileges_after.get::<_, bool>(1));
        assert!(!privileges_after.get::<_, bool>(2));
    }

    // R18-neg (wamn-2jkm.65) — the fail-CLOSED branch, exercised against a REAL
    // server booted with standard_conforming_strings=off. The positive above
    // proves the hook passes on a stock server; this proves it REJECTS an unsafe
    // one and that the guest sees `connection-unavailable`. Gated on a SEPARATE
    // url (WAMN_SCS_OFF_PG_URL) so it never runs against the stock test server;
    // skipped LOUDLY when unset. Recipe: docs/archive/build-and-test.md [R18-NEG].
    #[tokio::test]
    async fn live_scs_off_server_fails_checkout_closed() {
        let Some(url) = std::env::var("WAMN_SCS_OFF_PG_URL").ok() else {
            eprintln!(
                "WAMN_SCS_OFF_PG_URL unset — skipping the wamn-2jkm.65 R18 live negative \
                 (boot a postgres:18 with -c standard_conforming_strings=off; see \
                 docs/archive/build-and-test.md [R18-NEG])"
            );
            return;
        };

        // CONTROL: the server must be REACHABLE and genuinely report scs=off, so
        // the checkout failure below is the HOOK rejecting a live server, not a
        // dead url or a network-level connect failure. A raw connect that returns
        // "off" proves both — and if the url were dead this connect would panic,
        // so the test cannot false-pass against a server-down url.
        let raw = connect_raw(&url).await;
        let scs: String = raw
            .query_one("SHOW standard_conforming_strings", &[])
            .await
            .expect("control: server reachable for the scs probe")
            .get(0);
        assert_eq!(
            scs, "off",
            "control: point WAMN_SCS_OFF_PG_URL at a server booted with \
             standard_conforming_strings=off (got {scs:?}); otherwise this test is vacuous"
        );

        // The production path: build the plugin exactly as production does and
        // check out. build_pool installs the R18 post_create hook, which runs
        // `SHOW standard_conforming_strings` on the new physical connection and
        // fails the create; checkout maps that pool error to the WIT
        // `connection-unavailable` variant the guest sees.
        let pg = WamnPostgres::new(WamnPostgresConfig {
            database_url: Some(url.clone()),
            pool_max_size: 1,
            wait_timeout_ms: 2_000,
            statement_timeout_ms: 5_000,
            row_limit: 1_000,
        })
        .unwrap();
        // (`matches!`, not `expect_err`, so the Ok type need not be `Debug`.)
        let result = pg.checkout(DEFAULT_PROJECT).await;
        assert!(
            matches!(result, Err(PgError::ConnectionUnavailable)),
            "scs=off must fail CLOSED as the guest-visible connection-unavailable \
             variant — a checkout that succeeded means the hook did not reject"
        );

        // Hook-SPECIFICITY: reach the raw pool error (which checkout collapses to
        // connection-unavailable) and confirm it is the R18 post_create hook —
        // not an auth/other failure that ALSO maps to connection-unavailable. The
        // control above already ruled out server-down; this pins the cause.
        let pool = WamnPostgres::build_pool(&ProjectConfig {
            database_url: url,
            pool_max_size: 1,
            wait_timeout_ms: 2_000,
            statement_timeout_ms: 5_000,
            row_limit: 1_000,
        })
        .expect("pool builds (url parses; the hook runs at checkout, not build)");
        let raw_err = match pool.get().await {
            Ok(_) => panic!("raw checkout unexpectedly SUCCEEDED against a scs=off server"),
            Err(e) => e,
        };
        let rendered = raw_err.to_string();
        assert!(
            rendered.contains("standard_conforming_strings"),
            "the pool error must be the R18 fail-closed hook, got: {rendered}"
        );
    }
}
