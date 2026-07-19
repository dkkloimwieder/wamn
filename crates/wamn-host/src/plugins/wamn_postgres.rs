//! Real `wamn:postgres` host plugin (S2).
//!
//! Contract source of truth: docs/wamn-postgres.wit. Host-enforced invariants:
//!
//! - The guest never holds a socket. Connections live in a deadpool pool
//!   owned by the plugin; guests get resource handles only.
//! - Claims are derived from the executing component's identity
//!   (`Ctx::component_id` → tenant, registered at workload bind time from
//!   `localResources.config["wamn.tenant"]` or via [`WamnPostgres::set_tenant`])
//!   and injected by one fully-bound `set_config(…, is_local => true)` statement
//!   — the `SET LOCAL` equivalent — inside the plugin-managed transaction; every
//!   claim value travels as a bind parameter, so no interpolation path exists
//!   (R2/R16). Guest SQL that tries to set or reset a session variable or
//!   role in-band (`SET` / `RESET` / `set_config`, e.g. a later
//!   `SET app.tenant = 'other'` that would override the BEGIN-time claim) is
//!   rejected on the query/execute/cursor surface (see
//!   [`reject_claim_mutation`], wamn-cjv.2), closing the reachable
//!   transaction-API override. This is a defense-in-depth blocklist, **not** a
//!   structural close: raw dynamic SQL (`DO` / `EXECUTE`) can still construct a
//!   claim mutation, so re-keying RLS onto a non-settable identity (a per-tenant
//!   DB role + `current_user`) is a prerequisite for enabling the raw-SQL node
//!   (wamn-1nd) — until then the claim is trusted only on the parameterized
//!   standard-node path.
//! - `statement_timeout` and a row limit are applied host-side per call.
//! - Abnormal instance death (store dropped mid-transaction, e.g. an epoch
//!   kill) destroys the underlying connection via [`Drop`] on
//!   [`PgTransaction`] — the connection is closed, which makes the server
//!   abort the open transaction, and it is never returned to the pool.
//! - No LISTEN/NOTIFY surface.
//!
//! All parameters travel through the extended-query protocol as bound values
//! (`$1..$n`); there is no interpolation path. Params are sent in the *text*
//! wire format so `numeric`/`timestamptz`/`json`/`uuid` strings are parsed
//! exactly by the server; results arrive in the binary format and are decoded
//! per-type (including a manual binary-NUMERIC → canonical-string decoder to
//! honor the exact-decimal rule).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context as _;
use chrono::{DateTime, SecondsFormat, Utc};
use deadpool_postgres::{
    Hook, HookError, Manager, ManagerConfig, Object, Pool, RecyclingMethod, Runtime, Timeouts,
};
use futures_util::TryStreamExt as _;
use tokio_postgres::NoTls;
use tokio_postgres::types::{Format, IsNull, ToSql, Type, to_sql_checked};

use tracing::Instrument as _;
use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::{Linker, Resource};
use wash_runtime::wit::{WitInterface, WitWorld};

use wamn_event_wire::Causation;

use crate::identifiers::{valid_project, valid_runner, valid_schema, valid_tenant};

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "postgres-plugin",
        imports: { default: async | trappable | tracing },
        with: {
            "wamn:postgres/client.transaction": super::PgTransaction,
            "wamn:postgres/client.cursor": super::PgCursor,
        },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wamn::postgres::client;
use bindings::wamn::postgres::types::{Column, PgError, RowSet, SqlValue};

pub const WAMN_POSTGRES_ID: &str = "wamn-postgres";

/// Wire the `wamn:postgres/client` host functions into a linker directly.
/// The host path calls this from [`HostPlugin::on_workload_item_bind`]; the
/// `pgbench` harness calls it to link the capability into a hand-built store.
pub fn add_to_linker(linker: &mut Linker<SharedCtx>) -> wash_runtime::wasmtime::Result<()> {
    client::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)
}

mod causation_bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "causation-plugin",
        imports: { default: async | trappable | tracing },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use causation_bindings::wamn::runner::causation;

/// Wire the TRUSTED `wamn:runner/causation` `set-run-context` channel into a
/// linker (l5i9.12.2). Call this ONLY for the trusted, compiled-in flow-runner
/// — the sole component allowed to declare the run it is driving. A custom node
/// must NOT get this: it never imports `wamn:runner`, and the frozen
/// `wamn:postgres` surface rejects a raw-SQL `wamn.*` emit, so guest causation
/// is unforgeable. The handler feeds the [`WamnPostgres`] plugin resolved from
/// the invoking context.
pub fn add_runner_causation_to_linker(
    linker: &mut Linker<SharedCtx>,
) -> wash_runtime::wasmtime::Result<()> {
    causation::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)
}

/// Per-workload config key carrying the tenant identity (plumbed end-to-end
/// from the WorkloadDeployment CRD's `localResources.config`, i.e. set by the
/// platform, not the guest).
pub const TENANT_CONFIG_KEY: &str = "wamn.tenant";

/// Per-workload config key carrying the `search_path` schema. Optional: absent
/// leaves the server's default search_path in place. Set by the platform (not
/// the guest), like the tenant claim.
pub const SCHEMA_CONFIG_KEY: &str = "wamn.schema";

/// Per-workload config key naming the project whose database this component
/// uses. Optional: absent ⇒ the default project (single-DB deployments and the
/// S2 bench). Set by the platform, not the guest.
pub const PROJECT_CONFIG_KEY: &str = "wamn.project";

/// Per-workload config key carrying the runner's durable-queue LEASE OWNER
/// identity (fqg.4). Optional: absent leaves `app.runner` unset (the S2..S6 and
/// gateway paths never claim from the queue). When set, the plugin injects
/// `SET LOCAL app.runner` alongside the tenant claim, so a flowrunner replica
/// that claims its own work reads a stable, non-spoofable owner
/// (`current_setting('app.runner', true)`) to lease/renew queue rows under —
/// the per-replica identity the reclaim + owner-guarded heartbeat need. Set by
/// the platform (the workload instance id), not the guest.
pub const RUNNER_CONFIG_KEY: &str = "wamn.runner";

/// The project id used when a component names none — the single database a
/// [`WamnPostgresConfig`] URL points at.
pub const DEFAULT_PROJECT: &str = "default";

// ---------------------------------------------------------------------------
// Plugin configuration
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct WamnPostgresConfig {
    /// `postgres://user:pass@host:port/db`. None = plugin registers but every
    /// call returns `connection-unavailable`.
    pub database_url: Option<String>,
    pub pool_max_size: usize,
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
            database_url: std::env::var("DATABASE_URL")
                .or_else(|_| std::env::var("WAMN_PG_URL"))
                .ok(),
            pool_max_size: num("WAMN_PG_POOL_MAX", 16),
            wait_timeout_ms: num("WAMN_PG_WAIT_TIMEOUT_MS", 2_000),
            statement_timeout_ms: num("WAMN_PG_STATEMENT_TIMEOUT_MS", 5_000),
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
    pub pool_max_size: usize,
    pub wait_timeout_ms: u64,
    pub statement_timeout_ms: u32,
    pub row_limit: u64,
}

impl ProjectConfig {
    /// The default project's config, from the single-DB [`WamnPostgresConfig`].
    fn from_global(url: String, cfg: &WamnPostgresConfig) -> Self {
        Self {
            database_url: url,
            pool_max_size: cfg.pool_max_size,
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
    fn resolve(&self, project: &str) -> anyhow::Result<Option<ProjectConfig>>;
}

/// v0 provider: an in-memory project→config map plus an optional default used
/// for any unlisted project (so a single-DB deployment and the S2 bench work
/// with no map at all). The map is populated from `WAMN_PG_PROJECTS_FILE` (a
/// JSON object mounted like a Secret/ConfigMap) or constructed directly.
pub struct StaticCredentialProvider {
    projects: HashMap<String, ProjectConfig>,
    default: Option<ProjectConfig>,
}

impl StaticCredentialProvider {
    pub fn new(projects: HashMap<String, ProjectConfig>, default: Option<ProjectConfig>) -> Self {
        Self { projects, default }
    }

    /// Default-only provider (single database = the default project).
    fn default_only(default: Option<ProjectConfig>) -> Self {
        Self {
            projects: HashMap::new(),
            default,
        }
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
                    pool_max_size: u64_or("pool_max_size", base.pool_max_size as u64) as usize,
                    wait_timeout_ms: u64_or("wait_timeout_ms", base.wait_timeout_ms),
                    statement_timeout_ms: u64_or(
                        "statement_timeout_ms",
                        base.statement_timeout_ms as u64,
                    ) as u32,
                    row_limit: u64_or("row_limit", base.row_limit),
                },
            );
        }
        Ok(out)
    }
}

impl CredentialProvider for StaticCredentialProvider {
    fn resolve(&self, project: &str) -> anyhow::Result<Option<ProjectConfig>> {
        Ok(self
            .projects
            .get(project)
            .cloned()
            .or_else(|| self.default.clone()))
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
    fn resolve(&self, _project: &str) -> anyhow::Result<Option<ProjectConfig>> {
        anyhow::bail!(
            "K8sSecretProvider (namespace {:?}) is not implemented yet — see wamn-5x0.1 [2.2b]; use StaticCredentialProvider",
            self.namespace
        )
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

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
    destroyed: Arc<AtomicU64>,
}

/// A project's live connection pool plus its host-enforced policy (statement
/// timeout + row limit travel with every call made against it).
struct ProjectPool {
    pool: Pool,
    statement_timeout_ms: u32,
    row_limit: u64,
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
fn reject_claim_mutation(sql: &str) -> Result<(), PgError> {
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
    if let Some(schema) = schema {
        if !valid_schema(schema) {
            return Err(PgError::QueryError((
                "WAMN0".to_string(),
                "invalid search_path schema".to_string(),
            )));
        }
    }
    if let Some(runner) = runner {
        if !valid_runner(runner) {
            return Err(PgError::QueryError((
                "WAMN0".to_string(),
                "invalid runner owner".to_string(),
            )));
        }
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
fn standard_conforming_strings_hook() -> Hook {
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

    fn tenant_for(&self, component_id: &str) -> Option<String> {
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

    fn project_for(&self, component_id: &str) -> String {
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

    fn schema_for(&self, component_id: &str) -> Option<String> {
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

    fn runner_for(&self, component_id: &str) -> Option<String> {
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

    fn current_run_for(&self, component_id: &str) -> Option<Causation> {
        self.current_run
            .read()
            .expect("current_run lock poisoned")
            .get(component_id)
            .cloned()
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

    fn destroy(&self, obj: Object) {
        destroy_connection(obj, &self.destroyed);
    }

    /// Check out a connection from a project's (lazily built) pool, returning
    /// the pool handle too so its statement-timeout/row-limit policy travels
    /// with the call.
    async fn checkout(&self, project: &str) -> Result<(Object, Arc<ProjectPool>), PgError> {
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
    async fn begin_with_claims(
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

    fn require_tenant(&self, component_id: &str) -> Result<String, PgError> {
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
    async fn one_shot(
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

enum OneShotResult {
    Rows(RowSet),
    Count(u64),
}

fn destroy_connection(obj: Object, counter: &AtomicU64) {
    // Removes the connection from the pool accounting and closes the socket;
    // the server aborts any open transaction on disconnect. Never repooled.
    let client = Object::take(obj);
    drop(client);
    counter.fetch_add(1, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// HostPlugin
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl HostPlugin for WamnPostgres {
    fn id(&self) -> &'static str {
        WAMN_POSTGRES_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([
                WitInterface::from("wamn:postgres/types@0.1.0"),
                WitInterface::from("wamn:postgres/client@0.1.0"),
            ]),
            exports: HashSet::new(),
        }
    }

    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        if !interfaces.contains("wamn", "postgres", &["client"]) {
            return Ok(());
        }
        if let Some(tenant) = item.local_resources().config.get(TENANT_CONFIG_KEY) {
            let tenant = tenant.clone();
            self.set_tenant(item.id(), &tenant)?;
            tracing::debug!(
                component = item.id(),
                tenant,
                "wamn:postgres tenant registered"
            );
        } else {
            tracing::warn!(
                component = item.id(),
                "component imports wamn:postgres but sets no {TENANT_CONFIG_KEY}; calls will be refused"
            );
        }
        if let Some(project) = item.local_resources().config.get(PROJECT_CONFIG_KEY) {
            let project = project.clone();
            self.set_project(item.id(), &project)?;
            tracing::debug!(
                component = item.id(),
                project,
                "wamn:postgres project registered"
            );
        }
        if let Some(schema) = item.local_resources().config.get(SCHEMA_CONFIG_KEY) {
            let schema = schema.clone();
            self.set_schema(item.id(), &schema)?;
            tracing::debug!(
                component = item.id(),
                schema,
                "wamn:postgres search_path schema registered"
            );
        }
        if let Some(runner) = item.local_resources().config.get(RUNNER_CONFIG_KEY) {
            let runner = runner.clone();
            self.set_runner(item.id(), &runner)?;
            tracing::debug!(
                component = item.id(),
                runner,
                "wamn:postgres runner lease-owner registered"
            );
        }
        client::add_to_linker::<_, SharedCtx>(item.linker(), extract_active_ctx)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Transaction / cursor resources
// ---------------------------------------------------------------------------

struct TxnState {
    /// Present while the transaction owns a connection. Taken out for the
    /// duration of each call (a std mutex guard cannot be held across await).
    conn: Option<Object>,
    /// True once COMMIT or ROLLBACK ran (connection repooled).
    finished: bool,
}

type SharedTxnState = Arc<std::sync::Mutex<TxnState>>;

/// Host side of a `wamn:postgres/client.transaction`.
///
/// The [`Drop`] impl is the crash-safety guarantee: if the resource dies
/// without an explicit finish — guest trap, epoch kill, store teardown — the
/// connection is destroyed (socket closed, server aborts the transaction),
/// never repooled.
pub struct PgTransaction {
    state: SharedTxnState,
    destroyed: Arc<AtomicU64>,
    cursor_seq: u32,
    /// Row limit of the project this transaction's connection belongs to.
    row_limit: u64,
}

impl Drop for PgTransaction {
    fn drop(&mut self) {
        let mut st = match self.state.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(obj) = st.conn.take() {
            if st.finished {
                drop(obj); // clean: back to the pool
            } else {
                tracing::warn!(
                    "wamn:postgres transaction dropped without commit/rollback; destroying connection"
                );
                destroy_connection(obj, &self.destroyed);
            }
        }
    }
}

/// Host side of a `wamn:postgres/client.cursor`. Shares the transaction's
/// connection slot; server-side cursors die with the transaction.
pub struct PgCursor {
    state: SharedTxnState,
    destroyed: Arc<AtomicU64>,
    name: String,
}

fn txn_closed() -> PgError {
    PgError::QueryError((
        "WAMN2".to_string(),
        "transaction already finished or connection lost".to_string(),
    ))
}

fn take_conn(state: &SharedTxnState) -> Result<Object, PgError> {
    let mut st = state.lock().map_err(|_| txn_closed())?;
    if st.finished {
        return Err(txn_closed());
    }
    st.conn.take().ok_or_else(txn_closed)
}

fn put_conn(state: &SharedTxnState, obj: Object) {
    if let Ok(mut st) = state.lock() {
        st.conn = Some(obj);
    }
}

/// Run `op` with the transaction's connection. Fatal (connection-level)
/// errors destroy the connection and poison the transaction; statement-level
/// errors return the connection to the slot (the transaction is aborted
/// server-side until the guest rolls back, mirroring libpq semantics).
async fn with_txn_conn<T, F, Fut>(
    state: &SharedTxnState,
    destroyed: &Arc<AtomicU64>,
    op: F,
) -> Result<T, PgError>
where
    F: FnOnce(Object) -> Fut,
    Fut: std::future::Future<Output = (Object, Result<T, tokio_postgres::Error>)>,
{
    let conn = take_conn(state)?;
    let (conn, result) = op(conn).await;
    match result {
        Ok(v) => {
            put_conn(state, conn);
            Ok(v)
        }
        Err(e) => {
            let mapped = map_pg_error(&e);
            if e.is_closed() {
                if let Ok(mut st) = state.lock() {
                    st.finished = true;
                }
                destroy_connection(conn, destroyed);
            } else {
                put_conn(state, conn);
            }
            Err(mapped)
        }
    }
}

// ---------------------------------------------------------------------------
// Statement execution helpers
// ---------------------------------------------------------------------------

async fn run_query(
    conn: &Object,
    sql: &str,
    params: &[SqlValue],
    row_limit: u64,
) -> Result<RowSet, PgError> {
    reject_claim_mutation(sql)?;
    let stmt = conn
        .prepare_cached(sql)
        .await
        .map_err(|e| map_pg_error(&e))?;
    let columns = columns_of(&stmt);
    let wrapped: Vec<PgParam> = params.iter().map(|p| PgParam(p.clone())).collect();
    let stream = conn
        .query_raw(&stmt, wrapped.iter().map(|p| p as &dyn ToSql))
        .await
        .map_err(|e| map_pg_error(&e))?;
    futures_util::pin_mut!(stream);
    let mut rows = Vec::new();
    while let Some(row) = stream.try_next().await.map_err(|e| map_pg_error(&e))? {
        if rows.len() as u64 >= row_limit {
            return Err(PgError::RowLimitExceeded(row_limit));
        }
        rows.push(decode_row(&row)?);
    }
    Ok(RowSet { columns, rows })
}

async fn run_execute(conn: &Object, sql: &str, params: &[SqlValue]) -> Result<u64, PgError> {
    reject_claim_mutation(sql)?;
    let stmt = conn
        .prepare_cached(sql)
        .await
        .map_err(|e| map_pg_error(&e))?;
    let wrapped: Vec<PgParam> = params.iter().map(|p| PgParam(p.clone())).collect();
    conn.execute_raw(&stmt, wrapped.iter().map(|p| p as &dyn ToSql))
        .await
        .map_err(|e| map_pg_error(&e))
}

fn columns_of(stmt: &tokio_postgres::Statement) -> Vec<Column> {
    stmt.columns()
        .iter()
        .map(|c| Column {
            name: c.name().to_string(),
            type_name: c.type_().name().to_string(),
        })
        .collect()
}

fn decode_row(row: &tokio_postgres::Row) -> Result<Vec<SqlValue>, PgError> {
    (0..row.len())
        .map(|i| {
            row.try_get::<_, SqlCell>(i).map(|c| c.0).map_err(|e| {
                PgError::QueryError((
                    "WAMN1".to_string(),
                    format!("column {i} decode failed: {e}"),
                ))
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Error mapping
// ---------------------------------------------------------------------------

fn map_pg_error(e: &tokio_postgres::Error) -> PgError {
    if let Some(db) = e.as_db_error() {
        let constraint = || db.constraint().unwrap_or_default().to_string();
        return match db.code().code() {
            "40001" | "40P01" => PgError::SerializationFailure,
            "57014" => PgError::StatementTimeout,
            "23505" => PgError::UniqueViolation(constraint()),
            "23503" => PgError::ForeignKeyViolation(constraint()),
            "23514" => PgError::CheckViolation(constraint()),
            // RLS / privilege denials deliberately carry no policy detail.
            "42501" => PgError::PermissionDenied,
            code => PgError::QueryError((code.to_string(), db.message().to_string())),
        };
    }
    if e.is_closed() {
        return PgError::ConnectionUnavailable;
    }
    PgError::QueryError(("XX000".to_string(), e.to_string()))
}

// ---------------------------------------------------------------------------
// Guest→host params: text-format wire encoding
// ---------------------------------------------------------------------------

/// Wraps a WIT `sql-value` as a bound parameter. Values are sent in the text
/// wire format, so the server parses them with the exact semantics of SQL
/// literals for the *declared* parameter type: `numeric`/`timestamptz`/
/// `json`/`uuid` strings stay exact, and there is no client-side type
/// negotiation to disagree with the server.
#[derive(Debug)]
struct PgParam(SqlValue);

impl ToSql for PgParam {
    fn to_sql(
        &self,
        _ty: &Type,
        out: &mut bytes::BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        use std::fmt::Write as _;
        match &self.0 {
            SqlValue::Null => return Ok(IsNull::Yes),
            SqlValue::Boolean(b) => out.extend_from_slice(if *b { b"t" } else { b"f" }),
            SqlValue::Int32(v) => {
                let mut s = String::new();
                let _ = write!(s, "{v}");
                out.extend_from_slice(s.as_bytes());
            }
            SqlValue::Int64(v) => {
                let mut s = String::new();
                let _ = write!(s, "{v}");
                out.extend_from_slice(s.as_bytes());
            }
            SqlValue::Float64(v) => {
                let s = if v.is_nan() {
                    "NaN".to_string()
                } else if v.is_infinite() {
                    if *v > 0.0 { "Infinity" } else { "-Infinity" }.to_string()
                } else {
                    // {:?} is the shortest round-trip representation.
                    format!("{v:?}")
                };
                out.extend_from_slice(s.as_bytes());
            }
            SqlValue::Text(s) => out.extend_from_slice(s.as_bytes()),
            SqlValue::Bytes(b) => {
                out.extend_from_slice(b"\\x");
                let mut s = String::with_capacity(b.len() * 2);
                for byte in b {
                    let _ = write!(s, "{byte:02x}");
                }
                out.extend_from_slice(s.as_bytes());
            }
            // Canonical-string types: pass through, server parses per the
            // parameter's declared type.
            SqlValue::Numeric(s)
            | SqlValue::Timestamptz(s)
            | SqlValue::Json(s)
            | SqlValue::Uuid(s) => out.extend_from_slice(s.as_bytes()),
        }
        Ok(IsNull::No)
    }

    fn accepts(_ty: &Type) -> bool {
        // The server validates the text form against the declared parameter
        // type; incompatible values fail there with a mappable error.
        true
    }

    fn encode_format(&self, _ty: &Type) -> Format {
        Format::Text
    }

    to_sql_checked!();
}

// ---------------------------------------------------------------------------
// Host→guest cells: binary wire decoding
// ---------------------------------------------------------------------------

struct SqlCell(SqlValue);

impl<'a> tokio_postgres::types::FromSql<'a> for SqlCell {
    fn from_sql(
        ty: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        let v = match ty.name() {
            "bool" => SqlValue::Boolean(bool::from_sql(ty, raw)?),
            "int2" => SqlValue::Int32(i16::from_sql(ty, raw)? as i32),
            "int4" => SqlValue::Int32(i32::from_sql(ty, raw)?),
            "int8" => SqlValue::Int64(i64::from_sql(ty, raw)?),
            "float4" => SqlValue::Float64(f32::from_sql(ty, raw)? as f64),
            "float8" => SqlValue::Float64(f64::from_sql(ty, raw)?),
            "text" | "varchar" | "bpchar" | "name" | "unknown" => {
                SqlValue::Text(String::from_sql(ty, raw)?)
            }
            "bytea" => SqlValue::Bytes(<&[u8]>::from_sql(ty, raw)?.to_vec()),
            "numeric" => SqlValue::Numeric(decode_binary_numeric(raw)?),
            "timestamptz" => SqlValue::Timestamptz(
                DateTime::<Utc>::from_sql(ty, raw)?.to_rfc3339_opts(SecondsFormat::Micros, false),
            ),
            "json" => SqlValue::Json(std::str::from_utf8(raw)?.to_string()),
            "jsonb" => {
                let (version, body) = raw.split_first().ok_or("empty jsonb value")?;
                if *version != 1 {
                    return Err(format!("unsupported jsonb version {version}").into());
                }
                SqlValue::Json(std::str::from_utf8(body)?.to_string())
            }
            "uuid" => {
                if raw.len() != 16 {
                    return Err("uuid value is not 16 bytes".into());
                }
                let h = |r: &[u8]| {
                    r.iter().fold(String::new(), |mut s, b| {
                        use std::fmt::Write as _;
                        let _ = write!(s, "{b:02x}");
                        s
                    })
                };
                SqlValue::Uuid(format!(
                    "{}-{}-{}-{}-{}",
                    h(&raw[0..4]),
                    h(&raw[4..6]),
                    h(&raw[6..8]),
                    h(&raw[8..10]),
                    h(&raw[10..16]),
                ))
            }
            other => return Err(format!("unsupported column type {other}").into()),
        };
        Ok(SqlCell(v))
    }

    fn from_sql_null(_ty: &Type) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(SqlCell(SqlValue::Null))
    }

    fn accepts(_ty: &Type) -> bool {
        true
    }
}

/// Decode Postgres's binary NUMERIC wire format into its canonical string
/// (the same text `numeric_out` would produce): base-10000 digit groups with
/// a weight (group index of the first group relative to the decimal point)
/// and a display scale.
fn decode_binary_numeric(raw: &[u8]) -> Result<String, Box<dyn std::error::Error + Sync + Send>> {
    fn rd_i16(raw: &[u8], at: usize) -> Result<i16, Box<dyn std::error::Error + Sync + Send>> {
        Ok(i16::from_be_bytes(
            raw.get(at..at + 2).ok_or("truncated numeric")?.try_into()?,
        ))
    }
    let ndigits = rd_i16(raw, 0)? as usize;
    let weight = rd_i16(raw, 2)? as i32;
    let sign = rd_i16(raw, 4)? as u16;
    let dscale = rd_i16(raw, 6)? as u16 as usize;
    match sign {
        0x0000 | 0x4000 => {}
        0xC000 => return Ok("NaN".to_string()),
        0xD000 => return Ok("Infinity".to_string()),
        0xF000 => return Ok("-Infinity".to_string()),
        other => return Err(format!("bad numeric sign {other:#x}").into()),
    }
    let mut digits = Vec::with_capacity(ndigits);
    for i in 0..ndigits {
        digits.push(rd_i16(raw, 8 + i * 2)? as u16);
    }

    use std::fmt::Write as _;
    let mut s = String::new();
    if sign == 0x4000 {
        s.push('-');
    }
    if weight < 0 || ndigits == 0 {
        s.push('0');
    } else {
        for i in 0..=(weight as usize) {
            let d = digits.get(i).copied().unwrap_or(0);
            if i == 0 {
                let _ = write!(s, "{d}");
            } else {
                let _ = write!(s, "{d:04}");
            }
        }
    }
    if dscale > 0 {
        let mut frac = String::new();
        let mut gw = -1i32;
        while frac.len() < dscale {
            let i = weight - gw; // digit index of the group with weight `gw`
            let d = if i >= 0 {
                digits.get(i as usize).copied().unwrap_or(0)
            } else {
                0
            };
            let _ = write!(frac, "{d:04}");
            gw -= 1;
        }
        frac.truncate(dscale);
        s.push('.');
        s.push_str(&frac);
    }
    Ok(s)
}

// ---------------------------------------------------------------------------
// WIT host implementations
// ---------------------------------------------------------------------------

fn plugin_of(ctx: &ActiveCtx<'_>) -> wash_runtime::wasmtime::Result<Arc<WamnPostgres>> {
    ctx.try_get_plugin::<WamnPostgres>(WAMN_POSTGRES_ID)
}

impl causation::Host for ActiveCtx<'_> {
    /// The trusted flow-runner declares (or clears, with `none`) the causation
    /// context of the run it is driving (l5i9.12.2). Only components linked with
    /// [`add_runner_causation_to_linker`] can call this. The declaration feeds
    /// the [`WamnPostgres`] plugin's per-component run map, so every subsequent
    /// transaction the plugin opens for this component stamps a `wamn.causation`
    /// message. If no postgres plugin is present in this context (a runner-less
    /// bench), the declaration is a harmless no-op.
    async fn set_run_context(
        &mut self,
        ctx: Option<causation::RunContext>,
    ) -> wash_runtime::wasmtime::Result<()> {
        let component = self.component_id.to_string();
        let run = ctx.map(|c| Causation {
            run: c.run,
            root: c.root,
            depth: c.depth,
        });
        tracing::debug!(
            target: "wamn::causation",
            component,
            run = ?run.as_ref().map(|c| &c.run),
            "per-run causation context declared"
        );
        if let Ok(plugin) = plugin_of(self) {
            plugin.set_current_run(&component, run);
        }
        Ok(())
    }
}

/// [9.1] A `wamn.postgres` span over one guest DB call, enriched host-side with
/// the executing component's tenant/project (the same claim maps that inject
/// `app.tenant`; the guest cannot spoof them). Emitted through the process's
/// global `tracing` subscriber, which the fork's `initialize_observability`
/// bridges to OTel and exports over OTLP when `OTEL_*` is set — so the span
/// nests under whatever span is current (a request handler, or a
/// [`crate::dispatch::trigger_span`]) and threads into that trace. Enriching a
/// host-created span keeps 9.1 wamn-side (no fork patch); `run_id`/`node_id`
/// enrichment on this span awaits the 9.2 guest→host run-context contract.
fn db_span(plugin: &WamnPostgres, component_id: &str, op: &'static str) -> tracing::Span {
    let tenant = plugin.tenant_for(component_id).unwrap_or_default();
    let project = plugin.project_for(component_id);
    tracing::info_span!(
        "wamn.postgres",
        db.system = "postgresql",
        db.operation = op,
        wamn.tenant = %tenant,
        wamn.project = %project,
        wamn.component = %component_id,
    )
}

impl client::Host for ActiveCtx<'_> {
    async fn query(
        &mut self,
        sql: String,
        params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<RowSet, PgError>> {
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
        let span = db_span(&plugin, &component_id, "query");
        Ok(
            match plugin
                .one_shot(&component_id, &sql, &params, true)
                .instrument(span)
                .await
            {
                Ok(OneShotResult::Rows(rs)) => Ok(rs),
                Ok(OneShotResult::Count(_)) => unreachable!("one_shot(want_rows) returns rows"),
                Err(e) => Err(e),
            },
        )
    }

    async fn execute(
        &mut self,
        sql: String,
        params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<u64, PgError>> {
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
        let span = db_span(&plugin, &component_id, "execute");
        Ok(
            match plugin
                .one_shot(&component_id, &sql, &params, false)
                .instrument(span)
                .await
            {
                Ok(OneShotResult::Count(n)) => Ok(n),
                Ok(OneShotResult::Rows(_)) => unreachable!("one_shot(!want_rows) returns count"),
                Err(e) => Err(e),
            },
        )
    }

    async fn begin(
        &mut self,
    ) -> wash_runtime::wasmtime::Result<Result<Resource<PgTransaction>, PgError>> {
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();

        let tenant = match plugin.require_tenant(&component_id) {
            Ok(t) => t,
            Err(e) => return Ok(Err(e)),
        };
        let project = plugin.project_for(&component_id);
        let schema = plugin.schema_for(&component_id);
        let runner = plugin.runner_for(&component_id);
        let run = plugin.current_run_for(&component_id);
        let (conn, pp) = match plugin.checkout(&project).await {
            Ok(c) => c,
            Err(e) => return Ok(Err(e)),
        };
        if let Err(e) = plugin
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
            plugin.destroy(conn);
            return Ok(Err(e));
        }
        let txn = PgTransaction {
            state: Arc::new(std::sync::Mutex::new(TxnState {
                conn: Some(conn),
                finished: false,
            })),
            destroyed: plugin.destroyed.clone(),
            cursor_seq: 0,
            row_limit: pp.row_limit,
        };
        Ok(Ok(self.table.push(txn)?))
    }
}

impl client::HostTransaction for ActiveCtx<'_> {
    async fn query(
        &mut self,
        rep: Resource<PgTransaction>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<RowSet, PgError>> {
        let plugin = plugin_of(self)?;
        let span = db_span(&plugin, self.component_id.as_ref(), "txn.query");
        let txn = self.table.get(&rep)?;
        let row_limit = txn.row_limit;
        let (state, destroyed) = (txn.state.clone(), txn.destroyed.clone());
        Ok(with_txn_conn(&state, &destroyed, |conn| async move {
            let r = run_query(&conn, &sql, &params, row_limit).await;
            // run_query maps errors already; re-split for with_txn_conn's
            // fatal/statement distinction by probing conn liveness.
            (conn, flatten_mapped(r))
        })
        .instrument(span)
        .await
        .and_then(|r| r))
    }

    async fn execute(
        &mut self,
        rep: Resource<PgTransaction>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<u64, PgError>> {
        let plugin = plugin_of(self)?;
        let span = db_span(&plugin, self.component_id.as_ref(), "txn.execute");
        let txn = self.table.get(&rep)?;
        let (state, destroyed) = (txn.state.clone(), txn.destroyed.clone());
        Ok(with_txn_conn(&state, &destroyed, |conn| async move {
            let r = run_execute(&conn, &sql, &params).await;
            (conn, flatten_mapped(r))
        })
        .instrument(span)
        .await
        .and_then(|r| r))
    }

    async fn open_cursor(
        &mut self,
        rep: Resource<PgTransaction>,
        sql: String,
        params: Vec<SqlValue>,
    ) -> wash_runtime::wasmtime::Result<Result<Resource<PgCursor>, PgError>> {
        // A cursor over `SELECT set_config('app.tenant', …)` would execute the
        // override on fetch; guard the same surface as query/execute (wamn-cjv.2).
        if let Err(e) = reject_claim_mutation(&sql) {
            return Ok(Err(e));
        }
        let txn = self.table.get_mut(&rep)?;
        txn.cursor_seq += 1;
        let name = format!("wamn_c{}", txn.cursor_seq);
        let (state, destroyed) = (txn.state.clone(), txn.destroyed.clone());

        let declare = format!("DECLARE {name} CURSOR FOR {sql}");
        let result = with_txn_conn(&state, &destroyed, |conn| async move {
            let r = async {
                let stmt = conn.prepare(&declare).await?;
                let wrapped: Vec<PgParam> = params.iter().map(|p| PgParam(p.clone())).collect();
                conn.execute_raw(&stmt, wrapped.iter().map(|p| p as &dyn ToSql))
                    .await
            }
            .await;
            (conn, r)
        })
        .await;
        Ok(match result {
            Ok(_) => Ok(self.table.push(PgCursor {
                state,
                destroyed,
                name,
            })?),
            Err(e) => Err(e),
        })
    }

    async fn commit(
        &mut self,
        rep: Resource<PgTransaction>,
    ) -> wash_runtime::wasmtime::Result<Result<(), PgError>> {
        let txn = self.table.get(&rep)?;
        let (state, destroyed) = (txn.state.clone(), txn.destroyed.clone());
        Ok(finish_txn(&state, &destroyed, "COMMIT").await)
    }

    async fn rollback(
        &mut self,
        rep: Resource<PgTransaction>,
    ) -> wash_runtime::wasmtime::Result<Result<(), PgError>> {
        let txn = self.table.get(&rep)?;
        let (state, destroyed) = (txn.state.clone(), txn.destroyed.clone());
        Ok(finish_txn(&state, &destroyed, "ROLLBACK").await)
    }

    async fn drop(&mut self, rep: Resource<PgTransaction>) -> wash_runtime::wasmtime::Result<()> {
        let txn = self.table.delete(rep)?;
        // Graceful guest-side drop without commit: contract says roll back.
        // The connection is protocol-clean after a successful ROLLBACK, so it
        // can be repooled; failure falls through to the destroying Drop.
        let (state, destroyed) = (txn.state.clone(), txn.destroyed.clone());
        let already_finished = state
            .lock()
            .map(|st| st.finished || st.conn.is_none())
            .unwrap_or(true);
        if !already_finished {
            let _ = finish_txn(&state, &destroyed, "ROLLBACK").await;
        }
        drop(txn); // Drop impl destroys the connection iff still unfinished
        Ok(())
    }
}

/// COMMIT or ROLLBACK, then repool the connection and mark the transaction
/// finished. On failure the connection is destroyed.
async fn finish_txn(
    state: &SharedTxnState,
    destroyed: &Arc<AtomicU64>,
    verb: &str,
) -> Result<(), PgError> {
    let conn = take_conn(state)?;
    match conn.batch_execute(verb).await {
        Ok(()) => {
            if let Ok(mut st) = state.lock() {
                st.finished = true;
            }
            drop(conn); // back to the pool
            Ok(())
        }
        Err(e) => {
            if let Ok(mut st) = state.lock() {
                st.finished = true;
            }
            destroy_connection(conn, destroyed);
            Err(map_pg_error(&e))
        }
    }
}

/// Adapter: our helpers return `Result<T, PgError>` but [`with_txn_conn`]
/// wants the raw `tokio_postgres::Error` to judge fatality. Statement-level
/// failures were already mapped, so wrap them back up as an Ok(Err(..)).
fn flatten_mapped<T>(r: Result<T, PgError>) -> Result<Result<T, PgError>, tokio_postgres::Error> {
    Ok(r)
}

impl client::HostCursor for ActiveCtx<'_> {
    async fn fetch(
        &mut self,
        rep: Resource<PgCursor>,
        max_rows: u32,
    ) -> wash_runtime::wasmtime::Result<Result<RowSet, PgError>> {
        let cursor = self.table.get(&rep)?;
        let (state, destroyed, name) = (
            cursor.state.clone(),
            cursor.destroyed.clone(),
            cursor.name.clone(),
        );
        Ok(with_txn_conn(&state, &destroyed, |conn| async move {
            let r = async {
                let sql = format!("FETCH FORWARD {max_rows} FROM {name}");
                let stmt = conn.prepare(&sql).await?;
                let columns = columns_of(&stmt);
                let rows = conn.query(&stmt, &[]).await?;
                Ok::<_, tokio_postgres::Error>((columns, rows))
            }
            .await;
            (conn, r)
        })
        .await
        .and_then(|(columns, rows)| {
            let rows = rows.iter().map(decode_row).collect::<Result<Vec<_>, _>>()?;
            Ok(RowSet { columns, rows })
        }))
    }

    async fn drop(&mut self, rep: Resource<PgCursor>) -> wash_runtime::wasmtime::Result<()> {
        // Server-side cursors die with their transaction; nothing to release.
        self.table.delete(rep)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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

    fn enc(ndigits: i16, weight: i16, sign: u16, dscale: u16, digits: &[u16]) -> Vec<u8> {
        let mut raw = Vec::new();
        raw.extend_from_slice(&ndigits.to_be_bytes());
        raw.extend_from_slice(&weight.to_be_bytes());
        raw.extend_from_slice(&sign.to_be_bytes());
        raw.extend_from_slice(&dscale.to_be_bytes());
        for d in digits {
            raw.extend_from_slice(&d.to_be_bytes());
        }
        raw
    }

    #[test]
    fn numeric_decode_basic() {
        // 12.3400
        assert_eq!(
            decode_binary_numeric(&enc(2, 0, 0, 4, &[12, 3400])).unwrap(),
            "12.3400"
        );
        // 0.0001
        assert_eq!(
            decode_binary_numeric(&enc(1, -1, 0, 4, &[1])).unwrap(),
            "0.0001"
        );
        // 0.00000001 (weight -2)
        assert_eq!(
            decode_binary_numeric(&enc(1, -2, 0, 8, &[1])).unwrap(),
            "0.00000001"
        );
        // 1234567.89
        assert_eq!(
            decode_binary_numeric(&enc(3, 1, 0, 2, &[123, 4567, 8900])).unwrap(),
            "1234567.89"
        );
        // -42
        assert_eq!(
            decode_binary_numeric(&enc(1, 0, 0x4000, 0, &[42])).unwrap(),
            "-42"
        );
        // 0 and 0.00
        assert_eq!(decode_binary_numeric(&enc(0, 0, 0, 0, &[])).unwrap(), "0");
        assert_eq!(
            decode_binary_numeric(&enc(0, 0, 0, 2, &[])).unwrap(),
            "0.00"
        );
        // 10000 (weight 1, single group)
        assert_eq!(
            decode_binary_numeric(&enc(1, 1, 0, 0, &[1])).unwrap(),
            "10000"
        );
        // NaN
        assert_eq!(
            decode_binary_numeric(&enc(0, 0, 0xC000, 0, &[])).unwrap(),
            "NaN"
        );
    }

    #[test]
    fn param_text_encoding() {
        use tokio_postgres::types::ToSql;
        let mut buf = bytes::BytesMut::new();
        let p = PgParam(SqlValue::Bytes(vec![0xde, 0xad, 0x01]));
        assert!(matches!(
            p.to_sql(&Type::BYTEA, &mut buf).unwrap(),
            IsNull::No
        ));
        assert_eq!(&buf[..], b"\\xdead01");

        let mut buf = bytes::BytesMut::new();
        let p = PgParam(SqlValue::Float64(1.5));
        p.to_sql(&Type::FLOAT8, &mut buf).unwrap();
        assert_eq!(&buf[..], b"1.5");

        let mut buf = bytes::BytesMut::new();
        let p = PgParam(SqlValue::Boolean(true));
        p.to_sql(&Type::BOOL, &mut buf).unwrap();
        assert_eq!(&buf[..], b"t");
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
