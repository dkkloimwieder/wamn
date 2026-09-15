//! The `provision-project-env` subcommand (wamn-q3n.7): stand up one
//! per-project-env Postgres **database** on an org's appropriate cluster (or the
//! T3 trials pool) and record it in the T1 control-plane registry.
//!
//! Identity is the `(org, project, env)` [`Triple`], and the database lives on
//! the cluster **derived** by
//! [`cluster_of`](wamn_control_registry::cluster_of) (D18) from the org's placement + the
//! env's policy — a dedicated org's `<org>-<owner(env)>` (so `canary` sharing prod
//! lands on `<org>-prod`, `canary` own on `<org>-canary`), or the shared pool for a
//! pooled org. One derivation path serves every placement.
//!
//! An imperative CLI (the `provision-org` precedent). It **renders + records**;
//! the runbook/Job applies the emitted artifacts, in this order:
//!
//! 1. the `wamn_db_owner` title role must exist **before** the `Database` CR
//!    (its `owner`), and the stable `wamn_app` + `wamn_dispatch_reader` ACL
//!    roles with it — both NOLOGIN grant carriers, neither a credential: apply
//!    the emitted
//!    **role SQL** to the target cluster's superuser. Applying the CR first
//!    fails reconciliation — CNPG maps `spec.owner` straight to `CREATE DATABASE
//!    … OWNER` / `ALTER DATABASE … OWNER TO`;
//! 2. `kubectl apply -f` the emitted **`Database` CR** and wait it applied — the
//!    CNPG operator declaratively creates the database owned by `wamn_db_owner`,
//!    and re-owns an already-existing one to it;
//! 3. apply the emitted **privilege SQL** (`ALTER DATABASE … OWNER TO
//!    wamn_db_owner`, then `REVOKE CONNECT, TEMPORARY FROM PUBLIC` / `REVOKE
//!    CONNECT FROM wamn_app` / `REVOKE CONNECT FROM wamn_dispatch_reader`) — the
//!    thin imperative step the `Database` CRD does
//!    not cover (topology fact 3), run **after** the database exists. The owner
//!    statement is first and must stay first: `ALTER DATABASE … OWNER TO`
//!    rewrites the outgoing owner's ACL entry, which is where a `CONNECT`
//!    granted to a role that still owns the database merges.
//!    **On an EXISTING environment this step is mandatory, not optional**: it
//!    is what converges a pre-`wamn-0h0g.22.6` environment's `CONNECT` off the
//!    stable `wamn_app` ACL role, and step 4's generation actions refuse to run
//!    until it has (`wamn-0h0g.12.179`);
//! 4. `kubectl apply -f` the emitted **credential Secret** and any independently
//!    requested management-author / route-caller PAT Secrets, then run each
//!    family's generation prepare — the LOGIN it mints is what actually reaches
//!    the database. The stable ACL roles do not: they are NOLOGIN grant carriers
//!    those generations inherit, and every one of them must stay connection-free
//!    (see [`privilege_sql`]). `wamn-0h0g.22.24` moved the last holdout,
//!    `wamn_dispatch_reader`, onto that shape.
//!
//! What this tool does directly (given `--system-database-url`): read the org's
//! placement to pick the target cluster, and record `registry.projects` +
//! `registry.project_envs` (as the `wamn_system` owner); when requested, resolve
//! or create stable service principals and assign project roles. The CLI requests
//! PATs from `wamn-identity` over HTTPS with an operator client certificate, then
//! authenticates each PAT against the system database. Kubernetes artifacts are
//! only emitted (no K8s client, no target-cluster connection).
//!
//! **RLS floor** at provision time: there are no tables yet, so wamn-q3n.7
//! establishes the RLS-**enforceable substrate** only — `wamn_app` is
//! `NOSUPERUSER NOCREATEDB NOBYPASSRLS` (the role SQL) and `CONNECT` is confined
//! (the privilege SQL). The per-table `FORCE ROW LEVEL SECURITY` floor is applied
//! at catalog-publish (2.4/2.5), where the tables are created.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{File, OpenOptions, Permissions};
#[cfg(test)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Context as _;
use chrono::{DateTime, SecondsFormat, Utc};
use ring::rand::SystemRandom;
use serde_json::{Value, json};
use tokio_postgres::{Client, Config as PgConfig, GenericClient, NoTls, Transaction};
use url::Url;

use wamn_control_provision::SystemReader;
use wamn_control_provision::session_target::{SessionTarget, validate_session_tenant_id};
use wamn_control_provision::tenant_key::tenant_key;
use wamn_control_provision::{
    APP_ROLE, CredentialGeneration, EffectWriterCredentialScope, EffectWriterCredentialValidity,
    INSTANCE_SUFFIX_LEN, PLATFORM_GROUP_ROLE, PlatformComponent, WorkloadRoleFamily,
    WorkloadRoleScope, WorkloadRoleScopeKind, WorkloadSecretBody, WorkloadSecretBodyKind,
    bind_platform_principal_sql, compose_url, effect_writer_credential,
    legacy_effect_writer_generation_role, project_env_database_name, project_env_namespace,
    project_env_secret_name, render_project_env_database, render_project_env_secret_manifest,
    render_workload_secret_manifest, sql, validate_instance_suffix, validate_project_env,
    workload_generation_role,
};
use wamn_control_registry::{Org, Placement, Triple, cluster_of};
use wamn_pg_core::quote_ident;
use wamn_platform_identity::{
    IdentityErrorKind, Principal, PrincipalKind, PrincipalStatus, assign_project_role,
    authenticate_pat, create_service, resolve_subject, revoke_pat, route_caller_subject,
};

use crate::env_policies::{ensure_env_policy_durability_schema, read_env_policy};
use crate::pat_client::{PatClient, PatIssuerConfig};

mod database;
mod grants;
mod output;
mod pat_secrets;
mod registry;
mod workload;

use database::{
    connect_config, exact_project_database_config, named_database_config, workload_config,
    workload_url,
};
use pat_secrets::issue_pat_secrets;
use registry::{mint_instance_suffix, record_project_env};

pub(crate) use output::write_output;
pub use output::{
    ProvisionedRoute, ensure_distinct_secret_paths, ensure_secret_path, read_json,
    secret_annotation, secret_value, write_secret_json,
};
pub use pat_secrets::{IssuedPatSecret, parse_pat_prefix, revoke_provisioning_pat};
pub use registry::{
    claim_environment_instance, project_tenant_environment, read_project_env_instance,
    resolve_cluster,
};
pub use workload::run_workload_action;

#[cfg(test)]
use output::SECRET_TEMP_SEQUENCE;
#[cfg(test)]
use pat_secrets::{MANAGEMENT_AUTHOR, PAT_TTL, ROUTE_CALLER, render_pat_secret};

/// Inputs of one project-environment provisioning.
///
/// An artifact path that is absent or `-` is not written; the outcome carries
/// every rendered artifact.
#[derive(Debug)]
pub struct ProvisionProjectEnvRequest {
    /// Org id (must already be registered — `provision-org`, or the T3 pool for a
    /// trials org). Names the target cluster and the `wamn-db-<org>--…` database.
    pub org: String,

    /// Project id: a lowercase slug `[a-z0-9-]` (start/end alphanumeric). The
    /// reserved `wamn` prefix is rejected.
    pub project: String,

    /// Environment slug: any policy in the ORG's `registry.env_policies` set.
    /// Derives the target cluster via `cluster_of`.
    pub env: String,

    /// Tenant identity for the environment's control-store projection. It is
    /// never inferred from the project or environment.
    pub tenant: Option<String>,

    /// Mark this environment DISPOSABLE: its admitted component facts may be
    /// REPLACED rather than frozen (wamn-10yt.38). The marker is recorded on the
    /// environment's registry row and projected into the control store.
    pub disposable: bool,

    /// Superuser Postgres URL to the T1 system DB (`wamn_system`): read the org's
    /// placement, read-or-mint the stored instance suffix, and record the project
    /// + project-env.
    pub system_database_url: Option<String>,

    /// Target CNPG `Cluster` name. When absent, it is read from the org's
    /// placement in the registry.
    pub cluster: Option<String>,

    /// Per-project-env `CONNECTION LIMIT`. Absent means no limit (`-1`).
    pub connection_limit: Option<i64>,

    /// Password carried by the legacy shared-app URL surface. It never reaches
    /// role SQL (`wamn-0h0g.12.140`).
    pub app_password: String,

    /// Host the runtime reaches the project-env database at. Defaults to the
    /// target cluster's read-write service `<cluster>-rw`.
    pub app_host: Option<String>,

    /// Port the runtime reaches the database at.
    pub app_port: u16,

    /// Namespace the rendered `Database` CR + `Secret` are applied to.
    pub namespace: String,

    /// Secret namespace to RECORD in the registry `SecretRef`. Absent records
    /// `NULL` (the resolving component's own namespace).
    pub secret_namespace: Option<String>,

    /// Write the CNPG `Database` CR (JSON) here.
    pub emit_database: Option<PathBuf>,

    /// Write the role-ensure SQL here.
    pub emit_role_sql: Option<PathBuf>,

    /// Write the privilege SQL here.
    pub emit_privilege_sql: Option<PathBuf>,

    /// Write the database credential `Secret` (JSON) here. It must name a file.
    pub emit_secret: PathBuf,

    /// Operator authentication for PAT issuance through `wamn-identity`.
    pub pat_issuer: PatIssuerConfig,

    /// Issue a management-author PAT and write its Kubernetes `Secret` JSON here.
    pub emit_management_author_pat_secret: Option<PathBuf>,

    /// Issue a route-caller PAT and write its Kubernetes `Secret` JSON here.
    pub emit_route_caller_pat_secret: Option<PathBuf>,
}

/// The names and rendered artifacts of one provisioned project-env.
#[derive(Debug)]
pub struct ProvisionProjectEnvOutcome {
    pub triple: Triple,
    /// The environment's stored instance suffix.
    pub instance: String,
    pub database: String,
    pub cluster: String,
    /// The environment namespace the stored instance suffix names.
    pub namespace: String,
    pub database_cr: Value,
    pub role_sql: String,
    pub privilege_sql: String,
    /// The PAT Secrets issued and written, in issuance order.
    pub pat_secrets: Vec<IssuedPatSecret>,
}

/// The three verbs of the `wamn-0h0g.13.59` unified generation lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkloadActionVerb {
    Prepare,
    Retire,
    Abort,
}

impl WorkloadActionVerb {
    pub const ALL: [Self; 3] = [Self::Prepare, Self::Retire, Self::Abort];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Prepare => "prepare",
            Self::Retire => "retire",
            Self::Abort => "abort",
        }
    }
}

/// One selected workload-generation action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkloadGenerationAction {
    pub family: WorkloadRoleFamily,
    pub verb: WorkloadActionVerb,
    pub generation: CredentialGeneration,
}

/// Inputs of one workload-generation action.
#[derive(Debug)]
pub struct WorkloadActionRequest {
    pub org: String,
    pub project: String,
    pub env: String,

    /// Tenant identity for tenant-scoped workload credential generations.
    /// It is never inferred from the project or environment.
    pub tenant: Option<String>,

    /// Superuser Postgres URL to the T1 system DB (`wamn_system`).
    pub system_database_url: Option<String>,

    /// Explicit target project-database admin URL for the families that address
    /// the project-env database. Provisioning authority only: never persisted or
    /// emitted.
    pub target_admin_database_url: Option<String>,

    /// Namespace the credential `Secret` is applied to.
    pub namespace: String,

    pub action: WorkloadGenerationAction,

    /// Write the prepared credential `Secret` here. A prepare requires it.
    pub secret: Option<PathBuf>,

    /// Write the shared App-login retirement role SQL here. Only App prepare may
    /// name it.
    pub emit_role_sql: Option<PathBuf>,
}

/// What one workload-generation action did.
#[derive(Debug)]
pub enum WorkloadActionOutcome {
    /// The generation was prepared and authenticated, and its `Secret` written.
    Prepared {
        secret: PathBuf,
        /// The shared App-login retirement role SQL, when the App prepare named
        /// an output for it.
        app_retirement_role_sql: Option<String>,
    },
    Retired,
    Aborted,
}

/// `--<verb>-<family>-generation`, the flag one family's one verb answers to.
pub fn workload_action_flag(family: WorkloadRoleFamily, verb: WorkloadActionVerb) -> String {
    format!("{}-{}-generation", verb.as_str(), family.cli_stem())
}

/// `--emit-<family>-secret`, the path one family's prepared credential is
/// written to.
pub fn workload_secret_flag(family: WorkloadRoleFamily) -> String {
    format!("emit-{}-secret", family.cli_stem())
}

/// The role batch the runbook applies to the TARGET cluster's superuser before
/// the `Database` CR (step 1). Both `wamn_app` and `wamn_db_owner` precede the
/// CR because `wamn_db_owner` is its `spec.owner` and the CR cannot reconcile
/// against a role that does not exist yet; `wamn_dispatch_reader` is here
/// (wamn-0h0g.12.122) because it is cluster-global exactly as they are, and
/// because the reconcile step's read-surface grants name it.
///
/// It takes NO dispatch-reader password any more (`wamn-0h0g.22.24`). The role
/// is minted by the generic ACL-role builder as a connection-free NOLOGIN grant
/// carrier; the dispatcher's credential is a GENERATION, and a generation's
/// password is CREATED by its own prepare action rather than handed to
/// provisioning on a flag.
///
/// The app-password parameter remains for the legacy URL surface owned
/// by `wamn-xv69`, but [`sql::ensure_app_role_sql`] deliberately emits
/// none of it. The app role is the same stable passwordless NOLOGIN carrier as
/// every other generation family.
///
/// psql commits each complete statement in this standalone artifact. That
/// makes the app-role hardening visible before the final bounded session drain.
/// Rust callers needing the same ordering apply [`role_posture_sql`], await its
/// commit, then apply [`sql::drain_app_role_sessions_sql`].
///
/// `pub` so the live gate applies the SAME fragments production uses instead of
/// hand-transcribed copies — the `reconcile_run_plane::reconcile` precedent.
pub fn role_sql(app_password: &str) -> String {
    format!(
        "{posture}\n{drain}\n",
        posture = role_posture_sql(app_password),
        drain = sql::drain_app_role_sessions_sql(),
    )
}

/// Role creation and hardening half of [`role_sql`].
pub fn role_posture_sql(app_password: &str) -> String {
    format!(
        "{app}\n{owner}\n{reader}\n",
        app = sql::ensure_app_role_sql(app_password),
        owner = sql::ensure_db_owner_role_sql(),
        reader = sql::ensure_workload_acl_role_sql(WorkloadRoleFamily::DispatchReader),
    )
}

/// The privilege batch the runbook applies AFTER the database exists (step 3).
///
/// **Ownership converges FIRST and must stay first.** `ALTER DATABASE … OWNER
/// TO` rewrites the outgoing owner's ACL entry, and that entry is where a
/// `CONNECT` granted to a role that still owned the database has merged (the
/// hazard measured at `47b404cf`). Everything else therefore follows it.
///
/// **`wamn_app` is REVOKED, not granted (`wamn-0h0g.12.179`).** Until the
/// `wamn-0h0g.22.6` cutover `wamn_app` was the guest LOGIN role and this batch
/// granted it `CONNECT`. Guest SQL now authenticates as a per-tenant generation
/// login that `prepare_workload_generation_sql` grants `CONNECT` directly, and
/// `wamn_app` became the stable NOLOGIN ACL role those generations INHERIT. A
/// `CONNECT` left on it is therefore not a leftover that merely offends a
/// checker: measured on PostgreSQL 18, a generation minted for one project-env
/// database and holding zero direct `CONNECT` grants of its own authenticates
/// into ANY OTHER project-env database this batch has run against, because
/// `wamn_app` is cluster-global and the membership inherits. That is what the
/// guest family's generation prepare guards when it refuses a stable ACL role
/// that is not connection-free, and the `REVOKE` is what converges an
/// environment provisioned before the cutover back under that refusal.
///
/// **`wamn_dispatch_reader` is REVOKED for the identical reason
/// (`wamn-0h0g.22.24`).** It was the LAST family still on the stable-LOGIN
/// shape: the dispatcher authenticated as the cluster-global role itself and
/// this batch GRANTED it `CONNECT` per environment, so its generations — once
/// the family gained any — would inherit reach into every environment on the
/// cluster, exactly as the guest's did. The dispatcher now mounts a
/// dispatch-reader GENERATION, `prepare_workload_generation_sql` grants that
/// generation `CONNECT` on its one database, and this `REVOKE` is what converges
/// a pre-cutover environment back under the prepare's refusal.
///
/// `pub` for the same reason as [`role_sql`].
pub fn privilege_sql(database: &str) -> String {
    let db = quote_ident(database);
    format!(
        "{owner};\n\
         REVOKE CONNECT, TEMPORARY ON DATABASE {db} FROM PUBLIC; \
         REVOKE CONNECT ON DATABASE {db} FROM {app};\n\
         {reader_connect}\n",
        owner = sql::set_database_owner_sql(database),
        app = quote_ident(APP_ROLE),
        reader_connect = sql::revoke_dispatch_reader_connect_sql(database),
    )
}

/// Begin a system database transaction that writes as `wamn:provisioning`.
///
/// The identity relations stamp the bound actor. The binding ends with the
/// transaction.
pub async fn provisioning_transaction(client: &mut Client) -> anyhow::Result<Transaction<'_>> {
    let transaction = client
        .transaction()
        .await
        .context("begin a system database write transaction")?;
    transaction
        .batch_execute(&bind_platform_principal_sql(
            PlatformComponent::Provisioning,
        ))
        .await
        .context("bind wamn:provisioning as the actor")?;
    Ok(transaction)
}

/// Record one project-env, render its artifacts, and write the requested files
/// and credential Secrets.
pub async fn provision_project_env(
    args: &ProvisionProjectEnvRequest,
) -> anyhow::Result<ProvisionProjectEnvOutcome> {
    let db_secret_path = args.emit_secret.as_path();
    ensure_secret_path(db_secret_path, "--emit-secret")?;
    if let Some(path) = args.emit_management_author_pat_secret.as_deref() {
        ensure_secret_path(path, "--emit-management-author-pat-secret")?;
    }
    if let Some(path) = args.emit_route_caller_pat_secret.as_deref() {
        ensure_secret_path(path, "--emit-route-caller-pat-secret")?;
    }
    ensure_distinct_secret_paths([
        ("--emit-secret", Some(db_secret_path)),
        (
            "--emit-management-author-pat-secret",
            args.emit_management_author_pat_secret.as_deref(),
        ),
        (
            "--emit-route-caller-pat-secret",
            args.emit_route_caller_pat_secret.as_deref(),
        ),
    ])?;
    let issues_pat = args.emit_management_author_pat_secret.is_some()
        || args.emit_route_caller_pat_secret.is_some();
    if issues_pat && args.system_database_url.is_none() {
        anyhow::bail!(
            "PAT issuance requires --system-database-url to resolve the stable service principal"
        );
    }
    // Refuse incomplete TLS configuration before registry writes or artifacts.
    let pat_client = issues_pat
        .then(|| PatClient::new(&args.pat_issuer))
        .transpose()?;

    let org = args.org.as_str();
    let project = args.project.as_str();
    let env = args.env.as_str();
    let triple = Triple::new(org, project, env);

    // Validate the project id + the assembled `wamn-<org>--<project>--<env>`
    // namespace and `wamn-db-<org>--<project>--<env>--<instance>` database lengths
    // before any effect. This is the one point that mints an environment's
    // names, so a triple that breaches a bound is refused here — never shortened.
    validate_project_env(org, project, env)
        .map_err(|e| anyhow::anyhow!("project-env names: {e}"))?;

    let system_url = args.system_database_url.as_deref().context(
        "--system-database-url is required to read or mint the project-env instance suffix",
    )?;
    let secret_name = project_env_secret_name(org, project, env);
    let instance = record_project_env(
        system_url,
        &triple,
        args.tenant.as_deref(),
        &secret_name,
        args.secret_namespace.as_deref(),
        &mint_instance_suffix()?,
        args.disposable,
    )
    .await?;

    // Pick the target cluster: an explicit `--cluster` wins (render-only / manual);
    // otherwise derive it from the org's placement + the env policy (`cluster_of`).
    let cluster = match &args.cluster {
        Some(c) => c.clone(),
        None => {
            let url = args.system_database_url.as_deref().context(
                "pass --cluster, or --system-database-url to resolve the target cluster from the registry",
            )?;
            resolve_cluster(url, org, env).await?
        }
    };

    let db_name = project_env_database_name(org, project, env, &instance);
    let app_host = args
        .app_host
        .clone()
        .unwrap_or_else(|| format!("{cluster}-rw"));
    let app_url = compose_url(
        APP_ROLE,
        &args.app_password,
        &app_host,
        args.app_port,
        &db_name,
    );

    // Render the artifacts the runbook applies.
    let db_cr = render_project_env_database(&triple, &instance, &cluster, args.connection_limit);
    // Ordinary provisioning establishes cluster roles before it creates the
    // database. The shared-login drain is an operator finalizer and is emitted
    // only by a successful App-generation prepare after every carrier has a
    // replacement credential.
    let role_sql = role_posture_sql(&args.app_password);
    let privilege_sql = privilege_sql(&db_name);
    let secret_doc = render_project_env_secret_manifest(&triple, &args.namespace, &app_url);

    write_output(
        args.emit_database.as_deref(),
        &serde_json::to_string_pretty(&db_cr)?,
    )?;
    write_output(args.emit_role_sql.as_deref(), &role_sql)?;
    write_output(args.emit_privilege_sql.as_deref(), &privilege_sql)?;
    write_secret_json(db_secret_path, &secret_doc)?;

    let pat_secrets = match pat_client.as_ref() {
        Some(pat_client) => {
            issue_pat_secrets(
                system_url,
                pat_client,
                &triple,
                &args.namespace,
                args.emit_management_author_pat_secret.as_deref(),
                args.emit_route_caller_pat_secret.as_deref(),
            )
            .await?
        }
        None => Vec::new(),
    };

    Ok(ProvisionProjectEnvOutcome {
        namespace: project_env_namespace(org, project, env, &instance),
        triple,
        instance,
        database: db_name,
        cluster,
        database_cr: db_cr,
        role_sql,
        privilege_sql,
        pat_secrets,
    })
}

#[cfg(test)]
mod tests;
