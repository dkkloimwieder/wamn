//! The claim boundary as ONE reviewable security unit (SR4, wamn-cjv.18): the
//! `WamnPostgres` plugin state (the correlated claim maps), the identity-format
//! validators it imports, the in-band claim/causation-mutation guard, and the
//! `set_config()`-bound claim injection (`begin_with_claims`). This is the exact
//! surface the injection review (R2/R16/R16b/cjv.2/l5i9.12.2) reasons about.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context as _;
use deadpool_postgres::{Manager, ManagerConfig, Object, Pool, RecyclingMethod, Runtime, Timeouts};
use serde::Deserialize;
use tokio_postgres::NoTls;
use tokio_postgres::types::ToSql;
use tracing::Instrument as _;

use wamn_catalog::ManifestDigest;
use wamn_control_registry::identifiers::{valid_project, valid_runner, valid_schema, valid_tenant};
use wamn_event_wire::Causation;
use wamn_run_state::AuthorityClass;

use super::pool::{
    CheckoutProbe, ClassCredentials, CredentialProvider, PlatformAsyncMessage, PlatformConnect,
    PoolKey, PoolLifecycle, ProjectConfig, ProjectPool, ResolvedCredential,
    StaticCredentialProvider, WamnPostgresConfig, credential_exactness_hook,
    credential_generation_role, destroy_connection, session_statement_timeout_hook,
    standard_conforming_strings_hook,
};
use super::resources::{StatementConnectionGuard, run_execute, run_query, run_verified_query};
use super::statements::{StatementScopes, VerifiedStatement};
use super::types::map_pg_error;
use super::{DEFAULT_PROJECT, PgError, RowSet, SqlValue, StatementError};

const OPERATION_PERMISSIONS_SQL: &str = "SELECT permission \
    FROM app_system.permissions \
    WHERE tenant_id = $1 AND role_name = $2 \
    ORDER BY permission";

pub struct WamnPostgres {
    /// Resolves a project id → its database connection + policy.
    provider: Arc<dyn CredentialProvider>,
    /// Guest-visible project pools, built lazily and never shared with host-owned
    /// claim or authorization work.
    guest_pools: std::sync::RwLock<HashMap<PoolKey, Arc<ProjectPool>>>,
    /// Host-owned claim, authorization, and plan-supply project pools. Keeping a
    /// distinct cache makes cross-lifecycle session reuse unrepresentable.
    platform_pools: std::sync::RwLock<HashMap<PoolKey, Arc<ProjectPool>>>,
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
/// ([`ReleaseManifestWeld`](crate::release_manifest::ReleaseManifestWeld)).
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
    pub package_id: &'a str,
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

/// Resolve one host-attested wiring node and its component-grain connection.
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
       AND member.package_id = $2 \
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
       node.value ->> 'operation', \
       component.operations #>> ARRAY[node.value ->> 'operation', 'registered-operation'], \
       requirement.requirement_json::text, requirement.requirement_hash, \
       COALESCE( \
           node.value IS NOT NULL \
           AND node.value ->> 'component' = component.component \
           AND node.value ->> 'interface-version' = component.interface_version \
           AND component.operations ? (node.value ->> 'operation'), \
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
    ON component.tenant_id = wiring.tenant_id \
   AND component.package_id = wiring.package_id \
   AND component.package_version = wiring.package_version \
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
///   `NULLIF(current_setting('app.user_id', true), '')::uuid` use in the static
///   application-schema RLS contract. Re-asserting whatever the
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

/// The GUEST claim statement: [`CLAIM_SQL`] WITHOUT `app.tenant`
/// (`wamn-0h0g.22.6.7`).
///
/// *** THE GUEST'S TENANT IS ITS LOGIN, NOT A CLAIM. *** Every relation the
/// guest can reach now keys on
/// `wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key()`,
/// which reads `current_user` — so injecting `app.tenant` here would set a GUC
/// that nothing the guest can read consults. Leaving it would not be belt and
/// braces; it would be a second, SETTABLE statement about an authority the
/// session no longer derives that way, and the next person to add a policy
/// would have two boundaries to choose between.
///
/// `app.role` and `app.user_id` STAY. They key the RESTRICTIVE per-role and
/// per-user policies, a different claim class layered INSIDE the tenant floor
/// and explicitly outside `wamn-0h0g.22.6`'s scope.
/// The SESSION-scoped settings an autocommit read still needs.
///
/// `search_path` and `statement_timeout` are not claims -- they are how the
/// connection resolves this package's unqualified relations and how long it may
/// run. A read outside a transaction cannot take them transaction-locally, and
/// without `search_path` every generated statement fails to resolve its own
/// tables. They are POOL-UNIFORM: the pool is keyed by class, project and
/// tenant, so every borrower of this connection wants the same two values.
///
/// `app.role` and `app.user_id` are deliberately ABSENT. Those are per-caller
/// claims, and a session-scoped claim would outlive the request and reach the
/// next borrower of the pooled connection -- the exact leak the claim model
/// exists to prevent. A request carrying either one takes the transactional
/// path instead.
const GUEST_AUTOCOMMIT_SETTINGS_SQL: &str = "SELECT \
     set_config('statement_timeout', $1, false), \
     set_config('search_path', COALESCE($2, current_setting('search_path')), false), \
     set_config('app.runner', COALESCE($3, current_setting('app.runner', true)), false)";

const GUEST_CLAIM_SQL: &str = "SELECT \
     set_config('statement_timeout', $1, true), \
     set_config('search_path', COALESCE($2, current_setting('search_path')), true), \
     set_config('app.runner', COALESCE($3, current_setting('app.runner', true)), true), \
     set_config('app.role', $4, true), \
     set_config('app.user_id', $5, true)";

/// The bound claim statement one authority class binds. ONE function, so the
/// pipelined path's warm-up and the transaction that follows it cannot disagree
/// about which cache entry they mean (wamn-0h0g.17.33).
const fn claim_sql(class: AuthorityClass) -> &'static str {
    match class {
        AuthorityClass::GuestSql => GUEST_CLAIM_SQL,
        _ => CLAIM_SQL,
    }
}

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

    /// Build a deadpool pool for one resolved credential.
    fn build_pool(
        cfg: &ResolvedCredential,
        class: AuthorityClass,
        project: &str,
        platform_messages: &tokio::sync::mpsc::UnboundedSender<PlatformAsyncMessage>,
    ) -> anyhow::Result<Pool> {
        let lifecycle = PoolLifecycle::for_class(class);
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
    fn ensure_pool(
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
        let pp = match Self::build_pool(&cfg, class, project, &self.platform_messages) {
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
            .checkout_platform(project, AuthorityClass::ExecutorPlatform)
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
    /// [`ReleaseManifestWeld`](crate::release_manifest::ReleaseManifestWeld),
    /// whose pair is derived from verified manifest content. Absent leaves the
    /// claim recording nothing.
    ///
    /// The digest arrives as [`ManifestDigest`], so its shape is already proven;
    /// only the integer release identity still needs a check.
    ///
    /// # Why effect authority needs no equality check against this record
    ///
    /// The pair comes from the same welded object every reader resolves against,
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
            let params: [&(dyn ToSql + Sync); 11] = [
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
        // docs/perf/2026.09/2a-auth-instrument.md.
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

    pub(super) fn destroy(&self, obj: Object) {
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
    pub(super) async fn checkout_guest(
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
    pub(super) async fn checkout_workload(
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
    pub(super) async fn checkout_platform(
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
        class: AuthorityClass,
        tenant: &str,
        schema: Option<&str>,
        runner: Option<&str>,
        role: Option<&str>,
        user_id: Option<&str>,
        run: Option<&Causation>,
        statement_timeout_ms: u32,
    ) -> Result<(), PgError> {
        // The tenant is still VALIDATED on the guest path even though it is no
        // longer bound: it selected the credential this connection was checked
        // out with, so a malformed one is a bug worth failing on.
        validate_claims(tenant, schema, runner, role, user_id)?;
        let guest = class == AuthorityClass::GuestSql;
        // A CACHE HIT SENDS NOTHING AND DOES NOT YIELD (deadpool's cell is
        // checked before the init future is ever awaited), so this call is the
        // whole reason the flight below can be interleaved with a caller's own
        // statement. A MISS is a Parse and a full round trip, and whatever is
        // polled during that await reaches the server FIRST -- so a caller that
        // pipelines must run [`WamnPostgres::warm_claim_statement`] before it
        // starts the other half (wamn-0h0g.17.33).
        let stmt = conn
            .prepare_cached(claim_sql(class))
            .await
            .map_err(|e| map_pg_error(&e))?;
        // statement_timeout binds as TEXT (a bare-integer string = ms).
        let timeout = statement_timeout_ms.to_string();
        // An absent role / user id binds the empty claim, not NULL: `''` is the
        // value the compiled policies' COALESCE / NULLIF floors deny on.
        let role = role.unwrap_or_default();
        let user_id = user_id.unwrap_or_default();
        let platform_params: [&(dyn ToSql + Sync); 6] =
            [&tenant, &timeout, &schema, &runner, &role, &user_id];
        let guest_params: [&(dyn ToSql + Sync); 5] = [&timeout, &schema, &runner, &role, &user_id];
        let params: &[&(dyn ToSql + Sync)] = if guest {
            &guest_params
        } else {
            &platform_params
        };
        // l5i9.12.2: stamp the run's causation onto this txn, IN THE BEGIN
        // BATCH. The TRANSACTIONAL emit rides the commit; a rolled-back txn
        // emits nothing and the reader (l5i9.12.1) stitches it onto the txn's
        // row events regardless of frame order. It carries no bind params, so
        // the already-escaped simple-query emit is unchanged by R2.
        //
        // RIDING BEGIN RATHER THAN FOLLOWING IT is what keeps the claim half of
        // a run-owned request to ONE flight: as a separate `batch_execute` it
        // could not be sent until BEGIN's own reply came back, and it would then
        // be the only message this function still had outstanding when a
        // pipelined caller's statement failed -- so an aborted transaction
        // surfaced here as a claim error and misattributed the cause.
        let begin_sql = match run {
            Some(run) => format!("BEGIN;{}", causation_emit_sql(run)),
            None => "BEGIN".to_string(),
        };
        // ONE FLIGHT. `batch_execute` and `execute` each enqueue their whole
        // message batch synchronously on their FIRST poll, and `biased;` pins
        // that poll order, so BEGIN is on the wire ahead of the bound claim
        // statement and tokio-postgres's FIFO ordering does the rest: the txn is
        // open before the transaction-LOCAL `set_config`s run. Nothing here
        // awaits before those two sends, which is the property a pipelining
        // caller depends on.
        //
        // Claim binding is a full server round trip on every request. It sat
        // inside wamn.postgres with no span of its own, so it was
        // indistinguishable from pool checkout and statement time.
        async {
            let (begin, claims) = tokio::join!(
                biased;
                conn.batch_execute(&begin_sql),
                conn.execute(&stmt, params),
            );
            begin.map_err(|e| map_pg_error(&e))?;
            claims.map_err(|e| map_pg_error(&e))?;
            Ok::<(), PgError>(())
        }
        .instrument(tracing::info_span!(
            "wamn.postgres.bind_claims",
            wamn.authority_class = class.as_str(),
        ))
        .await
    }

    /// Parse the bound claim statement on `conn` if this connection has not
    /// parsed it yet, so that the [`begin_with_claims`] that follows reaches its
    /// `BEGIN` inside its own first poll.
    ///
    /// This exists ONLY for the pipelined path (wamn-0h0g.17.33). A caller that
    /// simply awaits `begin_with_claims` does not need it -- the `prepare_cached`
    /// in there is the same call and the same cache entry.
    ///
    /// [`begin_with_claims`]: WamnPostgres::begin_with_claims
    async fn warm_claim_statement(
        &self,
        conn: &Object,
        class: AuthorityClass,
    ) -> Result<(), PgError> {
        conn.prepare_cached(claim_sql(class))
            .await
            .map(drop)
            .map_err(|e| map_pg_error(&e))
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
        let (conn, pp, authority) = self
            .checkout_workload(component_id, project, &tenant)
            .await?;
        if let Err(e) = self
            .begin_with_claims(
                &conn,
                authority,
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

    /// Execute one admitted statement in an implicit claim-aware transaction.
    /// Contract drift rolls the transaction back before any mutation commits.
    pub(super) async fn one_shot_statement(
        &self,
        component_id: &str,
        digest: &str,
        statement: &VerifiedStatement,
        binds: &[SqlValue],
    ) -> Result<RowSet, StatementError> {
        let project = self.project_for(component_id);
        let tenant = self
            .require_tenant(component_id)
            .map_err(StatementError::Postgres)?;
        let schema = self.schema_for(component_id);
        let runner = self.runner_for(component_id);
        let role = self.role_for(component_id);
        let user_id = self.user_id_for(component_id);
        let run = self.current_run_for(component_id);
        let (connection, policy, authority) = self
            .checkout_workload(component_id, &project, &tenant)
            .await
            .map_err(StatementError::Postgres)?;
        let connection = StatementConnectionGuard::new(connection, Arc::clone(&self.destroyed));
        // AUTOCOMMIT WHEN THE SERVER SAYS NO TRANSACTION IS NEEDED. PostgreSQL
        // classified this statement at generation time: it neither writes nor
        // takes a row lock, so BEGIN and COMMIT are ceremony around a read.
        // Measured cost of that ceremony: bind_claims 0.45-0.89 ms plus a
        // COMMIT of 2.1-3.8 ms, around a 0.6 ms statement
        // (docs/perf/2026.09/3b-pipeline.md).
        //
        // A read carrying a per-caller claim keeps the transaction: a
        // session-scoped app.role or app.user_id would outlive the request and
        // reach the next borrower of this pooled connection.
        if !statement.transactional && role.is_none() && user_id.is_none() {
            let timeout = policy.statement_timeout_ms.to_string();
            // The settings must be APPLIED before the statement that depends on
            // search_path. They cannot ride the statement's flight: each side is
            // a prepare followed by an execute, and interleaving two such
            // futures does not order the two EXECUTES -- only the sends. So this
            // is two flights, not one, and it still removes the COMMIT.
            //
            // wamn-0h0g.17.18 moves these to connection setup, where they belong:
            // they are pool-uniform, so paying for them per request is waste.
            async {
                let prepared = connection
                    .connection()
                    .prepare_cached(GUEST_AUTOCOMMIT_SETTINGS_SQL)
                    .await
                    .map_err(|error| map_pg_error(&error))?;
                connection
                    .connection()
                    .execute(
                        &prepared,
                        &[&timeout, &schema.as_deref(), &runner.as_deref()],
                    )
                    .await
                    .map_err(|error| map_pg_error(&error))
            }
            .instrument(tracing::info_span!("wamn.postgres.session_settings"))
            .await
            .map_err(StatementError::Postgres)?;
            let rows = run_verified_query(
                connection.connection(),
                digest,
                statement,
                binds,
                policy.row_limit,
            )
            .await?;
            connection.repool();
            return Ok(rows);
        }

        // THE CLAIMS LAND BEFORE ANYTHING ELSE TOUCHES THE CONNECTION, AND THEY
        // STILL TRAVEL IN THE STATEMENT'S FLIGHT.
        //
        // The claim transaction and the statement are issued without an await
        // between them, on the reasoning that tokio-postgres preserves FIFO
        // order per connection. That reasoning was right about FIFO and wrong
        // about what had been SENT: `begin_with_claims` OPENED by awaiting its
        // own `prepare_cached`, and on a newly created connection that Parse is
        // a full round trip. The statement half, polled during that await, sent
        // its Parse first -- measured in the Receiving journey cluster with
        // log_statement=all and reproducible by restarting the hosts
        // (wamn-0h0g.15.137.15): the guest statement parsed before BEGIN and
        // failed with `relation "purchase_order" does not exist`, carrying
        // neither the `search_path` nor the `app.role` / `app.user_id` the
        // claims install. Awaiting the claims outright fixed the order and cost
        // the round trip the flight saved -- bind_claims at 0.45-0.89 ms plus a
        // wakeup, against a 0.6 ms statement (docs/perf/2026.09/3a-instrument.md).
        //
        // Parsing the claim statement FIRST deletes the await that let it
        // happen, and deletes it for a COLD connection too, which is why this
        // needs no knowledge of what the statement cache holds.
        // `begin_with_claims` then reaches its `BEGIN` inside its own first
        // poll, so `BEGIN` and the bound claim statement are the first two
        // messages this request enqueues whatever this connection has cached. A
        // cold statement's Parse is enqueued behind them and the server, reading
        // its socket in order, runs it INSIDE the transaction with the claims
        // already applied. `biased;` pins that poll order instead of leaving it
        // to `join!`'s rotation (wamn-0h0g.17.33).
        //
        // Proven by `live_a_cold_connection_parses_inside_the_claim_transaction`,
        // which fails against either the pre-fix shape or a swapped `join!`.
        if let Err(error) = self
            .warm_claim_statement(connection.connection(), authority)
            .await
        {
            // Nothing is open yet -- the guard destroys the connection.
            return Err(StatementError::Postgres(error));
        }
        let (claims, result) = tokio::join!(
            biased;
            self.begin_with_claims(
                connection.connection(),
                authority,
                &tenant,
                schema.as_deref(),
                runner.as_deref(),
                role.as_deref(),
                user_id.as_deref(),
                run.as_ref(),
                policy.statement_timeout_ms,
            ),
            run_verified_query(
                connection.connection(),
                digest,
                statement,
                binds,
                policy.row_limit,
            ),
        );
        if let Err(error) = claims {
            // The statement rode the same flight into a transaction that never
            // opened, so its own error is a consequence, not the cause.
            if connection
                .connection()
                .batch_execute("ROLLBACK")
                .await
                .is_err()
            {
                tracing::warn!("rollback after failed claim binding also failed");
            }
            return Err(StatementError::Postgres(error));
        }
        match result {
            // COMMIT is a SECOND full server round trip on every request, and it
            // sat inside wamn.postgres with no span: statement ended at 13.7 ms
            // and the postgres span at 15.9 ms, so 2.1 ms was invisible here.
            Ok(rows) => match async { connection.connection().batch_execute("COMMIT").await }
                .instrument(tracing::info_span!("wamn.postgres.commit"))
                .await
            {
                Ok(()) => {
                    connection.repool();
                    Ok(rows)
                }
                Err(error) => Err(StatementError::Postgres(map_pg_error(&error))),
            },
            Err(error) => {
                if let Err(rollback_error) = connection.connection().batch_execute("ROLLBACK").await
                {
                    tracing::warn!(
                        error = %rollback_error,
                        "rollback after failed verified statement also failed; destroying connection"
                    );
                } else {
                    connection.repool();
                }
                Err(error)
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
    use super::super::statements::{StatementField, StatementValueType};
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

    /// Building a pool must REQUIRE a probeable credential.
    ///
    /// Without this, removing the exactness hook from `build_pool` would be an
    /// inert change: every other test constructs the hook directly, so nothing
    /// would notice the pool no longer carries it. Here the hook's construction
    /// is the only thing that can reject this url, so its absence is visible.
    #[test]
    fn building_a_pool_requires_a_probeable_credential() {
        let (messages, _rx) = tokio::sync::mpsc::unbounded_channel();
        let unprobeable = ResolvedCredential {
            // Parses as a manager config, but names no principal, so the
            // exactness hook cannot be built for it.
            database_url: "postgres://host:5432/db".to_string(),
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 100,
            statement_timeout_ms: 100,
            row_limit: 10,
        };
        assert!(
            WamnPostgres::build_pool(
                &unprobeable,
                AuthorityClass::GuestSql,
                DEFAULT_PROJECT,
                &messages,
            )
            .is_err(),
            "a pool whose credential cannot be probed must not be built"
        );
    }

    /// Guest-sql must never be usable as a platform authority, and the refusal
    /// must survive `--release`, where a `debug_assert` would not exist.
    #[tokio::test]
    async fn a_platform_checkout_refuses_the_guest_authority() {
        let postgres = WamnPostgres::from_env(Some(ClassCredentials::every_class(
            "postgres://wamn_app_refusal_a@localhost/refusal-proof",
        )))
        .expect("compose");
        let refused = postgres
            .checkout_platform(DEFAULT_PROJECT, AuthorityClass::GuestSql)
            .await;
        assert!(
            refused.is_err(),
            "platform work under the guest authority must fail closed"
        );
    }

    /// REGRESSION GUARD (`wamn-0h0g.22.8.3`).
    ///
    /// Removing the ambient read in split B silently cut the HOST's default
    /// project, because `from_env` derived it from the config field that split
    /// had just forced to `None`. Nothing in the sweep exercised that path, so
    /// the only thing standing between that and a deployed host with no
    /// database is this test. It asserts the composed credential actually
    /// reaches resolution, and that composing without one resolves NOTHING
    /// rather than falling back.
    /// A guest credential url for `tenant` in `database`, named the way
    /// provisioning names one: the login carries the tenant key as its scope
    /// digest (`wamn-0h0g.22.6.4`).
    fn guest_url(tenant: &str, database: &str) -> String {
        format!(
            "postgres://wamn_app_{}_a:pw@db/{database}",
            wamn_run_state::app_scope_hash(tenant, database)
        )
    }

    /// *** THE REFUSAL THAT MAKES THE CREDENTIAL THE AUTHORITY. ***
    ///
    /// After `wamn-0h0g.22.6` a guest's tenant is its LOGIN, so handing back a
    /// credential minted for another tenant is not a mis-selection, it is a
    /// cross-tenant read. Resolution refuses instead, and refuses again when the
    /// caller names no tenant at all.
    #[test]
    fn a_guest_credential_resolves_for_its_own_tenant_and_no_other() {
        let host = WamnPostgres::from_env(Some(ClassCredentials::every_class(guest_url(
            "acme",
            "host-default",
        ))))
        .expect("compose with a credential");
        assert!(
            host.provider
                .resolve(DEFAULT_PROJECT, AuthorityClass::GuestSql, Some("acme"))
                .expect("the credential's own tenant resolves")
                .is_some()
        );
        assert!(
            host.provider
                .resolve(DEFAULT_PROJECT, AuthorityClass::GuestSql, Some("evil"))
                .is_err(),
            "a credential minted for another tenant must be REFUSED, not borrowed"
        );
        assert!(
            host.provider
                .resolve(DEFAULT_PROJECT, AuthorityClass::GuestSql, None)
                .is_err(),
            "guest resolution without a tenant has no authority to check"
        );
        // Platform classes are project-environment scoped, so the tenant is not
        // part of their binding and its absence is not an error.
        assert!(
            host.provider
                .resolve(DEFAULT_PROJECT, AuthorityClass::ExecutorPlatform, None)
                .expect("platform resolution needs no tenant")
                .is_some()
        );
    }

    #[test]
    fn the_composed_credential_becomes_the_default_project() {
        let composed = WamnPostgres::from_env(Some(ClassCredentials::every_class(guest_url(
            "acme",
            "host-default",
        ))))
        .expect("compose with a credential");
        assert!(
            composed
                .provider
                .resolve(DEFAULT_PROJECT, AuthorityClass::GuestSql, Some("acme"))
                .expect("resolve default")
                .is_some(),
            "a host composed WITH a credential must resolve the default project; \
             deploy/platform injects it via secretKeyRef and a host that cannot \
             resolve it has no database at all"
        );

        let bare = WamnPostgres::from_env(None).expect("compose without a credential");
        assert!(
            bare.provider
                .resolve(DEFAULT_PROJECT, AuthorityClass::GuestSql, Some("acme"))
                .expect("resolve default")
                .is_none(),
            "composing without a credential must resolve nothing, not reach for \
             an ambient one"
        );
    }

    #[test]
    fn a_serving_host_binds_its_credential_to_the_declared_project() {
        let executor_platform_url = "postgres://executor-platform@db/host-receiving";
        let composed = WamnPostgres::from_env_for_project(
            "receiving",
            Some(
                ClassCredentials::default()
                    .with_class(AuthorityClass::ExecutorPlatform, executor_platform_url),
            ),
        )
        .expect("compose the declared project credential");
        let resolved = composed
            .provider
            .resolve("receiving", AuthorityClass::ExecutorPlatform, None)
            .expect("resolve the declared project")
            .expect("the declared project has an executor-platform credential");
        assert_eq!(
            resolved.database_url, executor_platform_url,
            "the host's exact credential must resolve under its trusted project"
        );
        assert!(
            composed
                .provider
                .resolve(DEFAULT_PROJECT, AuthorityClass::ExecutorPlatform, None)
                .expect("resolve the unrelated default project")
                .is_none(),
            "binding a named project must not mint a default-project alias"
        );
    }

    #[test]
    fn a_serving_host_refuses_an_invalid_declared_project() {
        let error = WamnPostgres::from_env_for_project("receiving.prod", None)
            .err()
            .expect("an invalid project must refuse before reading ambient configuration");
        assert!(error.to_string().contains("invalid composed project"));
    }

    #[test]
    fn an_explicit_project_credential_refuses_a_second_source() {
        let cfg = WamnPostgresConfig::from_env();
        let credentials = ClassCredentials::every_class(guest_url("acme", "host-receiving"));
        let projects = HashMap::from([(
            "receiving".to_owned(),
            ProjectConfig::from_global(credentials.clone(), &cfg),
        )]);
        let error = bind_composed_project(projects, "receiving", Some(credentials), &cfg)
            .expect_err("two credential sources for one project must refuse");
        assert!(
            error.to_string().contains(
                "both an explicit composition credential and a WAMN_PG_PROJECTS_FILE entry"
            )
        );
    }

    #[test]
    fn guest_and_platform_pool_caches_remain_distinct_under_interleaving() {
        let postgres = WamnPostgres::new(WamnPostgresConfig {
            // wamn-0h0g.22.8.2: a provisioned credential NAMES ITS GENERATION ROLE,
            // and the pool key is derived from it. A url with no user carries no
            // credential identity and is now refused, so this fixture names one
            // rather than relying on a libpq-style implicit OS user.
            credentials: Some(ClassCredentials::every_class(format!(
                "postgres://wamn_app_{}_a@localhost/pool-lifecycle-proof",
                wamn_run_state::app_scope_hash("acme", "pool-lifecycle-proof")
            ))),
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 100,
            statement_timeout_ms: 100,
            row_limit: 10,
        })
        .expect("construct lazy lifecycle pools");

        let guest_first = postgres
            .ensure_pool(AuthorityClass::GuestSql, DEFAULT_PROJECT, Some("acme"))
            .expect("first guest pool");
        let platform_first = postgres
            .ensure_pool(AuthorityClass::ExecutorPlatform, DEFAULT_PROJECT, None)
            .expect("first platform pool");
        let platform_second = postgres
            .ensure_pool(AuthorityClass::ExecutorPlatform, DEFAULT_PROJECT, None)
            .expect("memoized platform pool");
        let guest_second = postgres
            .ensure_pool(AuthorityClass::GuestSql, DEFAULT_PROJECT, Some("acme"))
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
            "FROM catalog.effective_release_packages AS member",
            "JOIN catalog.wirings AS wiring",
            "member.effective_release_id = $3",
            "member.package_id = $2",
            "wiring.package_id = member.package_id",
            "wiring.package_version = member.package_version",
            "wiring.wiring_id = $5",
            "wiring.version = $6",
            "wiring.graph_json ->> 'wiring-id' = $5",
            "component.tenant_id = wiring.tenant_id",
            "component.package_id = wiring.package_id",
            "component.package_version = wiring.package_version",
            "component.component_digest = $8",
            "component.operations #>> ARRAY[node.value ->> 'operation', 'registered-operation']",
            "wiring.graph_json #> ARRAY['nodes', $7]",
            "requirement.component_digest = $8",
            "requirement.store_alias = $9",
            "binding.effective_release_id = $3",
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
            "artifact_hash =",
            "requirement_name =",
            "catalog_id",
            "catalog_version",
            "gated_catalog_version",
            "catalog_heads",
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

    // R31 — unbind reaps every per-component claim registry plus the closed
    // workload-authority discriminator while leaving another workload's
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
            pg.bind_workload_authority(c, "event-materializer").unwrap();
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
        assert_eq!(
            pg.workload_authority_for("wl-a-component-0"),
            AuthorityClass::GuestSql
        );
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
            pg.workload_authority_for("wl-b-component-0"),
            AuthorityClass::EventMaterializer
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

    /// The tenant every live guest checkout in this module authenticates for.
    const LIVE_TENANT: &str = "acme";

    /// Ensure the stable guest ACL role exists, race-tolerantly.
    ///
    /// EVERY guest connection is checked by the credential-exactness hook
    /// (`wamn-0h0g.22.8.4`), which requires the session to be a MEMBER of
    /// `wamn_app` — so on a fresh cluster, where no cluster-wide role exists
    /// yet, a guest checkout fails before it reaches anything under test. That
    /// is the production shape (a generation inherits the stable ACL role), so
    /// the fixtures reproduce it rather than weaken the hook.
    const ENSURE_GUEST_ACL_ROLE_SQL: &str = "DO $acl$ BEGIN \
           BEGIN CREATE ROLE wamn_app NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
             NOREPLICATION NOBYPASSRLS; \
           EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END; \
         END $acl$;";

    fn live_database(admin_url: &str) -> String {
        url::Url::parse(admin_url)
            .expect("parse the live test url")
            .path()
            .trim_start_matches('/')
            .to_string()
    }

    /// Rewrite a disposable-database URL onto a PROPERLY NAMED guest generation,
    /// creating the login if it is missing.
    ///
    /// `wamn-0h0g.22.6.7` binds a guest credential to its tenant: resolution
    /// verifies that the login carries `app_scope_hash(tenant, database)`, so a
    /// URL naming an arbitrary user no longer resolves for the guest class.
    /// That is the point — a shared login is exactly what item 2 retires — and
    /// it means a live guest test has to authenticate as a real generation.
    async fn live_guest_url(admin_url: &str, tenant: &str) -> String {
        let mut url = url::Url::parse(admin_url).expect("parse the live test url");
        let database = live_database(admin_url);
        let role = format!(
            "wamn_app_{}_a",
            wamn_run_state::app_scope_hash(tenant, &database)
        );
        let admin = connect_raw(admin_url).await;
        admin
            .batch_execute(&format!(
                // The tests in this module run in PARALLEL against one cluster
                // and roles are cluster-wide, so IF NOT EXISTS races: two
                // sessions both see the role absent and both create it. The
                // exception guard is what makes the create idempotent under
                // concurrency, not the existence check.
                "{ENSURE_GUEST_ACL_ROLE_SQL} \
                 DO $$ BEGIN \
                   BEGIN \
                     CREATE ROLE \"{role}\" LOGIN PASSWORD 'live-guest'; \
                   EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; \
                   END; \
                 END $$; \
                 GRANT wamn_app TO \"{role}\";"
            ))
            .await
            .expect("ensure the live guest generation");
        url.set_username(&role).expect("set the guest login");
        url.set_password(Some("live-guest"))
            .expect("set the password");
        url.to_string()
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

    // R2/R16 — the REAL plugin path: begin_with_claims injects the guest claim
    // set via the bound statement, they are visible in-txn, and revert after the
    // txn. `app.tenant` is NOT among them (`wamn-0h0g.22.6.7`): a guest's tenant
    // is its LOGIN, and injecting a GUC nothing it can read consults would be a
    // second, settable statement about an authority derived elsewhere.
    #[tokio::test]
    async fn live_begin_with_claims_sets_the_guest_set_without_a_tenant_claim() {
        let Some(admin_url) = test_pg_url() else {
            return;
        };
        let pg = WamnPostgres::new(WamnPostgresConfig {
            credentials: Some(ClassCredentials::every_class(
                live_guest_url(&admin_url, LIVE_TENANT).await,
            )),
            guest_pool_max_size: 2,
            platform_pool_max_size: 2,
            wait_timeout_ms: 2_000,
            statement_timeout_ms: 5_000,
            row_limit: 1_000,
        })
        .unwrap();
        let user_id = "11111111-1111-4111-8111-111111111111";
        let (conn, _pp) = pg
            .checkout_guest(DEFAULT_PROJECT, LIVE_TENANT)
            .await
            .unwrap();
        pg.begin_with_claims(
            &conn,
            AuthorityClass::GuestSql,
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
        // THE DELETION, ASSERTED — and NULL is the sharper result. A custom GUC
        // reads back as the EMPTY STRING once it has been set and the SET LOCAL
        // scope ended; it reads NULL only if it was never set in this session at
        // all. So `None` here says more than "the claim was cleared": it says
        // the guest transaction never touched `app.tenant`.
        assert_eq!(
            tenant, None,
            "the guest claim set must NOT inject app.tenant"
        );
        // …and the session it runs on authenticates as the tenant's own
        // generation, which is where its tenant actually comes from.
        let who: String = conn
            .query_one("SELECT current_user::text", &[])
            .await
            .unwrap()
            .get(0);
        assert!(
            who.contains(&wamn_run_state::app_scope_hash(
                LIVE_TENANT,
                &live_database(&admin_url)
            )),
            "the guest session must authenticate as {LIVE_TENANT}'s generation, got {who:?}"
        );
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
            .query_one("SELECT current_setting('app.role', true)", &[])
            .await
            .unwrap()
            .get(0);
        // `app.role` IS injected by the guest set, so after the commit it reads
        // back as the empty string — the reset value, which is what proves the
        // claim was transaction-LOCAL rather than a session-level leak.
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

    /// SCAFFOLDING FLOOR, NOT THE PRODUCTION ONE. Production keys the permissive
    /// floor on `current_user` through `wamn_authority.tenant_key`
    /// (`wamn-0h0g.22.6`), which needs the authority derivations installed — and
    /// those are built by the provisioner, which the shipped runtime
    /// deliberately does not link. The floor is not this fixture's subject: the
    /// RESTRICTIVE per-role and per-user layer is, and that is a different claim
    /// class item 2 leaves alone. The production floor is proven live by
    /// `crates/control/provision/tests/deploy_sql_authority.rs` and the static
    /// `deploy/sql/app-schema.sql` contract.
    ///
    /// The 3.2 tenant floor plus the row-ownership rule documented by
    /// `deploy/sql/app-schema.sql`, over a table the probe role does not own so
    /// RLS applies to it.
    fn rls_fixture_sql(schema: &str, probe: &str, tenant: &str, u1: &str, u2: &str) -> String {
        format!(
            "DROP SCHEMA IF EXISTS {schema} CASCADE; \
             DROP ROLE IF EXISTS {probe}; \
             {ENSURE_GUEST_ACL_ROLE_SQL} \
             CREATE ROLE {probe} LOGIN PASSWORD '{probe}' NOSUPERUSER NOBYPASSRLS; \
             GRANT wamn_app TO {probe}; \
             CREATE SCHEMA {schema}; \
             CREATE TABLE {schema}.dispositions ( \
                 tenant_id text NOT NULL, id int NOT NULL, inspector_id uuid NOT NULL); \
             ALTER TABLE {schema}.dispositions ENABLE ROW LEVEL SECURITY; \
             CREATE POLICY dispositions_tenant ON {schema}.dispositions \
                 USING (tenant_id = '{tenant}'); \
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
    // path while isolated policy tests passed on hand-written `SET LOCAL`.
    // This drives the REAL plugin (`one_shot`) as a NOSUPERUSER
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
        // The probe login is NAMED AS A GUEST GENERATION for this test's tenant:
        // guest credential resolution verifies that the login carries
        // `app_scope_hash(tenant, database)` (`wamn-0h0g.22.6.7`), so a probe
        // with an arbitrary name would be refused before the policy under test
        // ever ran. The two tenants differ per test, so the derived logins do
        // too and the tests stay parallel-safe.
        let probe = format!(
            "wamn_app_{}_a",
            wamn_run_state::app_scope_hash(TENANT, &live_database(&admin_url))
        );
        let _ = suffix;
        let admin = connect_raw(&admin_url).await;
        admin
            .batch_execute(&rls_fixture_sql(&schema, &probe, TENANT, U1, U2))
            .await
            .expect("seed the per-user RLS fixture as the superuser owner");

        let pg = WamnPostgres::new(WamnPostgresConfig {
            credentials: Some(ClassCredentials::every_class(database_url_for_role(
                &admin_url, &probe, &probe,
            ))),
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
        // The probe login is NAMED AS A GUEST GENERATION for this test's tenant:
        // guest credential resolution verifies that the login carries
        // `app_scope_hash(tenant, database)` (`wamn-0h0g.22.6.7`), so a probe
        // with an arbitrary name would be refused before the policy under test
        // ever ran. The two tenants differ per test, so the derived logins do
        // too and the tests stay parallel-safe.
        let probe = format!(
            "wamn_app_{}_a",
            wamn_run_state::app_scope_hash(TENANT, &live_database(&admin_url))
        );
        let _ = suffix;
        let admin = connect_raw(&admin_url).await;
        admin
            .batch_execute(&rls_fixture_sql(&schema, &probe, TENANT, U1, U2))
            .await
            .expect("seed the per-user RLS fixture as the superuser owner");

        let pg = WamnPostgres::new(WamnPostgresConfig {
            credentials: Some(ClassCredentials::every_class(database_url_for_role(
                &admin_url, &probe, &probe,
            ))),
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
        let (conn, _pp) = pg.checkout_guest(DEFAULT_PROJECT, TENANT).await.unwrap();
        pg.begin_with_claims(
            &conn,
            AuthorityClass::GuestSql,
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
            credentials: None,
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
            package_id: "catalog",
            effective_release_id: 1,
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
            credentials: None,
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
            package_id: "catalog",
            effective_release_id: 1,
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

    /// A provider that RECORDS which authority class each resolution was asked
    /// for, and names no credential for any of them.
    ///
    /// `Ok(None)` is what keeps the proof hermetic: `ensure_pool` refuses on the
    /// spot, so the class under test is observed with no pool built and no socket
    /// opened.
    #[derive(Default)]
    struct RecordingProvider {
        asked: std::sync::Mutex<Vec<AuthorityClass>>,
    }

    #[tokio::test]
    async fn workload_binding_selects_only_the_materializer_authority() {
        let provider = Arc::new(RecordingProvider::default());
        let pg = WamnPostgres::with_provider(Arc::clone(&provider) as Arc<dyn CredentialProvider>);
        pg.bind_workload_authority("materializer", "event-materializer")
            .expect("the closed materializer value is admitted");
        pg.bind_workload_authority("invalid", "executor-platform")
            .expect_err("no other explicit authority is admitted");

        assert!(
            pg.checkout_workload("materializer", DEFAULT_PROJECT, "tenant-a")
                .await
                .is_err(),
            "the recording provider deliberately names no credential"
        );
        assert!(
            pg.checkout_workload("ordinary-guest", DEFAULT_PROJECT, "tenant-a")
                .await
                .is_err(),
            "the recording provider deliberately names no credential"
        );

        let asked = provider
            .asked
            .lock()
            .expect("recording provider lock poisoned")
            .clone();
        assert_eq!(
            asked,
            vec![AuthorityClass::EventMaterializer, AuthorityClass::GuestSql]
        );
    }

    impl CredentialProvider for RecordingProvider {
        fn resolve(
            &self,
            _project: &str,
            class: AuthorityClass,
            _tenant: Option<&str>,
        ) -> anyhow::Result<Option<ResolvedCredential>> {
            self.asked
                .lock()
                .expect("recording provider lock poisoned")
                .push(class);
            Ok(None)
        }
    }

    /// THE TRUSTED HTTP EFFECT CHECKS OUT UNDER `CallableHttp` AND NOTHING ELSE
    /// (`wamn-0h0g.22.11`).
    ///
    /// THE COVERAGE THIS CLOSES. Until this test the callable-HTTP checkout class
    /// had NO coverage at all: a mutant making this method check out
    /// `AuthorityClass::ExecutorPlatform` instead was INERT — nothing in the
    /// crate distinguished it. The three offline `connection_effect_snapshot`
    /// tests refuse before a pool is reached, and `checkout_platform` maps every
    /// failure to the one `PgError::ConnectionUnavailable`, so no error message
    /// can name the class either. The provider is the seam where the class IS
    /// observable: it is the exact argument `ensure_pool` forwards, so recording
    /// it pins the production routing rather than restating it.
    ///
    /// Sequence equality, not containment. `checkout_platform` refuses
    /// `AuthorityClass::GuestSql` BEFORE consulting the provider, so a mutant
    /// naming the guest records nothing at all; asserting the whole sequence
    /// kills that arm too, along with any second checkout under another class.
    #[tokio::test]
    async fn effect_snapshot_checks_out_under_the_callable_http_authority() {
        const COMPONENT: &str = "warm-instance-0";
        let provider = Arc::new(RecordingProvider::default());
        let pg = WamnPostgres::with_provider(Arc::clone(&provider) as Arc<dyn CredentialProvider>);
        pg.bind_session_claims(
            COMPONENT,
            &SessionClaims {
                tenant: "tenant-a".to_string(),
                ..SessionClaims::default()
            },
        )
        .expect("the acquiring tenant binds");

        let lookup = ConnectionEffectLookup {
            package_id: "catalog",
            effective_release_id: 1,
            environment: "dev",
            wiring_id: "wiring",
            wiring_version: 1,
            node_id: "node",
            component_digest: "digest",
            store_alias: "manager",
            candidate_binding: None,
        };

        let refused = pg
            .connection_effect_snapshot(COMPONENT, DEFAULT_PROJECT, "tenant-a", &lookup)
            .await
            .expect_err("a provider that names no credential resolves nothing");
        assert!(
            !refused
                .to_string()
                .contains("disagrees with the tenant bound"),
            "the bound tenant must reach the checkout, or the class below is \
             never asked for: {refused}"
        );

        let asked = provider
            .asked
            .lock()
            .expect("recording provider lock poisoned")
            .clone();
        assert_eq!(
            asked,
            vec![AuthorityClass::CallableHttp],
            "the callable-HTTP authority snapshot must check out as the \
             callable-HTTP family and no other"
        );
    }

    #[tokio::test]
    async fn operation_permissions_reuse_only_the_callable_http_authority() {
        let provider = Arc::new(RecordingProvider::default());
        let pg = WamnPostgres::with_provider(Arc::clone(&provider) as Arc<dyn CredentialProvider>);

        pg.operation_permissions(DEFAULT_PROJECT, "tenant-a", "route-caller")
            .await
            .expect_err("a provider that names no credential resolves nothing");

        let asked = provider
            .asked
            .lock()
            .expect("recording provider lock poisoned")
            .clone();
        assert_eq!(asked, vec![AuthorityClass::CallableHttp]);
    }

    /// The tenant floor over rows belonging to TWO tenants, keyed on
    /// `current_user` — the shape `wamn-0h0g.22.6` put in production.
    ///
    /// The literal role names stand in for `wamn_authority.tenant_key`, which
    /// this crate cannot install (the derivations are built by the provisioner,
    /// which the shipped runtime deliberately does not link). What the fixture
    /// reproduces faithfully is the thing under test: the row filter reads the
    /// CONNECTED ROLE, so a session cannot talk its way into another tenant's
    /// rows — there is no claim to rewrite.
    fn two_tenant_rls_fixture_sql(
        schema: &str,
        role_a: &str,
        role_b: &str,
        a: &str,
        b: &str,
    ) -> String {
        format!(
            "DROP SCHEMA IF EXISTS {schema} CASCADE; \
             DO $reset$ BEGIN \
               IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{role_a}') THEN \
                 DROP OWNED BY {role_a}; DROP ROLE {role_a}; END IF; \
               IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{role_b}') THEN \
                 DROP OWNED BY {role_b}; DROP ROLE {role_b}; END IF; \
             END $reset$; \
             {ENSURE_GUEST_ACL_ROLE_SQL} \
             CREATE ROLE {role_a} LOGIN PASSWORD 'live-guest' NOSUPERUSER NOBYPASSRLS; \
             CREATE ROLE {role_b} LOGIN PASSWORD 'live-guest' NOSUPERUSER NOBYPASSRLS; \
             GRANT wamn_app TO {role_a}, {role_b}; \
             CREATE SCHEMA {schema}; \
             CREATE TABLE {schema}.dispositions (tenant_id text NOT NULL, id int NOT NULL); \
             ALTER TABLE {schema}.dispositions ENABLE ROW LEVEL SECURITY; \
             CREATE POLICY dispositions_tenant ON {schema}.dispositions \
                 USING ((tenant_id = '{a}' AND current_user = '{role_a}') \
                     OR (tenant_id = '{b}' AND current_user = '{role_b}')); \
             INSERT INTO {schema}.dispositions \
                 VALUES ('{a}', 1), ('{a}', 2), ('{b}', 3); \
             GRANT USAGE ON SCHEMA {schema} TO {role_a}, {role_b}; \
             GRANT SELECT ON {schema}.dispositions TO {role_a}, {role_b};"
        )
    }

    /// *** THE ADVERSARIAL ARM, RE-EXPRESSED ON THE NEW MECHANISM. ***
    ///
    /// This test used to prove that two interleaved CLAIM sets each saw only
    /// their own rows. That subject is retired: after `wamn-0h0g.22.6` a guest's
    /// tenant is its LOGIN, and under the owner ruling on `wamn-0h0g.22.6.7` a
    /// host holds ONE guest credential per project-environment. So the property
    /// worth proving is stronger and simpler — a second tenant is REFUSED rather
    /// than quietly served the credential the host does hold.
    ///
    /// The logins are NOSUPERUSER NOBYPASSRLS, so the server cannot be talked
    /// out of the floor either.
    #[tokio::test]
    async fn live_a_second_tenant_is_refused_rather_than_served_the_first_tenants_rows() {
        const TENANT_A: &str = "seam-live-a";
        const TENANT_B: &str = "seam-live-b";

        let Some(admin_url) = test_pg_url() else {
            return;
        };
        let suffix = std::process::id();
        let schema = format!("wamn_seam_{suffix}");
        let database = live_database(&admin_url);
        // Named exactly as provisioning names a guest generation, so the
        // credential the host resolves is bound to TENANT_A by its own digest.
        let role_a = format!(
            "wamn_app_{}_a",
            wamn_run_state::app_scope_hash(TENANT_A, &database)
        );
        let role_b = format!(
            "wamn_app_{}_a",
            wamn_run_state::app_scope_hash(TENANT_B, &database)
        );
        let admin = connect_raw(&admin_url).await;
        admin
            .batch_execute(&two_tenant_rls_fixture_sql(
                &schema, &role_a, &role_b, TENANT_A, TENANT_B,
            ))
            .await
            .expect("seed the two-tenant RLS fixture as the superuser owner");

        let mut url = url::Url::parse(&admin_url).expect("parse the live test url");
        url.set_username(&role_a).expect("set A's login");
        url.set_password(Some("live-guest"))
            .expect("set A's password");
        let pg = WamnPostgres::new(WamnPostgresConfig {
            credentials: Some(ClassCredentials::every_class(url.to_string())),
            guest_pool_max_size: 2,
            platform_pool_max_size: 2,
            wait_timeout_ms: 2_000,
            statement_timeout_ms: 5_000,
            row_limit: 1_000,
        })
        .unwrap();

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

        // CONTROL: the table is reachable on this path at all, so the refusal
        // below is a refusal and not a broken fixture or search_path.
        assert_eq!(
            visible_rows(&pg, scope_a, "SELECT id FROM dispositions ORDER BY id").await,
            2,
            "A sees exactly its own two rows through its own login"
        );

        // B's acquisition is legitimate; the HOST simply holds no credential for
        // it. That must refuse. Serving A's credential would hand B two rows
        // belonging to another tenant, which is the failure this design exists
        // to make impossible.
        let refused = pg
            .one_shot(scope_b, "SELECT id FROM dispositions", &[], true)
            .await
            .err()
            .expect("a tenant this host holds no credential for cannot query");
        assert!(
            matches!(&refused, PgError::ConnectionUnavailable),
            "a second tenant must be REFUSED at credential resolution, not served \
             the first tenant's connection: {refused:?}"
        );

        assert_eq!(
            visible_rows(&pg, scope_a, "SELECT id FROM dispositions ORDER BY id").await,
            2,
            "A's rows are unchanged by B's refused acquisition"
        );

        // Ending A's checkout revokes A's identity and nothing else. Matched on
        // the NO-TENANT code specifically: a revoke that cleared only the
        // search_path would also fail this query, for an unrelated reason.
        pg.revoke_session_claims(scope_a);
        assert_eq!(pg.session_claims(scope_a), None);
        let unbound = pg
            .one_shot(scope_a, "SELECT id FROM dispositions", &[], true)
            .await
            .err()
            .expect("an unbound claim scope cannot query at all");
        assert!(
            matches!(&unbound, PgError::QueryError((code, _)) if code == "WAMN0"),
            "an instance whose checkout ended must resolve NO tenant, not merely \
             fail for some other reason: {unbound:?}"
        );

        admin
            .batch_execute(&format!(
                "DROP SCHEMA {schema} CASCADE; \
                 DROP OWNED BY {role_a}; DROP ROLE {role_a}; \
                 DROP OWNED BY {role_b}; DROP ROLE {role_b};"
            ))
            .await
            .expect("drop the fixture");
    }

    // R18 — the post_create hook runs on connect; a successful checkout from the
    // pool proves the assertion passed on this server (stock PG18 = on).
    #[tokio::test]
    async fn live_connect_asserts_standard_conforming_strings() {
        let Some(admin_url) = test_pg_url() else {
            return;
        };
        let pg = WamnPostgres::new(WamnPostgresConfig {
            credentials: Some(ClassCredentials::every_class(
                live_guest_url(&admin_url, LIVE_TENANT).await,
            )),
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
            .checkout_guest(DEFAULT_PROJECT, LIVE_TENANT)
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
        let admin_url = std::env::var("WAMN_POOL_LIFECYCLE_PG_URL")
            .expect("set WAMN_POOL_LIFECYCLE_PG_URL to a disposable PostgreSQL database");
        let url = live_guest_url(&admin_url, LIVE_TENANT).await;
        let postgres = WamnPostgres::new(WamnPostgresConfig {
            credentials: Some(ClassCredentials::every_class(url)),
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 250,
            statement_timeout_ms: 1_000,
            row_limit: 10,
        })
        .expect("construct size-one lifecycle pools");

        let (guest, _) = postgres
            .checkout_guest(DEFAULT_PROJECT, LIVE_TENANT)
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
            .checkout_platform(DEFAULT_PROJECT, AuthorityClass::ExecutorPlatform)
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
            .checkout_guest(DEFAULT_PROJECT, LIVE_TENANT)
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
            .checkout_platform(DEFAULT_PROJECT, AuthorityClass::ExecutorPlatform)
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
    // skipped LOUDLY when unset. Recipe: docs/operations/build-and-test.md [R18-NEG].
    #[tokio::test]
    async fn live_scs_off_server_fails_checkout_closed() {
        let Some(url) = std::env::var("WAMN_SCS_OFF_PG_URL").ok() else {
            eprintln!(
                "WAMN_SCS_OFF_PG_URL unset — skipping the wamn-2jkm.65 R18 live negative \
                 (boot a postgres:18 with -c standard_conforming_strings=off; see \
                 docs/operations/build-and-test.md [R18-NEG])"
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
        // The url names a REAL guest generation, so resolution succeeds and the
        // refusal below can only come from the hook. A url with an arbitrary
        // user would now be refused at RESOLUTION (`wamn-0h0g.22.6.7`) — the same
        // `connection-unavailable` variant for a different reason, which is
        // exactly the false positive this test's second half exists to rule out.
        let guest_url = live_guest_url(&url, LIVE_TENANT).await;
        let pg = WamnPostgres::new(WamnPostgresConfig {
            credentials: Some(ClassCredentials::every_class(guest_url)),
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 2_000,
            statement_timeout_ms: 5_000,
            row_limit: 1_000,
        })
        .unwrap();
        // (`matches!`, not `expect_err`, so the Ok type need not be `Debug`.)
        let result = pg.checkout_guest(DEFAULT_PROJECT, LIVE_TENANT).await;
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
            &ResolvedCredential {
                database_url: url,
                guest_pool_max_size: 1,
                platform_pool_max_size: 1,
                wait_timeout_ms: 2_000,
                statement_timeout_ms: 5_000,
                row_limit: 1_000,
            },
            AuthorityClass::GuestSql,
            DEFAULT_PROJECT,
            &platform_messages,
        )
        // Hook ORDER matters to this test: R18 is pushed first, so a scs=off
        // server fails on standard_conforming_strings before the
        // wamn-0h0g.22.8.4 exactness hook is reached.
        .expect("pool builds (url parses; the hooks run at checkout, not build)");
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

    // ------------------------------------------------------------------
    // wamn-0h0g.17.33 — the pipelined claim flight, and its proof obligation.
    // ------------------------------------------------------------------

    /// The fixture a cold-parse check needs: a schema the session's own
    /// `search_path` does NOT contain, holding the only relation the statement
    /// names, readable by the guest generation.
    fn cold_parse_fixture_sql(schema: &str, role: &str) -> String {
        format!(
            "DROP SCHEMA IF EXISTS {schema} CASCADE; \
             CREATE SCHEMA {schema}; \
             CREATE TABLE {schema}.cold_parse (id int NOT NULL); \
             INSERT INTO {schema}.cold_parse VALUES (1), (2); \
             GRANT USAGE ON SCHEMA {schema} TO \"{role}\"; \
             GRANT SELECT ON {schema}.cold_parse TO \"{role}\";"
        )
    }

    /// The statement a cold-parse check runs: it names `cold_parse`
    /// UNQUALIFIED, so Parse resolves it only under the claimed `search_path`.
    fn cold_parse_statement() -> VerifiedStatement {
        VerifiedStatement {
            exact_sql: "SELECT id FROM cold_parse ORDER BY id".into(),
            binds: Box::new([]),
            columns: Box::new([StatementField {
                value_type: StatementValueType::Int32,
                nullable: false,
            }]),
            // The claim path under test. A non-transactional statement with no
            // per-caller claim takes the autocommit branch instead.
            transactional: true,
        }
    }

    /// *** A COLD CONNECTION STILL PARSES INSIDE THE CLAIM TRANSACTION. ***
    ///
    /// The regression guard for `wamn-0h0g.15.137.15` and the correctness half
    /// of `wamn-0h0g.17.33`. The pool is brand new, so nothing on the physical
    /// connection has been parsed; the statement names an unqualified relation
    /// that exists ONLY in a schema outside the session's `search_path`. Parse
    /// is where a relation name resolves, so this can succeed only if the
    /// server ran the Parse after the transaction-LOCAL `search_path` the
    /// claims install — that is, inside the claim transaction.
    ///
    /// IT IS NOT RACY. On the pre-fix shape `begin_with_claims` opens by
    /// awaiting its OWN `prepare_cached`, which on a cold connection is a
    /// guaranteed round trip, and the statement half — polled during that await
    /// — always sends its Parse first. Reverting to that shape, or swapping the
    /// two branches of the `join!`, fails this every run.
    #[tokio::test]
    async fn live_a_cold_connection_parses_inside_the_claim_transaction() {
        const TENANT: &str = "coldparse";
        let Some(admin_url) = test_pg_url() else {
            return;
        };
        let schema = format!("wamn_coldparse_{}", std::process::id());
        let role = format!(
            "wamn_app_{}_a",
            wamn_run_state::app_scope_hash(TENANT, &live_database(&admin_url))
        );
        let guest_url = live_guest_url(&admin_url, TENANT).await;
        let admin = connect_raw(&admin_url).await;
        admin
            .batch_execute(&cold_parse_fixture_sql(&schema, &role))
            .await
            .expect("seed the cold-parse fixture as the superuser owner");

        let pg = WamnPostgres::new(WamnPostgresConfig {
            credentials: Some(ClassCredentials::every_class(guest_url)),
            // ONE connection, and a pool built in this test, so the checkout
            // below is guaranteed to be a NEW physical connection with an empty
            // statement cache.
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 2_000,
            statement_timeout_ms: 5_000,
            row_limit: 1_000,
        })
        .expect("the plugin builds from the guest generation's url");
        let scope = "coldparse-instance-0";
        pg.bind_session_claims(
            scope,
            &SessionClaims {
                tenant: TENANT.to_string(),
                schema: Some(schema.clone()),
                ..SessionClaims::default()
            },
        )
        .expect("the cold-parse scope binds");

        let statement = cold_parse_statement();
        let rows = pg
            .one_shot_statement(scope, "sha256:cold-parse", &statement, &[])
            .await
            .expect(
                "a COLD connection must parse the statement inside the claim transaction: a \
                 `relation \"cold_parse\" does not exist` here is the statement reaching the \
                 server ahead of the claims",
            );
        assert_eq!(
            rows.rows.len(),
            2,
            "the claimed search_path resolved the relation"
        );

        // And the same connection, now WARM, still runs it: the flight that
        // saves the round trip is the one this second call takes.
        let again = pg
            .one_shot_statement(scope, "sha256:cold-parse", &statement, &[])
            .await
            .expect("the warm connection runs the pipelined flight");
        assert_eq!(again.rows.len(), 2);

        admin
            .batch_execute(&format!(
                "DROP SCHEMA {schema} CASCADE; DROP OWNED BY \"{role}\"; DROP ROLE \"{role}\";"
            ))
            .await
            .expect("drop the fixture");
    }

    /// The wamn-0h0g.17.33 measurement, runnable on demand.
    ///
    /// OFF unless `WAMN_PG_PIPELINE_BENCH` is set, because it is a two-thousand
    /// request loop, not an assertion. What it measures is ROUND TRIPS, and on a
    /// loopback server one round trip is roughly 50 us -- under the noise of a
    /// busy machine. Point `WAMN_PG_TEST_URL` at a server whose latency you can
    /// see (a delaying TCP proxy in front of a container, or a real host) and
    /// the count is legible: the claim transaction and the statement are ONE
    /// flight, and `WAMN_PG_PIPELINE_BENCH_RUN` adds the run-owned causation
    /// emit that rides the same BEGIN.
    #[tokio::test]
    async fn bench_pipelined_claim_flight() {
        const TENANT: &str = "pipebench";
        if std::env::var("WAMN_PG_PIPELINE_BENCH").is_err() {
            return;
        }
        let Some(admin_url) = test_pg_url() else {
            return;
        };
        let schema = format!("wamn_pipebench_{}", std::process::id());
        let role = format!(
            "wamn_app_{}_a",
            wamn_run_state::app_scope_hash(TENANT, &live_database(&admin_url))
        );
        let guest_url = live_guest_url(&admin_url, TENANT).await;
        let admin = connect_raw(&admin_url).await;
        admin
            .batch_execute(&format!(
                "DROP SCHEMA IF EXISTS {schema} CASCADE; \
                 CREATE SCHEMA {schema}; \
                 CREATE TABLE {schema}.cold_parse (id int NOT NULL); \
                 INSERT INTO {schema}.cold_parse VALUES (1), (2); \
                 GRANT USAGE ON SCHEMA {schema} TO \"{role}\"; \
                 GRANT SELECT ON {schema}.cold_parse TO \"{role}\";"
            ))
            .await
            .unwrap();
        let pg = WamnPostgres::new(WamnPostgresConfig {
            credentials: Some(ClassCredentials::every_class(guest_url)),
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 5_000,
            statement_timeout_ms: 5_000,
            row_limit: 1_000,
        })
        .unwrap();
        let scope = "pipebench-instance-0";
        let run = std::env::var("WAMN_PG_PIPELINE_BENCH_RUN").is_ok();
        pg.bind_session_claims(
            scope,
            &SessionClaims {
                tenant: TENANT.to_string(),
                schema: Some(schema.clone()),
                ..SessionClaims::default()
            },
        )
        .unwrap();
        if run {
            pg.set_current_run(
                scope,
                Some(Causation {
                    run: "bench-run".to_string(),
                    root: "bench-run".to_string(),
                    depth: 0,
                }),
            );
        }
        let statement = VerifiedStatement {
            exact_sql: "SELECT id FROM cold_parse ORDER BY id".into(),
            binds: Box::new([]),
            columns: Box::new([StatementField {
                value_type: StatementValueType::Int32,
                nullable: false,
            }]),
            transactional: true,
        };
        for _ in 0..200 {
            pg.one_shot_statement(scope, "sha256:bench", &statement, &[])
                .await
                .unwrap();
        }
        let iterations: u32 = 2_000;
        let mut samples = Vec::with_capacity(iterations as usize);
        let start = std::time::Instant::now();
        for _ in 0..iterations {
            let one = std::time::Instant::now();
            pg.one_shot_statement(scope, "sha256:bench", &statement, &[])
                .await
                .unwrap();
            samples.push(one.elapsed().as_secs_f64() * 1_000.0);
        }
        let total = start.elapsed().as_secs_f64() * 1_000.0;
        samples.sort_by(f64::total_cmp);
        let pct = |percent: usize| samples[(samples.len() - 1) * percent / 100];
        println!(
            "BENCH run={run} n={iterations} total={total:.1}ms mean={:.4}ms \
             p50={:.4}ms p90={:.4}ms p99={:.4}ms",
            total / f64::from(iterations),
            pct(50),
            pct(90),
            pct(99),
        );
        admin
            .batch_execute(&format!(
                "DROP SCHEMA {schema} CASCADE; DROP OWNED BY \"{role}\"; DROP ROLE \"{role}\";"
            ))
            .await
            .unwrap();
    }
}
