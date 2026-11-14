//! Render and record an environment. CNPG owns its database and role passwords.

use sqlx::Connection as _;
use sqlx::PgConnection as Client;
pub(super) mod plan;
pub use plan::{privilege_sql, role_posture_sql, role_sql};

use std::ffi::OsString;
use std::fs::{File, OpenOptions, Permissions};
#[cfg(test)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Context as _;
use ring::rand::SystemRandom;
use serde_json::{Value, json};

use postgres_protocol::escape::escape_identifier;
use wamn_control_provision::{
    APP_ROLE, INSTANCE_SUFFIX_LEN, PlatformComponent, bind_platform_principal_sql,
    project_env_database_name, project_env_namespace, project_env_secret_name,
    render_project_env_database, sql, validate_instance_suffix, validate_project_env,
};
use wamn_control_provision::{Org, Placement, cluster_of};
use wamn_platform_identity::{
    IdentityErrorType, Principal, PrincipalStatus, PrincipalType, assign_project_role,
    authenticate_pat, create_service, operator_subject, resolve_subject, revoke_pat,
};
use wamn_registry::Triple;

use wamn_identity_client::{PatClient, PatIssuerConfig};
use wamn_release_control::env_policies::read_env_policy;

mod database;
mod output;
pub mod pat_secrets;
pub(crate) mod registry;
pub(crate) mod workload;

pub(crate) use database::database_host;

use pat_secrets::issue_pat_secrets;
use registry::{mint_instance_suffix, record_project_env};

pub(crate) use output::write_output;
pub use output::{
    ProvisionedRoute, ensure_distinct_secret_paths, ensure_secret_path, read_json,
    secret_annotation, secret_value, write_secret_json,
};
pub(crate) use pat_secrets::resolve_or_create_service;
pub use pat_secrets::{
    IssuedPatSecret, mint_management_author_pat, parse_pat_prefix, revoke_provisioning_pat,
};
pub use registry::{
    read_project_env_instance, read_project_env_policy, require_connected, resolve_cluster,
};

#[cfg(test)]
use output::SECRET_TEMP_SEQUENCE;

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

    /// The environment policy a new environment row names. Absent names the
    /// policy whose name is the env. A recorded row keeps its policy: the
    /// environment document owns it
    /// (docs/architecture/overview.md#environment-lifecycle). The project is
    /// derived from the coordinate (`project_env_key`).
    pub policy: Option<String>,

    /// The `registry.target` name where the environment runs. The first
    /// provision records it, and it never changes
    /// (`environment_target_immutable`). A name that `registry.target` does
    /// not hold is refused.
    pub target: String,

    /// Mark this environment DISPOSABLE: its admitted component facts may be
    /// REPLACED rather than frozen (wamn-10yt.38). The marker is recorded on the
    /// environment's registry row and projected into the control store.
    /// `None` keeps the recorded marker, and a new row is durable: only
    /// `wamn dev up` sets it, and `env apply` passes `None` (wamn-p9xix).
    pub disposable: Option<bool>,

    /// Superuser Postgres URL to the T1 system DB (`wamn_system`): read the org's
    /// placement, read-or-mint the stored instance suffix, and record the project
    /// + project-env.
    pub system_database_url: Option<String>,

    /// Target CNPG `Cluster` name. When absent, it is read from the org's
    /// placement in the registry.
    pub cluster: Option<String>,

    /// Per-project-env `CONNECTION LIMIT`. Absent means no limit (`-1`).
    pub connection_limit: Option<i64>,

    /// Namespace of the CNPG `Cluster`, which the rendered `Database` shares.
    pub cluster_namespace: String,

    /// Namespace the rendered credential `Secret` is applied to.
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
    /// Absent only for a run that issues a PAT and writes no database Secret.
    pub emit_secret: Option<PathBuf>,

    /// Operator authentication for PAT issuance through `wamn-identity`.
    pub pat_issuer: PatIssuerConfig,

    /// Publisher for committed principal and project-role changes.
    pub changes: wamn_broker::Publisher,

    /// Issue a management-author PAT and write its Kubernetes `Secret` JSON here.
    pub emit_management_author_pat_secret: Option<PathBuf>,

    /// Issue an operator PAT and write its Kubernetes `Secret` JSON here.
    pub emit_operator_pat_secret: Option<PathBuf>,
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

/// Begin a system database transaction that writes as `wamn:provisioning`.
///
/// The identity relations stamp the bound actor. The binding ends with the
/// transaction.
pub async fn provisioning_transaction(
    client: &mut Client,
) -> anyhow::Result<sqlx::Transaction<'_, sqlx::Postgres>> {
    let mut transaction = client
        .begin()
        .await
        .context("begin a system database write transaction")?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(bind_platform_principal_sql(
        PlatformComponent::Provisioning,
    )))
    .execute(&mut *transaction)
    .await
    .map(drop)
    .context("bind wamn:provisioning as the actor")?;
    Ok(transaction)
}

/// Record one project-env, render its artifacts, and write the requested files
/// and credential Secrets.
pub async fn provision_project_env(
    args: &ProvisionProjectEnvRequest,
) -> anyhow::Result<ProvisionProjectEnvOutcome> {
    let db_secret_path = args.emit_secret.as_deref();
    if let Some(path) = db_secret_path {
        ensure_secret_path(path, "--emit-secret")?;
    }
    if let Some(path) = args.emit_management_author_pat_secret.as_deref() {
        ensure_secret_path(path, "--emit-management-author-pat-secret")?;
    }
    if let Some(path) = args.emit_operator_pat_secret.as_deref() {
        ensure_secret_path(path, "--emit-operator-pat-secret")?;
    }
    ensure_distinct_secret_paths([
        ("--emit-secret", db_secret_path),
        (
            "--emit-management-author-pat-secret",
            args.emit_management_author_pat_secret.as_deref(),
        ),
        (
            "--emit-operator-pat-secret",
            args.emit_operator_pat_secret.as_deref(),
        ),
    ])?;
    let issues_pat =
        args.emit_management_author_pat_secret.is_some() || args.emit_operator_pat_secret.is_some();
    // A run with any provisioning output provisions and may also mint. A run
    // with only PAT outputs mints for a recorded environment and writes nothing
    // to the registry (wamn-rjtf).
    let provisions = args.emit_database.is_some()
        || args.emit_role_sql.is_some()
        || args.emit_privilege_sql.is_some()
        || db_secret_path.is_some();
    let mints_only = issues_pat && !provisions;
    if mints_only && (args.secret_namespace.is_some() || args.disposable.is_some()) {
        anyhow::bail!(
            "--secret-namespace and --disposable belong to a provisioning run, which passes an --emit-* output"
        );
    }
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
    let triple = Triple::new(org, env, project);

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
    let instance = if mints_only {
        read_project_env_instance(system_url, &triple).await?
    } else {
        record_project_env(
            system_url,
            &triple,
            args.policy.as_deref(),
            &args.target,
            &secret_name,
            args.secret_namespace.as_deref(),
            &mint_instance_suffix()?,
            args.disposable,
        )
        .await?
    };

    // Pick the target cluster: an explicit `--cluster` wins (render-only / manual);
    // otherwise derive it from the org's placement + the env policy (`cluster_of`).
    let cluster = if let Some(cluster) = &args.cluster {
        cluster.clone()
    } else {
        let url = args.system_database_url.as_deref().context(
            "pass --cluster, or --system-database-url to resolve the target cluster from the registry",
        )?;
        resolve_cluster(url, org, args.policy.as_deref().unwrap_or(env)).await?
    };

    let db_name = project_env_database_name(org, project, env, &instance);

    // Render the artifacts the runbook applies.
    let db_cr = render_project_env_database(
        &triple,
        &instance,
        &cluster,
        &args.cluster_namespace,
        args.connection_limit,
    );
    // Ordinary provisioning establishes cluster roles before it creates the
    // database. The shared-login drain is an operator finalizer and is emitted
    // only by a successful App login preparation after every carrier has a
    // replacement credential.
    let role_sql = role_posture_sql();
    let privilege_sql = privilege_sql(&db_name);
    let secret_doc = serde_json::to_value(wamn_kubernetes::secret::Secret::new(
        &args.namespace,
        &wamn_registry::credential_secret_name(&triple.org, &triple.project, &triple.env),
    ))?;

    write_output(
        args.emit_database.as_deref(),
        &serde_json::to_string_pretty(&db_cr)?,
    )?;
    write_output(args.emit_role_sql.as_deref(), &role_sql)?;
    write_output(args.emit_privilege_sql.as_deref(), &privilege_sql)?;
    if let Some(path) = db_secret_path {
        write_secret_json(path, &secret_doc)?;
    }

    let pat_secrets = match pat_client.as_ref() {
        Some(pat_client) => {
            issue_pat_secrets(
                system_url,
                pat_client,
                &triple,
                &args.namespace,
                args.emit_management_author_pat_secret.as_deref(),
                args.emit_operator_pat_secret.as_deref(),
                &args.changes,
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
