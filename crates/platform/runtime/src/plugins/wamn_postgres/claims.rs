//! The claim boundary as ONE reviewable security unit (SR4, wamn-cjv.18): the
//! `WamnPostgres` plugin state (the correlated claim maps), the identity-format
//! validators it imports, the in-band claim/causation-mutation guard, and the
//! `set_config()`-bound claim injection (`begin_with_claims`). This is the exact
//! surface the injection review (R2/R16/R16b/cjv.2/l5i9.12.2) reasons about.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use anyhow::Context as _;
use serde::Deserialize;
use tokio_postgres::types::ToSql;
use tracing::Instrument as _;

use wamn_catalog::ManifestDigest;
use wamn_control_registry::identifiers::{valid_project, valid_runner, valid_schema, valid_tenant};
use wamn_event_wire::Causation;
use wamn_run_state::AuthorityClass;

use super::pool::{
    ClassCredentials, CredentialProvider, PoolKey, ProjectConfig, ProjectPool,
    StaticCredentialProvider, WamnPostgresConfig,
};
use super::statements::StatementScopes;
use super::{DEFAULT_PROJECT, PgError, RowSet};

mod pools;
mod transactions;

const OPERATION_PERMISSIONS_SQL: &str = "SELECT permission \
    FROM app_system.permissions \
    WHERE tenant_id = $1 AND role_name = $2 \
    ORDER BY permission";

const SESSION_OPERATION_PERMISSIONS_SQL: &str = "SELECT DISTINCT permission \
    FROM app_system.permissions \
    WHERE tenant_id = $1 AND role_name = ANY($2::text[]) \
    ORDER BY permission";

const USER_OPERATION_PERMISSIONS_SQL: &str = "SELECT DISTINCT permissions.permission \
    FROM app_system.users AS users \
    JOIN app_system.user_roles AS user_roles \
      ON user_roles.tenant_id = users.tenant_id AND user_roles.user_id = users.id \
    JOIN app_system.permissions AS permissions \
      ON permissions.tenant_id = user_roles.tenant_id \
      AND permissions.role_name = user_roles.role_name \
    WHERE users.tenant_id = $1 AND users.id = $2::text::uuid AND users.status = 'active' \
    ORDER BY permissions.permission";

pub struct WamnPostgres {
    /// Resolves a project id → its database connection + policy.
    provider: Arc<dyn CredentialProvider>,
    /// Guest-visible project pools, built lazily and never shared with host-owned
    /// claim or authorization work.
    guest_pools: std::sync::RwLock<HashMap<PoolKey, Arc<ProjectPool>>>,
    /// Host-owned claim, authorization, and plan-supply project pools. Keeping a
    /// distinct cache makes cross-lifecycle session reuse unrepresentable.
    platform_pools: std::sync::RwLock<HashMap<PoolKey, Arc<ProjectPool>>>,
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
    /// `SET LOCAL app.runner` so a runner replica reads its owner identity to
    /// claim/renew queue rows under.
    runners: std::sync::RwLock<HashMap<String, String>>,
    /// component id → the one host-owned workload authority admitted on the
    /// guest-visible WIT surface. Absence preserves ordinary `GuestSql`.
    /// Values enter only from the workload binding config; the guest cannot
    /// select or mutate this discriminator.
    workload_authorities: std::sync::RwLock<HashMap<String, AuthorityClass>>,
    /// component id → the caller's `app.role` claim (a `roles.name`). Absent
    /// (the default) binds the empty role, which is the deny floor every
    /// compiled role gate coalesces to. When set, a per-role RLS policy using
    /// the claim contract documented by `deploy/sql/app-schema.sql` gates on
    /// the caller's role instead of denying.
    roles: std::sync::RwLock<HashMap<String, String>>,
    /// component id → the caller's `app.user_id` claim (a `users.id` uuid).
    /// Absent (the default) binds the empty string, which the compiled
    /// ownership predicate `NULLIF(…, '')::uuid` turns into NULL → deny. When
    /// set, a per-user RLS policy compares against the caller's own id.
    users: std::sync::RwLock<HashMap<String, String>>,
    /// component id → the `(effective release id, manifest digest)` this pod carries.
    /// Absent (the default) ⇒ the production claim records nothing, so every
    /// path that never mounted a release identity is byte-unchanged. When set,
    /// the claim writes the pair onto the run it leases, write-once.
    release_identities: std::sync::RwLock<HashMap<String, ReleaseIdentity>>,
    /// component id → the causation context {run, root, depth} of the run the
    /// caller is currently driving (l5i9.12.2). Declared through
    /// [`set_current_run`](WamnPostgres::set_current_run), cleared (removed)
    /// between runs. Absent (the default) ⇒ no causation is stamped — so every
    /// non-run path (S2..S6, the gateway, benches without a declaration) is
    /// byte-unchanged. When set, [`begin_with_claims`] appends a
    /// TRANSACTIONAL `wamn.causation` logical message to every transaction the
    /// plugin opens for that component, which the CDC reader (l5i9.12.1)
    /// stitches onto the txn's row events.
    current_run: std::sync::RwLock<HashMap<String, Causation>>,
    /// Verified SQL facts bound by operation plus the one invocation-active
    /// scope. The active scope is host-selected; a guest can only name a digest
    /// inside it.
    pub(super) statement_scopes: std::sync::RwLock<StatementScopes>,
    /// Connections destroyed instead of repooled (chaos-gate observability).
    pub(super) destroyed: Arc<AtomicU64>,
    /// How many times each half of the un-fused workload bind has run.
    pub(super) bind_counters: super::BindCounters,
}

/// The release a pod carries — the `(effective release id, manifest digest)` pair
/// derived from the verified content of its mounted serving manifest
/// ([`LoadedRelease`](crate::release_manifest::LoadedRelease)).
///
/// Admission pins the effective release. The production claim verifies that
/// pin and records the claiming pod's manifest digest. Both values are
/// host-injected identity, never guest-supplied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseIdentity {
    /// The release identity — `runs.effective_release_id`.
    pub effective_release_id: i32,
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
    /// The `(effective release id, manifest digest)` the claiming pod carries.
    pub release: Option<ReleaseIdentity>,
}

/// Host-only identity used to load one HTTP effect authorization snapshot.
#[derive(Debug, Clone, Copy)]
pub struct ConnectionEffectLookup<'a> {
    pub wiring_package_id: &'a str,
    pub origin_package_id: &'a str,
    pub origin_component_digest: &'a str,
    pub origin_component: &'a str,
    pub origin_interface_version: &'a str,
    pub origin_operation: &'a str,
    pub package_id: &'a str,
    pub operation: &'a str,
    pub effective_release_id: i32,
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
    pub operation: Option<String>,
    pub registered_operation: Option<String>,
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

/// Resolve the original wiring node and the executing component's connection.
///
/// No run, plan, frame, or effect-ledger row participates. The selected wiring
/// version is immutable and stays valid for the lifetime of the delivery even
/// if the environment's hot pointer flips concurrently. The mounted release is
/// checked separately by `ConnectionHttp`, because its canonical bytes are not
/// a database relation and must not be projected back into Postgres.
static CONNECTION_EFFECT_SNAPSHOT_SQL: &str = "\
WITH member AS MATERIALIZED ( \
    SELECT member.tenant_id, member.package_id, member.package_version \
      FROM catalog.effective_release_packages AS member \
     WHERE member.tenant_id = $1 \
       AND member.package_id = $12 \
       AND member.effective_release_id = $3 \
), selected_wiring AS MATERIALIZED ( \
    SELECT wiring.wiring_hash, wiring.graph_json, \
           member.tenant_id, member.package_id, member.package_version \
      FROM member \
      JOIN catalog.wirings AS wiring \
        ON wiring.tenant_id = member.tenant_id \
       AND wiring.package_id = member.package_id \
       AND wiring.package_version = member.package_version \
     WHERE wiring.wiring_id = $5 \
       AND wiring.version = $6 \
       AND wiring.graph_json ->> 'wiring-id' = $5 \
       AND wiring.graph_json ->> 'version' = $6::text \
) \
SELECT wiring.wiring_hash, component.component, component.interface_version, \
       $18::text, \
       component.operations #>> ARRAY[$18, 'registered-operation'], \
       requirement.requirement_json::text, requirement.requirement_hash, \
       COALESCE( \
           node.value IS NOT NULL \
           AND node.value ->> 'component' = origin_component.component \
           AND node.value ->> 'interface-version' = origin_component.interface_version \
           AND node.value ->> 'operation' = $17 \
           AND origin_component.operations ? $17 \
           AND component.operations ? $18, \
           false \
       ), \
       binding.binding_status = 'active', binding.validation_status = 'valid', \
       instance.instance_id, binding.validation_hash, \
       instance.requirement_type, instance.contract, \
       instance.lifecycle_status = 'enabled', instance.active_generation, instance.revision, \
       generation.generation, generation.definition_json::text, generation.definition_hash, \
       generation.credential_set_handle \
  FROM selected_wiring AS wiring \
  LEFT JOIN catalog.effective_release_packages AS origin_member \
    ON origin_member.tenant_id = wiring.tenant_id \
   AND origin_member.effective_release_id = $3 \
   AND origin_member.package_id = $13 \
  LEFT JOIN catalog.component_library AS origin_component \
    ON origin_component.tenant_id = origin_member.tenant_id \
   AND origin_component.package_id = origin_member.package_id \
   AND origin_component.package_version = origin_member.package_version \
   AND origin_component.component_digest = $14 \
   AND origin_component.component = $15 \
   AND origin_component.interface_version = $16 \
  LEFT JOIN catalog.effective_release_packages AS executing_member \
    ON executing_member.tenant_id = wiring.tenant_id \
   AND executing_member.effective_release_id = $3 \
   AND executing_member.package_id = $2 \
  LEFT JOIN catalog.component_library AS component \
    ON component.tenant_id = executing_member.tenant_id \
   AND component.package_id = executing_member.package_id \
   AND component.package_version = executing_member.package_version \
   AND component.component_digest = $8 \
  LEFT JOIN LATERAL ( \
      SELECT wiring.graph_json #> ARRAY['nodes', $7] AS value \
  ) AS node ON true \
  LEFT JOIN catalog.connection_requirements AS requirement \
    ON requirement.tenant_id = $1 \
   AND requirement.component_digest = $8 \
   AND requirement.store_alias = $9 \
  LEFT JOIN catalog.connection_bindings AS binding \
    ON binding.tenant_id = $1 \
   AND binding.effective_release_id = $3 \
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
/// rewrite a host-injected claim (or switch roles) and defeat RLS isolation
/// (wamn-cjv.2 / review C4-1). The extended-query protocol forbids statement
/// chaining, so a claim override can only arrive as a *standalone* `SET` /
/// `RESET` / `set_config(…)` statement — which this catches.
///
/// # The TENANT axis is structurally closed, and this is no longer what holds it
///
/// The `wamn-0h0g.22.6` lineage re-keyed guest tenant authority onto a
/// NON-SETTABLE identity: `.22.6.2` re-keyed the generated tenant floor onto
/// `current_user`, `.22.6.4` minted per-tenant guest LOGIN generations, and
/// `.22.6.7` cut the guest SQL path onto per-tenant connections.
/// `wamn_authority.current_tenant_key()` reads the CONNECTED ROLE, which no
/// session can rewrite, so a guest that succeeds in setting `app.tenant`
/// changes nothing. Measured on PostgreSQL 18 as a guest generation login
/// asserted NOT (`rolsuper` OR `rolbypassrls`): plain `SET`, `set_config`, `SET
/// LOCAL` and `DO` + `EXECUTE` all SUCCEEDED and all left the visible row set
/// unchanged.
///
/// # What remains is a live MECHANISM, not a stale citation
///
/// This is still a defense-in-depth **blocklist**:
/// [`statement_mutates_session`] matches only a leading `set`/`reset` keyword
/// or the literal `set_config`, so a `DO` block whose `EXECUTE` string carries
/// `SET app.role` passes it untouched. The guard therefore applies to reachable
/// guest APIs bearing RLS policies that read `app.role` or `app.user_id`; none
/// exists in the Receiving slice. Its host-only operation-permission read binds
/// predicates directly and installs neither caller-derived claim
/// (`wamn-10yt.3.2`), so it does not arm this escape. `wamn-0h0g.22.23` (OPEN)
/// owns the matcher defect and carries the trigger for the first such reachable
/// API — do not close this comment against it.
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

/// Reject a malformed claim identity before it is bound (R16). Since R2 these
/// validators are NO LONGER the injection boundary — every claim value binds as a
/// parameter into [`transactions::CLAIM_SQL`], so a `'`/`;`/`--` value is inert data — but a
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

fn bind_composed_project(
    mut projects: HashMap<String, ProjectConfig>,
    project: &str,
    credentials: Option<ClassCredentials>,
    cfg: &WamnPostgresConfig,
) -> anyhow::Result<HashMap<String, ProjectConfig>> {
    if let Some(credentials) = credentials {
        anyhow::ensure!(
            !projects.contains_key(project),
            "project {project:?} has both an explicit composition credential and a WAMN_PG_PROJECTS_FILE entry"
        );
        projects.insert(
            project.to_owned(),
            ProjectConfig::from_global(credentials, cfg),
        );
    }
    Ok(projects)
}

impl WamnPostgres {
    /// Plugin over a single default database (the [`WamnPostgresConfig`]
    /// credentials). Pools are built lazily; `credentials: None` ⇒ every call
    /// returns `connection-unavailable`.
    pub fn new(cfg: WamnPostgresConfig) -> anyhow::Result<Self> {
        let default = cfg
            .credentials
            .clone()
            .map(|credentials| ProjectConfig::from_global(credentials, &cfg));
        Ok(Self::with_provider(Arc::new(
            StaticCredentialProvider::default_only(default),
        )))
    }

    /// Plugin over an explicit [`CredentialProvider`] (multi-project / tests).
    pub fn with_provider(provider: Arc<dyn CredentialProvider>) -> Self {
        Self {
            provider,
            guest_pools: std::sync::RwLock::new(HashMap::new()),
            platform_pools: std::sync::RwLock::new(HashMap::new()),
            tenants: std::sync::RwLock::new(HashMap::new()),
            projects: std::sync::RwLock::new(HashMap::new()),
            schemas: std::sync::RwLock::new(HashMap::new()),
            runners: std::sync::RwLock::new(HashMap::new()),
            workload_authorities: std::sync::RwLock::new(HashMap::new()),
            roles: std::sync::RwLock::new(HashMap::new()),
            users: std::sync::RwLock::new(HashMap::new()),
            release_identities: std::sync::RwLock::new(HashMap::new()),
            current_run: std::sync::RwLock::new(HashMap::new()),
            statement_scopes: std::sync::RwLock::new(StatementScopes::default()),
            destroyed: Arc::new(AtomicU64::new(0)),
            bind_counters: super::BindCounters::default(),
        }
    }

    /// Build from the deployment's configuration: the default project from the
    /// credential the COMPOSITION ROOT names, plus explicit projects listed in
    /// `WAMN_PG_PROJECTS_FILE` JSON.
    ///
    /// # Why the caller passes the credential (`wamn-0h0g.22.8.3`)
    ///
    /// `wamn-0h0g.22.8.2` removed the ambient `WAMN_PG_URL` read from
    /// [`WamnPostgresConfig::from_env`], because a credential picked up
    /// implicitly there made the runtime a SECOND credential source competing
    /// with whatever a caller supplied — the conflict
    /// [`AmbientCredentialState`](super::credential_exactness::AmbientCredentialState)
    /// already declares.
    ///
    /// The environment is still the TRANSPORT; that is how Kubernetes injects a
    /// Secret, and `deploy/platform` does exactly that via `secretKeyRef`. What
    /// changed is WHERE it is read: once, at trusted composition, where it is
    /// the named explicit source rather than a silent fallback buried in the
    /// config layer. `services/executor` already composed this way; this is the
    /// host taking the same shape.
    ///
    /// `wamn-0h0g.22.16`: the parameter is the caller's PER-CLASS credential
    /// set, not one url, so the composition root states which authority each
    /// login belongs to instead of leaving one login to serve every authority
    /// implicitly.
    pub fn from_env(credentials: Option<ClassCredentials>) -> anyhow::Result<Self> {
        let cfg = WamnPostgresConfig::from_env();
        let default = credentials.map(|credentials| ProjectConfig::from_global(credentials, &cfg));
        let projects = Self::configured_projects(&cfg)?;
        Ok(Self::with_provider(Arc::new(
            StaticCredentialProvider::new(projects, default),
        )))
    }

    /// Build from the deployment's configuration with the composition root's
    /// credential bound to its declared project rather than the default key.
    ///
    /// A per-project serving host already carries the trusted project identity.
    /// Registering that host's Secret under `default` makes every named-project
    /// lookup refuse despite having the exact credential. A mounted projects
    /// file may still supply other projects, but it cannot name this project a
    /// second time.
    pub fn from_env_for_project(
        project: &str,
        credentials: Option<ClassCredentials>,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            valid_project(project),
            "invalid composed project {project:?}: 1-64 chars of [A-Za-z0-9_-] required"
        );
        let cfg = WamnPostgresConfig::from_env();
        let projects =
            bind_composed_project(Self::configured_projects(&cfg)?, project, credentials, &cfg)?;
        Ok(Self::with_provider(Arc::new(
            StaticCredentialProvider::new(projects, None),
        )))
    }

    fn configured_projects(
        cfg: &WamnPostgresConfig,
    ) -> anyhow::Result<HashMap<String, ProjectConfig>> {
        let Ok(path) = std::env::var("WAMN_PG_PROJECTS_FILE") else {
            return Ok(HashMap::new());
        };
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("read WAMN_PG_PROJECTS_FILE {path}"))?;
        StaticCredentialProvider::projects_from_json(&text, cfg)
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
    /// `SET LOCAL app.runner`, so a runner replica reads a stable owner to
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

    /// Bind the sole elevated authority admitted by the closed workload-config
    /// vocabulary. Ordinary components never call this and remain `GuestSql`.
    pub(super) fn bind_workload_authority(
        &self,
        component_id: &str,
        value: &str,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            value == super::EVENT_MATERIALIZER_AUTHORITY_CONFIG_VALUE,
            "invalid {} value {value:?}: the sole admitted explicit value is {:?}",
            super::AUTHORITY_CONFIG_KEY,
            super::EVENT_MATERIALIZER_AUTHORITY_CONFIG_VALUE,
        );
        self.workload_authorities
            .write()
            .expect("workload authorities lock poisoned")
            .insert(component_id.to_string(), AuthorityClass::EventMaterializer);
        Ok(())
    }

    pub(super) fn workload_authority_for(&self, component_id: &str) -> AuthorityClass {
        self.workload_authorities
            .read()
            .expect("workload authorities lock poisoned")
            .get(component_id)
            .copied()
            .unwrap_or(AuthorityClass::GuestSql)
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
    /// claim verifies its effective release against every run it leases and
    /// records the manifest digest write-once. The bench harness and live tests
    /// call this directly; the host path feeds it from the loaded
    /// [`LoadedRelease`](crate::release_manifest::LoadedRelease),
    /// whose pair is derived from verified manifest content. Absent leaves the
    /// claim recording nothing.
    ///
    /// The digest arrives as [`ManifestDigest`], so its shape is already validated;
    /// only the integer release identity still needs a check.
    ///
    /// # Why effect authority needs no equality check against this record
    ///
    /// The pair comes from the same loaded object every reader resolves against,
    /// so the digest recorded on a run IS the digest of the manifest the recording
    /// pod loaded — structurally, not because anything compares them (owner ruling
    /// `wamn-0h0g.15.102`, after `wamn-0h0g.15.103` struck the asserted carrier).
    /// The admission-pinned effective release and write-once digest are what
    /// make the host-side closure check honest without a second identity
    /// carrier.
    pub fn set_release_identity(
        &self,
        component_id: &str,
        effective_release_id: i32,
        manifest_digest: ManifestDigest,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            effective_release_id > 0,
            "invalid effective release id {effective_release_id}: a positive value is required"
        );
        self.release_identities
            .write()
            .expect("release identities lock poisoned")
            .insert(
                component_id.to_string(),
                ReleaseIdentity {
                    effective_release_id,
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
    /// component is driving (l5i9.12.2). While set, every transaction the plugin
    /// opens for the component carries a `wamn.causation` message. Host-injected
    /// like the other claim registries — callers set it directly, exactly like
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
        // An acquisition starts with no statement authority. The router
        // activates the selected operation only after claims bind.
        self.revoke_statement_operation(component_id);
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
                release.effective_release_id,
                release.manifest_digest.clone(),
            )?,
            None => drop(
                self.release_identities
                    .write()
                    .expect("release identities lock poisoned")
                    .remove(component_id),
            ),
        }
        // Causation is RUN-scoped, declared through `set_current_run` once the
        // run is in hand. A binding acquisition has no run yet, so it starts
        // undeclared — carrying the previous acquisition's run context forward
        // would misattribute its CDC stitch.
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
    /// lets a test assert that two concurrent acquisitions never share one
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
        self.revoke_statement_operation(component_id);
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

    /// Reap EVERY per-component-id claim, workload-authority, and verified
    /// statement registry this plugin keeps for a workload
    /// on teardown (R31): tenant, project, search_path schema, runner lease-owner,
    /// the caller's role / user id, the carried release identity, and the
    /// causation run context — all set at
    /// workload bind (or via the runner channel) and keyed by component id.
    /// Without this a stale claim
    /// survives unbind, the maps grow across workload churn, and a rebound
    /// component id inherits the prior claim. The lifecycle pool maps are
    /// deliberately NOT touched: they are keyed by PROJECT (shared and memoized
    /// within each lifecycle for the plugin's lifetime), not by component id.
    /// Keyed like the runtime's builtin postgres plugin — a workload's component ids
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
        self.workload_authorities
            .write()
            .expect("workload authorities lock poisoned")
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
        self.clear_statement_bindings(workload_id);
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
    /// wash-runtime, so nothing outside it can swap the plugin — while every other
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
            .checkout_platform(project, AuthorityClass::CallableHttp)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if let Err(error) = self
            .begin_with_claims(
                &conn,
                AuthorityClass::CallableHttp,
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
            let params: [&(dyn ToSql + Sync); 18] = [
                &tenant,
                &lookup.package_id,
                &lookup.effective_release_id,
                &lookup.environment,
                &lookup.wiring_id,
                &lookup.wiring_version,
                &lookup.node_id,
                &lookup.component_digest,
                &lookup.store_alias,
                &candidate_instance,
                &candidate_generation,
                &lookup.wiring_package_id,
                &lookup.origin_package_id,
                &lookup.origin_component_digest,
                &lookup.origin_component,
                &lookup.origin_interface_version,
                &lookup.origin_operation,
                &lookup.operation,
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
                operation: row.try_get(3)?,
                registered_operation: row.try_get(4)?,
                requirement_json: json(5)?,
                requirement_hash: row.try_get(6)?,
                node_permitted: row.try_get::<_, Option<bool>>(7)?.unwrap_or(false),
                binding_active: row.try_get::<_, Option<bool>>(8)?.unwrap_or(false),
                binding_valid: row.try_get::<_, Option<bool>>(9)?.unwrap_or(false),
                instance_id: row.try_get(10)?,
                validation_hash: row.try_get(11)?,
                requirement_type: row.try_get(12)?,
                contract: row.try_get(13)?,
                instance_enabled: row.try_get::<_, Option<bool>>(14)?.unwrap_or(false),
                active_generation: row.try_get(15)?,
                instance_revision: row.try_get(16)?,
                generation: row.try_get(17)?,
                definition: json(18)?,
                definition_hash: row.try_get(19)?,
                credential_handle: row.try_get(20)?,
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

    /// Load the exact registered-operation tokens granted to one application role.
    ///
    /// This is host-only authorization work. It reuses the callable-HTTP
    /// platform pool, selects the tenant and role through bound predicates, and
    /// installs no `app.role` or `app.user_id` session claim. The returned set is
    /// attached to the originating caller once and compared at every registered
    /// invocation, including nested router steps.
    pub async fn operation_permissions(
        &self,
        project: &str,
        tenant: &str,
        role: &str,
    ) -> anyhow::Result<BTreeSet<String>> {
        anyhow::ensure!(
            valid_project(project),
            "invalid operation-permission project"
        );
        anyhow::ensure!(valid_tenant(tenant), "invalid operation-permission tenant");
        anyhow::ensure!(!role.is_empty(), "operation-permission role is empty");
        let (connection, _policy) = self
            .checkout_platform(project, AuthorityClass::CallableHttp)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        // ONE ROUND TRIP, AUTOCOMMIT. This read installs no session claim -- see
        // the contract above -- so it is exactly the shape 3c proved needs no
        // transaction, and the BEGIN/COMMIT around it were ceremony: 0.662 ms of
        // every authenticated request, against a 0.740 ms read. The
        // statement_timeout it used to SET per request is pool-uniform and now
        // rides the pool's post_create hook. Measured in
        // evidence/perf/2026.09/2a-auth-instrument.md.
        //
        // prepare_cached, not a bare &str: deadpool caches the parse per
        // connection, so the Parse round trip is paid once per connection rather
        // than once per request.
        let statement = match connection.prepare_cached(OPERATION_PERMISSIONS_SQL).await {
            Ok(statement) => statement,
            Err(error) => {
                self.destroy(connection);
                return Err(error).context("prepare registered-operation permissions");
            }
        };
        let rows = connection
            .query(&statement, &[&tenant, &role])
            .instrument(tracing::info_span!("wamn.auth.perm.query"))
            .await
            .context("read registered-operation permissions")?;
        rows.into_iter()
            .map(|row| {
                row.try_get::<_, String>(0)
                    .context("decode registered-operation permission")
            })
            .collect()
    }

    /// Read the fresh permission union for roles from a verified session.
    ///
    /// This single tenant read uses the existing callable-HTTP authority.
    /// Unknown roles contribute nothing. Identity and role assignments retain
    /// the signed snapshot's lifetime; this query never refreshes that snapshot.
    pub async fn session_operation_permissions(
        &self,
        project: &str,
        tenant: &str,
        roles: &[String],
    ) -> anyhow::Result<BTreeSet<String>> {
        anyhow::ensure!(
            valid_project(project),
            "invalid operation-permission project"
        );
        anyhow::ensure!(valid_tenant(tenant), "invalid operation-permission tenant");
        let (connection, _policy) = self
            .checkout_platform(project, AuthorityClass::CallableHttp)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let statement = match connection
            .prepare_cached(SESSION_OPERATION_PERMISSIONS_SQL)
            .await
        {
            Ok(statement) => statement,
            Err(error) => {
                self.destroy(connection);
                return Err(error).context("prepare session operation permissions");
            }
        };
        let rows = connection
            .query(&statement, &[&tenant, &roles])
            .instrument(tracing::info_span!("wamn.auth.perm.query"))
            .await
            .context("read session operation permissions")?;
        rows.into_iter()
            .map(|row| {
                row.try_get::<_, String>(0)
                    .context("decode session operation permission")
            })
            .collect()
    }

    /// Read the current permissions of an org-issued user in this tenant.
    ///
    /// Route authentication first requires membership in the release environment.
    /// This single tenant read uses the callable-HTTP authority, with no session claims.
    pub async fn user_operation_permissions(
        &self,
        project: &str,
        tenant: &str,
        principal_id: &wamn_platform_identity::PrincipalId,
    ) -> anyhow::Result<BTreeSet<String>> {
        anyhow::ensure!(
            valid_project(project),
            "invalid operation-permission project"
        );
        anyhow::ensure!(valid_tenant(tenant), "invalid operation-permission tenant");
        let (connection, _policy) = self
            .checkout_platform(project, AuthorityClass::CallableHttp)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let statement = match connection
            .prepare_cached(USER_OPERATION_PERMISSIONS_SQL)
            .await
        {
            Ok(statement) => statement,
            Err(error) => {
                self.destroy(connection);
                return Err(error).context("prepare user operation permissions");
            }
        };
        let rows = connection
            .query(&statement, &[&tenant, &principal_id.as_str()])
            .instrument(tracing::info_span!("wamn.auth.perm.query"))
            .await
            .context("read user operation permissions")?;
        rows.into_iter()
            .map(|row| {
                row.try_get::<_, String>(0)
                    .context("decode user operation permission")
            })
            .collect()
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
}

pub(super) enum OneShotResult {
    Rows(RowSet),
    Count(u64),
}

#[cfg(test)]
mod tests;
