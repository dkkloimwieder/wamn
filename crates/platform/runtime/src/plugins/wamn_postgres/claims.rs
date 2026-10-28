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
use wamn_control_registry::identifiers::{
    valid_env_key, valid_project, valid_runner, valid_schema,
};
use wamn_engine::release_manifest::LoadedRelease;
use wamn_event_wire::Causation;
use wamn_run_state::AuthorityClass;

use crate::plugins::connection_http::ConnectionInvocation;

use super::pool::{
    ClassCredentials, CredentialProvider, PoolKey, ProjectConfig, ProjectPool,
    StaticCredentialProvider, WamnPostgresConfig,
};
use super::statements::{StatementScopes, VerifiedStatement};
use super::{DEFAULT_PROJECT, PgError, RowSet};

mod pools;
pub(super) mod transactions;

/// The status row of the environment, which `wamn_app` reads under its
/// database boundary.
const ENVIRONMENT_INACTIVE_SQL: &str = "SELECT status = 'inactive' FROM platform.environment";

/// The user row of one principal, each role it holds and each permission
/// of that role, in one read. No row means no user row. A role without
/// permissions, such as `admin`, yields one row with a null permission, and a
/// principal without roles yields one row with a null role.
const PRINCIPAL_GRANTS_SQL: &str = "SELECT u.status = 'active', r.role_name, p.permission, u.type = 'user' \
    FROM platform.\"user\" u LEFT JOIN platform.user_role r \
    ON r.user_id = u.id \
    LEFT JOIN platform.permission p \
    ON p.role_name = r.role_name \
    WHERE u.id = $1::text::uuid";

/// One row of the bound principal, read under the tenant's own guest login.
///
/// Row existence is the whole question, so the projection is a constant. The
/// database is the org boundary, so the table has no tenant column.
const PROVISIONED_PRINCIPAL_SQL: &str = "SELECT 1 FROM platform.\"user\" \
    WHERE id = $1::text::uuid";

/// Each role that the active user holds, with each permission of that
/// role.
const USER_OPERATION_PERMISSIONS_SQL: &str = "SELECT user_role.role_name, permission.permission \
    FROM platform.\"user\" AS \"user\" \
    JOIN platform.user_role AS user_role \
      ON user_role.user_id = \"user\".id \
    LEFT JOIN platform.permission AS permission \
      ON permission.role_name = user_role.role_name \
    WHERE \"user\".id = $1::text::uuid AND \"user\".status = 'active'";

/// The application authority a caller holds now: whether it holds `admin`, and
/// the stable operation references its other roles grant under the loaded
/// release (docs/architecture/execution.md#routing-and-availability).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeldOperationGrants {
    pub admin: bool,
    /// A service holds permissions but never selects a user's active role.
    pub user: bool,
    pub references: BTreeSet<String>,
    /// The names of the held roles.
    pub roles: BTreeSet<String>,
}

impl HeldOperationGrants {
    /// Fold rows of `(role_name, permission)`, where a null permission is a
    /// role without permissions. The held references are the rows.
    pub fn from_rows(rows: &[tokio_postgres::Row]) -> Result<Self, tokio_postgres::Error> {
        let mut held = Self::default();
        for row in rows {
            let role: String = row.try_get(0)?;
            held.admin |= role == wamn_project_state::ADMIN_ROLE;
            if let Some(reference) = row.try_get::<_, Option<String>>(1)? {
                held.references.insert(reference);
            }
            held.roles.insert(role);
        }
        Ok(held)
    }

    /// The one role a caller with these grants acts with
    /// (docs/architecture/execution.md#environment-workflows): `admin` for a
    /// holder of `admin`, the single role of a caller that holds exactly one,
    /// and none for a caller that holds no role or more than one.
    pub fn active_role(&self) -> Option<Box<str>> {
        if !self.user {
            return None;
        }
        self.pat_role()
    }

    /// A PAT caller selects only a role held by its own active principal.
    /// Service callers never inherit the role of a user session.
    pub fn pat_role(&self) -> Option<Box<str>> {
        if self.admin {
            return Some(wamn_project_state::ADMIN_ROLE.into());
        }
        let mut roles = self.roles.iter();
        match (roles.next(), roles.next()) {
            (Some(role), None) => Some(role.as_str().into()),
            _ => None,
        }
    }
}

fn principal_grants_from_rows(
    rows: &[tokio_postgres::Row],
) -> Result<crate::auth_cache::PrincipalGrants, tokio_postgres::Error> {
    let mut grants = crate::auth_cache::PrincipalGrants {
        present: !rows.is_empty(),
        ..Default::default()
    };
    for row in rows {
        grants.active = row.try_get(0)?;
        grants.user = Some(row.try_get(3)?);
        if let Some(role) = row.try_get::<_, Option<String>>(1)? {
            let permissions = grants.roles.entry(role).or_default();
            permissions.extend(row.try_get::<_, Option<String>>(2)?);
        }
    }
    Ok(grants)
}

/// A session presents authority that its principal does not currently hold.
#[derive(Debug)]
pub(crate) struct SessionGrantRefusal {
    kind: &'static str,
}

impl std::fmt::Display for SessionGrantRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.kind)
    }
}

impl std::error::Error for SessionGrantRefusal {}

/// The grants an active principal holds, from all its roles or only from the
/// `signed` roles it still holds.
fn session_grants(
    grants: &crate::auth_cache::PrincipalGrants,
    roles: &[String],
) -> anyhow::Result<HeldOperationGrants> {
    if !grants.active || grants.user != Some(true) {
        return Err(SessionGrantRefusal {
            kind: "session principal is not an active user",
        }
        .into());
    }
    if !roles.iter().all(|role| grants.roles.contains_key(role)) {
        return Err(SessionGrantRefusal {
            kind: "session principal does not hold a signed role",
        }
        .into());
    }
    Ok(held_grants(grants, Some(roles)))
}

fn held_grants(
    grants: &crate::auth_cache::PrincipalGrants,
    signed: Option<&[String]>,
) -> HeldOperationGrants {
    let mut held = HeldOperationGrants {
        user: grants.user == Some(true),
        ..HeldOperationGrants::default()
    };
    if !grants.active || grants.user.is_none() {
        return held;
    }
    for (role, permissions) in &grants.roles {
        if signed.is_some_and(|signed| !signed.contains(role)) {
            continue;
        }
        held.admin |= role == wamn_project_state::ADMIN_ROLE;
        held.references.extend(permissions.iter().cloned());
        held.roles.insert(role.clone());
    }
    held
}

/// Names the shape only: the credential provider and the live pools carry
/// connection material that must not reach a log.
impl std::fmt::Debug for WamnPostgres {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WamnPostgres")
            .finish_non_exhaustive()
    }
}

pub struct WamnPostgres {
    pub(super) operation_metrics: super::operation::OperationMetrics,
    pub(super) operation_workloads:
        std::sync::RwLock<HashMap<String, Arc<super::operation::OperationWorkload>>>,
    pub(super) local_application: Option<Arc<crate::local_application::LocalApplication>>,
    /// Resolves a project id → its database connection + policy.
    provider: Arc<dyn CredentialProvider>,
    /// Guest-visible project pools, built lazily and never shared with host-owned
    /// claim or authorization work.
    guest_pools: std::sync::RwLock<HashMap<PoolKey, Arc<ProjectPool>>>,
    /// Host-owned claim, authorization, and plan-supply project pools. Keeping a
    /// distinct cache makes cross-lifecycle session reuse unrepresentable.
    platform_pools: std::sync::RwLock<HashMap<PoolKey, Arc<ProjectPool>>>,
    /// component id → tenant claim.
    env_keys: std::sync::RwLock<HashMap<String, String>>,
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
    /// component id → the executing principal's `app.user_id` claim (a
    /// `users.id` uuid): the caller principal, or the platform principal of a
    /// callerless delivery. The record-history triggers stamp it on every
    /// write. Absent (the default) binds the empty string, which the triggers
    /// refuse with actor-required. Until `wamn-0h0g.22` replaces
    /// caller-settable authority, modified application SQL can forge this
    /// attribution.
    users: std::sync::RwLock<HashMap<String, String>>,
    /// component id → the `app.operation` claim: the token of the operation
    /// that executes, or `wamn:<component>` for a platform component outside a
    /// registered operation. The record-history log records it as the source
    /// of a write. Absent (the default) binds the empty string. Modified
    /// application SQL can forge it, as it can forge `app.user_id`.
    operations: std::sync::RwLock<HashMap<String, String>>,
    /// component id → the `app.application` claim: the application name from
    /// the environment map for the operation's package. Numbering keys its
    /// sequences on this host state, which the operation transaction keeps,
    /// never on the claim (docs/architecture/data-access.md#rendering). Absent
    /// (the default) binds the empty string.
    applications: std::sync::RwLock<HashMap<String, String>>,
    /// component id → the release digest this pod carries.
    /// Absent (the default) ⇒ the production claim verifies nothing, so every
    /// path that never mounted a release identity is byte-unchanged. When set,
    /// the claim leases only a run admitted under that digest.
    release_identities: std::sync::RwLock<HashMap<String, ReleaseIdentity>>,
    /// component id → the causation context {run, root, depth} of the run the
    /// caller is currently driving (l5i9.12.2). Declared through
    /// [`set_current_run`](WamnPostgres::set_current_run), cleared (removed)
    /// between runs. Absent (the default) ⇒ no causation is stamped — so every
    /// non-run path (S2..S6, the gateway, benches without a declaration) is
    /// byte-unchanged. When set, [`begin_with_claims`] appends a
    /// TRANSACTIONAL `wamn.causation` logical message to each write transaction the
    /// plugin opens for that component, which the CDC reader (l5i9.12.1)
    /// stitches onto the txn's row events.
    current_run: std::sync::RwLock<HashMap<String, Causation>>,
    /// component id → the host-attested invocation the pooled instance serves:
    /// the wiring, node position, occurrence and component tuple this checkout
    /// runs under (`wamn-0h0g.7.9`). The driver binds it before `handler.run`
    /// and revokes it before returning the instance to the pool, exactly as the
    /// HTTP and blobstore surfaces do. Absent (the default) ⇒ this plugin holds
    /// no wiring position, which is what every non-router path (S2..S6, the
    /// gateway, benches) carries.
    ///
    /// Its OWN registry rather than a read of the HTTP plugin's, for the reason
    /// `wamn_blobstore::plugin` records in its module doc: a second reader on
    /// one plugin's registry couples siblings over something neither owns.
    /// Nothing authorizes against it — it is the host-attested half of the
    /// coordinates an effect span names, and the run half is
    /// [`current_run_for`](Self::current_run_for).
    invocations: std::sync::RwLock<HashMap<String, ConnectionInvocation>>,
    /// The host transaction of each scope whose operation item has one.
    pub(super) operation_transactions:
        std::sync::Mutex<super::operation_transaction::OperationTransactions>,
    /// Verified SQL facts bound by operation plus the one invocation-active
    /// scope. The active scope is host-selected; a guest can only name a digest
    /// inside it.
    pub(super) statement_scopes: std::sync::RwLock<StatementScopes>,
    /// Connections destroyed instead of repooled (chaos-gate observability).
    pub(super) destroyed: Arc<AtomicU64>,
    /// How many times each half of the un-fused workload bind has run.
    pub(super) bind_counters: super::BindCounters,
    /// The auth facts this host keeps
    /// (docs/architecture/execution.md#auth-memory).
    auth: crate::auth_cache::AuthCache,
}

/// The release a pod carries — the manifest digest derived from the verified
/// content of its mounted serving manifest
/// ([`LoadedRelease`](wamn_engine::release_manifest::LoadedRelease)).
///
/// Admission pins the release digest. The production claim verifies that pin
/// against the claiming pod's digest. The value is host-injected identity,
/// never guest-supplied.
///
/// The field is private, so production code builds the value only from a
/// loaded release (wamn-p77b).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseIdentity {
    /// The serving manifest's digest — `runs.manifest_digest`. The
    /// `sha256:<64 lowercase hex>` shape the run plane's
    /// `runs_release_record_check` admits is carried by the type, so there is no
    /// hand-rolled shape check on this path.
    manifest_digest: ManifestDigest,
}

impl ReleaseIdentity {
    /// The identity of the release a pod loaded.
    pub fn from_loaded(release: &LoadedRelease) -> Self {
        Self {
            manifest_digest: release.release().manifest_digest.clone(),
        }
    }

    /// An identity with no loaded release, for tests whose rows name it.
    #[cfg(feature = "test-util")]
    pub fn for_test(manifest_digest: ManifestDigest) -> Self {
        Self { manifest_digest }
    }

    /// `runs.manifest_digest`.
    pub fn manifest_digest(&self) -> &ManifestDigest {
        &self.manifest_digest
    }
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
    /// `app.env_key` — the RLS claim. Required: a session with no tenant is
    /// refused by [`WamnPostgres::require_env_key`] rather than run unscoped.
    pub env_key: String,
    /// Which project database this session resolves against. `None` ⇒ the
    /// default project.
    pub project: Option<String>,
    /// `SET LOCAL search_path`. `None` ⇒ the server's own search_path.
    pub schema: Option<String>,
    /// `app.runner` — the durable-queue lease owner.
    pub runner: Option<String>,
    /// `app.role` — the caller's `roles.name` for compiled per-role RLS.
    pub role: Option<String>,
    /// `app.user_id` — the executing principal's `users.id`, which the
    /// record-history triggers stamp. A transactional statement with none is
    /// refused.
    pub user_id: Option<String>,
    /// `app.operation`: the executing operation token, or `wamn:<component>`
    /// for a platform component outside a registered operation.
    pub operation: Option<String>,
    /// `app.application`: the environment map name of the application that
    /// holds the operation's package.
    pub application: Option<String>,
    /// The release digest the claiming pod carries.
    pub release: Option<ReleaseIdentity>,
}

/// The bound principal has no `platform."user"` row in the tenant it claims
/// (`wamn-0h0g.9.19`).
///
/// A credential for such a principal is a provisioning defect: every write it
/// makes records an actor that names no row. The bind refuses it, and the
/// refusal is an authorization fact, so the owning boundary translates it to
/// the `permission-denied` the operation vocabulary already carries. The host
/// router does that translation in
/// `crates/execution/host/src/operation/native_policy.rs`.
///
/// It carries the tenant, the project database read, and the principal id, so
/// an operator can find the missing row. It carries no connection material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnprovisionedPrincipal {
    env_key: String,
    project: String,
    principal_id: String,
}

impl UnprovisionedPrincipal {
    /// The tenant whose `platform."user"` table was read.
    #[must_use]
    pub fn env_key(&self) -> &str {
        &self.env_key
    }

    /// The project database the read used.
    #[must_use]
    pub fn project(&self) -> &str {
        &self.project
    }

    /// The `app.user_id` claim that named no row.
    #[must_use]
    pub fn principal_id(&self) -> &str {
        &self.principal_id
    }
}

impl std::fmt::Display for UnprovisionedPrincipal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "principal {} has no platform.\"user\" row in tenant {} of project {}",
            self.principal_id, self.env_key, self.project
        )
    }
}

impl std::error::Error for UnprovisionedPrincipal {}

/// The route or wiring position one effect snapshot resolves under.
#[derive(Debug, Clone, Copy)]
pub struct ConnectionEntryLookup<'a> {
    /// The package that owns the route or the wiring.
    pub package_id: &'a str,
    /// `None` for a route, which has no wiring position.
    pub wiring: Option<WiringLookup<'a>>,
}

/// One node of an exact wiring version.
#[derive(Debug, Clone, Copy)]
pub struct WiringLookup<'a> {
    pub wiring_id: &'a str,
    pub wiring_version: i32,
    pub node_id: &'a str,
}

/// Host-only identity used to load one HTTP effect authorization snapshot.
#[derive(Debug, Clone, Copy)]
pub struct ConnectionEffectLookup<'a> {
    pub entry: ConnectionEntryLookup<'a>,
    pub origin_package_id: &'a str,
    pub origin_component_digest: &'a str,
    pub origin_component: &'a str,
    pub origin_interface_version: &'a str,
    pub origin_operation: &'a str,
    pub package_id: &'a str,
    pub operation: &'a str,
    /// The release the effect runs under, as its manifest digest.
    pub manifest_digest: &'a str,
    pub environment: &'a str,
    pub component_digest: &'a str,
    pub store_alias: &'a str,
    pub candidate_binding: Option<&'a CandidateConnectionBinding>,
}

/// One DB-derived, non-secret connection fact frozen on a candidate run.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CandidateConnectionBinding {
    pub read_only: bool,
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
    /// None for a `gcs` blobstore, which signs with the host pod's service account.
    pub credential_set_handle: Option<String>,
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
            && self
                .credential_set_handle
                .as_deref()
                .is_none_or(|handle| !handle.is_empty())
    }

    pub(crate) fn matches_snapshot(&self, snapshot: &ConnectionEffectSnapshot) -> bool {
        snapshot.read_only == self.read_only
            && snapshot.requirement_hash.as_deref() == Some(self.requirement_hash.as_str())
            && snapshot.instance_id.as_deref() == Some(self.instance_id.as_str())
            && snapshot
                .instance_revision
                .is_some_and(|revision| revision >= self.instance_revision)
            && snapshot.requirement_type.as_deref() == Some(self.requirement_type.as_str())
            && snapshot.contract.as_deref() == Some(self.contract.as_str())
            && snapshot.validation_hash.as_deref() == Some(self.validation_hash.as_str())
            && snapshot.pinned_generation == Some(self.generation)
            && snapshot.generation == Some(self.generation)
            && snapshot.definition_hash.as_deref() == Some(self.definition_hash.as_str())
            && snapshot.credential_handle == self.credential_set_handle
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
#[expect(
    clippy::struct_excessive_bools,
    reason = "the four booleans are four separate admission facts one query read in one \
              transaction (the node is permitted, the binding is active, the binding is valid, \
              the instance is enabled); each is decided by its own column, and a faithful copy \
              of the row is what makes the authorization auditable"
)]
pub struct ConnectionEffectSnapshot {
    pub connection_name: Option<String>,
    pub read_only: bool,
    /// `None` for a route, which has no wiring.
    pub wiring_hash: Option<String>,
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
    /// The generation frozen by admission, if this invocation carries a pin.
    pub pinned_generation: Option<i64>,
    pub instance_revision: Option<i64>,
    pub generation: Option<i64>,
    pub definition: Option<serde_json::Value>,
    pub definition_hash: Option<String>,
    pub credential_handle: Option<String>,
}

/// Resolve the original route or wiring node and the executing component's
/// connection.
///
/// A route passes no wiring (`$5`, `$6` and `$7` are NULL). Its entry is
/// permitted when the origin component exports the origin operation, and the
/// mounted release proves the route. No run, plan, frame, or effect row
/// participates. The selected workflow version stays valid for the delivery
/// even if the environment selects another version. The mounted release is
/// checked separately by `ConnectionHttp`, because its canonical bytes are not
/// a database relation and must not be projected back into Postgres.
///
/// The origin and executing component facts are the descriptors of the
/// components of release `$3`, the one owner of component facts (R2). The
/// workflow graph comes from its pinned `catalog.workflows` row. The reader
/// derives its hash, and the connection owner compares it with the run.
static CONNECTION_EFFECT_SNAPSHOT_SQL: &str = "\
WITH release_manifest AS MATERIALIZED ( \
    SELECT release.env_key, \
           convert_from(release.canonical_bytes, 'UTF8')::jsonb AS manifest \
      FROM catalog.releases AS release \
     WHERE release.env_key = $1 \
       AND release.manifest_digest = $3 \
), release_member AS MATERIALIZED ( \
    SELECT release_manifest.env_key, \
           package.value ->> 'package-id' AS package_id, \
           package.value ->> 'package-version' AS package_version \
      FROM release_manifest \
     CROSS JOIN LATERAL jsonb_array_elements(release_manifest.manifest #> '{release,packages}') \
           AS package(value) \
), release_component AS MATERIALIZED ( \
    SELECT release_manifest.env_key, \
           member.value ->> 'package-id' AS package_id, \
           member.value -> 'descriptor' ->> 'component' AS component, \
           member.value -> 'descriptor' ->> 'interface-version' AS interface_version, \
           member.value -> 'descriptor' ->> 'component-digest' AS component_digest, \
           member.value -> 'descriptor' -> 'operations' AS operations \
      FROM release_manifest \
     CROSS JOIN LATERAL jsonb_array_elements(release_manifest.manifest -> 'components') \
           AS member(value) \
), member AS MATERIALIZED ( \
    SELECT env_key, $12::text AS package_id FROM release_manifest \
     WHERE $5::text IS NOT NULL OR EXISTS (SELECT 1 FROM release_member WHERE package_id = $12) \
), selected_wiring AS MATERIALIZED ( \
    SELECT workflow.graph_json \
      FROM catalog.workflows AS workflow \
     WHERE workflow.env_key = $1 AND workflow.environment = $4 \
       AND workflow.application = $12 AND workflow.workflow_id = $5 AND workflow.version = $6 \
) \
SELECT wiring.graph_json::text, component.component, component.interface_version, \
       $18::text, \
       component.operations #>> ARRAY[$18, 'registered-operation'], \
       requirement.requirement_json::text, requirement.requirement_hash, \
       COALESCE( \
           CASE WHEN $5::text IS NULL THEN \
               origin_component.operations ? $17 \
               AND component.operations ? $18 \
           ELSE \
               node.value IS NOT NULL \
               AND node.value ->> 'component' = origin_component.component \
               AND node.value ->> 'interface-version' = origin_component.interface_version \
               AND node.value ->> 'operation' = $17 \
               AND origin_component.operations ? $17 \
               AND component.operations ? $18 \
           END, \
           false \
       ), \
       binding.binding_status = 'active', binding.validation_status = 'valid', \
       instance.instance_id, binding.validation_hash, \
       instance.requirement_type, instance.contract, \
       instance.lifecycle_status = 'enabled', instance.active_generation, instance.revision, \
       generation.generation, generation.definition_json::text, generation.definition_hash, \
       generation.credential_set_handle, binding.mode, instance.org_connection_name \
  FROM member \
  LEFT JOIN selected_wiring AS wiring ON true \
  LEFT JOIN release_member AS origin_member \
    ON origin_member.env_key = member.env_key \
   AND origin_member.package_id = $13 \
  LEFT JOIN release_component AS origin_component \
    ON origin_component.env_key = origin_member.env_key \
   AND origin_component.package_id = origin_member.package_id \
   AND origin_component.component_digest = $14 \
   AND origin_component.component = $15 \
   AND origin_component.interface_version = $16 \
  LEFT JOIN release_member AS executing_member \
    ON executing_member.env_key = member.env_key \
   AND executing_member.package_id = $2 \
  LEFT JOIN release_component AS component \
    ON component.env_key = executing_member.env_key \
   AND component.package_id = executing_member.package_id \
   AND component.component_digest = $8 \
  LEFT JOIN LATERAL ( \
      SELECT wiring.graph_json #> ARRAY['nodes', $7] AS value \
  ) AS node ON true \
  LEFT JOIN catalog.connection_requirements AS requirement \
    ON requirement.env_key = $1 \
   AND requirement.component_digest = $8 \
   AND requirement.store_alias = $9 \
  LEFT JOIN catalog.connection_bindings AS binding \
    ON binding.env_key = $1 \
   AND binding.manifest_digest = $3 \
   AND binding.component_digest = $8 \
   AND binding.store_alias = $9 \
   AND binding.environment = $4 \
   AND binding.package_id = $2 \
   AND ($10::text IS NULL OR binding.instance_id = $10) \
  LEFT JOIN catalog.connection_instances AS instance \
    ON instance.env_key = binding.env_key \
   AND instance.environment = binding.environment \
   AND instance.instance_id = binding.instance_id \
  LEFT JOIN catalog.connection_generations AS generation \
    ON generation.env_key = instance.env_key \
   AND generation.environment = instance.environment \
   AND generation.instance_id = instance.instance_id \
   AND generation.generation = COALESCE($11::bigint, instance.active_generation) \
 WHERE $5::text IS NULL OR wiring.graph_json IS NOT NULL";

/// Reject guest SQL that would set or reset a session variable or role in-band.
///
/// A guest on the transaction / one-shot / cursor API must not be able to
/// rewrite a host-injected claim (or switch roles) and defeat RLS isolation
/// (wamn-cjv.2 / review C4-1). The extended-query protocol forbids statement
/// chaining, so a claim override can only arrive as a *standalone* `SET` /
/// `RESET` / `set_config(…)` statement — which this catches.
///
/// It runs on the raw `wamn:postgres/client` surface, which only platform
/// components import, and on every verified statement. Admission also parses
/// declared SQL and refuses these constructs on the parse tree; this guard
/// still covers a release admitted before that check, or a forged descriptor.
///
/// # The TENANT axis is structurally closed, and this is no longer what holds it
///
/// The `wamn-0h0g.22.6` lineage re-keyed guest tenant authority onto a
/// NON-SETTABLE identity: `.22.6.2` re-keyed the generated tenant floor onto
/// `current_user`, `.22.6.4` minted per-tenant guest LOGIN generations, and
/// `.22.6.7` cut the guest SQL path onto per-tenant connections.
/// `wamn_authority.current_tenant_key()` reads the CONNECTED ROLE, which no
/// session can rewrite, so a guest that succeeds in setting `app.env_key`
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
    if statement_calls_counter_function(sql) {
        tracing::warn!(
            target: "wamn::security",
            "rejected a guest call of a platform counter function on the guest SQL surface"
        );
        return Err(PgError::QueryError((
            "WAMN0".to_string(),
            "calling a platform counter function is not permitted".to_string(),
        )));
    }
    if statement_writes_platform(sql) {
        tracing::warn!(
            target: "wamn::security",
            "rejected a guest platform.* write on the guest SQL surface"
        );
        return Err(PgError::QueryError((
            "WAMN0".to_string(),
            "writing a platform.* relation is not permitted".to_string(),
        )));
    }
    Ok(())
}

/// The platform functions that write `number_counter`. Only the numbering
/// renderer of this plugin calls them, on its own connection access, which does
/// not pass this raw surface (docs/architecture/data-access.md#actors).
/// Admission also refuses declared statements that name them.
const COUNTER_FUNCTIONS: [&str; 3] = ["bump_counter", "assign_counter", "set_counter"];

/// True if `sql` names a platform counter function. Each function runs as
/// its owner on a sequence id alone, so a guest that reached one could
/// advance or set a counter of any application in the database. Matching is
/// on the bare function name as a whole identifier, so a quoted or
/// unqualified call is caught too, and `reset_counter` is not. Like
/// [`statement_forges_causation`], this is a text-level blocklist: it
/// over-rejects a statement that merely names one in a literal, which is
/// fail-closed.
fn statement_calls_counter_function(sql: &str) -> bool {
    let identifier = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '$';
    normalized_sql(sql).iter().any(|text| {
        COUNTER_FUNCTIONS.iter().any(|function| {
            text.match_indices(function).any(|(start, _)| {
                let before = text[..start].chars().next_back();
                let after = text[start + function.len()..].chars().next();
                !before.is_some_and(identifier) && !after.is_some_and(identifier)
            })
        })
    })
}

/// True if `sql` inserts, updates, deletes, merges, truncates or copies into
/// a `platform.*` relation. The platform writes these rows; the guest only
/// reads them. On SQLite, which has no roles, this is the only thing that
/// keeps the guest out of them. A text-level blocklist like
/// [`statement_forges_causation`].
fn statement_writes_platform(sql: &str) -> bool {
    const WRITES: [&str; 12] = [
        "insert into platform.",
        "update platform.",
        "update only platform.",
        "delete from platform.",
        "delete from only platform.",
        "merge into platform.",
        "merge into only platform.",
        "truncate platform.",
        "truncate only platform.",
        "truncate table platform.",
        "truncate table only platform.",
        "copy platform.",
    ];
    normalized_sql(sql)
        .iter()
        .any(|text| WRITES.iter().any(|write| text.contains(write)))
}

/// `sql` in two forms to match a blocklist against: lowercased, with
/// double quotes removed, each run of whitespace made one space, and no space
/// around a `.`. The first form also replaces each comment with a space, so
/// `UPDATE /* x */ "platform" . "setting"` reads as `update platform.setting`.
/// The second keeps the text of comments and blanks only their markers, so a
/// write hidden between a `/*` and a `*/` inside string literals is still
/// seen. A statement is refused when either form matches.
fn normalized_sql(sql: &str) -> [String; 2] {
    let lower = sql.to_ascii_lowercase();
    let mut uncommented = String::with_capacity(lower.len());
    let mut rest = lower.as_str();
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("--") {
            rest = after.find('\n').map_or("", |end| &after[end..]);
            uncommented.push(' ');
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = after.find("*/").map_or("", |end| &after[end + 2..]);
            uncommented.push(' ');
        } else {
            let next = rest.chars().next().expect("rest is not empty");
            uncommented.push(next);
            rest = &rest[next.len_utf8()..];
        }
    }
    let marked = lower
        .replace("--", " ")
        .replace("/*", " ")
        .replace("*/", " ");
    [uncommented, marked].map(|text| {
        text.replace('"', "")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .replace(" .", ".")
            .replace(". ", ".")
    })
}

/// Refuse a transactional statement that has no executing principal.
///
/// A write stamps its executing principal, so such a statement is a platform
/// defect. The host refuses it before it reaches PostgreSQL, with the SQLSTATE
/// and message that the record-history trigger raises.
pub(super) fn refuse_unattributed_statement(
    statement: &VerifiedStatement,
    user_id: Option<&str>,
) -> Result<(), PgError> {
    if statement.transactional && user_id.is_none() {
        tracing::warn!(
            target: "wamn::security",
            "refused a transactional statement with no executing principal"
        );
        return Err(PgError::QueryError((
            "55000".to_string(),
            "actor-required".to_string(),
        )));
    }
    Ok(())
}

/// True if `sql` calls `pg_logical_emit_message` (either overload) AND names the
/// reserved `wamn.` message-prefix namespace (l5i9.12.2). Only the plugin's own
/// [`begin_with_claims`] emit — which runs through the plugin, NOT this
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
    env_key: &str,
    schema: Option<&str>,
    runner: Option<&str>,
    role: Option<&str>,
    user_id: Option<&str>,
    operation: Option<&str>,
    application: Option<&str>,
) -> Result<(), PgError> {
    if !valid_env_key(env_key) {
        return Err(PgError::QueryError((
            "WAMN0".to_string(),
            "invalid env_key identity".to_string(),
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
    if let Some(operation) = operation
        && !valid_operation(operation)
    {
        return Err(PgError::QueryError((
            "WAMN0".to_string(),
            "invalid executing operation".to_string(),
        )));
    }
    if let Some(application) = application
        && !valid_application(application)
    {
        return Err(PgError::QueryError((
            "WAMN0".to_string(),
            "invalid executing application".to_string(),
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

/// An `app.operation`: a non-empty operation token or platform component name.
/// The value binds as a parameter, so no charset rule applies. `''` is the
/// unbound value, so it is not a claim.
fn valid_operation(operation: &str) -> bool {
    !operation.is_empty()
}

/// An `app.application`: a non-empty application name. The value binds as a
/// parameter, so no charset rule applies. `''` is the unbound value, so it is
/// not a claim.
fn valid_application(application: &str) -> bool {
    !application.is_empty()
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
    /// Select an explicitly local definition closure without changing database authority.
    pub fn with_local_application(
        mut self,
        application: crate::local_application::LocalApplication,
    ) -> Self {
        self.local_application = Some(Arc::new(application));
        self
    }

    /// Plugin over a single default database (the [`WamnPostgresConfig`]
    /// credentials). Pools are built lazily; `credentials: None` ⇒ every call
    /// returns `connection-unavailable`.
    pub fn new(cfg: WamnPostgresConfig) -> anyhow::Result<Self> {
        Ok(Self::with_provider(Arc::new(
            StaticCredentialProvider::default_only(ProjectConfig::from_global_config(cfg)),
        )))
    }

    /// Plugin over an explicit [`CredentialProvider`] (multi-project / tests).
    pub fn with_provider(provider: Arc<dyn CredentialProvider>) -> Self {
        Self {
            local_application: None,
            provider,
            guest_pools: std::sync::RwLock::new(HashMap::new()),
            platform_pools: std::sync::RwLock::new(HashMap::new()),
            env_keys: std::sync::RwLock::new(HashMap::new()),
            projects: std::sync::RwLock::new(HashMap::new()),
            schemas: std::sync::RwLock::new(HashMap::new()),
            runners: std::sync::RwLock::new(HashMap::new()),
            workload_authorities: std::sync::RwLock::new(HashMap::new()),
            roles: std::sync::RwLock::new(HashMap::new()),
            users: std::sync::RwLock::new(HashMap::new()),
            operations: std::sync::RwLock::new(HashMap::new()),
            applications: std::sync::RwLock::new(HashMap::new()),
            release_identities: std::sync::RwLock::new(HashMap::new()),
            current_run: std::sync::RwLock::new(HashMap::new()),
            invocations: std::sync::RwLock::new(HashMap::new()),
            statement_scopes: std::sync::RwLock::new(StatementScopes::default()),
            operation_workloads: std::sync::RwLock::default(),
            operation_metrics: super::operation::OperationMetrics::default(),
            operation_transactions: std::sync::Mutex::default(),
            destroyed: Arc::new(AtomicU64::new(0)),
            bind_counters: super::BindCounters::default(),
            auth: crate::auth_cache::AuthCache::default(),
        }
    }

    /// The auth facts this host keeps. A message on
    /// [`replicate`](crate::auth_cache::replicate) keeps them in step with
    /// the other hosts.
    pub fn auth_cache(&self) -> &crate::auth_cache::AuthCache {
        &self.auth
    }

    /// Build from the deployment's configuration: the default project from the
    /// credential the COMPOSITION ROOT names.
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
        Ok(Self::with_provider(Arc::new(
            StaticCredentialProvider::default_only(default),
        )))
    }

    /// Build from the deployment's configuration with the composition root's
    /// credential bound to its declared project rather than the default key.
    ///
    /// A per-project serving host already carries the trusted project identity.
    /// Registering that host's Secret under `default` makes every named-project
    /// lookup refuse despite having the exact credential. No other project is
    /// registered, so an unlisted project still refuses at resolution.
    pub fn from_env_for_project(
        project: &str,
        credentials: Option<ClassCredentials>,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            valid_project(project),
            "invalid composed project {project:?}: a lowercase slug [a-z0-9-] of at most 40 bytes, \
             starting and ending alphanumeric, with no consecutive hyphens and not under the \
             reserved `wamn` prefix, is required"
        );
        let cfg = WamnPostgresConfig::from_env();
        let mut projects = HashMap::new();
        if let Some(credentials) = credentials {
            projects.insert(
                project.to_owned(),
                ProjectConfig::from_global(credentials, &cfg),
            );
        }
        Ok(Self::with_provider(Arc::new(
            StaticCredentialProvider::new(projects, None),
        )))
    }

    /// Register the tenant claim for a component id. The bench harness calls
    /// this directly; the host path feeds it from workload bind.
    pub fn set_env_key(&self, component_id: &str, env_key: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            valid_env_key(env_key),
            "invalid env_key {env_key:?}: 1-64 chars of [A-Za-z0-9_-] required"
        );
        self.env_keys
            .write()
            .expect("env_keys lock poisoned")
            .insert(component_id.to_string(), env_key.to_string());
        Ok(())
    }

    pub(super) fn env_key_for(&self, component_id: &str) -> Option<String> {
        self.env_keys
            .read()
            .expect("env_keys lock poisoned")
            .get(component_id)
            .cloned()
    }

    /// Register which project's database a component uses. The bench harness
    /// calls this directly; the host path feeds it from the `wamn.project`
    /// workload config. Absent ⇒ the default project.
    pub fn set_project(&self, component_id: &str, project: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            valid_project(project),
            "invalid project {project:?}: a lowercase slug [a-z0-9-] of at most 40 bytes, \
             starting and ending alphanumeric, with no consecutive hyphens and not under the \
             reserved `wamn` prefix, is required"
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

    /// Register the executing principal's `app.user_id` claim for a component
    /// id. When set, every transaction the plugin opens for that component
    /// binds `app.user_id`, so the record-history triggers stamp that
    /// `users.id`. Host-injected identity like the tenant.
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

    /// Register the `app.operation` claim for a component id. When set, every
    /// transaction the plugin opens for that component binds `app.operation`,
    /// so the record-history log records the source of each write.
    /// Host-injected identity like the tenant.
    pub fn set_operation(&self, component_id: &str, operation: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            valid_operation(operation),
            "invalid executing operation {operation:?}: a non-empty operation is required"
        );
        self.operations
            .write()
            .expect("operations lock poisoned")
            .insert(component_id.to_string(), operation.to_string());
        Ok(())
    }

    pub(super) fn operation_for(&self, component_id: &str) -> Option<String> {
        self.operations
            .read()
            .expect("operations lock poisoned")
            .get(component_id)
            .cloned()
    }

    /// Register the `app.application` claim for a component id. When set,
    /// every transaction the plugin opens for that component binds
    /// `app.application`, the declared application that runs the operation.
    /// Host-injected identity like the tenant.
    pub fn set_application(&self, component_id: &str, application: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            valid_application(application),
            "invalid executing application {application:?}: a non-empty application name is required"
        );
        self.applications
            .write()
            .expect("applications lock poisoned")
            .insert(component_id.to_string(), application.to_string());
        Ok(())
    }

    pub(super) fn application_for(&self, component_id: &str) -> Option<String> {
        self.applications
            .read()
            .expect("applications lock poisoned")
            .get(component_id)
            .cloned()
    }

    /// Register the release this pod carries for a component id. The production
    /// claim verifies its digest against the admission pin of every run it
    /// leases. The bench harness and live tests call this directly; the host
    /// path feeds it from the loaded
    /// [`LoadedRelease`](wamn_engine::release_manifest::LoadedRelease), whose
    /// digest is derived from verified manifest content. Absent leaves the claim
    /// verifying nothing.
    ///
    /// The digest arrives as [`ManifestDigest`], so its shape is already validated.
    ///
    /// # Why effect authority needs no equality check against this record
    ///
    /// The digest comes from the same loaded object every reader resolves
    /// against, so a run leased under it runs the manifest the pod loaded —
    /// structurally, not because anything compares them (owner ruling
    /// `wamn-0h0g.15.102`, after `wamn-0h0g.15.103` struck the asserted carrier).
    pub fn set_release_identity(
        &self,
        component_id: &str,
        identity: ReleaseIdentity,
    ) -> anyhow::Result<()> {
        self.release_identities
            .write()
            .expect("release identities lock poisoned")
            .insert(component_id.to_string(), identity);
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
    /// component is driving (l5i9.12.2). While set, each write transaction the plugin
    /// opens for the component carries a `wamn.causation` message. Host-injected
    /// like the other claim registries — callers set it directly, exactly like
    /// [`set_env_key`](Self::set_env_key) / [`set_runner`](Self::set_runner).
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
        // Read-only calls produce no row events. Emitting a logical message would
        // write WAL and invalidate the weak ETag of the read itself.
        if self.active_read(component_id) {
            return None;
        }
        self.current_run
            .read()
            .expect("current_run lock poisoned")
            .get(component_id)
            .cloned()
    }

    /// Bind the exact invocation facts before entering one pooled component
    /// (`wamn-0h0g.7.9`), so a postgres effect can name the wiring and node
    /// position it was raised under.
    ///
    /// A still-bound owner refuses rather than silently replacing leaked
    /// state — the rule the HTTP and blobstore registries hold, for the same
    /// reason: a stale invocation would label the next guest's effects with the
    /// previous one's wiring position.
    pub fn bind_invocation(
        &self,
        component_id: &str,
        invocation: ConnectionInvocation,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(!component_id.is_empty(), "component id must be present");
        let mut invocations = self
            .invocations
            .write()
            .map_err(|_| anyhow::anyhow!("postgres invocation registry is poisoned"))?;
        anyhow::ensure!(
            !invocations.contains_key(component_id),
            "component {component_id} still holds a bound postgres invocation"
        );
        invocations.insert(component_id.to_owned(), invocation);
        Ok(())
    }

    /// Release the invocation when the instance returns to the pool.
    pub fn revoke_invocation(&self, component_id: &str) {
        if let Ok(mut invocations) = self.invocations.write() {
            invocations.remove(component_id);
        }
    }

    /// The invocation currently bound to one pooled instance.
    #[must_use]
    pub fn invocation(&self, component_id: &str) -> Option<ConnectionInvocation> {
        self.invocations
            .read()
            .ok()
            .and_then(|invocations| invocations.get(component_id).cloned())
    }

    /// Inspect active invocation scopes for cross-crate lifecycle tests.
    #[cfg(feature = "test-util")]
    pub fn invocation_scopes_for_test(&self) -> Vec<String> {
        self.invocations
            .read()
            .expect("invocation registry lock")
            .keys()
            .cloned()
            .collect()
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
    ///
    /// # The executing principal must own a user row (`wamn-0h0g.9.19`)
    ///
    /// A bind that names an `app.user_id` refuses when the principal owns no
    /// `platform."user"` row in the tenant's project database. The credential
    /// of such a principal is a provisioning defect, and every write it makes
    /// records an actor that names no row. The host's
    /// [auth memory](crate::auth_cache) answers first: the route authenticator
    /// read the same row a moment before. On a miss the bind reads the row and
    /// keeps the answer. An item that begins its own transaction binds twice,
    /// and its second bind is
    /// [`bind_item_session_claims`](Self::bind_item_session_claims), which
    /// does not check again.
    ///
    /// On a host that watches the auth bucket, a user row deleted after the
    /// check stays accepted until a change message clears the memory, or
    /// until the memory's TTL ends if the message is lost. A host without
    /// the bucket keeps no answer and reads the row on every bind.
    ///
    /// The read runs LAST, after every claim is written and validated, so a
    /// bind that is going to be refused for a malformed claim never opens a
    /// connection. A refused bind leaves the claim maps half-written, which is
    /// the contract above: the caller destroys the instance.
    ///
    /// A platform principal carries its own `wamn:<component>` row, which
    /// provisioning writes, so it binds like any other principal.
    pub async fn bind_session_claims(
        &self,
        component_id: &str,
        claims: &SessionClaims,
    ) -> anyhow::Result<()> {
        self.write_session_claims(component_id, claims)?;
        // An acquisition that names no principal reads nothing. The
        // record-history triggers already refuse such a write with
        // actor-required.
        if let Some(user_id) = claims.user_id.as_deref() {
            let project = claims.project.as_deref().unwrap_or(DEFAULT_PROJECT);
            self.require_provisioned_principal(&claims.env_key, project, user_id)
                .await?;
        }
        Ok(())
    }

    /// Bind `claims` as [`bind_session_claims`](Self::bind_session_claims)
    /// does, without its principal read, for the call inside an item whose
    /// transaction a bind of the same claims began (`wamn-uq4r`).
    ///
    /// The item's bind read the principal's user row moments before, in the
    /// same invocation, so a second read answers nothing new and takes a
    /// second guest connection. The caller passes the claims of that bind.
    pub fn bind_item_session_claims(
        &self,
        component_id: &str,
        claims: &SessionClaims,
    ) -> anyhow::Result<()> {
        self.write_session_claims(component_id, claims)
    }

    fn write_session_claims(
        &self,
        component_id: &str,
        claims: &SessionClaims,
    ) -> anyhow::Result<()> {
        // An acquisition starts with no statement authority. The router
        // activates the selected operation only after claims bind.
        self.revoke_statement_operation(component_id);
        self.set_env_key(component_id, &claims.env_key)?;
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
        match claims.operation.as_deref() {
            Some(operation) => self.set_operation(component_id, operation)?,
            None => drop(
                self.operations
                    .write()
                    .expect("operations lock poisoned")
                    .remove(component_id),
            ),
        }
        match claims.application.as_deref() {
            Some(application) => self.set_application(component_id, application)?,
            None => drop(
                self.applications
                    .write()
                    .expect("applications lock poisoned")
                    .remove(component_id),
            ),
        }
        match claims.release.as_ref() {
            Some(release) => self.set_release_identity(component_id, release.clone())?,
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

    /// Read one `platform."user"` row and refuse the principal that owns none
    /// (`wamn-0h0g.9.19`).
    ///
    /// The read uses the tenant's own guest login, for two reasons. It is the
    /// only authority class every composition root names, because the platform
    /// classes are unnamed when their generation is absent. And it is the
    /// credential that already holds `SELECT` on this table, so the check
    /// needs no new grant. The database is the org boundary.
    ///
    /// A read that cannot run refuses the bind with its own error rather than
    /// the `UnprovisionedPrincipal` refusal. An unreachable table is an
    /// infrastructure defect and not an authorization fact, and the bind must
    /// fail closed either way.
    async fn require_provisioned_principal(
        &self,
        env_key: &str,
        project: &str,
        principal_id: &str,
    ) -> anyhow::Result<()> {
        let Err(ticket) = self.auth.provisioned(env_key, principal_id) else {
            return Ok(());
        };
        let (connection, _policy) = self
            .checkout_guest(project, env_key)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))
            .context("check out the env_key connection that reads platform.\"user\"")?;
        let statement = match connection.prepare_cached(PROVISIONED_PRINCIPAL_SQL).await {
            Ok(statement) => statement,
            Err(error) => {
                self.destroy(connection);
                return Err(error).context("prepare the provisioned-principal read");
            }
        };
        let rows = connection
            .query(&statement, &[&principal_id])
            .instrument(tracing::info_span!("wamn.claims.principal.query"))
            .await
            .context("read the executing principal's platform.\"user\" row")?;
        if rows.is_empty() {
            tracing::warn!(
                env_key,
                project,
                principal_id,
                "wamn:postgres: refusing a bind for a principal with no platform.\"user\" row"
            );
            return Err(UnprovisionedPrincipal {
                env_key: env_key.to_owned(),
                project: project.to_owned(),
                principal_id: principal_id.to_owned(),
            }
            .into());
        }
        self.auth
            .keep_provisioned(env_key, principal_id, ticket)
            .await;
        Ok(())
    }

    /// Read back the claim set one component id currently resolves to.
    ///
    /// `None` is the deny floor: no tenant is bound, so
    /// [`require_env_key`](Self::require_env_key) refuses every call made under
    /// this id. That is what an idle, never-acquired execution instance must
    /// report, and it is the read half of the checkout-identity seam — the pair
    /// this and [`bind_session_claims`](Self::bind_session_claims) form is what
    /// lets a test assert that two concurrent acquisitions never share one
    /// identity.
    pub fn session_claims(&self, component_id: &str) -> Option<SessionClaims> {
        Some(SessionClaims {
            env_key: self.env_key_for(component_id)?,
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
            operation: self.operation_for(component_id),
            application: self.application_for(component_id),
            release: self.release_identity_for(component_id),
        })
    }

    /// Clear every registration [`bind_session_claims`](Self::bind_session_claims)
    /// installed under one component id.
    ///
    /// Called when a checkout ends, on every path — repooled, retired, or
    /// dropped. An idle instance that still resolved a tenant would be a warm
    /// store carrying identity, which is the whole defect this seam closes; and
    /// a `require_env_key` failure is how an unbound instance fails closed
    /// instead of serving someone else's rows.
    pub fn revoke_session_claims(&self, component_id: &str) {
        self.revoke_statement_operation(component_id);
        self.env_keys
            .write()
            .expect("env_keys lock poisoned")
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
        self.operations
            .write()
            .expect("operations lock poisoned")
            .remove(component_id);
        self.applications
            .write()
            .expect("applications lock poisoned")
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
    /// the caller's role / user id, the executing operation and application, the carried
    /// release identity, the causation run context, and the bound invocation —
    /// all set at workload bind (or via the runner or driver channel) and keyed
    /// by component id.
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
        self.env_keys
            .write()
            .expect("env_keys lock poisoned")
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
        self.operations
            .write()
            .expect("operations lock poisoned")
            .retain(|c, _| retain(c));
        self.applications
            .write()
            .expect("applications lock poisoned")
            .retain(|c, _| retain(c));
        self.release_identities
            .write()
            .expect("release identities lock poisoned")
            .retain(|c, _| retain(c));
        self.current_run
            .write()
            .expect("current_run lock poisoned")
            .retain(|c, _| retain(c));
        if let Ok(mut invocations) = self.invocations.write() {
            invocations.retain(|c, _| retain(c));
        }
        self.operation_workloads
            .write()
            .expect("operation workloads lock poisoned")
            .retain(|c, _| retain(c));
        self.clear_statement_bindings(workload_id);
    }

    /// Share the host reader with local binding readiness and dispatch.
    pub(crate) fn set_connection_credentials(
        &self,
        credentials: Arc<crate::plugins::wamn_credentials::WamnCredentials>,
        project: &str,
    ) {
        if let Some(local) = &self.local_application {
            local.set_connection_credentials(credentials, project);
        }
    }

    /// Load every host-derived HTTP authorization input in one read-only
    /// transaction under the component's injected tenant.
    ///
    /// # Why the caller's tenant is checked against the registry (wamn-0h0g.17.7)
    ///
    /// `env_key` arrives from `ConnectionHttp`, which froze it into a `Box<str>`
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
    /// [`require_env_key`](Self::require_env_key) applies to every other read:
    /// nothing resolves under an unbound claim scope.
    pub async fn connection_effect_snapshot(
        &self,
        component_id: &str,
        project: &str,
        env_key: &str,
        lookup: &ConnectionEffectLookup<'_>,
    ) -> anyhow::Result<Option<ConnectionEffectSnapshot>> {
        let registered = self.env_key_for(component_id).ok_or_else(|| {
            anyhow::anyhow!(
                "HTTP effect authorization for component {component_id:?} has no bound env_key \
                 claim; refusing to resolve under an unbound identity"
            )
        })?;
        anyhow::ensure!(
            registered == env_key,
            "HTTP effect authorization env_key {env_key:?} disagrees with the env_key bound \
             to component {component_id:?}; refusing to resolve under a stale identity"
        );
        let schema = self.schema_for(component_id);
        let (conn, policy) = self
            .checkout_platform(project, AuthorityClass::CallableHttp)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        // This read writes nothing, so it binds no `app.user_id` and no
        // `app.operation`.
        if let Err(error) = self
            .begin_with_claims(
                &conn,
                AuthorityClass::CallableHttp,
                false,
                env_key,
                schema.as_deref(),
                None,
                None,
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
            if let Some(local) = &self.local_application {
                return local.effect_snapshot(&conn, env_key, lookup).await;
            }
            let candidate_instance = lookup
                .candidate_binding
                .map(|binding| binding.instance_id.as_str());
            let candidate_generation = lookup.candidate_binding.map(|binding| binding.generation);
            let wiring_id = lookup.entry.wiring.map(|wiring| wiring.wiring_id);
            let wiring_version = lookup.entry.wiring.map(|wiring| wiring.wiring_version);
            let node_id = lookup.entry.wiring.map(|wiring| wiring.node_id);
            let params: [&(dyn ToSql + Sync); 18] = [
                &env_key,
                &lookup.package_id,
                &lookup.manifest_digest,
                &lookup.environment,
                &wiring_id,
                &wiring_version,
                &node_id,
                &lookup.component_digest,
                &lookup.store_alias,
                &candidate_instance,
                &candidate_generation,
                &lookup.entry.package_id,
                &lookup.origin_package_id,
                &lookup.origin_component_digest,
                &lookup.origin_component,
                &lookup.origin_interface_version,
                &lookup.origin_operation,
                &lookup.operation,
            ];
            let statement = conn
                .prepare_cached(CONNECTION_EFFECT_SNAPSHOT_SQL)
                .await
                .context("prepare HTTP effect authorization snapshot")?;
            let row = conn
                .query_opt(&statement, &params)
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
                read_only: row.try_get::<_, Option<String>>(21)?.as_deref() != Some("write"),
                connection_name: row.try_get(22)?,
                wiring_hash: json(0)?
                    .map(|graph| {
                        wamn_catalog::WiringDocument::parse(&graph)
                            .map(|graph| graph.wiring_hash().as_str().to_owned())
                            .context("validate the released wiring graph")
                    })
                    .transpose()?,
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
                pinned_generation: candidate_generation,
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
                if let Err(error) = transactions::finish_transaction(&conn, "COMMIT").await {
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

    /// The grants of the signed roles of a verified session that the active
    /// user still holds, from [`principal_grants`](Self::principal_grants).
    pub async fn session_operation_permissions(
        &self,
        project: &str,
        env_key: &str,
        roles: &[String],
        principal: &wamn_platform_identity::PrincipalId,
    ) -> anyhow::Result<HeldOperationGrants> {
        let grants = self.principal_grants(project, env_key, principal).await?;
        session_grants(&grants, roles)
    }

    /// The current WAL position used by weak ETags.
    ///
    /// The shared WAL position covers commits from every writer without a counter.
    /// Unrelated writes can invalidate the tag too. Read it before the query snapshot.
    pub async fn model_version(&self, project: &str) -> anyhow::Result<String> {
        anyhow::ensure!(valid_project(project), "invalid model-version project");
        let (connection, _policy) = self
            .checkout_platform(project, AuthorityClass::CallableHttp)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let statement = connection
            .prepare_cached("SELECT pg_current_wal_lsn()::text")
            .await
            .context("prepare the WAL position for model versions")?;
        let row = connection
            .query_one(&statement, &[])
            .await
            .context("read the WAL position for model versions")?;
        Ok(row.try_get(0)?)
    }

    /// The grants every role of an active org-issued user gives it in this
    /// tenant, from [`principal_grants`](Self::principal_grants).
    ///
    /// Route authentication first requires membership in the release environment.
    pub async fn user_operation_permissions(
        &self,
        project: &str,
        env_key: &str,
        principal_id: &wamn_platform_identity::PrincipalId,
    ) -> anyhow::Result<HeldOperationGrants> {
        let grants = self
            .principal_grants(project, env_key, principal_id)
            .await?;
        Ok(held_grants(&grants, None))
    }

    /// The user row, roles and permissions of one principal, from the host's
    /// memory or, on a miss, from one read under the callable-HTTP authority
    /// with no session claims. The read also answers the provisioned-principal
    /// check of the claim bind.
    pub async fn principal_grants(
        &self,
        project: &str,
        env_key: &str,
        principal: &wamn_platform_identity::PrincipalId,
    ) -> anyhow::Result<crate::auth_cache::PrincipalGrants> {
        anyhow::ensure!(
            valid_project(project),
            "invalid operation-permission project"
        );
        anyhow::ensure!(
            valid_env_key(env_key),
            "invalid operation-permission tenant"
        );
        let ticket = match self.auth.grants(env_key, principal.as_str()) {
            Ok(grants) => return Ok(grants),
            Err(ticket) => ticket,
        };
        let (connection, _policy) = self
            .checkout_platform(project, AuthorityClass::CallableHttp)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let statement = match connection.prepare_cached(PRINCIPAL_GRANTS_SQL).await {
            Ok(statement) => statement,
            Err(error) => {
                self.destroy(connection);
                return Err(error).context("prepare the principal grants");
            }
        };
        let rows = connection
            .query(&statement, &[&principal.as_str()])
            .instrument(tracing::info_span!("wamn.auth.perm.query"))
            .await
            .context("read the principal grants")?;
        let grants = principal_grants_from_rows(&rows).context("decode the principal grants")?;
        self.auth
            .keep_grants(env_key, principal.as_str(), ticket, grants.clone())
            .await;
        Ok(grants)
    }

    /// Resolve the projected name of a connection principal from its identity subject.
    pub async fn connection_principal_name(
        &self,
        project: &str,
        subject: &str,
    ) -> anyhow::Result<Option<String>> {
        let Some(id) = subject.strip_prefix("connection-") else {
            return Ok(None);
        };
        let id = id
            .parse::<uuid::Uuid>()
            .context("invalid connection principal subject")?
            .to_string();
        let (client, _) = self
            .checkout_platform(project, AuthorityClass::CallableHttp)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        Ok(client
            .query_opt(
                "SELECT name FROM platform.connection WHERE id=$1::text::uuid",
                &[&id],
            )
            .await?
            .map(|row| row.get(0)))
    }

    /// The application roles and stored permissions a caller holds, read by a
    /// host route (docs/architecture/execution.md#host-run-routes) in an
    /// [administration transaction](Self::administration_transaction).
    pub async fn held_operation_grants(
        &self,
        project: &str,
        env_key: &str,
        principal_id: &wamn_platform_identity::PrincipalId,
        operation: &str,
    ) -> anyhow::Result<HeldOperationGrants> {
        let rows = self
            .administration_transaction(project, env_key, principal_id, operation, async |client| {
                client
                    .query(USER_OPERATION_PERMISSIONS_SQL, &[&principal_id.as_str()])
                    .await
                    .context("read held operation grants")
            })
            .await??;
        HeldOperationGrants::from_rows(&rows).context("decode held operation grant")
    }

    /// Run `work` in a host-owned READ COMMITTED transaction under the
    /// administration credential of `project`, for a host route
    /// (docs/architecture/execution.md#host-run-routes). The transaction binds
    /// the caller and the route's sealed operation id as `app.user_id` and
    /// `app.operation`, so every row it writes is stamped with them. It commits
    /// when `work` returns `Ok` and rolls back when `work` returns `Err`, whose
    /// value is the inner result. The outer error is a failure of the
    /// transaction itself. Whether the environment of `env_key` is inactive,
    /// from its row of `platform.environment`, read as `wamn_app`
    /// (docs/architecture/execution.md#routing-and-availability). No row means
    /// active. The host keeps the answer in its [auth
    /// memory](crate::auth_cache).
    pub async fn environment_inactive(&self, project: &str, env_key: &str) -> anyhow::Result<bool> {
        anyhow::ensure!(valid_project(project), "invalid environment-status project");
        anyhow::ensure!(valid_env_key(env_key), "invalid environment-status tenant");
        let ticket = match self.auth.environment_inactive(env_key) {
            Ok(inactive) => return Ok(inactive),
            Err(ticket) => ticket,
        };
        let (connection, _pool) = self
            .checkout_guest(project, env_key)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        let statement = connection
            .prepare_cached(ENVIRONMENT_INACTIVE_SQL)
            .await
            .context("prepare the environment status")?;
        let row = match connection.query_opt(&statement, &[]).await {
            Ok(row) => row,
            Err(error) => {
                self.destroy(connection);
                return Err(error).context("read the environment status");
            }
        };
        let inactive = row.is_some_and(|row| row.get(0));
        self.auth
            .keep_environment_inactive(env_key, ticket, inactive)
            .await;
        Ok(inactive)
    }

    pub async fn administration_transaction<T, E>(
        &self,
        project: &str,
        env_key: &str,
        principal_id: &wamn_platform_identity::PrincipalId,
        operation: &str,
        work: impl AsyncFnOnce(&tokio_postgres::Client) -> Result<T, E>,
    ) -> anyhow::Result<Result<T, E>> {
        anyhow::ensure!(valid_project(project), "invalid host-route project");
        anyhow::ensure!(valid_env_key(env_key), "invalid host-route tenant");
        let (connection, policy) = self
            .checkout_platform(project, AuthorityClass::Administration)
            .await
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
        if let Err(error) = self
            .begin_with_claims(
                &connection,
                AuthorityClass::Administration,
                false,
                env_key,
                None,
                None,
                None,
                Some(principal_id.as_str()),
                Some(operation),
                None,
                None,
                policy.statement_timeout_ms,
            )
            .await
        {
            self.destroy(connection);
            return Err(anyhow::anyhow!(error.to_string()));
        }
        let result = work(&connection).await;
        let end = if result.is_ok() { "COMMIT" } else { "ROLLBACK" };
        if let Err(error) = transactions::finish_transaction(&connection, end).await {
            self.destroy(connection);
            return Err(error).context("end the host route transaction");
        }
        Ok(result)
    }

    /// Refresh the identity and permissions of the principal of a queued run,
    /// with `role` active or, without one, with every role it holds
    /// ([`queued_run_caller`](crate::plugins::route_authentication::queued_run_caller)).
    pub async fn queued_run_caller(
        &self,
        project: &str,
        env_key: &str,
        principal_id: &str,
        role: Option<&str>,
    ) -> anyhow::Result<wamn_engine::flow_http_routing::AuthenticatedCaller> {
        anyhow::ensure!(valid_project(project), "invalid queued-run project");
        anyhow::ensure!(valid_env_key(env_key), "invalid queued-run tenant");
        let (connection, _policy) = self
            .checkout_platform(project, AuthorityClass::CallableHttp)
            .await?;
        crate::plugins::route_authentication::queued_run_caller(&**connection, principal_id, role)
            .await
    }

    /// The caller of one node of a workflow run that executes as `principal_id`
    /// with `role` active, from the grants the principal holds now, read
    /// through the host's auth memory as a request reads them
    /// (docs/architecture/execution.md#environment-workflows). None when the
    /// principal is inactive or no longer holds `role`.
    pub async fn workflow_node_caller(
        &self,
        project: &str,
        env_key: &str,
        principal_id: &str,
        role: &str,
    ) -> anyhow::Result<Option<wamn_engine::flow_http_routing::AuthenticatedCaller>> {
        let principal: wamn_platform_identity::PrincipalId = principal_id
            .parse()
            .context("the run's principal is not a principal id")?;
        let grants = self.principal_grants(project, env_key, &principal).await?;
        Ok(grants
            .active
            .then(|| {
                crate::plugins::route_authentication::role_caller(
                    principal.as_str(),
                    role,
                    &grants.roles,
                )
            })
            .flatten())
    }

    pub(super) fn require_env_key(&self, component_id: &str) -> Result<String, PgError> {
        self.env_key_for(component_id).ok_or_else(|| {
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

impl AsRef<crate::auth_cache::AuthCache> for WamnPostgres {
    fn as_ref(&self) -> &crate::auth_cache::AuthCache {
        &self.auth
    }
}

#[cfg(test)]
pub(in crate::plugins::wamn_postgres) mod tests;
