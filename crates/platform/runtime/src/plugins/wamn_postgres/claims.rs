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
use serde::Deserialize;
use tokio_postgres::NoTls;
use tokio_postgres::types::ToSql;

use wamn_catalog::ManifestDigest;
use wamn_control_registry::identifiers::{valid_project, valid_runner, valid_schema, valid_tenant};
use wamn_event_wire::Causation;

use super::pool::{
    CheckoutProbe, CredentialProvider, PlatformAsyncMessage, PlatformConnect, PoolLifecycle,
    ProjectConfig, ProjectPool, StaticCredentialProvider, WamnPostgresConfig, destroy_connection,
    standard_conforming_strings_hook,
};
use super::resources::{run_execute, run_query};
use super::types::map_pg_error;
use super::{DEFAULT_PROJECT, PgError, RowSet, SqlValue};

pub struct WamnPostgres {
    /// Resolves a project id → its database connection + policy.
    provider: Arc<dyn CredentialProvider>,
    /// Guest-visible project pools, built lazily and never shared with host-owned
    /// claim or authorization work.
    guest_pools: std::sync::RwLock<HashMap<String, Arc<ProjectPool>>>,
    /// Host-owned claim, authorization, and plan-supply project pools. Keeping a
    /// distinct cache makes cross-lifecycle session reuse unrepresentable.
    platform_pools: std::sync::RwLock<HashMap<String, Arc<ProjectPool>>>,
    platform_messages: tokio::sync::mpsc::UnboundedSender<PlatformAsyncMessage>,
    platform_message_receiver:
        std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<PlatformAsyncMessage>>>,
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
    /// component id → the caller's `app.role` claim (a `roles.name`). Absent
    /// (the default) binds the empty role, which is the deny floor every
    /// compiled role gate coalesces to. When set, a per-role RLS policy
    /// (`crates/schema/compiler/src/rls/compile.rs`) gates on the caller's role
    /// instead of denying.
    roles: std::sync::RwLock<HashMap<String, String>>,
    /// component id → the caller's `app.user_id` claim (a `users.id` uuid).
    /// Absent (the default) binds the empty string, which the compiled
    /// ownership predicate `NULLIF(…, '')::uuid` turns into NULL → deny. When
    /// set, a per-user RLS policy compares against the caller's own id.
    users: std::sync::RwLock<HashMap<String, String>>,
    /// component id → the `(release version, manifest digest)` this pod carries.
    /// Absent (the default) ⇒ the production claim records nothing, so every
    /// path that never mounted a release identity is byte-unchanged. When set,
    /// the claim writes the pair onto the run it leases, write-once.
    release_identities: std::sync::RwLock<HashMap<String, ReleaseIdentity>>,
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

/// The release a pod carries — the `(release version, manifest digest)` pair
/// derived from the verified content of its mounted serving manifest
/// ([`ReleaseManifestWeld`](crate::release_manifest::ReleaseManifestWeld)).
///
/// Runs are never version-pinned: a run executes under the release its CLAIMING
/// pod carries, and the production claim records this pair onto that run exactly
/// once. It is host-injected identity like the tenant and the lease owner, never
/// guest-supplied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseIdentity {
    /// The release (catalog) version — `runs.release_version`.
    pub release_version: i32,
    /// The serving manifest's digest — `runs.manifest_digest`. The
    /// `sha256:<64 lowercase hex>` shape the run plane's
    /// `runs_release_record_check` admits is carried by the type, so there is no
    /// hand-rolled shape check on this path.
    pub manifest_digest: ManifestDigest,
}

/// The complete host-injected claim set one component id resolves to.
///
/// Every field is a registry this plugin keys by component id, so this type is
/// the whole of what [`WamnPostgres::bind_session_claims`] writes and
/// [`WamnPostgres::revoke_session_claims`] clears. Adding a registry without
/// adding a field here leaves a claim that no acquisition rebinds — which is
/// exactly the cross-tenant leak `wamn-0h0g.17.7` closes.
///
/// `None` means *no claim*, not *keep the previous one*: the deny floors
/// (`app.role` = `''`, `app.user_id` = NULL, no `search_path` override) are what
/// an acquisition that declares nothing must get.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionClaims {
    /// `app.tenant` — the RLS claim. Required: a session with no tenant is
    /// refused by [`WamnPostgres::require_tenant`] rather than run unscoped.
    pub tenant: String,
    /// Which project database this session resolves against. `None` ⇒ the
    /// default project.
    pub project: Option<String>,
    /// `SET LOCAL search_path`. `None` ⇒ the server's own search_path.
    pub schema: Option<String>,
    /// `app.runner` — the durable-queue lease owner.
    pub runner: Option<String>,
    /// `app.role` — the caller's `roles.name` for compiled per-role RLS.
    pub role: Option<String>,
    /// `app.user_id` — the caller's `users.id` for compiled ownership RLS.
    pub user_id: Option<String>,
    /// The `(release version, manifest digest)` the claiming pod carries.
    pub release: Option<ReleaseIdentity>,
}

/// Host-only identity used to load one HTTP effect authorization snapshot.
#[derive(Debug, Clone, Copy)]
pub struct ConnectionEffectLookup<'a> {
    pub catalog_id: &'a str,
    pub catalog_version: i32,
    pub environment: &'a str,
    pub wiring_id: &'a str,
    pub wiring_version: i32,
    pub node_id: &'a str,
    pub component_digest: &'a str,
    pub store_alias: &'a str,
    pub candidate_binding: Option<&'a CandidateConnectionBinding>,
}

/// One DB-derived, non-secret connection fact frozen on a candidate run.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CandidateConnectionBinding {
    pub component_digest: String,
    pub store_alias: String,
    pub requirement_hash: String,
    pub instance_id: String,
    pub instance_revision: i64,
    pub requirement_type: String,
    pub contract: String,
    pub validation_hash: String,
    pub generation: i64,
    pub definition_hash: String,
    pub credential_set_handle: String,
}

impl CandidateConnectionBinding {
    fn is_complete(&self) -> bool {
        !self.component_digest.is_empty()
            && !self.store_alias.is_empty()
            && !self.requirement_hash.is_empty()
            && !self.instance_id.is_empty()
            && self.instance_revision >= 0
            && !self.requirement_type.is_empty()
            && !self.contract.is_empty()
            && !self.validation_hash.is_empty()
            && self.generation > 0
            && !self.definition_hash.is_empty()
            && !self.credential_set_handle.is_empty()
    }

    pub(crate) fn matches_snapshot(&self, snapshot: &ConnectionEffectSnapshot) -> bool {
        snapshot.requirement_hash.as_deref() == Some(self.requirement_hash.as_str())
            && snapshot.instance_id.as_deref() == Some(self.instance_id.as_str())
            && snapshot.instance_revision == Some(self.instance_revision)
            && snapshot.requirement_type.as_deref() == Some(self.requirement_type.as_str())
            && snapshot.contract.as_deref() == Some(self.contract.as_str())
            && snapshot.validation_hash.as_deref() == Some(self.validation_hash.as_str())
            && snapshot.active_generation == Some(self.generation)
            && snapshot.generation == Some(self.generation)
            && snapshot.definition_hash.as_deref() == Some(self.definition_hash.as_str())
            && snapshot.credential_handle.as_deref() == Some(self.credential_set_handle.as_str())
    }
}

/// Canonically ordered binding world persisted by private candidate admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateBindingWorld(Arc<[CandidateConnectionBinding]>);

impl CandidateBindingWorld {
    /// Decode the exact persisted JSON boundary and reject partial, duplicate,
    /// or non-canonical rows before any component executes.
    pub fn from_json(value: serde_json::Value) -> anyhow::Result<Self> {
        let bindings: Vec<CandidateConnectionBinding> =
            serde_json::from_value(value).context("decode candidate binding world")?;
        anyhow::ensure!(
            bindings.iter().all(CandidateConnectionBinding::is_complete),
            "candidate-binding-world-incomplete"
        );
        anyhow::ensure!(
            bindings.windows(2).all(|pair| {
                (&pair[0].component_digest, &pair[0].store_alias)
                    < (&pair[1].component_digest, &pair[1].store_alias)
            }),
            "candidate-binding-world-not-canonical"
        );
        Ok(Self(bindings.into()))
    }

    /// Find the one frozen binding selected by a component store alias.
    pub fn binding(
        &self,
        component_digest: &str,
        store_alias: &str,
    ) -> Option<&CandidateConnectionBinding> {
        self.0.iter().find(|binding| {
            binding.component_digest == component_digest && binding.store_alias == store_alias
        })
    }
}

/// One transactionally consistent set of admitted HTTP effect facts.
#[derive(Debug, Clone)]
pub struct ConnectionEffectSnapshot {
    pub wiring_hash: String,
    pub component: Option<String>,
    pub interface_version: Option<String>,
    pub requirement_json: Option<serde_json::Value>,
    pub requirement_hash: Option<String>,
    pub node_permitted: bool,
    pub binding_active: bool,
    pub binding_valid: bool,
    pub instance_id: Option<String>,
    pub validation_hash: Option<String>,
    pub requirement_type: Option<String>,
    pub contract: Option<String>,
    pub instance_enabled: bool,
    pub active_generation: Option<i64>,
    pub instance_revision: Option<i64>,
    pub generation: Option<i64>,
    pub definition: Option<serde_json::Value>,
    pub definition_hash: Option<String>,
    pub credential_handle: Option<String>,
}

/// Resolve one host-attested wiring node and its component-grain connection.
///
/// No run, plan, frame, or effect-ledger row participates. The selected wiring
/// version is immutable and stays valid for the lifetime of the delivery even
/// if the environment's hot pointer flips concurrently. The mounted release is
/// checked separately by `ConnectionHttp`, because its canonical bytes are not
/// a database relation and must not be projected back into Postgres.
static CONNECTION_EFFECT_SNAPSHOT_SQL: &str = "\
WITH selected_wiring AS MATERIALIZED ( \
    SELECT wiring.wiring_hash, wiring.graph_json \
      FROM catalog.wirings AS wiring \
     WHERE wiring.tenant_id = $1 \
       AND wiring.catalog_id = $2 \
       AND wiring.gated_catalog_version = $3 \
       AND wiring.wiring_id = $5 \
       AND wiring.version = $6 \
       AND wiring.graph_json ->> 'wiring-id' = $5 \
       AND wiring.graph_json ->> 'version' = $6::text \
) \
SELECT wiring.wiring_hash, component.component, component.interface_version, \
       requirement.requirement_json::text, requirement.requirement_hash, \
       COALESCE( \
           node.value IS NOT NULL \
           AND node.value ->> 'component' = component.component \
           AND node.value ->> 'interface-version' = component.interface_version \
           AND node.value ->> 'operation' = component.operation, \
           false \
       ), \
       binding.binding_status = 'active', binding.validation_status = 'valid', \
       instance.instance_id, binding.validation_hash, \
       instance.requirement_type, instance.contract, \
       instance.lifecycle_status = 'enabled', instance.active_generation, instance.revision, \
       generation.generation, generation.definition_json::text, generation.definition_hash, \
       generation.credential_set_handle \
  FROM selected_wiring AS wiring \
  LEFT JOIN catalog.component_library AS component \
    ON component.tenant_id = $1 \
   AND component.catalog_id = $2 \
   AND component.catalog_version = $3 \
   AND component.component_digest = $8 \
  LEFT JOIN LATERAL ( \
      SELECT wiring.graph_json #> ARRAY['nodes', $7] AS value \
  ) AS node ON true \
  LEFT JOIN catalog.connection_requirements AS requirement \
    ON requirement.tenant_id = $1 \
   AND requirement.artifact_hash IS NULL \
   AND requirement.requirement_name IS NULL \
   AND requirement.component_digest = $8 \
   AND requirement.store_alias = $9 \
  LEFT JOIN catalog.connection_bindings AS binding \
    ON binding.tenant_id = $1 \
   AND binding.catalog_id = $2 \
   AND binding.catalog_version = $3 \
   AND binding.artifact_hash IS NULL \
   AND binding.requirement_name IS NULL \
   AND binding.component_digest = $8 \
   AND binding.store_alias = $9 \
   AND binding.environment = $4 \
   AND ($10::text IS NULL OR binding.instance_id = $10) \
  LEFT JOIN catalog.connection_instances AS instance \
    ON instance.tenant_id = binding.tenant_id \
   AND instance.environment = binding.environment \
   AND instance.instance_id = binding.instance_id \
  LEFT JOIN catalog.connection_generations AS generation \
    ON generation.tenant_id = instance.tenant_id \
   AND generation.environment = instance.environment \
   AND generation.instance_id = instance.instance_id \
   AND generation.generation = COALESCE($11::bigint, instance.active_generation) \
   AND ($11::bigint IS NULL OR instance.active_generation = $11)";

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
/// (R2/R16). Every claim value travels as a bind parameter (`$1..$6`) — there is
/// NO string-interpolation path, so an injection-shaped tenant / schema / runner
/// / role / user id is *unrepresentable* as SQL, not merely rejected by
/// validation. `set_config` with `is_local => true` is the exact `SET LOCAL`
/// equivalent (scoped to the current transaction). Parameter order:
///
/// - `$1` `app.tenant` — the RLS claim (always present).
/// - `$2` `statement_timeout` — as TEXT (a bare-integer string = milliseconds).
/// - `$3` `search_path` — `COALESCE($3, current_setting('search_path'))`, so a
///   NULL bind (absent schema) preserves the server's default search_path; the
///   S2/pgbench path is byte-unchanged.
/// - `$4` `app.runner` — `COALESCE($4, current_setting('app.runner', true))`, so
///   a NULL bind (absent runner) re-asserts the current value (a no-op), exactly
///   like the pre-fqg.4 "no `app.runner` statement" path.
/// - `$5` `app.role` / `$6` `app.user_id` — the per-role / per-user RLS claims
///   the compiled policies key on (wamn-0h0g.23.1). Bound UNCONDITIONALLY, not
///   COALESCEd to the current value like `$3`/`$4`: an absent claim binds `''`,
///   which is exactly the deny floor
///   `COALESCE(current_setting('app.role', true), '')` and
///   `NULLIF(current_setting('app.user_id', true), '')::uuid` compile to
///   (`crates/schema/compiler/src/rls/compile.rs`). Re-asserting whatever the
///   pooled connection currently carries would let a session-level value survive
///   into the next component's transaction, turning a shared connection into a
///   role escalation; binding the floor cannot.
///
/// The `wamn.causation` emit (l5i9.12.2) is NOT part of this statement — it is a
/// separate, already-escaped simple-query emit appended by [`begin_with_claims`]
/// only for a run-owned transaction.
const CLAIM_SQL: &str = "SELECT \
     set_config('app.tenant', $1, true), \
     set_config('statement_timeout', $2, true), \
     set_config('search_path', COALESCE($3, current_setting('search_path')), true), \
     set_config('app.runner', COALESCE($4, current_setting('app.runner', true)), true), \
     set_config('app.role', $5, true), \
     set_config('app.user_id', $6, true)";

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
    role: Option<&str>,
    user_id: Option<&str>,
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
    if let Some(role) = role
        && !valid_role(role)
    {
        return Err(PgError::QueryError((
            "WAMN0".to_string(),
            "invalid caller role".to_string(),
        )));
    }
    if let Some(user_id) = user_id
        && !valid_user_id(user_id)
    {
        return Err(PgError::QueryError((
            "WAMN0".to_string(),
            "invalid caller user id".to_string(),
        )));
    }
    Ok(())
}

/// A caller's `app.role`: a non-empty `roles.name`. The column is free-form
/// `text` and the value binds as a parameter, so no charset rule applies —
/// but `''` is the deny floor every compiled role gate coalesces to, so it must
/// not be spellable as a claim.
fn valid_role(role: &str) -> bool {
    !role.is_empty()
}

/// A caller's `app.user_id`: the canonical `8-4-4-4-12` hex uuid a `users.id`
/// renders as. Local rather than imported because this claim is a uuid, not one
/// of the `[A-Za-z0-9_-]` identities `wamn-control-registry` owns. Since R2 it
/// is not the injection boundary (the value binds as `$6`); it fails closed on a
/// value the compiled `NULLIF(…, '')::uuid` coercion would raise 22P02 on inside
/// every ownership predicate.
fn valid_user_id(user_id: &str) -> bool {
    let mut groups = user_id.split('-');
    for len in [8, 4, 4, 4, 12] {
        match groups.next() {
            Some(g) if g.len() == len && g.bytes().all(|b| b.is_ascii_hexdigit()) => {}
            _ => return false,
        }
    }
    groups.next().is_none()
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
        let (platform_messages, platform_message_receiver) = tokio::sync::mpsc::unbounded_channel();
        Self {
            provider,
            guest_pools: std::sync::RwLock::new(HashMap::new()),
            platform_pools: std::sync::RwLock::new(HashMap::new()),
            platform_messages,
            platform_message_receiver: std::sync::Mutex::new(Some(platform_message_receiver)),
            tenants: std::sync::RwLock::new(HashMap::new()),
            projects: std::sync::RwLock::new(HashMap::new()),
            schemas: std::sync::RwLock::new(HashMap::new()),
            runners: std::sync::RwLock::new(HashMap::new()),
            roles: std::sync::RwLock::new(HashMap::new()),
            users: std::sync::RwLock::new(HashMap::new()),
            release_identities: std::sync::RwLock::new(HashMap::new()),
            current_run: std::sync::RwLock::new(HashMap::new()),
            destroyed: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Build from the environment: the default project from `WAMN_PG_URL`, plus
    /// explicit projects listed in `WAMN_PG_PROJECTS_FILE` JSON.
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
    fn build_pool(
        cfg: &ProjectConfig,
        lifecycle: PoolLifecycle,
        platform_messages: &tokio::sync::mpsc::UnboundedSender<PlatformAsyncMessage>,
    ) -> anyhow::Result<Pool> {
        let pg_config: tokio_postgres::Config = cfg
            .database_url
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid database url: {e}"))?;
        let manager_config = ManagerConfig {
            recycling_method: RecyclingMethod::Fast,
        };
        let mgr = match lifecycle {
            PoolLifecycle::Guest => Manager::from_config(pg_config, NoTls, manager_config),
            PoolLifecycle::Platform => Manager::from_connect(
                pg_config,
                PlatformConnect::new(platform_messages.clone()),
                manager_config,
            ),
        };
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
            .runtime(Runtime::Tokio1)
            .build()?)
    }

    /// Resolve + lazily build (memoized) the pool for a project. Unknown project
    /// or a build/resolution failure ⇒ `connection-unavailable`.
    fn pools(
        &self,
        lifecycle: PoolLifecycle,
    ) -> &std::sync::RwLock<HashMap<String, Arc<ProjectPool>>> {
        match lifecycle {
            PoolLifecycle::Guest => &self.guest_pools,
            PoolLifecycle::Platform => &self.platform_pools,
        }
    }

    fn ensure_pool(
        &self,
        lifecycle: PoolLifecycle,
        project: &str,
    ) -> Result<Arc<ProjectPool>, PgError> {
        let pools = self.pools(lifecycle);
        if let Some(pp) = pools.read().expect("pools lock poisoned").get(project) {
            return Ok(pp.clone());
        }
        let cfg = match self.provider.resolve(project) {
            Ok(Some(c)) => c,
            Ok(None) => {
                tracing::warn!(
                    project,
                    lifecycle = lifecycle.label(),
                    "wamn:postgres: no credentials for project"
                );
                return Err(PgError::ConnectionUnavailable);
            }
            Err(e) => {
                tracing::warn!(
                    project,
                    lifecycle = lifecycle.label(),
                    error = %e,
                    "wamn:postgres: credential resolution failed"
                );
                return Err(PgError::ConnectionUnavailable);
            }
        };
        let pp = match Self::build_pool(&cfg, lifecycle, &self.platform_messages) {
            Ok(pool) => Arc::new(ProjectPool {
                pool,
                statement_timeout_ms: cfg.statement_timeout_ms,
                row_limit: cfg.row_limit,
            }),
            Err(e) => {
                tracing::warn!(
                    project,
                    lifecycle = lifecycle.label(),
                    error = %e,
                    "wamn:postgres: pool build failed"
                );
                return Err(PgError::ConnectionUnavailable);
            }
        };
        let mut w = pools.write().expect("pools lock poisoned");
        Ok(w.entry(project.to_string()).or_insert(pp).clone())
    }

    fn ensure_guest_pool(&self, project: &str) -> Result<Arc<ProjectPool>, PgError> {
        self.ensure_pool(PoolLifecycle::Guest, project)
    }

    fn ensure_platform_pool(&self, project: &str) -> Result<Arc<ProjectPool>, PgError> {
        self.ensure_pool(PoolLifecycle::Platform, project)
    }

    /// Take the one async-message stream driven by this instance's existing
    /// platform pools. A second subscriber would be a second doorbell owner and
    /// is refused before it can LISTEN.
    pub(crate) fn take_platform_messages(
        &self,
    ) -> anyhow::Result<tokio::sync::mpsc::UnboundedReceiver<PlatformAsyncMessage>> {
        self.platform_message_receiver
            .lock()
            .map_err(|_| anyhow::anyhow!("platform-message-receiver-lock-poisoned"))?
            .take()
            .ok_or_else(|| anyhow::anyhow!("platform-message-receiver-already-taken"))
    }

    /// Hold one existing platform-pool connection in LISTEN for the wiring
    /// doorbell. The returned object stays checked out for the subscription's
    /// lifetime; no direct connection or second pool is created.
    pub(crate) async fn checkout_wiring_listener(
        &self,
        project: &str,
    ) -> anyhow::Result<(Object, i32)> {
        let (connection, _) = self
            .checkout_platform(project)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let backend_pid = async {
            connection
                .batch_execute(&format!(
                    "LISTEN {}",
                    wamn_catalog::WIRING_ACTIVATION_CHANNEL
                ))
                .await
                .context("LISTEN wiring activation through platform pool")?;
            connection
                .query_one("SELECT pg_backend_pid()", &[])
                .await
                .context("read wiring doorbell backend id")?
                .try_get(0)
                .context("decode wiring doorbell backend id")
        }
        .await;
        match backend_pid {
            Ok(backend_pid) => Ok((connection, backend_pid)),
            Err(error) => {
                // LISTEN is session state. A half-constructed listener may not
                // return to the general platform pool even when the socket is
                // otherwise healthy.
                self.destroy(connection);
                Err(error)
            }
        }
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

    /// Register the caller's `app.role` claim for a component id (4.2). When
    /// set, every transaction the plugin opens for that component binds
    /// `app.role`, so a compiled per-role RLS policy gates on the caller's role
    /// instead of the `''` floor that denies it. Host-injected identity like the
    /// tenant: the guest has no path to it, and [`reject_claim_mutation`] refuses
    /// an in-band override.
    pub fn set_role(&self, component_id: &str, role: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            valid_role(role),
            "invalid caller role {role:?}: a non-empty roles.name is required"
        );
        self.roles
            .write()
            .expect("roles lock poisoned")
            .insert(component_id.to_string(), role.to_string());
        Ok(())
    }

    pub(super) fn role_for(&self, component_id: &str) -> Option<String> {
        self.roles
            .read()
            .expect("roles lock poisoned")
            .get(component_id)
            .cloned()
    }

    /// Register the caller's `app.user_id` claim for a component id (4.2). When
    /// set, every transaction the plugin opens for that component binds
    /// `app.user_id`, so a compiled row-ownership RLS policy compares against
    /// the caller's own `users.id` instead of NULL. Host-injected identity like
    /// the tenant.
    pub fn set_user_id(&self, component_id: &str, user_id: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            valid_user_id(user_id),
            "invalid caller user id {user_id:?}: a canonical 8-4-4-4-12 uuid is required"
        );
        self.users
            .write()
            .expect("users lock poisoned")
            .insert(component_id.to_string(), user_id.to_string());
        Ok(())
    }

    pub(super) fn user_id_for(&self, component_id: &str) -> Option<String> {
        self.users
            .read()
            .expect("users lock poisoned")
            .get(component_id)
            .cloned()
    }

    /// Register the release this pod carries for a component id. The production
    /// claim writes it onto every run it leases, write-once. The bench harness
    /// and live tests call this directly; the host path feeds it from the loaded
    /// [`ReleaseManifestWeld`](crate::release_manifest::ReleaseManifestWeld),
    /// whose pair is derived from verified manifest content. Absent leaves the
    /// claim recording nothing.
    ///
    /// The digest arrives as [`ManifestDigest`], so its shape is already proven;
    /// only the version still needs a check, and `wamn-0h0g.15.65` owns giving
    /// that value a type too.
    ///
    /// # Why effect authority needs no equality check against this record
    ///
    /// The pair comes from the same welded object every reader resolves against,
    /// so the digest recorded on a run IS the digest of the manifest the recording
    /// pod loaded — structurally, not because anything compares them (owner ruling
    /// `wamn-0h0g.15.102`, after `wamn-0h0g.15.103` struck the asserted carrier).
    /// That survives a re-claim by a differently-released pod: the record is
    /// write-once, and the run plane only lets it be cleared while the run has NO
    /// effect attempts at all (`run-release-record-immutable`,
    /// `deploy/sql/run-state.sql`), so whichever pod holds a run's lease always
    /// carries the release that run records. This is what makes the host-side
    /// plan-closure check honest without a comparator (`wamn-0h0g.15.66`).
    pub fn set_release_identity(
        &self,
        component_id: &str,
        release_version: i32,
        manifest_digest: ManifestDigest,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            release_version > 0,
            "invalid release version {release_version}: a positive catalog version is required"
        );
        self.release_identities
            .write()
            .expect("release identities lock poisoned")
            .insert(
                component_id.to_string(),
                ReleaseIdentity {
                    release_version,
                    manifest_digest,
                },
            );
        Ok(())
    }

    pub(super) fn release_identity_for(&self, component_id: &str) -> Option<ReleaseIdentity> {
        self.release_identities
            .read()
            .expect("release identities lock poisoned")
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

    /// Bind EVERY per-component-id claim registry this plugin keeps to one
    /// identity, for the length of one execution checkout (wamn-0h0g.17.7).
    ///
    /// The registries are process-resident and keyed by component id, so a
    /// component id is a *claim scope*, not a fact about which tenant a store
    /// was built for. This is the write half of that scope: a pooled instance is
    /// fungible compute, and the identity it serves arrives here at acquisition
    /// rather than at construction. Every registry
    /// [`revoke_session_claims`](Self::revoke_session_claims) clears is written
    /// here, so the two are exhaustive over the same set — a claim added to one
    /// and not the other is a leak channel.
    ///
    /// Absent optional claims REMOVE any prior registration rather than leaving
    /// it standing: an acquisition that declares no schema must not inherit the
    /// previous acquisition's `search_path`.
    ///
    /// Validation is the same as the individual `set_*` setters, so an invalid
    /// claim is refused here and the caller destroys the instance rather than
    /// serving it under a half-written identity.
    pub fn bind_session_claims(
        &self,
        component_id: &str,
        claims: &SessionClaims,
    ) -> anyhow::Result<()> {
        self.set_tenant(component_id, &claims.tenant)?;
        match claims.project.as_deref() {
            Some(project) => self.set_project(component_id, project)?,
            None => drop(
                self.projects
                    .write()
                    .expect("projects lock poisoned")
                    .remove(component_id),
            ),
        }
        match claims.schema.as_deref() {
            Some(schema) => self.set_schema(component_id, schema)?,
            None => drop(
                self.schemas
                    .write()
                    .expect("schemas lock poisoned")
                    .remove(component_id),
            ),
        }
        match claims.runner.as_deref() {
            Some(runner) => self.set_runner(component_id, runner)?,
            None => drop(
                self.runners
                    .write()
                    .expect("runners lock poisoned")
                    .remove(component_id),
            ),
        }
        match claims.role.as_deref() {
            Some(role) => self.set_role(component_id, role)?,
            None => drop(
                self.roles
                    .write()
                    .expect("roles lock poisoned")
                    .remove(component_id),
            ),
        }
        match claims.user_id.as_deref() {
            Some(user_id) => self.set_user_id(component_id, user_id)?,
            None => drop(
                self.users
                    .write()
                    .expect("users lock poisoned")
                    .remove(component_id),
            ),
        }
        match claims.release.as_ref() {
            Some(release) => self.set_release_identity(
                component_id,
                release.release_version,
                release.manifest_digest.clone(),
            )?,
            None => drop(
                self.release_identities
                    .write()
                    .expect("release identities lock poisoned")
                    .remove(component_id),
            ),
        }
        // Causation is RUN-scoped, declared by the trusted runner through
        // `wamn:runner/causation` once the run is in hand. A binding acquisition
        // has no run yet, so it starts undeclared — carrying the previous
        // acquisition's run context forward would misattribute its CDC stitch.
        self.current_run
            .write()
            .expect("current_run lock poisoned")
            .remove(component_id);
        Ok(())
    }

    /// Read back the claim set one component id currently resolves to.
    ///
    /// `None` is the deny floor: no tenant is bound, so
    /// [`require_tenant`](Self::require_tenant) refuses every call made under
    /// this id. That is what an idle, never-acquired execution instance must
    /// report, and it is the read half of the checkout-identity seam — the pair
    /// this and [`bind_session_claims`](Self::bind_session_claims) form is what
    /// lets a proof assert that two concurrent acquisitions never share one
    /// identity.
    pub fn session_claims(&self, component_id: &str) -> Option<SessionClaims> {
        Some(SessionClaims {
            tenant: self.tenant_for(component_id)?,
            project: self
                .projects
                .read()
                .expect("projects lock poisoned")
                .get(component_id)
                .cloned(),
            schema: self.schema_for(component_id),
            runner: self.runner_for(component_id),
            role: self.role_for(component_id),
            user_id: self.user_id_for(component_id),
            release: self.release_identity_for(component_id),
        })
    }

    /// Clear every registration [`bind_session_claims`](Self::bind_session_claims)
    /// installed under one component id.
    ///
    /// Called when a checkout ends, on every path — repooled, retired, or
    /// dropped. An idle instance that still resolved a tenant would be a warm
    /// store carrying identity, which is the whole defect this seam closes; and
    /// a `require_tenant` failure is how an unbound instance fails closed
    /// instead of serving someone else's rows.
    pub fn revoke_session_claims(&self, component_id: &str) {
        self.tenants
            .write()
            .expect("tenants lock poisoned")
            .remove(component_id);
        self.projects
            .write()
            .expect("projects lock poisoned")
            .remove(component_id);
        self.schemas
            .write()
            .expect("schemas lock poisoned")
            .remove(component_id);
        self.runners
            .write()
            .expect("runners lock poisoned")
            .remove(component_id);
        self.roles
            .write()
            .expect("roles lock poisoned")
            .remove(component_id);
        self.users
            .write()
            .expect("users lock poisoned")
            .remove(component_id);
        self.release_identities
            .write()
            .expect("release identities lock poisoned")
            .remove(component_id);
        self.current_run
            .write()
            .expect("current_run lock poisoned")
            .remove(component_id);
    }

    /// Reap EVERY per-component-id claim registry this plugin keeps for a workload
    /// on teardown (R31): tenant, project, search_path schema, runner lease-owner,
    /// the caller's role / user id, the carried release identity, and the
    /// causation run context — all set at
    /// workload bind (or via the runner channel) and keyed by component id.
    /// Without this a stale claim
    /// survives unbind, the maps grow across workload churn, and a rebound
    /// component id inherits the prior claim. The lifecycle pool maps are
    /// deliberately NOT touched: they are keyed by PROJECT (shared and memoized
    /// within each lifecycle for the plugin's lifetime), not by component id.
    /// Keyed like the fork's builtin postgres plugin — a workload's component ids
    /// are prefixed by the workload id — so everything NOT under it is retained;
    /// an unknown workload id is a no-op.
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
        self.roles
            .write()
            .expect("roles lock poisoned")
            .retain(|c, _| retain(c));
        self.users
            .write()
            .expect("users lock poisoned")
            .retain(|c, _| retain(c));
        self.release_identities
            .write()
            .expect("release identities lock poisoned")
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

    fn pool_status_all_by_lifecycle(&self) -> Vec<(PoolLifecycle, String, (usize, usize, usize))> {
        let mut statuses = Vec::new();
        for (lifecycle, pools) in [
            (PoolLifecycle::Guest, &self.guest_pools),
            (PoolLifecycle::Platform, &self.platform_pools),
        ] {
            statuses.extend(pools.read().expect("pools lock poisoned").iter().map(
                |(project, pp)| {
                    let status = pp.pool.status();
                    (
                        lifecycle,
                        project.clone(),
                        (status.size, status.available, status.waiting),
                    )
                },
            ));
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
    pub async fn probe_checkout(&self) -> anyhow::Result<CheckoutProbe> {
        self.probe_checkout_of(DEFAULT_PROJECT).await
    }

    /// Check out a raw connection from a project's (lazily built) pool and
    /// report its state *before* any claim injection. Gate verification only —
    /// not reachable from guests and not a platform work path. It deliberately
    /// observes the guest lifecycle that the conformance gate is proving.
    pub async fn probe_checkout_of(&self, project: &str) -> anyhow::Result<CheckoutProbe> {
        let pp = self
            .ensure_guest_pool(project)
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
    /// transaction under the component's injected tenant.
    ///
    /// # Why the caller's tenant is checked against the registry (wamn-0h0g.17.7)
    ///
    /// `tenant` arrives from `ConnectionHttp`, which froze it into a `Box<str>`
    /// when its store was constructed
    /// (`crates/platform/runtime/src/plugins/connection_http.rs:77`). That value
    /// is NOT rebound at checkout — the store's `Ctx.plugins` map is private to
    /// the fork, so nothing outside it can swap the plugin — while every other
    /// claim on this path IS
    /// ([`bind_session_claims`](Self::bind_session_claims)). A pooled instance
    /// serving tenant B would therefore carry tenant A's frozen value here and
    /// read A's rows under B's run.
    ///
    /// This plugin's registry is the authority on a component id's tenant, so a
    /// caller-supplied tenant that DISAGREES with it is refused rather than
    /// honored. The check is a denial, not a correction: silently substituting
    /// the registered tenant would hide the divergence it exists to catch.
    ///
    /// An UNBOUND component id is refused outright (wamn-0h0g.17.11), not
    /// allowed to fall through to the caller's tenant. Agreement with the
    /// registry is only meaningful when the registry HAS an entry, so a guard
    /// that skipped the absent case would be a guard whose strength depended on
    /// the caller having been bound. It is the same deny floor
    /// [`require_tenant`](Self::require_tenant) applies to every other read:
    /// nothing resolves under an unbound claim scope.
    pub async fn connection_effect_snapshot(
        &self,
        component_id: &str,
        project: &str,
        tenant: &str,
        lookup: &ConnectionEffectLookup<'_>,
    ) -> anyhow::Result<Option<ConnectionEffectSnapshot>> {
        let registered = self.tenant_for(component_id).ok_or_else(|| {
            anyhow::anyhow!(
                "HTTP effect authorization for component {component_id:?} has no bound tenant \
                 claim; refusing to resolve under an unbound identity"
            )
        })?;
        anyhow::ensure!(
            registered == tenant,
            "HTTP effect authorization tenant {tenant:?} disagrees with the tenant bound \
             to component {component_id:?}; refusing to resolve under a stale identity"
        );
        let schema = self.schema_for(component_id);
        let (conn, policy) = self
            .checkout_platform(project)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if let Err(error) = self
            .begin_with_claims(
                &conn,
                tenant,
                schema.as_deref(),
                None,
                None,
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
            let candidate_instance = lookup
                .candidate_binding
                .map(|binding| binding.instance_id.as_str());
            let candidate_generation = lookup.candidate_binding.map(|binding| binding.generation);
            let params: [&(dyn ToSql + Sync); 11] = [
                &tenant,
                &lookup.catalog_id,
                &lookup.catalog_version,
                &lookup.environment,
                &lookup.wiring_id,
                &lookup.wiring_version,
                &lookup.node_id,
                &lookup.component_digest,
                &lookup.store_alias,
                &candidate_instance,
                &candidate_generation,
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
                wiring_hash: row.try_get(0)?,
                component: row.try_get(1)?,
                interface_version: row.try_get(2)?,
                requirement_json: json(3)?,
                requirement_hash: row.try_get(4)?,
                node_permitted: row.try_get::<_, Option<bool>>(5)?.unwrap_or(false),
                binding_active: row.try_get::<_, Option<bool>>(6)?.unwrap_or(false),
                binding_valid: row.try_get::<_, Option<bool>>(7)?.unwrap_or(false),
                instance_id: row.try_get(8)?,
                validation_hash: row.try_get(9)?,
                requirement_type: row.try_get(10)?,
                contract: row.try_get(11)?,
                instance_enabled: row.try_get::<_, Option<bool>>(12)?.unwrap_or(false),
                active_generation: row.try_get(13)?,
                instance_revision: row.try_get(14)?,
                generation: row.try_get(15)?,
                definition: json(16)?,
                definition_hash: row.try_get(17)?,
                credential_handle: row.try_get(18)?,
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

    pub(super) fn destroy(&self, obj: Object) {
        destroy_connection(obj, &self.destroyed);
    }

    async fn checkout_lifecycle(
        &self,
        lifecycle: PoolLifecycle,
        project: &str,
    ) -> Result<(Object, Arc<ProjectPool>), PgError> {
        let pp = match lifecycle {
            PoolLifecycle::Guest => self.ensure_guest_pool(project)?,
            PoolLifecycle::Platform => self.ensure_platform_pool(project)?,
        };
        let obj = pp.pool.get().await.map_err(|e| {
            tracing::warn!(
                project,
                lifecycle = lifecycle.label(),
                error = %e,
                "wamn:postgres pool checkout failed"
            );
            PgError::ConnectionUnavailable
        })?;
        Ok((obj, pp))
    }

    /// Check out a connection reserved for guest-visible `wamn:postgres` calls.
    pub(super) async fn checkout_guest(
        &self,
        project: &str,
    ) -> Result<(Object, Arc<ProjectPool>), PgError> {
        self.checkout_lifecycle(PoolLifecycle::Guest, project).await
    }

    /// Check out a connection reserved for host-owned platform work.
    pub(super) async fn checkout_platform(
        &self,
        project: &str,
    ) -> Result<(Object, Arc<ProjectPool>), PgError> {
        self.checkout_lifecycle(PoolLifecycle::Platform, project)
            .await
    }

    /// `BEGIN` + claim/limit injection. The claims are injected by ONE fully
    /// bound statement ([`CLAIM_SQL`]) whose every value travels as a bind
    /// parameter — there is no interpolation path (R2/R16). `tenant` is always
    /// present; `schema`/`runner` bind NULL when absent (COALESCE-to-current
    /// preserves the server default / prior value — the S2/pgbench path is
    /// byte-unchanged), and `role`/`user_id` bind `''` when absent, the deny
    /// floor the compiled RLS predicates read. A run-owned transaction also
    /// appends the transactional `wamn.causation` emit (l5i9.12.2).
    ///
    /// Cost: `BEGIN` and the bound claim statement are pipelined (issued without
    /// an await between them; tokio-postgres preserves FIFO order so `BEGIN`
    /// opens the txn before the transaction-LOCAL `set_config`s apply), and the
    /// claim statement is `prepare_cached`, so the steady-state round-trip count
    /// on a pooled connection matches the pre-R2 single batch.
    #[expect(
        clippy::too_many_arguments,
        reason = "every claim this transaction binds is an independently trusted input"
    )]
    pub(super) async fn begin_with_claims(
        &self,
        conn: &Object,
        tenant: &str,
        schema: Option<&str>,
        runner: Option<&str>,
        role: Option<&str>,
        user_id: Option<&str>,
        run: Option<&Causation>,
        statement_timeout_ms: u32,
    ) -> Result<(), PgError> {
        validate_claims(tenant, schema, runner, role, user_id)?;
        let stmt = conn
            .prepare_cached(CLAIM_SQL)
            .await
            .map_err(|e| map_pg_error(&e))?;
        // statement_timeout binds as TEXT (a bare-integer string = ms).
        let timeout = statement_timeout_ms.to_string();
        // An absent role / user id binds the empty claim, not NULL: `''` is the
        // value the compiled policies' COALESCE / NULLIF floors deny on.
        let role = role.unwrap_or_default();
        let user_id = user_id.unwrap_or_default();
        let params: [&(dyn ToSql + Sync); 6] =
            [&tenant, &timeout, &schema, &runner, &role, &user_id];
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
        let project = self.project_for(component_id);
        self.one_shot_for_project(component_id, &project, sql, params, want_rows)
            .await
    }

    /// Execute one statement using an explicitly selected named-import
    /// project. Named `wamn:postgres` interfaces must not fall back to the
    /// component's single `wamn.project` claim.
    pub(super) async fn one_shot_for_project(
        &self,
        component_id: &str,
        project: &str,
        sql: &str,
        params: &[SqlValue],
        want_rows: bool,
    ) -> Result<OneShotResult, PgError> {
        let tenant = self.require_tenant(component_id)?;
        let schema = self.schema_for(component_id);
        let runner = self.runner_for(component_id);
        let role = self.role_for(component_id);
        let user_id = self.user_id_for(component_id);
        let run = self.current_run_for(component_id);
        let (conn, pp) = self.checkout_guest(project).await?;
        if let Err(e) = self
            .begin_with_claims(
                &conn,
                &tenant,
                schema.as_deref(),
                runner.as_deref(),
                role.as_deref(),
                user_id.as_deref(),
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

    fn candidate_binding(component: &str, alias: &str) -> serde_json::Value {
        serde_json::json!({
            "component-digest": component,
            "store-alias": alias,
            "requirement-hash": "sha256:req",
            "instance-id": "orders",
            "instance-revision": 3,
            "requirement-type": "http",
            "contract": "wamn:connection/http@0.1.0",
            "validation-hash": "sha256:validation",
            "generation": 7,
            "definition-hash": "sha256:definition",
            "credential-set-handle": "orders-7"
        })
    }

    #[test]
    fn candidate_binding_world_requires_complete_canonical_unique_rows() {
        let first = candidate_binding("sha256:a", "primary");
        let second = candidate_binding("sha256:b", "primary");
        let world =
            CandidateBindingWorld::from_json(serde_json::json!([first.clone(), second.clone()]))
                .expect("ordered complete world");
        assert!(world.binding("sha256:a", "primary").is_some());
        assert!(CandidateBindingWorld::from_json(serde_json::json!([second, first])).is_err());
        let mut incomplete = candidate_binding("sha256:a", "primary");
        incomplete
            .as_object_mut()
            .expect("fixture object")
            .remove("generation");
        assert!(CandidateBindingWorld::from_json(serde_json::json!([incomplete])).is_err());
    }

    #[test]
    fn candidate_effect_snapshot_pins_instance_and_generation_without_fallback() {
        for predicate in [
            "($10::text IS NULL OR binding.instance_id = $10)",
            "generation.generation = COALESCE($11::bigint, instance.active_generation)",
            "($11::bigint IS NULL OR instance.active_generation = $11)",
        ] {
            assert!(CONNECTION_EFFECT_SNAPSHOT_SQL.contains(predicate));
        }
    }

    #[test]
    fn guest_and_host_owned_paths_are_pinned_to_distinct_pool_lifecycles() {
        // Scan the implementation half only. Every signature searched below is
        // also spelled in this module, so a whole-file scan lets a DELETED
        // subject match the test's own source: the `expect` never fires and the
        // span silently collapses onto the search text instead of reporting the
        // absence (wamn-22xc, an instance of wamn-0h0g.15.137).
        let claims = include_str!("claims.rs");
        let claims = &claims[..claims
            .find("#[cfg(test)]\nmod tests {")
            .expect("test module boundary")];
        let effect_start = claims
            .find("pub async fn connection_effect_snapshot(")
            .expect("effect snapshot method");
        let destroy_start = claims[effect_start..]
            .find("pub(super) fn destroy(")
            .map(|offset| effect_start + offset)
            .expect("destroy method after effect snapshot");
        assert!(claims[effect_start..destroy_start].contains(".checkout_platform(project)"));

        let guest_checkout_start = claims
            .find("pub(super) async fn checkout_guest(")
            .expect("guest checkout method");
        let platform_checkout_start = claims
            .find("pub(super) async fn checkout_platform(")
            .expect("platform checkout method");
        let claims_begin_start = claims[platform_checkout_start..]
            .find("pub(super) async fn begin_with_claims(")
            .map(|offset| platform_checkout_start + offset)
            .expect("claim injection method after lifecycle checkouts");
        assert!(
            claims[guest_checkout_start..platform_checkout_start].contains("PoolLifecycle::Guest")
        );
        assert!(
            claims[platform_checkout_start..claims_begin_start].contains("PoolLifecycle::Platform")
        );

        let one_shot_start = claims
            .find("pub(super) async fn one_shot(")
            .expect("one-shot guest method");
        let one_shot_end = claims
            .find("pub(super) enum OneShotResult")
            .expect("one-shot result after method");
        assert!(claims[one_shot_start..one_shot_end].contains(".checkout_guest(project)"));

        let resources = include_str!("resources.rs");
        let begin_start = resources
            .find("async fn begin_transaction(")
            .expect("guest begin helper");
        let host_start = resources
            .find("impl client::Host for ActiveCtx")
            .expect("client host implementation after the begin helper");
        assert!(resources[begin_start..host_start].contains("plugin.checkout_guest(project)"));
    }

    #[test]
    fn guest_and_platform_pool_caches_remain_distinct_under_interleaving() {
        let postgres = WamnPostgres::new(WamnPostgresConfig {
            database_url: Some("postgres://localhost/pool-lifecycle-proof".to_string()),
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 100,
            statement_timeout_ms: 100,
            row_limit: 10,
        })
        .expect("construct lazy lifecycle pools");

        let guest_first = postgres
            .ensure_guest_pool(DEFAULT_PROJECT)
            .expect("first guest pool");
        let platform_first = postgres
            .ensure_platform_pool(DEFAULT_PROJECT)
            .expect("first platform pool");
        let platform_second = postgres
            .ensure_platform_pool(DEFAULT_PROJECT)
            .expect("memoized platform pool");
        let guest_second = postgres
            .ensure_guest_pool(DEFAULT_PROJECT)
            .expect("memoized guest pool");

        assert!(Arc::ptr_eq(&guest_first, &guest_second));
        assert!(Arc::ptr_eq(&platform_first, &platform_second));
        assert!(!Arc::ptr_eq(&guest_first, &platform_first));

        let mut lifecycle_labels = postgres
            .pool_status_all_by_lifecycle()
            .into_iter()
            .map(|(lifecycle, project, _)| (lifecycle.label(), project))
            .collect::<Vec<_>>();
        lifecycle_labels.sort_unstable();
        assert_eq!(
            lifecycle_labels,
            [
                ("guest", DEFAULT_PROJECT.to_string()),
                ("platform", DEFAULT_PROJECT.to_string()),
            ]
        );
    }

    // The claim-time record must match `runs_release_record_check` exactly, or a
    // claim that would otherwise succeed dies on a CHECK inside the lease grant.
    // The hand-rolled shape check this used to exercise is retired: the invariant
    // now rides `ManifestDigest`, so what is pinned here is that the TYPE admits
    // exactly what the run-plane CHECK admits. Same coupling, one owner.
    #[test]
    fn manifest_digest_shape_matches_the_run_plane_check() {
        assert!(ManifestDigest::parse(format!("sha256:{}", "0".repeat(64))).is_ok());
        assert!(ManifestDigest::parse(format!("sha256:{}b", "af9".repeat(21))).is_ok());
        for rejected in [
            String::new(),
            "sha256:".to_string(),
            "deadbeef".to_string(),
            format!("sha256:{}", "a".repeat(63)),
            format!("sha256:{}", "a".repeat(65)),
            format!("sha256:{}", "A".repeat(64)),
            format!("sha256:{}", "g".repeat(64)),
            format!("SHA256:{}", "a".repeat(64)),
        ] {
            assert!(
                ManifestDigest::parse(rejected.clone()).is_err(),
                "accepted {rejected:?}"
            );
        }
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
            // wamn-0h0g.23.1 — the two claims CLAIM_SQL now injects. Overriding
            // either is the privilege escalation the binding would otherwise open:
            // `app.role` clears an exempt-role gate, `app.user_id` reassigns row
            // ownership.
            "SET app.role = 'admin'",
            "set local app.user_id = '00000000-0000-4000-8000-000000000000'",
            "RESET app.role",
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
            "SELECT set_config('app.role','admin',true)",
            "WITH t AS (SELECT set_config('app.user_id','00000000-0000-4000-8000-000000000000',true)) SELECT 1",
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
            // wamn-0h0g.23.1 — bound, and bound to the FLOOR when absent: a
            // COALESCE-to-current here would carry a pooled connection's leftover
            // role into the next component's transaction.
            "set_config('app.role', $5, true)",
            "set_config('app.user_id', $6, true)",
        ] {
            assert!(CLAIM_SQL.contains(frag), "CLAIM_SQL missing {frag:?}");
        }
    }

    #[test]
    fn effect_authority_resolves_exact_wiring_component_and_store_alias() {
        for required in [
            "FROM catalog.wirings AS wiring",
            "wiring.gated_catalog_version = $3",
            "wiring.wiring_id = $5",
            "wiring.version = $6",
            "wiring.graph_json ->> 'wiring-id' = $5",
            "component.component_digest = $8",
            "wiring.graph_json #> ARRAY['nodes', $7]",
            "requirement.artifact_hash IS NULL",
            "requirement.requirement_name IS NULL",
            "requirement.component_digest = $8",
            "requirement.store_alias = $9",
            "binding.catalog_version = $3",
            "binding.environment = $4",
        ] {
            assert!(
                CONNECTION_EFFECT_SNAPSHOT_SQL.contains(required),
                "effect authority snapshot omits {required:?}"
            );
        }
    }

    #[test]
    fn effect_authority_has_no_run_plan_frame_or_legacy_requirement_fallback() {
        let sql = CONNECTION_EFFECT_SNAPSHOT_SQL.to_ascii_lowercase();
        for retired in [
            "wamn_run.",
            " from runs ",
            "effect_attempts",
            "execution_bundles",
            "plan_hash",
            "frame_id",
            "flow_id",
            "validated_flow_drafts",
            "artifact_hash =",
            "requirement_name =",
        ] {
            assert!(!sql.contains(retired), "effect authority retains {retired}");
        }
        for write in [" insert ", " update ", " delete "] {
            assert!(!sql.contains(write), "effect authority performs {write:?}");
        }
    }

    // R16 — the validators stay as the identity-format contract (demoted from the
    // injection boundary by R2): a malformed identity fails closed even though
    // the value would bind as inert data.
    #[test]
    fn validate_claims_rejects_malformed_identities() {
        const U1: &str = "11111111-1111-4111-8111-111111111111";
        assert!(
            validate_claims(
                "acme",
                Some("public"),
                Some("owner-1"),
                Some("inspector"),
                Some(U1)
            )
            .is_ok()
        );
        assert!(validate_claims("acme", None, None, None, None).is_ok());
        assert!(validate_claims("bad'tenant", None, None, None, None).is_err());
        assert!(validate_claims("acme", Some("has-hyphen"), None, None, None).is_err());
        assert!(validate_claims("acme", None, Some("bad;runner"), None, None).is_err());
        // `''` is the deny floor, never a claim.
        assert!(validate_claims("acme", None, None, Some(""), None).is_err());
        // A non-uuid user id would raise 22P02 inside every ownership predicate.
        assert!(validate_claims("acme", None, None, None, Some("not-a-uuid")).is_err());
        assert!(validate_claims("acme", None, None, None, Some(&U1[..35])).is_err());
        assert!(validate_claims("acme", None, None, None, Some(&format!("{U1}-1"))).is_err());
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

    // R31 — unbind reaps ALL SEVEN per-component claim registries for a workload
    // (tenant/project/schema/runner/role/user/causation) while leaving another workload's
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
            pg.set_role(c, "inspector").unwrap();
            pg.set_user_id(c, "6e1f2a3b-4c5d-4e6f-8a9b-0c1d2e3f4a5b")
                .unwrap();
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
        assert_eq!(pg.role_for("wl-a-component-0"), None);
        assert_eq!(pg.user_id_for("wl-a-component-0"), None);
        assert!(pg.current_run_for("wl-a-component-0").is_none());

        // The other workload's component is untouched across the board.
        assert_eq!(pg.tenant_for("wl-b-component-0").as_deref(), Some("acme"));
        assert_eq!(pg.project_for("wl-b-component-0"), "proj");
        assert_eq!(pg.schema_for("wl-b-component-0").as_deref(), Some("s_run"));
        assert_eq!(
            pg.runner_for("wl-b-component-0").as_deref(),
            Some("owner-1")
        );
        assert_eq!(
            pg.role_for("wl-b-component-0").as_deref(),
            Some("inspector")
        );
        assert_eq!(
            pg.user_id_for("wl-b-component-0").as_deref(),
            Some("6e1f2a3b-4c5d-4e6f-8a9b-0c1d2e3f4a5b")
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

        // (1) app.tenant / app.runner / app.role / app.user_id are free-form
        //     custom GUCs: injection-shaped
        //     + unicode values bind as DATA and round-trip VERBATIM; the absent
        //     schema ($3 NULL) leaves the server-default search_path untouched.
        let default_sp: Option<String> = client
            .query_one("SELECT current_setting('search_path', true)", &[])
            .await
            .unwrap()
            .get(0);
        let evil_tenant = format!("x'; DROP TABLE public.{marker}; -- 😀Ω");
        let evil_runner = format!("r'; DELETE FROM public.{marker}; --");
        let evil_role = format!("admin'); DROP TABLE public.{marker}; --");
        let evil_user = format!("u'; TRUNCATE public.{marker}; --");
        let no_schema: Option<&str> = None;
        client.batch_execute("BEGIN").await.unwrap();
        let params: [&(dyn ToSql + Sync); 6] = [
            &evil_tenant,
            &timeout,
            &no_schema,
            &evil_runner,
            &evil_role,
            &evil_user,
        ];
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
        let got_role: Option<String> = client
            .query_one("SELECT current_setting('app.role', true)", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(got_role.as_deref(), Some(evil_role.as_str()));
        let got_user: Option<String> = client
            .query_one("SELECT current_setting('app.user_id', true)", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(got_user.as_deref(), Some(evil_user.as_str()));
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
        let params2: [&(dyn ToSql + Sync); 6] = [
            &evil_tenant,
            &timeout,
            &evil_schema,
            &evil_runner,
            &evil_role,
            &evil_user,
        ];
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

    // R2/R16 — the REAL plugin path: begin_with_claims injects all six claims via
    // the bound statement, they are visible in-txn, and revert after the txn.
    #[tokio::test]
    async fn live_begin_with_claims_sets_all_six_and_reverts() {
        let Some(url) = test_pg_url() else {
            return;
        };
        let pg = WamnPostgres::new(WamnPostgresConfig {
            database_url: Some(url),
            guest_pool_max_size: 2,
            platform_pool_max_size: 2,
            wait_timeout_ms: 2_000,
            statement_timeout_ms: 5_000,
            row_limit: 1_000,
        })
        .unwrap();
        let user_id = "11111111-1111-4111-8111-111111111111";
        let (conn, _pp) = pg.checkout_guest(DEFAULT_PROJECT).await.unwrap();
        pg.begin_with_claims(
            &conn,
            "acme",
            Some("public"),
            Some("owner-1"),
            Some("inspector"),
            Some(user_id),
            None,
            4321,
        )
        .await
        .unwrap();
        let row = conn
            .query_one(
                "SELECT current_setting('app.tenant', true), \
                 current_setting('statement_timeout', true), \
                 current_setting('search_path', true), \
                 current_setting('app.runner', true), \
                 current_setting('app.role', true), \
                 current_setting('app.user_id', true)",
                &[],
            )
            .await
            .unwrap();
        let tenant: Option<String> = row.get(0);
        let timeout: Option<String> = row.get(1);
        let sp: Option<String> = row.get(2);
        let runner: Option<String> = row.get(3);
        let role: Option<String> = row.get(4);
        let user: Option<String> = row.get(5);
        assert_eq!(tenant.as_deref(), Some("acme"));
        assert_eq!(timeout.as_deref(), Some("4321ms"));
        assert_eq!(sp.as_deref(), Some("public"));
        assert_eq!(runner.as_deref(), Some("owner-1"));
        assert_eq!(role.as_deref(), Some("inspector"));
        assert_eq!(user.as_deref(), Some(user_id));

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

    /// Rows a component sees for `sql` on the production one-shot path.
    async fn visible_rows(pg: &WamnPostgres, component: &str, sql: &str) -> usize {
        match pg.one_shot(component, sql, &[], true).await {
            Ok(OneShotResult::Rows(rows)) => rows.rows.len(),
            Ok(OneShotResult::Count(_)) => unreachable!("one_shot(want_rows) returns rows"),
            Err(e) => panic!("one_shot {sql:?} failed: {e:?}"),
        }
    }

    /// The 3.2 tenant floor plus the 3.5 row-ownership rule in the shape
    /// `crates/schema/compiler/src/rls/compile.rs` emits (pinned by
    /// `crates/schema/compiler/tests/rls.rs`), over a table the probe role does
    /// not own so RLS applies to it.
    fn rls_fixture_sql(schema: &str, probe: &str, tenant: &str, u1: &str, u2: &str) -> String {
        format!(
            "DROP SCHEMA IF EXISTS {schema} CASCADE; \
             DROP ROLE IF EXISTS {probe}; \
             CREATE ROLE {probe} LOGIN PASSWORD '{probe}' NOSUPERUSER NOBYPASSRLS; \
             CREATE SCHEMA {schema}; \
             CREATE TABLE {schema}.dispositions ( \
                 tenant_id text NOT NULL, id int NOT NULL, inspector_id uuid NOT NULL); \
             ALTER TABLE {schema}.dispositions ENABLE ROW LEVEL SECURITY; \
             CREATE POLICY dispositions_tenant ON {schema}.dispositions \
                 USING (tenant_id = NULLIF(current_setting('app.tenant', true), '')); \
             CREATE POLICY \"dispositions_owner_0\" ON {schema}.dispositions AS RESTRICTIVE \
                 FOR ALL \
                 USING (COALESCE(current_setting('app.role', true), '') IN ('supervisor', 'admin') \
                        OR \"inspector_id\" = NULLIF(current_setting('app.user_id', true), '')::uuid); \
             INSERT INTO {schema}.dispositions VALUES ('{tenant}', 1, '{u1}'), ('{tenant}', 2, '{u2}'); \
             GRANT USAGE ON SCHEMA {schema} TO {probe}; \
             GRANT SELECT ON {schema}.dispositions TO {probe};"
        )
    }

    // wamn-0h0g.23.1 — the compiled per-user / per-role rules key on `app.role`
    // and `app.user_id`, and under their COALESCE / NULLIF deny floors a policy
    // that is never handed those claims denies EVERYTHING. Before this bead
    // `CLAIM_SQL` bound only tenant / statement_timeout / search_path /
    // app.runner, so every per-user policy silently denied on the production
    // path while `crates/schema/compiler/tests/rls.rs` passed on hand-written
    // `SET LOCAL`. This drives the REAL plugin (`one_shot`) as a NOSUPERUSER
    // NOBYPASSRLS role, so it fails against a `CLAIM_SQL` that does not inject
    // the caller's identity.
    #[tokio::test]
    async fn live_compiled_per_user_policy_permits_the_injected_caller() {
        const TENANT: &str = "rls-claim-live";
        const COMPONENT: &str = "rls-claim-live-component";
        const U1: &str = "11111111-1111-4111-8111-111111111111";
        const U2: &str = "22222222-2222-4222-8222-222222222222";

        let Some(admin_url) = test_pg_url() else {
            return;
        };
        let suffix = std::process::id();
        let schema = format!("wamn_rls_claim_{suffix}");
        let probe = format!("wamn_rls_probe_{suffix}");
        let admin = connect_raw(&admin_url).await;
        admin
            .batch_execute(&rls_fixture_sql(&schema, &probe, TENANT, U1, U2))
            .await
            .expect("seed the per-user RLS fixture as the superuser owner");

        let pg = WamnPostgres::new(WamnPostgresConfig {
            database_url: Some(database_url_for_role(&admin_url, &probe, &probe)),
            guest_pool_max_size: 2,
            platform_pool_max_size: 2,
            wait_timeout_ms: 2_000,
            statement_timeout_ms: 5_000,
            row_limit: 1_000,
        })
        .unwrap();
        pg.set_tenant(COMPONENT, TENANT).unwrap();
        pg.set_schema(COMPONENT, &schema).unwrap();
        pg.set_role(COMPONENT, "inspector").unwrap();
        pg.set_user_id(COMPONENT, U1).unwrap();

        // CONTROL: the table is REACHABLE on this path — an aggregate returns its
        // one row however many rows RLS filtered away, and a wrong search_path or
        // a missing GRANT would raise instead. So a zero below is a policy
        // denying, not a broken fixture.
        assert_eq!(
            visible_rows(&pg, COMPONENT, "SELECT count(*) FROM dispositions").await,
            1,
            "control: the probe role must reach the fixture table"
        );

        // An inspector sees ONLY the row it owns.
        assert_eq!(
            visible_rows(&pg, COMPONENT, "SELECT id FROM dispositions WHERE id = 1").await,
            1,
            "the injected app.user_id must permit the caller's OWN row"
        );
        assert_eq!(
            visible_rows(&pg, COMPONENT, "SELECT id FROM dispositions WHERE id = 2").await,
            0,
            "the ownership rule must still deny another user's row"
        );
        assert_eq!(
            visible_rows(&pg, COMPONENT, "SELECT id FROM dispositions ORDER BY id").await,
            1
        );

        // …and an exempt role sees both, through the injected app.role.
        pg.set_role(COMPONENT, "admin").unwrap();
        assert_eq!(
            visible_rows(&pg, COMPONENT, "SELECT id FROM dispositions ORDER BY id").await,
            2,
            "the injected app.role must satisfy the exempt-role gate"
        );

        admin
            .batch_execute(&format!(
                "DROP SCHEMA {schema} CASCADE; DROP OWNED BY {probe}; DROP ROLE {probe};"
            ))
            .await
            .expect("drop the per-user RLS fixture");
    }

    // wamn-0h0g.23.1 — the SET-override refusal, on the two claims the fix now
    // injects. `reject_claim_mutation` (wamn-cjv.2) is the mechanism the tenant
    // claim already carries and it is GUC-agnostic, so it covers these the moment
    // they exist — but coverage that is never exercised is not coverage, and a
    // binding WITHOUT refusal turns a silent-deny bug into privilege escalation.
    // The CONTROL below proves the escalation is real, so the refusal that
    // follows is load-bearing rather than vacuous.
    #[tokio::test]
    async fn live_guest_cannot_override_the_injected_role_or_user_claim() {
        const TENANT: &str = "rls-override-live";
        const COMPONENT: &str = "rls-override-live-component";
        const U1: &str = "11111111-1111-4111-8111-111111111111";
        const U2: &str = "22222222-2222-4222-8222-222222222222";

        let Some(admin_url) = test_pg_url() else {
            return;
        };
        let suffix = std::process::id();
        let schema = format!("wamn_rls_override_{suffix}");
        let probe = format!("wamn_rls_overrider_{suffix}");
        let admin = connect_raw(&admin_url).await;
        admin
            .batch_execute(&rls_fixture_sql(&schema, &probe, TENANT, U1, U2))
            .await
            .expect("seed the per-user RLS fixture as the superuser owner");

        let pg = WamnPostgres::new(WamnPostgresConfig {
            database_url: Some(database_url_for_role(&admin_url, &probe, &probe)),
            guest_pool_max_size: 2,
            platform_pool_max_size: 2,
            wait_timeout_ms: 2_000,
            statement_timeout_ms: 5_000,
            row_limit: 1_000,
        })
        .unwrap();
        pg.set_tenant(COMPONENT, TENANT).unwrap();
        pg.set_schema(COMPONENT, &schema).unwrap();
        pg.set_role(COMPONENT, "inspector").unwrap();
        pg.set_user_id(COMPONENT, U1).unwrap();

        // CONTROL: inside ONE plugin-managed transaction the injected claims admit
        // the caller's own row — and a bare `SET LOCAL` on that same transaction
        // clears the exempt-role gate and reveals BOTH. So the escalation the
        // guard refuses below is real, not hypothetical.
        let (conn, _pp) = pg.checkout_guest(DEFAULT_PROJECT).await.unwrap();
        pg.begin_with_claims(
            &conn,
            TENANT,
            Some(&schema),
            None,
            Some("inspector"),
            Some(U1),
            None,
            5_000,
        )
        .await
        .unwrap();
        let owned: i64 = conn
            .query_one("SELECT count(*) FROM dispositions", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(
            owned, 1,
            "control: the injected caller owns exactly one row"
        );
        conn.batch_execute("SET LOCAL app.role = 'admin'")
            .await
            .unwrap();
        let escalated: i64 = conn
            .query_one("SELECT count(*) FROM dispositions", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(
            escalated, 2,
            "control: an unguarded SET LOCAL app.role IS an escalation"
        );
        conn.batch_execute("ROLLBACK").await.unwrap();

        // The guest surface refuses exactly that, and every shape of it, before it
        // reaches the server — so the caller still sees only its own row.
        for attempt in [
            "SET app.role = 'admin'",
            "SET LOCAL app.role = 'admin'",
            "RESET app.role",
            &format!("SET app.user_id = '{U2}'"),
            "SELECT set_config('app.role', 'admin', true)",
            &format!("SELECT set_config('app.user_id', '{U2}', true)"),
        ] {
            let refused = pg.one_shot(COMPONENT, attempt, &[], false).await;
            assert!(
                matches!(refused, Err(PgError::QueryError(_))),
                "the guest surface must refuse {attempt:?}"
            );
        }
        assert_eq!(
            visible_rows(&pg, COMPONENT, "SELECT id FROM dispositions ORDER BY id").await,
            1,
            "the caller's claims are unchanged by the refused overrides"
        );

        admin
            .batch_execute(&format!(
                "DROP SCHEMA {schema} CASCADE; DROP OWNED BY {probe}; DROP ROLE {probe};"
            ))
            .await
            .expect("drop the per-user RLS fixture");
    }

    /// wamn-0h0g.17.7 — `ConnectionHttp` freezes its `(tenant, project)` at store
    /// construction and cannot be rebound at checkout, so the registry has to
    /// refuse it when the two disagree.
    ///
    /// Offline on purpose: the check runs before any pool is reached, so a
    /// disagreement is refused with THIS message while agreement falls through
    /// to the (absent) connection. Both halves are asserted, because a guard
    /// that refused everything would pass the first alone.
    #[tokio::test]
    async fn effect_snapshot_refuses_a_tenant_that_disagrees_with_the_bound_claim() {
        const COMPONENT: &str = "warm-instance-0";
        let pg = WamnPostgres::new(WamnPostgresConfig {
            database_url: None,
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 100,
            statement_timeout_ms: 100,
            row_limit: 10,
        })
        .unwrap();
        pg.bind_session_claims(
            COMPONENT,
            &SessionClaims {
                tenant: "tenant-b".to_string(),
                ..SessionClaims::default()
            },
        )
        .expect("the acquiring tenant binds");

        let lookup = ConnectionEffectLookup {
            catalog_id: "catalog",
            catalog_version: 1,
            environment: "dev",
            wiring_id: "wiring",
            wiring_version: 1,
            node_id: "node",
            component_digest: "digest",
            store_alias: "manager",
            candidate_binding: None,
        };

        // A stale ConnectionHttp still carrying the tenant its store was BUILT
        // for is refused before it can read a row.
        let stale = pg
            .connection_effect_snapshot(COMPONENT, DEFAULT_PROJECT, "tenant-a", &lookup)
            .await
            .expect_err("a disagreeing tenant is refused");
        assert!(
            stale.to_string().contains(
                "HTTP effect authorization tenant \"tenant-a\" disagrees with the tenant bound"
            ),
            "the refusal names the divergence rather than any other failure: {stale}"
        );

        // The agreeing tenant gets past the guard and fails only on the absent
        // connection, so the guard is not simply refusing everything.
        let agreeing = pg
            .connection_effect_snapshot(COMPONENT, DEFAULT_PROJECT, "tenant-b", &lookup)
            .await
            .expect_err("an offline plugin has no connection to resolve against");
        assert!(
            !agreeing
                .to_string()
                .contains("disagrees with the tenant bound"),
            "the bound tenant must pass the guard: {agreeing}"
        );
    }

    /// wamn-0h0g.17.11 — the guard requires a bound claim, it does not merely
    /// refuse disagreement.
    ///
    /// Agreement with the registry carries no information when the registry has
    /// no entry, so an unbound claim scope must be refused rather than trusted
    /// with whatever tenant the caller froze. This is the same deny floor
    /// `require_tenant` applies to every other read: an instance that skipped a
    /// bind resolves nothing.
    ///
    /// Offline on purpose: the refusal lands before any pool is reached, and the
    /// message is asserted so a mutant that lets the unbound case fall through
    /// fails on the message rather than passing on the (also absent) connection.
    #[tokio::test]
    async fn effect_snapshot_refuses_a_component_with_no_bound_tenant() {
        const COMPONENT: &str = "never-acquired-instance";
        let pg = WamnPostgres::new(WamnPostgresConfig {
            database_url: None,
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 100,
            statement_timeout_ms: 100,
            row_limit: 10,
        })
        .unwrap();
        assert_eq!(
            pg.session_claims(COMPONENT),
            None,
            "the scope under test resolves no identity at all"
        );

        let lookup = ConnectionEffectLookup {
            catalog_id: "catalog",
            catalog_version: 1,
            environment: "dev",
            wiring_id: "wiring",
            wiring_version: 1,
            node_id: "node",
            component_digest: "digest",
            store_alias: "manager",
            candidate_binding: None,
        };

        let unbound = pg
            .connection_effect_snapshot(COMPONENT, DEFAULT_PROJECT, "tenant-a", &lookup)
            .await
            .expect_err("an unbound claim scope resolves nothing");
        assert!(
            unbound.to_string().contains(
                "HTTP effect authorization for component \"never-acquired-instance\" has no \
                 bound tenant claim"
            ),
            "the refusal names the missing claim rather than any other failure: {unbound}"
        );
    }

    /// The 3.2 tenant floor over rows belonging to TWO tenants, so a claim that
    /// resolved to the wrong tenant returns the wrong rows instead of none.
    fn two_tenant_rls_fixture_sql(schema: &str, probe: &str, a: &str, b: &str) -> String {
        format!(
            "DROP SCHEMA IF EXISTS {schema} CASCADE; \
             DROP ROLE IF EXISTS {probe}; \
             CREATE ROLE {probe} LOGIN PASSWORD '{probe}' NOSUPERUSER NOBYPASSRLS; \
             CREATE SCHEMA {schema}; \
             CREATE TABLE {schema}.dispositions (tenant_id text NOT NULL, id int NOT NULL); \
             ALTER TABLE {schema}.dispositions ENABLE ROW LEVEL SECURITY; \
             CREATE POLICY dispositions_tenant ON {schema}.dispositions \
                 USING (tenant_id = NULLIF(current_setting('app.tenant', true), '')); \
             INSERT INTO {schema}.dispositions \
                 VALUES ('{a}', 1), ('{a}', 2), ('{b}', 3); \
             GRANT USAGE ON SCHEMA {schema} TO {probe}; \
             GRANT SELECT ON {schema}.dispositions TO {probe};"
        )
    }

    /// wamn-0h0g.17.7 — two tenants bound through the acquisition seam,
    /// INTERLEAVED, each seeing only its own rows under real RLS.
    ///
    /// [`WamnPostgres::bind_session_claims`] is exactly what the router driver
    /// calls on the instance serving one invocation, and `app.tenant` is exactly
    /// what the tenant policy gates on, so this is the row-level half of the
    /// seam's acceptance: two such claim sets admit two disjoint row sets on a
    /// server that cannot be talked out of it — the probe role is NOSUPERUSER
    /// NOBYPASSRLS.
    ///
    /// The interleaving is what makes it adversarial: A reads, B reads, A reads
    /// again. An implementation holding one claim per component rather than one
    /// per acquisition passes the first two reads and fails the third.
    #[tokio::test]
    async fn live_interleaved_bound_scopes_each_see_only_their_own_rows() {
        const TENANT_A: &str = "seam-live-a";
        const TENANT_B: &str = "seam-live-b";

        let Some(admin_url) = test_pg_url() else {
            return;
        };
        let suffix = std::process::id();
        let schema = format!("wamn_seam_{suffix}");
        let probe = format!("wamn_seam_probe_{suffix}");
        let admin = connect_raw(&admin_url).await;
        admin
            .batch_execute(&two_tenant_rls_fixture_sql(
                &schema, &probe, TENANT_A, TENANT_B,
            ))
            .await
            .expect("seed the two-tenant RLS fixture as the superuser owner");

        let pg = WamnPostgres::new(WamnPostgresConfig {
            database_url: Some(database_url_for_role(&admin_url, &probe, &probe)),
            guest_pool_max_size: 2,
            platform_pool_max_size: 2,
            wait_timeout_ms: 2_000,
            statement_timeout_ms: 5_000,
            row_limit: 1_000,
        })
        .unwrap();

        // Two warm instances of ONE component digest, acquired by two tenants.
        // The scopes name the INSTANCES; the tenants arrive with the acquisition.
        let scope_a = "warm-instance-0";
        let scope_b = "warm-instance-1";
        let claims = |tenant: &str| SessionClaims {
            tenant: tenant.to_string(),
            schema: Some(schema.clone()),
            ..SessionClaims::default()
        };
        pg.bind_session_claims(scope_a, &claims(TENANT_A))
            .expect("tenant A's acquisition binds");
        pg.bind_session_claims(scope_b, &claims(TENANT_B))
            .expect("tenant B's acquisition binds");

        // CONTROL: the table is reachable on this path at all, so a zero below is
        // a policy denying rather than a broken fixture or search_path.
        assert_eq!(
            visible_rows(&pg, scope_a, "SELECT count(*) FROM dispositions").await,
            1,
            "control: the probe role must reach the fixture table"
        );

        assert_eq!(
            visible_rows(&pg, scope_a, "SELECT id FROM dispositions ORDER BY id").await,
            2,
            "A sees exactly its own two rows"
        );
        assert_eq!(
            visible_rows(&pg, scope_b, "SELECT id FROM dispositions ORDER BY id").await,
            1,
            "B sees exactly its own one row, never A's"
        );
        assert_eq!(
            visible_rows(&pg, scope_a, "SELECT id FROM dispositions ORDER BY id").await,
            2,
            "A's rows are unchanged by B's acquisition of the same digest's pool"
        );

        // Ending A's checkout revokes A's identity and nothing else. The refusal
        // is matched on the NO-TENANT code specifically: a revoke that cleared
        // only the search_path would also make this query fail, for a reason that
        // has nothing to do with isolation.
        pg.revoke_session_claims(scope_a);
        assert_eq!(pg.session_claims(scope_a), None);
        let refused = pg
            .one_shot(scope_a, "SELECT id FROM dispositions", &[], true)
            .await
            .err()
            .expect("an unbound claim scope cannot query at all");
        assert!(
            matches!(&refused, PgError::QueryError((code, _)) if code == "WAMN0"),
            "an instance whose checkout ended must resolve NO tenant, not merely \
             fail for some other reason: {refused:?}"
        );
        assert_eq!(
            visible_rows(&pg, scope_b, "SELECT id FROM dispositions ORDER BY id").await,
            1,
            "revoking A must not disturb B's live identity"
        );

        admin
            .batch_execute(&format!(
                "DROP SCHEMA {schema} CASCADE; DROP OWNED BY {probe}; DROP ROLE {probe};"
            ))
            .await
            .expect("drop the two-tenant RLS fixture");
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
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 2_000,
            statement_timeout_ms: 5_000,
            row_limit: 1_000,
        })
        .unwrap();
        // The checkout builds the pool (with the R18 hook) and creates a physical
        // connection; the hook must pass for this to be Ok.
        let (conn, _pp) = pg
            .checkout_guest(DEFAULT_PROJECT)
            .await
            .expect("checkout ok (scs=on)");
        let scs: String = conn
            .query_one("SHOW standard_conforming_strings", &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(scs, "on");
    }

    #[tokio::test]
    #[ignore = "requires WAMN_POOL_LIFECYCLE_PG_URL for a disposable PostgreSQL database"]
    async fn live_size_one_guest_and_platform_pools_isolate_sessions_under_interleaving() {
        let url = std::env::var("WAMN_POOL_LIFECYCLE_PG_URL")
            .expect("set WAMN_POOL_LIFECYCLE_PG_URL to a disposable PostgreSQL database");
        let postgres = WamnPostgres::new(WamnPostgresConfig {
            database_url: Some(url),
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 250,
            statement_timeout_ms: 1_000,
            row_limit: 10,
        })
        .expect("construct size-one lifecycle pools");

        let (guest, _) = postgres
            .checkout_guest(DEFAULT_PROJECT)
            .await
            .expect("hold the only guest connection");
        let guest_row = guest
            .query_one(
                "SELECT pg_backend_pid(), \
                 set_config('wamn.pool_lifecycle_probe', 'guest', false)",
                &[],
            )
            .await
            .expect("mark guest session");
        let guest_pid = guest_row.get::<_, i32>(0);

        // This checkout happens while the sole guest slot remains held. Sharing
        // either pool/cache makes it hit the 250 ms wait bound and fail here.
        let (platform, _) = postgres
            .checkout_platform(DEFAULT_PROJECT)
            .await
            .expect("platform headroom remains available while guest is saturated");
        let platform_row = platform
            .query_one(
                "SELECT pg_backend_pid(), \
                 current_setting('wamn.pool_lifecycle_probe', true)",
                &[],
            )
            .await
            .expect("read untouched platform session");
        let platform_pid = platform_row.get::<_, i32>(0);
        let platform_marker = platform_row.get::<_, Option<String>>(1);
        assert_ne!(guest_pid, platform_pid);
        assert!(platform_marker.is_none());
        platform
            .query_one(
                "SELECT set_config('wamn.pool_lifecycle_probe', 'platform', false)",
                &[],
            )
            .await
            .expect("mark platform session");
        drop(platform);
        drop(guest);

        let (guest_again, _) = postgres
            .checkout_guest(DEFAULT_PROJECT)
            .await
            .expect("reacquire guest lifecycle");
        let guest_again_row = guest_again
            .query_one(
                "SELECT pg_backend_pid(), \
                 current_setting('wamn.pool_lifecycle_probe', true)",
                &[],
            )
            .await
            .expect("read guest session after repool");
        assert_eq!(guest_again_row.get::<_, i32>(0), guest_pid);
        assert_eq!(
            guest_again_row.get::<_, Option<String>>(1).as_deref(),
            Some("guest")
        );

        let (platform_again, _) = postgres
            .checkout_platform(DEFAULT_PROJECT)
            .await
            .expect("reacquire platform lifecycle");
        let platform_again_row = platform_again
            .query_one(
                "SELECT pg_backend_pid(), \
                 current_setting('wamn.pool_lifecycle_probe', true)",
                &[],
            )
            .await
            .expect("read platform session after repool");
        assert_eq!(platform_again_row.get::<_, i32>(0), platform_pid);
        assert_eq!(
            platform_again_row.get::<_, Option<String>>(1).as_deref(),
            Some("platform")
        );
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
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 2_000,
            statement_timeout_ms: 5_000,
            row_limit: 1_000,
        })
        .unwrap();
        // (`matches!`, not `expect_err`, so the Ok type need not be `Debug`.)
        let result = pg.checkout_guest(DEFAULT_PROJECT).await;
        assert!(
            matches!(result, Err(PgError::ConnectionUnavailable)),
            "scs=off must fail CLOSED as the guest-visible connection-unavailable \
             variant — a checkout that succeeded means the hook did not reject"
        );

        // Hook-SPECIFICITY: reach the raw pool error (which checkout collapses to
        // connection-unavailable) and confirm it is the R18 post_create hook —
        // not an auth/other failure that ALSO maps to connection-unavailable. The
        // control above already ruled out server-down; this pins the cause.
        // Only `PoolLifecycle::Platform` wires the async-message sender through
        // `PlatformConnect`; a guest pool never sends on it.
        let (platform_messages, _platform_message_receiver) =
            tokio::sync::mpsc::unbounded_channel();
        let pool = WamnPostgres::build_pool(
            &ProjectConfig {
                database_url: url,
                guest_pool_max_size: 1,
                platform_pool_max_size: 1,
                wait_timeout_ms: 2_000,
                statement_timeout_ms: 5_000,
                row_limit: 1_000,
            },
            PoolLifecycle::Guest,
            &platform_messages,
        )
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
