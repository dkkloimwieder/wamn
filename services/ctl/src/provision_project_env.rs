//! The `provision-project-env` subcommand (wamn-q3n.7): stand up one
//! per-project-env Postgres **database** on an org's appropriate cluster (or the
//! T3 trials pool) and record it in the T1 control-plane registry.
//!
//! The four-tier counterpart of `provision-project`: identity is the `(org,
//! project, env)` [`Triple`], and the database lives on the cluster **derived** by
//! [`cluster_of`](wamn_control_registry::cluster_of) (D18) from the org's placement + the
//! env's policy — a dedicated org's `<org>-<owner(env)>` (so `canary` sharing prod
//! lands on `<org>-prod`, `canary` own on `<org>-canary`), or the shared pool for a
//! pooled org. One derivation path serves every placement.
//!
//! An imperative CLI (the `provision-org` precedent). It **renders + records**;
//! the runbook/Job applies the emitted artifacts, in this order:
//!
//! 1. the `wamn_db_owner` title role must exist **before** the `Database` CR
//!    (its `owner`), and the shared `wamn_app` + scoped `wamn_dispatch_reader`
//!    roles with it: apply the emitted
//!    **role SQL** to the target cluster's superuser. Applying the CR first
//!    fails reconciliation — CNPG maps `spec.owner` straight to `CREATE DATABASE
//!    … OWNER` / `ALTER DATABASE … OWNER TO`;
//! 2. `kubectl apply -f` the emitted **`Database` CR** and wait it applied — the
//!    CNPG operator declaratively creates the database owned by `wamn_db_owner`,
//!    and re-owns an already-existing one to it;
//! 3. apply the emitted **privilege SQL** (`ALTER DATABASE … OWNER TO
//!    wamn_db_owner`, then `REVOKE CONNECT, TEMPORARY FROM PUBLIC` / `GRANT
//!    CONNECT TO wamn_app` / `GRANT CONNECT TO wamn_dispatch_reader`) — the
//!    thin imperative step the `Database` CRD does
//!    not cover (topology fact 3), run **after** the database exists. The owner
//!    statement is first and must stay first: `ALTER DATABASE … OWNER TO`
//!    rewrites the outgoing owner's ACL entry, which is where `wamn_app`'s
//!    granted `CONNECT` merges while `wamn_app` still owns the database.
//!    **On an EXISTING environment this step is mandatory, not optional**: the
//!    owner change (whether issued here or by the CR reconciler in step 2)
//!    takes `wamn_app`'s `CONNECT` with the old owner's ACL entry, and the
//!    `GRANT` that follows is what puts it back. Stopping after step 2 leaves
//!    the runtime unable to reach its own database;
//! 4. `kubectl apply -f` the emitted **credential Secret** and any independently
//!    requested management-author / route-caller PAT Secrets.
//!
//! What this tool does directly (given `--system-database-url`): read the org's
//! placement to pick the target cluster, and record `registry.projects` +
//! `registry.project_envs` (as the `wamn_system` owner); when requested, resolve
//! or create stable service principals, assign project roles, and issue then
//! authenticate PATs. Kubernetes artifacts are only emitted (no K8s client, no
//! target-cluster connection — the `provision-org` shape).
//!
//! **RLS floor** at provision time: there are no tables yet, so wamn-q3n.7
//! establishes the RLS-**enforceable substrate** only — `wamn_app` is
//! `NOSUPERUSER NOCREATEDB NOBYPASSRLS` (the role SQL) and `CONNECT` is confined
//! (the privilege SQL). The per-table `FORCE ROW LEVEL SECURITY` floor is applied
//! at catalog-publish (2.4/2.5), where the tables are created.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{File, OpenOptions, Permissions};
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Context as _;
use chrono::{DateTime, SecondsFormat, Utc};
use clap::Args;
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::{Value, json};
use tokio_postgres::{Config as PgConfig, GenericClient, NoTls};
use url::Url;

use wamn_control_provision::{
    APP_ROLE, CredentialGeneration, DB_OWNER_ROLE, EFFECT_WRITER_ROLE, EffectWriterCredentialScope,
    EffectWriterCredentialValidity, INSTANCE_SUFFIX_LEN, compose_url, effect_writer_credential,
    effect_writer_generation_role, effect_writer_scope_hash, project_env_database_name,
    project_env_namespace, project_env_secret_name, render_effect_writer_secret_manifest,
    render_project_env_database, render_project_env_secret_manifest, sql, validate_instance_suffix,
    validate_project_env,
};
use wamn_control_registry::{Org, Placement, Triple, cluster_of};
use wamn_platform_identity::{
    IdentityErrorKind, Principal, PrincipalKind, PrincipalStatus, assign_project_role,
    authenticate_pat, create_service, issue_pat, resolve_subject, revoke_pat,
};
use wamn_run_state::RUN_PROJECTION_WRITER_ROLE;

use crate::env_policies::read_env_policy;

#[derive(Debug, Args)]
pub struct ProvisionProjectEnvArgs {
    /// Org id (must already be registered — `provision-org`, or the T3 pool for a
    /// trials org). Names the target cluster and the `wamn-db-<org>--…` database.
    #[arg(long, required_unless_present = "revoke_pat_prefix")]
    pub org: Option<String>,

    /// Project id: a lowercase slug `[a-z0-9-]` (start/end alphanumeric). The
    /// reserved `wamn` prefix is rejected.
    #[arg(long, required_unless_present = "revoke_pat_prefix")]
    pub project: Option<String>,

    /// Environment slug: any policy in the ORG's `registry.env_policies` set
    /// (stamped from its template — `dev`/`prod`, plus `canary` on the dedicated
    /// templates; others are addable per org). Derives the target cluster via
    /// `cluster_of` — a dedicated org's `<org>-<owner(env)>`, or the shared pool.
    #[arg(long, required_unless_present = "revoke_pat_prefix")]
    pub env: Option<String>,

    /// Superuser Postgres URL to the T1 system DB (`wamn_system`): read the org's
    /// placement (pick the target cluster) and record the project + project-env.
    /// Env `WAMN_SYSTEM_ADMIN_URL`. Omit (and pass `--cluster`) to render only.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: Option<String>,

    /// Override the target CNPG `Cluster` name. When omitted, it is read from the
    /// org's placement in the registry. Required if `--system-database-url` is not
    /// given (render-only mode).
    #[arg(long)]
    pub cluster: Option<String>,

    /// Per-project-env `CONNECTION LIMIT` (noisy-neighbour governance within a
    /// cluster). Default: no limit (`-1`).
    #[arg(long)]
    pub connection_limit: Option<i64>,

    /// Password for the shared `wamn_app` role (embedded in the emitted URL + the
    /// role SQL). Env `WAMN_APP_PASSWORD`.
    #[arg(long, env = "WAMN_APP_PASSWORD", default_value = "wamn_app")]
    pub app_password: String,

    /// Password for the scoped `wamn_dispatch_reader` login role — the
    /// dispatcher's own credential (wamn-0h0g.12.66), never the runtime's.
    /// Env `WAMN_DISPATCH_READER_PASSWORD`.
    ///
    /// **Deliberately has no `default_value`, unlike `--app-password` above.**
    /// A default here would provision every project-env with a publicly known
    /// password on a role that is `LOGIN` and reachable from outside the
    /// cluster; provisioning refuses instead.
    #[arg(long, env = "WAMN_DISPATCH_READER_PASSWORD")]
    pub dispatch_reader_password: String,

    /// Host the runtime reaches the project-env database at. Defaults to the
    /// target cluster's read-write service `<cluster>-rw`.
    #[arg(long)]
    pub app_host: Option<String>,

    /// Port the runtime reaches the database at.
    #[arg(long, default_value_t = 5432)]
    pub app_port: u16,

    /// Namespace the emitted `Database` CR + `Secret` are applied to.
    #[arg(long, env = "WAMN_NAMESPACE", default_value = "wamn-system")]
    pub namespace: String,

    /// Secret namespace to RECORD in the registry `SecretRef`. Omit to record
    /// `NULL` (the resolving component's own namespace).
    #[arg(long)]
    pub secret_namespace: Option<String>,

    /// Explicit target project-database admin URL for effect-writer generation
    /// actions. Provisioning authority only: never persisted or emitted.
    #[arg(long, value_name = "URL")]
    pub target_admin_database_url: Option<String>,

    /// Prepare and authenticate the inactive effect-writer credential generation.
    #[arg(
        long,
        value_name = "a|b",
        conflicts_with_all = [
            "retire_effect_writer_generation",
            "abort_effect_writer_generation"
        ]
    )]
    pub prepare_effect_writer_generation: Option<CredentialGeneration>,

    /// Retire the old effect-writer credential generation after replacement use.
    #[arg(
        long,
        value_name = "a|b",
        conflicts_with_all = [
            "prepare_effect_writer_generation",
            "abort_effect_writer_generation"
        ]
    )]
    pub retire_effect_writer_generation: Option<CredentialGeneration>,

    /// Abort a prepared generation that was definitively not published.
    ///
    /// This action requires the exact active ACL and zero sessions. It does not
    /// accept retirement's replacement-active/use-proven contract.
    #[arg(
        long,
        value_name = "a|b",
        conflicts_with_all = [
            "prepare_effect_writer_generation",
            "retire_effect_writer_generation"
        ]
    )]
    pub abort_effect_writer_generation: Option<CredentialGeneration>,

    /// Write the fixed-mount effect-writer Secret. Required with prepare and
    /// forbidden otherwise; credentials are never written to stdout.
    #[arg(
        long,
        value_name = "PATH",
        value_parser = parse_secret_path,
        requires = "prepare_effect_writer_generation"
    )]
    pub emit_effect_writer_secret: Option<PathBuf>,

    /// Write the CNPG `Database` CR (JSON) here; `-` = stdout. Absent ⇒ printed
    /// with a labeled header.
    #[arg(long)]
    pub emit_database: Option<PathBuf>,

    /// Write the role-ensure SQL (apply to the target cluster BEFORE the `Database`
    /// CR — the CR's `owner` must exist) here; `-` = stdout.
    #[arg(long)]
    pub emit_role_sql: Option<PathBuf>,

    /// Write the privilege SQL (`ALTER DATABASE … OWNER TO wamn_db_owner`, then
    /// `REVOKE CONNECT,TEMPORARY FROM PUBLIC` / `GRANT CONNECT TO wamn_app`;
    /// apply AFTER the database is ready) here; `-` = stdout.
    #[arg(long)]
    pub emit_privilege_sql: Option<PathBuf>,

    /// Write the database credential `Secret` (JSON) here. Required for
    /// provisioning and must name a file; credentials are never written to stdout.
    #[arg(
        long,
        value_name = "PATH",
        value_parser = parse_secret_path,
        required_unless_present_any = [
            "revoke_pat_prefix",
            "prepare_effect_writer_generation",
            "retire_effect_writer_generation",
            "abort_effect_writer_generation"
        ]
    )]
    pub emit_secret: Option<PathBuf>,

    /// Issue a management-author PAT and write its Kubernetes `Secret` JSON here.
    #[arg(
        long,
        value_name = "PATH",
        value_parser = parse_secret_path,
        conflicts_with = "revoke_pat_prefix"
    )]
    pub emit_management_author_pat_secret: Option<PathBuf>,

    /// Issue a route-caller PAT and write its Kubernetes `Secret` JSON here.
    #[arg(
        long,
        value_name = "PATH",
        value_parser = parse_secret_path,
        conflicts_with = "revoke_pat_prefix"
    )]
    pub emit_route_caller_pat_secret: Option<PathBuf>,

    /// Revoke one PAT by its non-secret 16-lowercase-hex lookup prefix. This is
    /// a separate invocation and performs no provisioning or Kubernetes work.
    #[arg(
        long,
        value_name = "16-LOWERCASE-HEX",
        value_parser = parse_pat_prefix,
        conflicts_with_all = [
            "emit_management_author_pat_secret",
            "emit_route_caller_pat_secret"
        ]
    )]
    pub revoke_pat_prefix: Option<String>,
}

/// The role batch the runbook applies to the TARGET cluster's superuser before
/// the `Database` CR (step 1). Both `wamn_app` and `wamn_db_owner` precede the
/// CR because `wamn_db_owner` is its `spec.owner` and the CR cannot reconcile
/// against a role that does not exist yet; `wamn_dispatch_reader` is here
/// (wamn-0h0g.12.122) because it is cluster-global exactly as they are, and
/// because the dispatcher's projects file already names it.
///
/// `pub` so the live gate applies the SAME text production uses instead of a
/// hand-transcribed copy — the `reconcile_run_plane::reconcile` precedent.
pub fn role_sql(app_password: &str, dispatch_reader_password: &str) -> String {
    format!(
        "{app}\n{owner}\n{reader}\n",
        app = sql::ensure_app_role_sql(app_password),
        owner = sql::ensure_db_owner_role_sql(),
        reader = sql::ensure_dispatch_reader_role_sql(dispatch_reader_password),
    )
}

/// The privilege batch the runbook applies AFTER the database exists (step 3).
///
/// **Ownership converges FIRST and must stay first.** `ALTER DATABASE … OWNER
/// TO` rewrites the outgoing owner's ACL entry, and that entry is where a
/// `CONNECT` granted to `wamn_app` while `wamn_app` still owned the database
/// has merged (the hazard measured at `47b404cf`). Both `CONNECT` grants
/// therefore follow it: `grant_dispatch_reader_connect_sql` is ADDITIVE and
/// deliberately separate from `grant_connect_on_database_sql`, which confines
/// `CONNECT` to `wamn_app` and revokes `PUBLIC`.
///
/// `pub` for the same reason as [`role_sql`].
pub fn privilege_sql(database: &str) -> String {
    format!(
        "{owner};\n{connect}\n{reader_connect}\n",
        owner = sql::set_database_owner_sql(database),
        connect = sql::grant_connect_on_database_sql(database),
        reader_connect = sql::grant_dispatch_reader_connect_sql(database),
    )
}

pub async fn run(args: ProvisionProjectEnvArgs) -> anyhow::Result<()> {
    if let Some(prefix) = args.revoke_pat_prefix.as_deref() {
        let system_url = args
            .system_database_url
            .as_deref()
            .context("--revoke-pat-prefix requires --system-database-url")?;
        revoke_provisioning_pat(system_url, prefix).await?;
        println!("revoked PAT prefix {prefix}");
        return Ok(());
    }

    if args.prepare_effect_writer_generation.is_some()
        || args.retire_effect_writer_generation.is_some()
        || args.abort_effect_writer_generation.is_some()
    {
        return run_effect_writer_action(&args).await;
    }
    anyhow::ensure!(
        args.target_admin_database_url.is_none(),
        "--target-admin-database-url is valid only for an effect-writer generation action"
    );

    let db_secret_path = args
        .emit_secret
        .as_deref()
        .context("--emit-secret PATH is required and must not be '-'")?;
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

    let org = args
        .org
        .as_deref()
        .context("--org is required for provisioning")?;
    let project = args
        .project
        .as_deref()
        .context("--project is required for provisioning")?;
    let env = args
        .env
        .as_deref()
        .context("--env is required for provisioning")?;
    let triple = Triple::new(org, project, env);

    // Validate the project id + the assembled `wamn-<org>--<project>--<env>`
    // namespace and `wamn-db-<org>--<project>--<env>` database name lengths
    // before any effect. This is the one point that mints an environment's
    // names, so a triple that breaches a bound is refused here — never shortened.
    validate_project_env(org, project, env)
        .map_err(|e| anyhow::anyhow!("project-env names: {e}"))?;

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

    let db_name = project_env_database_name(org, project, env);
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
    let db_cr = render_project_env_database(&triple, &cluster, args.connection_limit);
    let role_sql = role_sql(&args.app_password, &args.dispatch_reader_password);
    let privilege_sql = privilege_sql(&db_name);
    let secret_doc = render_project_env_secret_manifest(&triple, &args.namespace, &app_url);

    println!("{}", provision_summary(&triple, &db_name, &cluster));

    emit_json(&args.emit_database, "Database CR (kubectl apply)", &db_cr)?;
    emit_text(
        &args.emit_role_sql,
        "role SQL (psql the TARGET cluster BEFORE the Database CR)",
        &role_sql,
    )?;
    emit_text(
        &args.emit_privilege_sql,
        "privilege SQL (psql the TARGET cluster AFTER the Database is ready)",
        &privilege_sql,
    )?;
    write_secret_json(db_secret_path, &secret_doc)?;
    println!(
        "wrote {} (database credential Secret; kubectl apply)",
        db_secret_path.display()
    );

    // Record the project + project-env in the registry (idempotent), when a system
    // DB URL is given. The Secret reference is what a triple resolves to.
    match &args.system_database_url {
        Some(url) => {
            let secret_name = project_env_secret_name(org, project, env);
            // The instance suffix is minted here and recorded read-or-mint: a
            // re-provision of an existing environment keeps the stored one, so
            // the derived names stay pointed at the resources that already exist.
            let instance = record_project_env(
                url,
                &triple,
                &secret_name,
                args.secret_namespace.as_deref(),
                &mint_instance_suffix()?,
            )
            .await?;
            println!(
                "recorded project {:?} + project-env {} in the registry (wamn_system)",
                project, triple
            );
            println!(
                "environment namespace {:?} (instance suffix {instance:?})",
                project_env_namespace(org, project, env, &instance)
            );

            if issues_pat {
                issue_pat_secrets(
                    url,
                    &triple,
                    &args.namespace,
                    args.emit_management_author_pat_secret.as_deref(),
                    args.emit_route_caller_pat_secret.as_deref(),
                )
                .await?;
            }
        }
        None => println!("(no --system-database-url: rendered artifacts only; not recorded)"),
    }

    Ok(())
}

const EFFECT_WRITER_CREDENTIAL_TTL_DAYS: i64 = 30;

#[derive(Debug, Clone, PartialEq, Eq)]
struct EffectWriterRoleState {
    login: bool,
    superuser: bool,
    inherit: bool,
    create_role: bool,
    create_db: bool,
    replication: bool,
    bypass_rls: bool,
    password_set: bool,
    valid_until: Option<String>,
    valid_until_finite: bool,
    memberships: Vec<String>,
    membership_options_exact: bool,
    member_roles: Vec<String>,
    member_options_exact: bool,
    generation_children_exact: bool,
    connect_databases: Vec<String>,
    sessions: i64,
    owned_objects: i64,
}

impl EffectWriterRoleState {
    fn is_active_for(&self, database: &str) -> bool {
        self.login
            && self.restrictive_attributes()
            && self.inherit
            && self.password_set
            && self.valid_until_finite
            && self.memberships == [EFFECT_WRITER_ROLE, RUN_PROJECTION_WRITER_ROLE]
            && self.membership_options_exact
            && self.member_roles.is_empty()
            && self.member_options_exact
            && self.generation_children_exact
            && self.connect_databases == [database]
            && self.owned_objects == 0
    }

    fn is_inactive(&self) -> bool {
        !self.login
            && self.restrictive_attributes()
            && self.inherit
            && !self.password_set
            && self.valid_until.as_deref() == Some("1970-01-01T00:00:00Z")
            && self.valid_until_finite
            && self.memberships.is_empty()
            && self.membership_options_exact
            && self.member_roles.is_empty()
            && self.member_options_exact
            && self.generation_children_exact
            && self.connect_databases.is_empty()
            && self.sessions == 0
            && self.owned_objects == 0
    }

    fn is_acl_role(&self) -> bool {
        !self.login
            && self.restrictive_attributes()
            && !self.inherit
            && !self.password_set
            && self.valid_until.is_none()
            && !self.valid_until_finite
            && self.memberships.is_empty()
            && self.membership_options_exact
            && self
                .member_roles
                .iter()
                .all(|role| is_effect_writer_generation_role(role))
            && self.member_options_exact
            && self.generation_children_exact
            && self.connect_databases.is_empty()
            && self.sessions == 0
            && self.owned_objects == 0
    }

    fn restrictive_attributes(&self) -> bool {
        !self.superuser
            && !self.create_role
            && !self.create_db
            && !self.replication
            && !self.bypass_rls
    }
}

fn is_effect_writer_generation_role(role: &str) -> bool {
    let Some(scoped) = role.strip_prefix("wamn_effect_writer_") else {
        return false;
    };
    let Some((hash, generation)) = scoped.split_once('_') else {
        return false;
    };
    hash.len() == 40
        && hash
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        && matches!(generation, "a" | "b")
}

async fn run_effect_writer_action(args: &ProvisionProjectEnvArgs) -> anyhow::Result<()> {
    anyhow::ensure!(
        args.cluster.is_none()
            && args.connection_limit.is_none()
            && args.app_host.is_none()
            && args.emit_database.is_none()
            && args.emit_role_sql.is_none()
            && args.emit_privilege_sql.is_none()
            && args.emit_secret.is_none()
            && args.emit_management_author_pat_secret.is_none()
            && args.emit_route_caller_pat_secret.is_none(),
        "effect-writer generation actions cannot render ordinary provisioning or PAT artifacts"
    );
    let org = args
        .org
        .as_deref()
        .context("--org is required for an effect-writer generation action")?;
    let project = args
        .project
        .as_deref()
        .context("--project is required for an effect-writer generation action")?;
    let environment = args
        .env
        .as_deref()
        .context("--env is required for an effect-writer generation action")?;
    validate_project_env(org, project, environment)
        .map_err(|error| anyhow::anyhow!("project-env names: {error}"))?;
    let database = project_env_database_name(org, project, environment);
    let admin_url = args
        .target_admin_database_url
        .as_deref()
        .context("effect-writer generation actions require --target-admin-database-url")?;
    let admin_config = exact_project_database_config(admin_url, &database)?;

    if let Some(generation) = args.prepare_effect_writer_generation {
        let secret_path = args.emit_effect_writer_secret.as_deref().context(
            "--prepare-effect-writer-generation requires --emit-effect-writer-secret PATH",
        )?;
        ensure_secret_path(secret_path, "--emit-effect-writer-secret")?;
        prepare_effect_writer_generation(
            args,
            &admin_config,
            org,
            project,
            environment,
            &database,
            generation,
            secret_path,
            Utc::now(),
        )
        .await
    } else if let Some(generation) = args.retire_effect_writer_generation {
        anyhow::ensure!(
            args.emit_effect_writer_secret.is_none(),
            "--emit-effect-writer-secret is valid only when preparing a generation"
        );
        retire_effect_writer_generation(
            &admin_config,
            org,
            project,
            environment,
            &database,
            generation,
        )
        .await
    } else if let Some(generation) = args.abort_effect_writer_generation {
        anyhow::ensure!(
            args.emit_effect_writer_secret.is_none(),
            "--emit-effect-writer-secret is valid only when preparing a generation"
        );
        abort_effect_writer_generation(
            &admin_config,
            org,
            project,
            environment,
            &database,
            generation,
        )
        .await
    } else {
        anyhow::bail!("no effect-writer generation action selected")
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the action, admin authority, exact scope, selected generation, output, and injected clock are distinct security inputs"
)]
async fn prepare_effect_writer_generation(
    args: &ProvisionProjectEnvArgs,
    admin_config: &PgConfig,
    org: &str,
    project: &str,
    environment: &str,
    database: &str,
    generation: CredentialGeneration,
    secret_path: &Path,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let role = effect_writer_generation_role(org, project, environment, database, generation);
    let other_role =
        effect_writer_generation_role(org, project, environment, database, generation.other());
    let (mut admin, admin_task) = connect_config(admin_config, "effect-writer admin").await?;
    let scope_key = effect_writer_scope_hash(org, project, environment, database);
    lock_effect_writer_scope(&admin, &scope_key).await?;
    let transaction = admin
        .transaction()
        .await
        .context("begin effect-writer generation prepare")?;
    transaction
        .batch_execute(sql::revoke_public_connect_floor_sql())
        .await
        .context("converge cluster PUBLIC CONNECT floor")?;
    verify_public_access_floor(&transaction).await?;
    let desired = read_effect_writer_role_state(&transaction, &role).await?;
    let other = read_effect_writer_role_state(&transaction, &other_role).await?;
    let recovering_active = match (generation, desired.as_ref(), other.as_ref()) {
        (CredentialGeneration::A, desired, None)
            if desired.is_none_or(EffectWriterRoleState::is_inactive) =>
        {
            false
        }
        (CredentialGeneration::A, Some(desired), None)
            if desired.is_active_for(database) && desired.sessions == 0 =>
        {
            true
        }
        (_, desired, Some(other))
            if desired.is_none_or(EffectWriterRoleState::is_inactive)
                && other.is_active_for(database) =>
        {
            false
        }
        (_, Some(desired), Some(other))
            if desired.is_active_for(database)
                && desired.sessions == 0
                && other.is_active_for(database) =>
        {
            true
        }
        (CredentialGeneration::B, None, None) => {
            anyhow::bail!("initial effect-writer credential generation must be a")
        }
        _ => anyhow::bail!(
            "effect-writer generation prepare requires an inactive target, or an exact zero-session active target recovered after failed Secret publication"
        ),
    };
    let desired_acl = if recovering_active {
        RoleAclExpectation::Generation { database }
    } else {
        RoleAclExpectation::None
    };
    verify_role_acl_inventory(admin_config, &role, desired_acl).await?;
    if other.is_some() {
        verify_role_acl_inventory(
            admin_config,
            &other_role,
            RoleAclExpectation::Generation { database },
        )
        .await?;
    }
    if read_effect_writer_role_state(&transaction, EFFECT_WRITER_ROLE)
        .await?
        .is_some()
    {
        verify_role_acl_inventory(
            admin_config,
            EFFECT_WRITER_ROLE,
            RoleAclExpectation::EffectAclRole,
        )
        .await?;
    }
    if read_effect_writer_role_state(&transaction, RUN_PROJECTION_WRITER_ROLE)
        .await?
        .is_some()
    {
        verify_role_acl_inventory(
            admin_config,
            RUN_PROJECTION_WRITER_ROLE,
            RoleAclExpectation::ProjectionAclRole,
        )
        .await?;
    }

    let password = random_lower_hex(32)?;
    let credential_id = random_lower_hex(16)?;
    let validity = effect_writer_validity(now);
    transaction
        .batch_execute(&sql::prepare_effect_writer_generation_sql(
            database,
            &role,
            &password,
            &validity.expires_at,
        ))
        .await
        .context("prepare effect-writer credential generation")?;
    transaction
        .commit()
        .await
        .context("commit effect-writer credential generation prepare")?;

    let publish_result = async {
        let writer_config = writer_config(admin_config, &role, &password, database);
        authenticate_effect_writer(&writer_config, &role, database)
            .await
            .context("authenticate prepared effect-writer generation")?;

        let prepared = read_effect_writer_role_state(&admin, &role)
            .await?
            .context("prepared effect-writer generation disappeared")?;
        anyhow::ensure!(
            prepared.is_active_for(database),
            "prepared effect-writer generation did not have the exact active ACL"
        );
        anyhow::ensure!(
            prepared.valid_until.as_deref() == Some(validity.expires_at.as_str()),
            "prepared effect-writer generation VALID UNTIL does not match credential expires-at"
        );
        verify_role_acl_inventory(
            admin_config,
            &role,
            RoleAclExpectation::Generation { database },
        )
        .await?;
        let acl_role = read_effect_writer_role_state(&admin, EFFECT_WRITER_ROLE)
            .await?
            .context("stable effect-writer ACL role disappeared")?;
        anyhow::ensure!(
            acl_role.is_acl_role(),
            "stable effect-writer ACL role is not a connection-free NOLOGIN role"
        );
        verify_role_acl_inventory(
            admin_config,
            EFFECT_WRITER_ROLE,
            RoleAclExpectation::EffectAclRole,
        )
        .await?;
        let projection_role = read_effect_writer_role_state(&admin, RUN_PROJECTION_WRITER_ROLE)
            .await?
            .context("stable run-projection ACL role disappeared")?;
        anyhow::ensure!(
            projection_role.is_acl_role(),
            "stable run-projection ACL role is not a connection-free NOLOGIN role"
        );
        verify_role_acl_inventory(
            admin_config,
            RUN_PROJECTION_WRITER_ROLE,
            RoleAclExpectation::ProjectionAclRole,
        )
        .await?;

        let scope = EffectWriterCredentialScope {
            org: org.to_string(),
            project: project.to_string(),
            environment: environment.to_string(),
            database: database.to_string(),
        };
        let credential_url = writer_url(
            args.target_admin_database_url
                .as_deref()
                .expect("action validated target admin URL"),
            &role,
            &password,
            database,
        )?;
        let credential = effect_writer_credential(
            &scope,
            &credential_id,
            generation,
            &validity,
            &credential_url,
        );
        let triple = Triple::new(org, project, environment);
        let secret = render_effect_writer_secret_manifest(&triple, &args.namespace, &credential);
        write_secret_json(secret_path, &secret)
            .context("write authenticated effect-writer Secret")?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if let Err(error) = publish_result {
        let rollback =
            rollback_prepared_effect_writer_generation(&admin, admin_config, database, &role).await;
        drop(admin);
        let _ = admin_task.await;
        if let Err(rollback_error) = rollback {
            anyhow::bail!(
                "effect-writer prepare failed after LOGIN was enabled: {error:#}; rollback also failed: {rollback_error:#}"
            );
        }
        return Err(error);
    }
    println!(
        "prepared and authenticated effect-writer credential generation {} for {}/{}/{}; wrote {}",
        generation.as_str(),
        org,
        project,
        environment,
        secret_path.display()
    );
    drop(admin);
    let _ = admin_task.await;
    Ok(())
}

async fn rollback_prepared_effect_writer_generation(
    admin: &(impl GenericClient + Sync),
    admin_config: &PgConfig,
    database: &str,
    role: &str,
) -> anyhow::Result<()> {
    admin
        .batch_execute(&sql::retire_effect_writer_generation_sql(database, role))
        .await
        .context("revoke prepared effect-writer generation authority")?;
    admin
        .batch_execute(&sql::terminate_effect_writer_generation_sessions_sql(role))
        .await
        .context("terminate prepared effect-writer generation sessions")?;
    let state = read_effect_writer_role_state(admin, role)
        .await?
        .context("rolled-back effect-writer generation disappeared")?;
    anyhow::ensure!(
        state.is_inactive(),
        "rolled-back effect-writer generation did not converge to inactive"
    );
    verify_role_acl_inventory(admin_config, role, RoleAclExpectation::None).await
}

async fn retire_effect_writer_generation(
    admin_config: &PgConfig,
    org: &str,
    project: &str,
    environment: &str,
    database: &str,
    generation: CredentialGeneration,
) -> anyhow::Result<()> {
    let old_role = effect_writer_generation_role(org, project, environment, database, generation);
    let replacement_role =
        effect_writer_generation_role(org, project, environment, database, generation.other());
    let (mut admin, admin_task) = connect_config(admin_config, "effect-writer admin").await?;
    let scope_key = effect_writer_scope_hash(org, project, environment, database);
    lock_effect_writer_scope(&admin, &scope_key).await?;
    let transaction = admin
        .transaction()
        .await
        .context("begin effect-writer generation retirement")?;
    verify_public_access_floor(&transaction).await?;
    let old = read_effect_writer_role_state(&transaction, &old_role)
        .await?
        .context("old effect-writer generation does not exist")?;
    let replacement = read_effect_writer_role_state(&transaction, &replacement_role)
        .await?
        .context("replacement effect-writer generation does not exist")?;
    anyhow::ensure!(
        old.is_active_for(database),
        "old effect-writer generation is not the exact active credential"
    );
    anyhow::ensure!(
        replacement.is_active_for(database),
        "replacement effect-writer generation is not LOGIN-capable with exact ACL"
    );
    anyhow::ensure!(
        replacement.sessions > 0,
        "replacement effect-writer generation has no verified live private-pool session"
    );
    verify_role_acl_inventory(
        admin_config,
        &old_role,
        RoleAclExpectation::Generation { database },
    )
    .await?;
    verify_role_acl_inventory(
        admin_config,
        &replacement_role,
        RoleAclExpectation::Generation { database },
    )
    .await?;
    transaction
        .batch_execute(&sql::retire_effect_writer_generation_sql(
            database, &old_role,
        ))
        .await
        .context("retire old effect-writer credential generation")?;
    transaction
        .commit()
        .await
        .context("commit effect-writer generation retirement")?;
    admin
        .batch_execute(&sql::terminate_effect_writer_generation_sessions_sql(
            &old_role,
        ))
        .await
        .context("terminate retired effect-writer generation sessions")?;
    let retired = read_effect_writer_role_state(&admin, &old_role)
        .await?
        .context("retired effect-writer generation disappeared")?;
    anyhow::ensure!(
        retired.is_inactive(),
        "old effect-writer generation did not converge to inactive"
    );
    println!(
        "retired effect-writer credential generation {} for {}/{}/{}",
        generation.as_str(),
        org,
        project,
        environment
    );
    drop(admin);
    let _ = admin_task.await;
    Ok(())
}

async fn abort_effect_writer_generation(
    admin_config: &PgConfig,
    org: &str,
    project: &str,
    environment: &str,
    database: &str,
    generation: CredentialGeneration,
) -> anyhow::Result<()> {
    let role = effect_writer_generation_role(org, project, environment, database, generation);
    let (mut admin, admin_task) = connect_config(admin_config, "effect-writer admin").await?;
    let scope_key = effect_writer_scope_hash(org, project, environment, database);
    lock_effect_writer_scope(&admin, &scope_key).await?;
    let transaction = admin
        .transaction()
        .await
        .context("begin effect-writer generation abort")?;
    verify_public_access_floor(&transaction).await?;
    let prepared = read_effect_writer_role_state(&transaction, &role)
        .await?
        .context("prepared effect-writer generation does not exist")?;
    anyhow::ensure!(
        prepared.is_active_for(database),
        "prepared effect-writer generation is not the exact active credential"
    );
    anyhow::ensure!(
        prepared.sessions == 0,
        "published or in-use effect-writer generation cannot be aborted"
    );
    verify_role_acl_inventory(
        admin_config,
        &role,
        RoleAclExpectation::Generation { database },
    )
    .await?;
    let acl_role = read_effect_writer_role_state(&transaction, EFFECT_WRITER_ROLE)
        .await?
        .context("stable effect-writer ACL role does not exist")?;
    anyhow::ensure!(
        acl_role.is_acl_role(),
        "stable effect-writer ACL role is not a connection-free NOLOGIN role"
    );
    verify_role_acl_inventory(
        admin_config,
        EFFECT_WRITER_ROLE,
        RoleAclExpectation::EffectAclRole,
    )
    .await?;
    let projection_role = read_effect_writer_role_state(&transaction, RUN_PROJECTION_WRITER_ROLE)
        .await?
        .context("stable run-projection ACL role does not exist")?;
    anyhow::ensure!(
        projection_role.is_acl_role(),
        "stable run-projection ACL role is not a connection-free NOLOGIN role"
    );
    verify_role_acl_inventory(
        admin_config,
        RUN_PROJECTION_WRITER_ROLE,
        RoleAclExpectation::ProjectionAclRole,
    )
    .await?;
    transaction
        .batch_execute(&sql::retire_effect_writer_generation_sql(database, &role))
        .await
        .context("abort unpublished effect-writer credential generation")?;
    transaction
        .commit()
        .await
        .context("commit effect-writer generation abort")?;
    admin
        .batch_execute(&sql::terminate_effect_writer_generation_sessions_sql(&role))
        .await
        .context("terminate aborted effect-writer generation sessions")?;
    let aborted = read_effect_writer_role_state(&admin, &role)
        .await?
        .context("aborted effect-writer generation disappeared")?;
    anyhow::ensure!(
        aborted.is_inactive(),
        "aborted effect-writer generation did not converge to inactive"
    );
    verify_role_acl_inventory(admin_config, &role, RoleAclExpectation::None).await?;
    println!(
        "aborted unpublished effect-writer credential generation {} for {}/{}/{}",
        generation.as_str(),
        org,
        project,
        environment
    );
    drop(admin);
    let _ = admin_task.await;
    Ok(())
}

fn effect_writer_validity(now: DateTime<Utc>) -> EffectWriterCredentialValidity {
    let expires_at = now + chrono::Duration::days(EFFECT_WRITER_CREDENTIAL_TTL_DAYS);
    EffectWriterCredentialValidity {
        issued_at: now.to_rfc3339_opts(SecondsFormat::Secs, true),
        not_before: now.to_rfc3339_opts(SecondsFormat::Secs, true),
        expires_at: expires_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        revoked_at: None,
    }
}

fn random_lower_hex(bytes: usize) -> anyhow::Result<String> {
    let mut material = vec![0_u8; bytes];
    SystemRandom::new()
        .fill(&mut material)
        .map_err(|_| anyhow::anyhow!("operating system could not supply credential entropy"))?;
    Ok(hex::encode(material))
}

/// Alphabet of the provision-minted instance suffix: `[a-z0-9]`, 36 symbols, so
/// eight of them carry ~41 bits. Narrower than an identity slug's on purpose —
/// the suffix is the LAST bytes of a DNS-1123 label, which must end alphanumeric.
const INSTANCE_SUFFIX_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";

/// Largest multiple of the alphabet size that fits in a byte (252). A draw at or
/// above it is redrawn rather than folded: a plain `% 36` would over-weight the
/// first four symbols, and the suffix's uniform randomness IS the non-reuse
/// mechanism (wamn-0h0g.13.57) — nothing else keeps a recreated environment off
/// a deleted one's names.
const INSTANCE_SUFFIX_REJECT_AT: usize = 256 - 256 % INSTANCE_SUFFIX_ALPHABET.len();

/// Mint one environment's instance suffix. The randomness is the whole
/// uniqueness mechanism: no naming registry, no collision-retry loop, no
/// derivation from the triple (an owner ruling of wamn-0h0g.13.57).
///
/// The mint lives HERE and not in `wamn-control-provision` because that crate is
/// deliberately pure — no DB, no K8s client, no clock, and no entropy. It takes
/// the suffix as a parameter and derives names from it.
fn mint_instance_suffix() -> anyhow::Result<String> {
    let random = SystemRandom::new();
    let mut suffix = String::with_capacity(INSTANCE_SUFFIX_LEN);
    let mut draw = [0_u8; INSTANCE_SUFFIX_LEN];
    while suffix.len() < INSTANCE_SUFFIX_LEN {
        random.fill(&mut draw).map_err(|_| {
            anyhow::anyhow!("operating system could not supply instance-suffix entropy")
        })?;
        for byte in draw {
            if usize::from(byte) >= INSTANCE_SUFFIX_REJECT_AT {
                continue;
            }
            let index = usize::from(byte) % INSTANCE_SUFFIX_ALPHABET.len();
            suffix.push(char::from(INSTANCE_SUFFIX_ALPHABET[index]));
            if suffix.len() == INSTANCE_SUFFIX_LEN {
                break;
            }
        }
    }
    Ok(suffix)
}

fn exact_project_database_config(admin_url: &str, database: &str) -> anyhow::Result<PgConfig> {
    let config = PgConfig::from_str(admin_url).context("parse target admin database URL")?;
    anyhow::ensure!(
        config.get_dbname() == Some(database),
        "--target-admin-database-url must name the exact project database"
    );
    Ok(config)
}

fn writer_config(admin: &PgConfig, role: &str, password: &str, database: &str) -> PgConfig {
    let mut config = admin.clone();
    config.user(role);
    config.password(password);
    config.dbname(database);
    config
}

fn writer_url(
    admin_url: &str,
    role: &str,
    password: &str,
    database: &str,
) -> anyhow::Result<String> {
    let mut url = Url::parse(admin_url).context("parse target admin URL for credential")?;
    anyhow::ensure!(
        matches!(url.scheme(), "postgres" | "postgresql"),
        "target admin URL must use postgres or postgresql"
    );
    url.set_username(role)
        .map_err(|_| anyhow::anyhow!("set effect-writer URL username"))?;
    url.set_password(Some(password))
        .map_err(|_| anyhow::anyhow!("set effect-writer URL password"))?;
    url.set_path(&format!("/{database}"));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.into())
}

async fn connect_config(
    config: &PgConfig,
    purpose: &'static str,
) -> anyhow::Result<(
    tokio_postgres::Client,
    tokio::task::JoinHandle<Result<(), tokio_postgres::Error>>,
)> {
    let (client, connection) = config
        .connect(NoTls)
        .await
        .with_context(|| format!("connect {purpose}"))?;
    Ok((client, tokio::spawn(connection)))
}

async fn authenticate_effect_writer(
    config: &PgConfig,
    role: &str,
    database: &str,
) -> anyhow::Result<()> {
    let (client, task) = connect_config(config, "prepared effect-writer generation").await?;
    let row = client
        .query_one(
            "SELECT current_user::text, current_database()::text, \
                    has_database_privilege(current_user, current_database(), 'TEMPORARY')",
            &[],
        )
        .await
        .context("probe prepared effect-writer generation")?;
    let current_user: String = row.get(0);
    let current_database: String = row.get(1);
    let can_create_temporary: bool = row.get(2);
    anyhow::ensure!(
        current_user == role,
        "prepared generation authenticated as wrong role"
    );
    anyhow::ensure!(
        current_database == database,
        "prepared generation authenticated to wrong database"
    );
    anyhow::ensure!(
        !can_create_temporary,
        "prepared generation inherited TEMPORARY on the project database"
    );
    drop(client);
    task.await
        .context("join effect-writer authentication connection")??;
    Ok(())
}

async fn lock_effect_writer_scope(
    client: &(impl GenericClient + Sync),
    scope_key: &str,
) -> anyhow::Result<()> {
    client
        .query_one(sql::effect_writer_scope_lock_sql(), &[&scope_key])
        .await
        .context("acquire effect-writer scope rotation lock")?;
    Ok(())
}

async fn verify_public_access_floor(client: &(impl GenericClient + Sync)) -> anyhow::Result<()> {
    let databases: Vec<String> = client
        .query(sql::public_connect_databases_sql(), &[])
        .await
        .context("verify cluster PUBLIC CONNECT floor")?
        .into_iter()
        .map(|row| row.get(0))
        .collect();
    anyhow::ensure!(
        databases.is_empty(),
        "effect-writer preparation requires PUBLIC CONNECT revoked on every connectable database (template1 included); still granted on {databases:?}"
    );
    let public_temporary: bool = client
        .query_one(sql::public_temporary_on_current_database_sql(), &[])
        .await
        .context("verify target database PUBLIC TEMPORARY floor")?
        .get(0);
    anyhow::ensure!(
        !public_temporary,
        "effect-writer generation actions require PUBLIC TEMPORARY revoked on the exact project database"
    );
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoleAclExpectation<'a> {
    None,
    Generation { database: &'a str },
    EffectAclRole,
    ProjectionAclRole,
}

async fn verify_role_acl_inventory(
    admin_config: &PgConfig,
    role: &str,
    expectation: RoleAclExpectation<'_>,
) -> anyhow::Result<()> {
    let (catalog, catalog_task) = connect_config(admin_config, "role ACL inventory").await?;
    let databases: Vec<String> = catalog
        .query(sql::non_template_databases_sql(), &[])
        .await
        .context("list databases for role ACL inventory")?
        .into_iter()
        .map(|row| row.get(0))
        .collect();
    drop(catalog);
    catalog_task
        .await
        .context("join ACL catalog connection")??;

    for database in databases {
        let mut config = admin_config.clone();
        config.dbname(&database);
        let (client, task) = connect_config(&config, "cross-database role ACL inventory").await?;
        let rows = client
            .query(sql::role_database_acl_inventory_sql(), &[&role])
            .await
            .with_context(|| format!("read role ACL inventory in database {database:?}"))?;
        let inventory: Vec<RoleAcl> = rows
            .into_iter()
            .map(|row| RoleAcl {
                object_kind: row.get("object_kind"),
                schema_name: row.get("schema_name"),
                object_name: row.get("object_name"),
                privilege: row.get("privilege_type"),
                grantable: row.get("is_grantable"),
            })
            .collect();
        for acl in &inventory {
            anyhow::ensure!(
                !acl.grantable,
                "role {role:?} may grant {} on {} {}.{} in database {database:?}",
                acl.privilege,
                acl.object_kind,
                acl.schema_name,
                acl.object_name,
            );
        }
        match expectation {
            RoleAclExpectation::EffectAclRole => {
                verify_effect_writer_acl_role_inventory(role, &database, &inventory)?;
            }
            RoleAclExpectation::ProjectionAclRole => {
                verify_run_projection_acl_role_inventory(role, &database, &inventory)?;
            }
            expectation => {
                for acl in &inventory {
                    let allowed = match expectation {
                        RoleAclExpectation::None => false,
                        RoleAclExpectation::Generation { database: expected } => {
                            acl.object_kind == "database"
                                && database == expected
                                && acl.object_name == expected
                                && acl.privilege == "CONNECT"
                        }
                        RoleAclExpectation::EffectAclRole
                        | RoleAclExpectation::ProjectionAclRole => {
                            unreachable!("handled above")
                        }
                    };
                    anyhow::ensure!(
                        allowed,
                        "role {role:?} carries unexpected direct {} on {} {}.{} in database {database:?}",
                        acl.privilege,
                        acl.object_kind,
                        acl.schema_name,
                        acl.object_name,
                    );
                }
            }
        }
        drop(client);
        task.await.context("join cross-database ACL connection")??;
    }
    Ok(())
}

fn verify_run_projection_acl_role_inventory(
    role: &str,
    database: &str,
    inventory: &[RoleAcl],
) -> anyhow::Result<()> {
    let mut by_schema: BTreeMap<String, BTreeSet<(String, String, String)>> = BTreeMap::new();
    for acl in inventory {
        anyhow::ensure!(
            matches!(acl.object_kind.as_str(), "schema" | "relation"),
            "stable role {role:?} carries non-projection {} ACL in database {database:?}",
            acl.object_kind
        );
        by_schema
            .entry(acl.schema_name.clone())
            .or_default()
            .insert((
                acl.object_kind.clone(),
                acl.object_name.clone(),
                acl.privilege.clone(),
            ));
    }
    for (schema, actual) in by_schema {
        anyhow::ensure!(
            !schema.starts_with("pg_")
                && !matches!(
                    schema.as_str(),
                    "public" | "information_schema" | "wamn_system" | "catalog" | "app"
                ),
            "stable role {role:?} carries projection ACLs in reserved schema {schema:?} in database {database:?}"
        );
        let mut expected =
            BTreeSet::from([("schema".to_string(), schema.clone(), "USAGE".to_string())]);
        for privilege in ["SELECT", "INSERT", "UPDATE", "DELETE"] {
            expected.insert((
                "relation".to_string(),
                "node_runs".to_string(),
                privilege.to_string(),
            ));
        }
        anyhow::ensure!(
            actual == expected,
            "stable role {role:?} ACLs in database {database:?} schema {schema:?} are not the exact run-projection grant set"
        );
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RoleAcl {
    object_kind: String,
    schema_name: String,
    object_name: String,
    privilege: String,
    grantable: bool,
}

fn verify_effect_writer_acl_role_inventory(
    role: &str,
    database: &str,
    inventory: &[RoleAcl],
) -> anyhow::Result<()> {
    let mut by_schema: BTreeMap<String, BTreeSet<(String, String, String)>> = BTreeMap::new();
    for acl in inventory {
        anyhow::ensure!(
            matches!(acl.object_kind.as_str(), "schema" | "relation" | "column"),
            "stable role {role:?} carries non-writer {} ACL in database {database:?}",
            acl.object_kind
        );
        by_schema
            .entry(acl.schema_name.clone())
            .or_default()
            .insert((
                acl.object_kind.clone(),
                acl.object_name.clone(),
                acl.privilege.clone(),
            ));
    }
    for (schema, actual) in by_schema {
        anyhow::ensure!(
            !schema.starts_with("pg_")
                && !matches!(
                    schema.as_str(),
                    "public" | "information_schema" | "wamn_system" | "catalog" | "app"
                ),
            "stable role {role:?} carries effect-writer ACLs in reserved schema {schema:?} in database {database:?}"
        );
        let mut expected =
            BTreeSet::from([("schema".to_string(), schema.clone(), "USAGE".to_string())]);
        for table in [
            "effect_attempts",
            "effect_attempt_dispatches",
            "effect_attempt_outcomes",
        ] {
            expected.insert((
                "relation".to_string(),
                table.to_string(),
                "SELECT".to_string(),
            ));
            expected.insert((
                "relation".to_string(),
                table.to_string(),
                "INSERT".to_string(),
            ));
        }
        for (table, columns) in [
            ("runs", &["tenant_id", "run_id", "status"][..]),
            (
                "run_queue",
                &[
                    "tenant_id",
                    "run_id",
                    "lease_owner",
                    "lease_expires_at",
                    "lease_generation",
                ][..],
            ),
        ] {
            for column in columns {
                expected.insert((
                    "column".to_string(),
                    format!("{table}.{column}"),
                    "SELECT".to_string(),
                ));
            }
        }
        anyhow::ensure!(
            actual == expected,
            "stable role {role:?} ACLs in database {database:?} schema {schema:?} are not the exact effect-writer grant set"
        );
    }
    Ok(())
}

async fn read_effect_writer_role_state(
    client: &(impl GenericClient + Sync),
    role: &str,
) -> anyhow::Result<Option<EffectWriterRoleState>> {
    let row = client
        .query_opt(sql::effect_writer_generation_state_sql(), &[&role])
        .await
        .context("read effect-writer generation state")?;
    Ok(row.map(|row| EffectWriterRoleState {
        login: row.get("rolcanlogin"),
        superuser: row.get("rolsuper"),
        inherit: row.get("rolinherit"),
        create_role: row.get("rolcreaterole"),
        create_db: row.get("rolcreatedb"),
        replication: row.get("rolreplication"),
        bypass_rls: row.get("rolbypassrls"),
        password_set: row.get("password_set"),
        valid_until: row.get("valid_until"),
        valid_until_finite: row.get("valid_until_finite"),
        memberships: row.get("memberships"),
        membership_options_exact: row.get("membership_options_exact"),
        member_roles: row.get("member_roles"),
        member_options_exact: row.get("member_options_exact"),
        generation_children_exact: row.get("generation_children_exact"),
        connect_databases: row.get("connect_databases"),
        sessions: row.get("sessions"),
        owned_objects: row.get("owned_objects"),
    }))
}

const PAT_TTL: Duration = Duration::from_secs(2_592_000);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PatPurpose {
    purpose: &'static str,
    subject_stem: &'static str,
    display_stem: &'static str,
    role: &'static str,
    secret_stem: &'static str,
}

const MANAGEMENT_AUTHOR: PatPurpose = PatPurpose {
    purpose: "management-author",
    subject_stem: "wamn-management-author",
    display_stem: "WAMN management author",
    role: "project-author",
    secret_stem: "wamn-pat-management-author",
};

const ROUTE_CALLER: PatPurpose = PatPurpose {
    purpose: "route-caller",
    subject_stem: "wamn-route-caller",
    display_stem: "WAMN route caller",
    role: "route-caller",
    secret_stem: "wamn-pat-route-caller",
};

impl PatPurpose {
    fn subject(self, triple: &Triple) -> String {
        format!(
            "{}-{}--{}--{}",
            self.subject_stem, triple.org, triple.project, triple.env
        )
    }

    fn display_name(self, triple: &Triple) -> String {
        format!(
            "{} {}/{}/{}",
            self.display_stem, triple.org, triple.project, triple.env
        )
    }

    fn secret_name(self, triple: &Triple) -> String {
        format!(
            "{}-{}--{}--{}",
            self.secret_stem, triple.org, triple.project, triple.env
        )
    }
}

async fn issue_pat_secrets(
    system_url: &str,
    triple: &Triple,
    namespace: &str,
    management_author_path: Option<&Path>,
    route_caller_path: Option<&Path>,
) -> anyhow::Result<()> {
    let (client, connection) = tokio_postgres::connect(system_url, NoTls)
        .await
        .context("system db connect for PAT issuance")?;
    let connection_task = tokio::spawn(connection);
    let result = async {
        client
            .batch_execute("SET ROLE wamn_system")
            .await
            .context("SET ROLE wamn_system for PAT issuance")?;
        if let Some(path) = management_author_path {
            issue_pat_secret(&client, triple, namespace, MANAGEMENT_AUTHOR, path).await?;
        }
        if let Some(path) = route_caller_path {
            issue_pat_secret(&client, triple, namespace, ROUTE_CALLER, path).await?;
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;
    drop(client);
    let _ = connection_task.await;
    result
}

async fn issue_pat_secret(
    client: &tokio_postgres::Client,
    triple: &Triple,
    namespace: &str,
    purpose: PatPurpose,
    path: &Path,
) -> anyhow::Result<()> {
    let subject = purpose.subject(triple);
    let display_name = purpose.display_name(triple);
    let principal = resolve_or_create_service(client, &subject, &display_name).await?;
    anyhow::ensure!(
        principal.status() == PrincipalStatus::Active,
        "service principal {subject:?} is disabled"
    );
    assign_project_role(
        client,
        principal.id(),
        &triple.org,
        &triple.project,
        purpose.role,
    )
    .await
    .with_context(|| format!("assign {} role", purpose.role))?;

    let issued = issue_pat(client, principal.id(), purpose.purpose, PAT_TTL)
        .await
        .with_context(|| format!("issue {} PAT", purpose.purpose))?;
    let authenticated = authenticate_pat(client, issued.token())
        .await
        .with_context(|| format!("authenticate newly issued {} PAT", purpose.purpose))?
        .with_context(|| format!("newly issued {} PAT did not authenticate", purpose.purpose))?;
    anyhow::ensure!(
        authenticated.principal() == &principal,
        "newly issued {} PAT authenticated as an unexpected principal",
        purpose.purpose
    );

    let secret = render_pat_secret(
        triple,
        namespace,
        purpose,
        principal.id().as_str(),
        issued.token(),
        issued.record().prefix(),
        issued.record().expires_at(),
    );
    write_secret_json(path, &secret)?;
    println!(
        "wrote {} ({} PAT Secret; kubectl apply)",
        path.display(),
        purpose.purpose
    );
    Ok(())
}

async fn resolve_or_create_service(
    client: &tokio_postgres::Client,
    subject: &str,
    display_name: &str,
) -> anyhow::Result<Principal> {
    if let Some(principal) = resolve_subject(client, PrincipalKind::Service, subject)
        .await
        .context("resolve service principal")?
    {
        return Ok(principal);
    }

    match create_service(client, subject, display_name).await {
        Ok(principal) => Ok(principal),
        Err(error) if error.kind() == IdentityErrorKind::Conflict => {
            resolve_subject(client, PrincipalKind::Service, subject)
                .await
                .context("resolve concurrently created service principal")?
                .context("service principal conflict was not resolvable")
        }
        Err(error) => Err(error).context("create service principal"),
    }
}

async fn revoke_provisioning_pat(system_url: &str, prefix: &str) -> anyhow::Result<()> {
    let (client, connection) = tokio_postgres::connect(system_url, NoTls)
        .await
        .context("system db connect for PAT revocation")?;
    let connection_task = tokio::spawn(connection);
    let result = async {
        client
            .batch_execute("SET ROLE wamn_system")
            .await
            .context("SET ROLE wamn_system for PAT revocation")?;
        revoke_pat(&client, prefix)
            .await
            .context("revoke PAT by prefix")?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    drop(client);
    let _ = connection_task.await;
    result
}

fn render_pat_secret(
    triple: &Triple,
    namespace: &str,
    purpose: PatPurpose,
    principal_id: &str,
    token: &str,
    prefix: &str,
    expires_at: &str,
) -> Value {
    let subject = purpose.subject(triple);
    json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": purpose.secret_name(triple),
            "namespace": namespace,
            "labels": {
                "app.kubernetes.io/managed-by": "wamn",
                "app.kubernetes.io/component": "project-env-pat",
                "wamn.org": triple.org,
                "wamn.project": triple.project,
                "wamn.env": triple.env.as_str(),
            },
            "annotations": {
                "wamn.io/credential-purpose": purpose.purpose,
                "wamn.io/principal-id": principal_id,
                "wamn.io/principal-kind": "service",
                "wamn.io/principal-subject": subject,
                "wamn.io/project-role": purpose.role,
                "wamn.io/pat-prefix": prefix,
                "wamn.io/pat-expires-at": expires_at,
            },
        },
        "type": "Opaque",
        "stringData": {
            "token": token,
        },
    })
}

fn provision_summary(triple: &Triple, database: &str, cluster: &str) -> String {
    format!(
        "project-env {triple}: database {database:?} on cluster {cluster:?} (owner {DB_OWNER_ROLE})"
    )
}

fn parse_secret_path(value: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    ensure_secret_path(&path, "secret output").map_err(|error| error.to_string())?;
    Ok(path)
}

fn ensure_secret_path(path: &Path, flag: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        path.as_os_str() != "-",
        "{flag} must name a file; '-' and stdout are forbidden for credentials"
    );
    Ok(())
}

fn ensure_distinct_secret_paths<const N: usize>(
    paths: [(&str, Option<&Path>); N],
) -> anyhow::Result<()> {
    let mut seen = Vec::with_capacity(N);
    for (flag, path) in paths {
        let Some(path) = path else {
            continue;
        };
        let absolute = std::path::absolute(path)
            .with_context(|| format!("resolve credential output path for {flag}"))?;
        let parent = absolute
            .parent()
            .context("credential output path has no parent directory")?;
        let file_name = absolute
            .file_name()
            .context("credential output path has no file name")?;
        let comparable = std::fs::canonicalize(parent)
            .with_context(|| format!("resolve credential output parent for {flag}"))?
            .join(file_name);
        if let Some((other_flag, _)) = seen
            .iter()
            .find(|(_, other_path)| other_path == &comparable)
        {
            anyhow::bail!("{flag} and {other_flag} must name distinct credential output files");
        }
        seen.push((flag, comparable));
    }
    Ok(())
}

fn parse_pat_prefix(value: &str) -> Result<String, String> {
    let valid = value.len() == 16
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'));
    if !valid {
        return Err("PAT prefix must be 16 lowercase hex digits".to_owned());
    }
    Ok(value.to_owned())
}

static SECRET_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn create_secret_temp(path: &Path) -> anyhow::Result<(PathBuf, File)> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .context("credential output path has no file name")?;

    for _ in 0..128 {
        let sequence = SECRET_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut temp_name = OsString::from(".");
        temp_name.push(file_name);
        temp_name.push(format!(".wamn-tmp-{}-{sequence}", std::process::id()));
        let temp_path = parent.join(temp_name);
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(&temp_path)
        {
            Ok(file) => return Ok((temp_path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("create credential output beside {}", path.display())
                });
            }
        }
    }
    anyhow::bail!(
        "could not allocate a temporary credential output beside {}",
        path.display()
    )
}

fn write_secret_json(path: &Path, doc: &Value) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(doc).context("serialize Secret JSON")?;
    bytes.push(b'\n');
    let (temp_path, mut file) = create_secret_temp(path)?;
    let result = (|| -> anyhow::Result<()> {
        file.set_permissions(Permissions::from_mode(0o600))
            .with_context(|| format!("set credential output mode on {}", temp_path.display()))?;
        file.write_all(&bytes)
            .with_context(|| format!("write credential output {}", temp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("sync credential output {}", temp_path.display()))?;
        drop(file);
        std::fs::rename(&temp_path, path)
            .with_context(|| format!("install credential output {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

/// Read the org's placement + the env's policy from the registry and **derive**
/// the target cluster via [`cluster_of`] (D18): a pooled org collapses onto its
/// pool; a dedicated org owns `<org>-<owner(env)>`. Connects as the `wamn_system`
/// owner (`SET ROLE`). Shared with the `enable-cdc-project-env` overlay
/// (wamn-l5i9.9), which targets the same derived cluster.
pub(crate) async fn resolve_cluster(
    system_url: &str,
    org: &str,
    env: &str,
) -> anyhow::Result<String> {
    let (client, conn) = tokio_postgres::connect(system_url, NoTls)
        .await
        .context("system db connect")?;
    let conn_task = tokio::spawn(conn);
    let result = do_resolve_cluster(&client, org, env).await;
    drop(client);
    let _ = conn_task.await;
    result
}

async fn do_resolve_cluster(
    client: &tokio_postgres::Client,
    org: &str,
    env: &str,
) -> anyhow::Result<String> {
    client
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("SET ROLE wamn_system")?;
    let row = client
        .query_opt(
            wamn_control_registry::sql::select_org_placement_sql(),
            &[&org],
        )
        .await
        .context("read org placement")?
        .with_context(|| {
            format!(
                "org {org:?} is not registered: run provision-org before provisioning a project-env"
            )
        })?;
    let placement_kind: String = row.get("placement_kind");
    let pool: Option<String> = row.get("pool_cluster");
    let placement = match placement_kind.as_str() {
        "pooled" => Placement::Pooled {
            pool: pool.context("pooled org row is missing its pool_cluster")?,
        },
        "dedicated" => Placement::Dedicated,
        other => anyhow::bail!("unknown placement_kind {other:?} for org {org:?}"),
    };
    let org_obj = Org {
        id: org.to_string(),
        placement,
    };
    // The env must name a policy in the ORG's own set (8df.4 — its recovery
    // domain drives the derivation); a pooled org ignores the policy but the env
    // must still resolve.
    let policy = read_env_policy(client, org, env).await?.with_context(|| {
        format!(
            "env {env:?} names none of org {org:?}'s env policies — provision-org stamps them \
             from a template; customize/add rows in registry.env_policies"
        )
    })?;
    Ok(cluster_of(&org_obj, &policy).name)
}

/// Record the project and the provisioned project-env in the registry (idempotent).
/// Connects as superuser and `SET ROLE wamn_system` (the registry owner — the
/// wamn-q3n.3 apply pattern), then runs the pure `wamn-control-registry` builders.
///
/// Returns the environment's STORED instance suffix, which is `minted` only on a
/// first provision — see [`do_record_project_env`].
async fn record_project_env(
    system_url: &str,
    triple: &Triple,
    secret_name: &str,
    secret_namespace: Option<&str>,
    minted: &str,
) -> anyhow::Result<String> {
    let (client, conn) = tokio_postgres::connect(system_url, NoTls)
        .await
        .context("system db connect")?;
    let conn_task = tokio::spawn(conn);
    let result =
        do_record_project_env(&client, triple, secret_name, secret_namespace, minted).await;
    drop(client);
    let _ = conn_task.await;
    result
}

async fn do_record_project_env(
    client: &tokio_postgres::Client,
    triple: &Triple,
    secret_name: &str,
    secret_namespace: Option<&str>,
    minted: &str,
) -> anyhow::Result<String> {
    client
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("SET ROLE wamn_system")?;
    client
        .execute(
            wamn_control_registry::sql::upsert_project_sql(),
            &[&triple.org, &triple.project],
        )
        .await
        .context("upsert registry.projects row")?;
    let env = triple.env.as_str();
    let row = client
        .query_one(
            wamn_control_registry::sql::upsert_project_env_sql(),
            &[
                &triple.org,
                &triple.project,
                &env,
                &secret_name,
                &secret_namespace,
                &minted,
            ],
        )
        .await
        .context("upsert registry.project_envs row")?;
    // Read-or-mint: the upsert RETURNS the STORED suffix, which is the freshly
    // minted one on a first provision and the EXISTING one when this triple was
    // already provisioned — the upsert deliberately never refreshes it, because
    // re-minting would orphan every resource the old suffix named. The registry
    // is a trust boundary, so the value is re-checked before any name derives
    // from it.
    let stored: String = row.get(0);
    validate_instance_suffix(&stored)
        .map_err(|error| anyhow::anyhow!("registry instance suffix: {error}"))?;
    Ok(stored)
}

/// Print a JSON document to a path, or to stdout with a labeled header when the
/// path is absent (`-` also means stdout).
fn emit_json(path: &Option<PathBuf>, label: &str, doc: &serde_json::Value) -> anyhow::Result<()> {
    emit_text(path, label, &serde_json::to_string_pretty(doc)?)
}

fn emit_text(path: &Option<PathBuf>, label: &str, text: &str) -> anyhow::Result<()> {
    match path {
        Some(p) if p.as_os_str() != "-" => {
            std::fs::write(p, text).with_context(|| format!("write {}", p.display()))?;
            println!("wrote {} ({label})", p.display());
        }
        _ => println!("--- {label} ---\n{text}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(flatten)]
        args: ProvisionProjectEnvArgs,
    }

    fn parse_args(extra: &[&str]) -> Result<ProvisionProjectEnvArgs, clap::Error> {
        let mut argv = vec![
            "test",
            "--org",
            "acme",
            "--project",
            "billing",
            "--env",
            "dev",
            // Required with no default (wamn-0h0g.12.122); every invocation of
            // this subcommand must now supply it.
            "--dispatch-reader-password",
            "reader-probe",
        ];
        argv.extend_from_slice(extra);
        TestCli::try_parse_from(argv).map(|cli| cli.args)
    }

    /// The mint is the whole non-reuse mechanism (wamn-0h0g.13.57): every draw
    /// must satisfy the pure crate's rule, and the draws must actually differ.
    /// A constant, a counter, or a triple-derived suffix collapses `seen`.
    #[test]
    fn the_minted_instance_suffix_is_a_fresh_valid_dns_label_tail() {
        // A duplicated symbol would bias the `% len()` fold; a symbol outside
        // `[a-z0-9]` would leave the namespace an illegal DNS-1123 label.
        assert_eq!(INSTANCE_SUFFIX_ALPHABET.len(), 36);
        assert_eq!(
            INSTANCE_SUFFIX_ALPHABET
                .iter()
                .collect::<BTreeSet<_>>()
                .len(),
            INSTANCE_SUFFIX_ALPHABET.len()
        );
        assert!(
            INSTANCE_SUFFIX_ALPHABET
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        );

        let mut seen = BTreeSet::new();
        for _ in 0..64 {
            let suffix = mint_instance_suffix().expect("mint an instance suffix");
            validate_instance_suffix(&suffix).expect("a minted suffix satisfies the pure rule");
            seen.insert(suffix);
        }
        assert_eq!(seen.len(), 64, "64 draws collapsed to {}", seen.len());
    }

    #[test]
    fn a_reserved_or_bad_project_id_is_rejected_before_any_effect() {
        // The name validation runs first — a reserved / non-slug project id fails
        // without touching the registry or emitting a CR.
        assert!(validate_project_env("acme", "wamn-x", "dev").is_err());
        assert!(validate_project_env("acme", "Bad", "prod").is_err());
        assert!(validate_project_env("acme", "billing", "prod").is_ok());
    }

    /// The target cluster is DERIVED (D18 `cluster_of`) from the org's placement +
    /// the env's policy: a dedicated org owns `<org>-<owner(env)>`, a pooled org
    /// collapses every env onto its pool. (The live routing through the DB is
    /// proven by the in-cluster gate; here we pin the pure derivation the
    /// subcommand calls.)
    #[test]
    fn cluster_is_derived_by_placement_and_policy() {
        use wamn_control_registry::EnvPolicy;
        let ded = Org::dedicated("acme");
        assert_eq!(cluster_of(&ded, &EnvPolicy::dev()).name, "acme-dev");
        assert_eq!(cluster_of(&ded, &EnvPolicy::prod()).name, "acme-prod");
        let pooled = Org::pooled("try", "wamn-pg");
        assert_eq!(cluster_of(&pooled, &EnvPolicy::prod()).name, "wamn-pg");
    }

    #[test]
    fn pat_literals_and_secret_documents_are_exact() {
        let triple = Triple::new("acme", "billing", "dev");
        assert_eq!(PAT_TTL, Duration::from_secs(2_592_000));
        assert_eq!(
            MANAGEMENT_AUTHOR.subject(&triple),
            "wamn-management-author-acme--billing--dev"
        );
        assert_eq!(
            MANAGEMENT_AUTHOR.display_name(&triple),
            "WAMN management author acme/billing/dev"
        );
        assert_eq!(
            ROUTE_CALLER.subject(&triple),
            "wamn-route-caller-acme--billing--dev"
        );
        assert_eq!(
            ROUTE_CALLER.display_name(&triple),
            "WAMN route caller acme/billing/dev"
        );

        let secret = render_pat_secret(
            &triple,
            "wamn-system",
            MANAGEMENT_AUTHOR,
            "6d3f2d1c-0000-4000-8000-00000000abcd",
            "wamn_pat_token-material",
            "0123456789abcdef",
            "2026-09-09T12:34:56Z",
        );
        assert_eq!(
            secret,
            json!({
                "apiVersion": "v1",
                "kind": "Secret",
                "metadata": {
                    "name": "wamn-pat-management-author-acme--billing--dev",
                    "namespace": "wamn-system",
                    "labels": {
                        "app.kubernetes.io/managed-by": "wamn",
                        "app.kubernetes.io/component": "project-env-pat",
                        "wamn.org": "acme",
                        "wamn.project": "billing",
                        "wamn.env": "dev",
                    },
                    "annotations": {
                        "wamn.io/credential-purpose": "management-author",
                        "wamn.io/principal-id": "6d3f2d1c-0000-4000-8000-00000000abcd",
                        "wamn.io/principal-kind": "service",
                        "wamn.io/principal-subject": "wamn-management-author-acme--billing--dev",
                        "wamn.io/project-role": "project-author",
                        "wamn.io/pat-prefix": "0123456789abcdef",
                        "wamn.io/pat-expires-at": "2026-09-09T12:34:56Z",
                    },
                },
                "type": "Opaque",
                "stringData": {
                    "token": "wamn_pat_token-material",
                },
            })
        );
        assert_eq!(
            secret["stringData"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            ["token"]
        );

        let route_secret = render_pat_secret(
            &triple,
            "wamn-system",
            ROUTE_CALLER,
            "6d3f2d1c-0000-4000-8000-00000000abcd",
            "wamn_pat_other-material",
            "fedcba9876543210",
            "2026-09-09T12:34:56Z",
        );
        assert_eq!(
            route_secret["metadata"]["name"],
            "wamn-pat-route-caller-acme--billing--dev"
        );
        assert_eq!(
            route_secret["metadata"]["annotations"]["wamn.io/project-role"],
            "route-caller"
        );
    }

    #[test]
    fn pat_issue_flags_select_independently_and_revoke_conflicts() {
        let management = parse_args(&[
            "--emit-secret",
            "/tmp/db.json",
            "--emit-management-author-pat-secret",
            "/tmp/management.json",
        ])
        .unwrap();
        assert!(management.emit_management_author_pat_secret.is_some());
        assert!(management.emit_route_caller_pat_secret.is_none());

        let route = parse_args(&[
            "--emit-secret",
            "/tmp/db.json",
            "--emit-route-caller-pat-secret",
            "/tmp/route.json",
        ])
        .unwrap();
        assert!(route.emit_management_author_pat_secret.is_none());
        assert!(route.emit_route_caller_pat_secret.is_some());

        let both = parse_args(&[
            "--emit-secret",
            "/tmp/db.json",
            "--emit-management-author-pat-secret",
            "/tmp/management.json",
            "--emit-route-caller-pat-secret",
            "/tmp/route.json",
        ])
        .unwrap();
        assert!(both.emit_management_author_pat_secret.is_some());
        assert!(both.emit_route_caller_pat_secret.is_some());

        // `--dispatch-reader-password` is required with no default
        // (wamn-0h0g.12.122), so even the revoke-only invocation — which
        // provisions nothing — must carry it.
        let revoke = TestCli::try_parse_from([
            "test",
            "--system-database-url",
            "postgresql://postgres@localhost/postgres",
            "--revoke-pat-prefix",
            "0123456789abcdef",
            "--dispatch-reader-password",
            "reader-probe",
        ])
        .unwrap()
        .args;
        assert_eq!(
            revoke.revoke_pat_prefix.as_deref(),
            Some("0123456789abcdef")
        );
        assert!(revoke.emit_secret.is_none());
        assert!(revoke.org.is_none());
        assert!(revoke.project.is_none());
        assert!(revoke.env.is_none());

        let abort = parse_args(&[
            "--target-admin-database-url",
            "postgresql://postgres@localhost/wamn-db-acme--billing--dev",
            "--abort-effect-writer-generation",
            "a",
        ])
        .unwrap();
        assert_eq!(
            abort.abort_effect_writer_generation,
            Some(CredentialGeneration::A)
        );
        assert!(abort.emit_secret.is_none());
        assert!(abort.emit_effect_writer_secret.is_none());

        for issue_flag in [
            "--emit-management-author-pat-secret",
            "--emit-route-caller-pat-secret",
        ] {
            assert!(
                parse_args(&[
                    "--revoke-pat-prefix",
                    "0123456789abcdef",
                    issue_flag,
                    "/tmp/pat.json",
                ])
                .is_err(),
                "revoke accepted conflicting {issue_flag}"
            );
        }
    }

    #[test]
    fn every_secret_output_rejects_stdout_and_prefix_is_strict() {
        assert!(
            parse_args(&[]).is_err(),
            "database Secret path became optional"
        );
        assert!(parse_args(&["--emit-secret", "-"]).is_err());
        for issue_flag in [
            "--emit-management-author-pat-secret",
            "--emit-route-caller-pat-secret",
        ] {
            assert!(
                parse_args(&["--emit-secret", "/tmp/db.json", issue_flag, "-"]).is_err(),
                "{issue_flag} accepted stdout"
            );
        }
        for invalid in ["0123456789abcde", "0123456789ABCDEF", "0123456789abcdeg"] {
            assert!(
                parse_args(&["--revoke-pat-prefix", invalid]).is_err(),
                "accepted invalid PAT prefix {invalid:?}"
            );
        }
    }

    #[test]
    fn parsed_credential_outputs_must_name_distinct_files() {
        for argv in [
            vec![
                "--emit-secret",
                "/tmp/shared-credential.json",
                "--emit-management-author-pat-secret",
                "/tmp/shared-credential.json",
            ],
            vec![
                "--emit-secret",
                "/tmp/shared-credential.json",
                "--emit-route-caller-pat-secret",
                "/tmp/shared-credential.json",
            ],
            vec![
                "--emit-secret",
                "/tmp/shared-credential.json",
                "--emit-management-author-pat-secret",
                "/tmp/./shared-credential.json",
            ],
            vec![
                "--emit-secret",
                "/tmp/db.json",
                "--emit-management-author-pat-secret",
                "/tmp/shared-credential.json",
                "--emit-route-caller-pat-secret",
                "/tmp/shared-credential.json",
            ],
        ] {
            let args = parse_args(&argv).unwrap();
            let error = ensure_distinct_secret_paths([
                ("--emit-secret", args.emit_secret.as_deref()),
                (
                    "--emit-management-author-pat-secret",
                    args.emit_management_author_pat_secret.as_deref(),
                ),
                (
                    "--emit-route-caller-pat-secret",
                    args.emit_route_caller_pat_secret.as_deref(),
                ),
            ])
            .expect_err("duplicate credential output was accepted");
            assert!(error.to_string().contains("must name distinct"));
        }

        let args = parse_args(&[
            "--emit-secret",
            "/tmp/db.json",
            "--emit-management-author-pat-secret",
            "/tmp/management.json",
            "--emit-route-caller-pat-secret",
            "/tmp/route.json",
        ])
        .unwrap();
        ensure_distinct_secret_paths([
            ("--emit-secret", args.emit_secret.as_deref()),
            (
                "--emit-management-author-pat-secret",
                args.emit_management_author_pat_secret.as_deref(),
            ),
            (
                "--emit-route-caller-pat-secret",
                args.emit_route_caller_pat_secret.as_deref(),
            ),
        ])
        .unwrap();
    }

    #[test]
    fn secret_files_are_plain_json_and_mode_0600() {
        let path = std::env::temp_dir().join(format!(
            "wamn-ctl-pat-secret-mode-{}.json",
            std::process::id()
        ));
        std::fs::write(&path, b"old credential material").unwrap();
        std::fs::set_permissions(&path, Permissions::from_mode(0o644)).unwrap();

        let document = json!({"stringData": {"token": "test-token"}});
        write_secret_json(&path, &document).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(stored, document);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn secret_writes_replace_links_without_following_or_sharing_them() {
        let sequence = SECRET_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "wamn-ctl-pat-secret-links-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();

        let db_path = root.join("db.json");
        let management_path = root.join("management.json");
        std::os::unix::fs::symlink(&db_path, &management_path).unwrap();
        let db_document = json!({"stringData": {"url": "db-secret"}});
        let management_document = json!({"stringData": {"token": "management-secret"}});
        write_secret_json(&db_path, &db_document).unwrap();
        write_secret_json(&management_path, &management_document).unwrap();
        assert!(
            !std::fs::symlink_metadata(&management_path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let stored_db: Value = serde_json::from_slice(&std::fs::read(&db_path).unwrap()).unwrap();
        let stored_management: Value =
            serde_json::from_slice(&std::fs::read(&management_path).unwrap()).unwrap();
        assert_eq!(stored_db, db_document);
        assert_eq!(stored_management, management_document);

        let route_path = root.join("route.json");
        std::fs::hard_link(&management_path, &route_path).unwrap();
        let route_document = json!({"stringData": {"token": "route-secret"}});
        write_secret_json(&route_path, &route_document).unwrap();
        let stored_management: Value =
            serde_json::from_slice(&std::fs::read(&management_path).unwrap()).unwrap();
        let stored_route: Value =
            serde_json::from_slice(&std::fs::read(&route_path).unwrap()).unwrap();
        assert_eq!(stored_management, management_document);
        assert_eq!(stored_route, route_document);

        std::fs::remove_file(db_path).unwrap();
        std::fs::remove_file(management_path).unwrap();
        std::fs::remove_file(route_path).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn provisioning_summary_contains_no_database_credentials() {
        let triple = Triple::new("acme", "billing", "dev");
        let summary = provision_summary(&triple, "wamn-db-acme--billing--dev", "acme-dev");
        assert_eq!(
            summary,
            "project-env acme/billing/dev: database \"wamn-db-acme--billing--dev\" on cluster \"acme-dev\" (owner wamn_db_owner)"
        );
        assert!(!summary.contains("postgres://"));
        assert!(!summary.contains("password"));
        assert!(!summary.contains("app url"));
    }

    /// wamn-0h0g.12.122. The emitted privilege batch is PINNED whole: a runtime
    /// gate that only asserts "the reader can connect" stays green when a
    /// builder is swapped for a wider one, and stays green when the `CONNECT`
    /// grant drifts back above the owner statement on a database that happens
    /// to be owned by `wamn_db_owner` already. The frozen literal is the guard.
    #[test]
    fn the_privilege_batch_grants_reader_connect_after_the_owner_statement() {
        let batch = privilege_sql("wamn-db-acme--billing--dev");
        assert_eq!(
            batch,
            "ALTER DATABASE \"wamn-db-acme--billing--dev\" OWNER TO \"wamn_db_owner\";\n\
             REVOKE CONNECT, TEMPORARY ON DATABASE \"wamn-db-acme--billing--dev\" FROM PUBLIC; \
             GRANT CONNECT ON DATABASE \"wamn-db-acme--billing--dev\" TO \"wamn_app\";\n\
             GRANT CONNECT ON DATABASE \"wamn-db-acme--billing--dev\" TO \"wamn_dispatch_reader\";\n"
        );

        // The ordering assertion, stated independently of the frozen literal so
        // a deliberate re-pin cannot silently drop it.
        let owner = batch
            .find("ALTER DATABASE")
            .expect("the owner statement is emitted");
        let reader_connect = batch
            .find("GRANT CONNECT ON DATABASE \"wamn-db-acme--billing--dev\" TO \"wamn_dispatch_reader\"")
            .expect("the reader CONNECT grant is emitted");
        assert!(
            owner < reader_connect,
            "reader CONNECT must follow ALTER DATABASE … OWNER TO: {batch}"
        );

        // The additive grant must not have displaced the app-role confinement.
        assert!(batch.contains("REVOKE CONNECT, TEMPORARY ON DATABASE"));
        assert!(batch.contains("TO \"wamn_app\";"));
    }

    /// The role batch must actually CREATE the principal the dispatcher's
    /// projects file names. Before wamn-0h0g.12.122 the example manifest named
    /// a role production provisioning never created.
    #[test]
    fn the_role_batch_creates_the_dispatch_reader_from_the_shipped_builder() {
        let batch = role_sql("app-secret", "reader-secret");
        assert_eq!(
            batch,
            format!(
                "{app}\n{owner}\n{reader}\n",
                app = sql::ensure_app_role_sql("app-secret"),
                owner = sql::ensure_db_owner_role_sql(),
                reader = sql::ensure_dispatch_reader_role_sql("reader-secret"),
            )
        );
        assert!(batch.contains(
            "CREATE ROLE \"wamn_dispatch_reader\" LOGIN PASSWORD 'reader-secret' NOSUPERUSER \
             NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS"
        ));
        // Each password reaches its own builder: a swapped argument would hand
        // the dispatcher the shared application credential.
        assert!(
            batch.contains("CREATE ROLE \"wamn_app\" LOGIN PASSWORD 'app-secret'"),
            "app role lost its own password: {batch}"
        );
        assert!(!batch.contains("\"wamn_dispatch_reader\" LOGIN PASSWORD 'app-secret'"));
    }

    /// The owner ruling: no `default_value`. `--app-password` above still
    /// carries one, which is a separate, filed defect — this test exists so the
    /// new argument cannot quietly acquire the same shape.
    #[test]
    fn the_dispatch_reader_password_has_no_default() {
        assert!(
            TestCli::try_parse_from([
                "test",
                "--org",
                "acme",
                "--project",
                "billing",
                "--env",
                "dev",
                "--emit-secret",
                "/tmp/db.json",
            ])
            .is_err(),
            "provisioning accepted a missing --dispatch-reader-password"
        );
        assert_eq!(
            parse_args(&["--emit-secret", "/tmp/db.json"])
                .unwrap()
                .dispatch_reader_password,
            "reader-probe"
        );
    }

    fn role_acl(kind: &str, schema: &str, object: &str, privilege: &str) -> RoleAcl {
        RoleAcl {
            object_kind: kind.to_string(),
            schema_name: schema.to_string(),
            object_name: object.to_string(),
            privilege: privilege.to_string(),
            grantable: false,
        }
    }

    #[test]
    fn stable_acl_inventory_accepts_only_the_complete_writer_schema_set() {
        let schema = "wamn_runner_demo";
        let mut exact = vec![role_acl("schema", schema, schema, "USAGE")];
        for table in [
            "effect_attempts",
            "effect_attempt_dispatches",
            "effect_attempt_outcomes",
        ] {
            exact.push(role_acl("relation", schema, table, "SELECT"));
            exact.push(role_acl("relation", schema, table, "INSERT"));
        }
        for (table, columns) in [
            ("runs", &["tenant_id", "run_id", "status"][..]),
            (
                "run_queue",
                &[
                    "tenant_id",
                    "run_id",
                    "lease_owner",
                    "lease_expires_at",
                    "lease_generation",
                ][..],
            ),
        ] {
            for column in columns {
                exact.push(role_acl(
                    "column",
                    schema,
                    &format!("{table}.{column}"),
                    "SELECT",
                ));
            }
        }
        verify_effect_writer_acl_role_inventory(EFFECT_WRITER_ROLE, "project_db", &exact).unwrap();

        let mut partial = exact.clone();
        partial.pop();
        assert!(
            verify_effect_writer_acl_role_inventory(EFFECT_WRITER_ROLE, "project_db", &partial)
                .is_err()
        );
        let mut unrelated = exact;
        unrelated.push(role_acl("relation", schema, "other_table", "SELECT"));
        assert!(
            verify_effect_writer_acl_role_inventory(EFFECT_WRITER_ROLE, "project_db", &unrelated)
                .is_err()
        );

        let mut reserved = vec![role_acl("schema", "app", "app", "USAGE")];
        for table in [
            "effect_attempts",
            "effect_attempt_dispatches",
            "effect_attempt_outcomes",
        ] {
            reserved.push(role_acl("relation", "app", table, "SELECT"));
            reserved.push(role_acl("relation", "app", table, "INSERT"));
        }
        assert!(
            verify_effect_writer_acl_role_inventory(EFFECT_WRITER_ROLE, "project_db", &reserved)
                .is_err()
        );
    }

    #[test]
    fn stable_acl_inventory_refuses_an_object_kind_outside_the_writer_set() {
        // The kind filter is this guard's first refusal, and the complete-set test
        // above cannot observe it: every fixture there is a schema, relation or
        // column, so admitting one further kind left that test green under the
        // wamn-0h0g.15.107 mutation run. A database CONNECT ACL is the realistic
        // out-of-set kind — it is what a scoped generation role is granted — and a
        // stable role must never hold one. Asserting the named refusal is the
        // load-bearing part: a widened kind set still fails, but on the exact
        // grant set instead, and would leave the filter unverified again.
        let error = verify_effect_writer_acl_role_inventory(
            EFFECT_WRITER_ROLE,
            "project_db",
            &[role_acl("database", "project_db", "project_db", "CONNECT")],
        )
        .expect_err("a database ACL is not an effect-writer ACL");
        assert!(
            error
                .to_string()
                .contains("carries non-writer database ACL"),
            "refused for the wrong reason: {error}"
        );
    }

    #[test]
    fn stable_acl_role_members_are_only_scoped_generation_roles() {
        assert!(is_effect_writer_generation_role(
            "wamn_effect_writer_0123456789abcdef0123456789abcdef01234567_a"
        ));
        for invalid in [
            "wamn_effect_writer_a",
            "wamn_effect_writer_0123456789ABCDEF0123456789abcdef01234567_a",
            "wamn_effect_writer_0123456789abcdef0123456789abcdef01234567_c",
            "unrelated_0123456789abcdef0123456789abcdef01234567_a",
        ] {
            assert!(
                !is_effect_writer_generation_role(invalid),
                "accepted {invalid}"
            );
        }
    }
}
