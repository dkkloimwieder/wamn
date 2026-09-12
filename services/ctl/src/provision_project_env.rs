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
use clap::Args;
use ring::rand::SystemRandom;
use serde_json::{Value, json};
use tokio_postgres::{Config as PgConfig, GenericClient, NoTls};
use url::Url;

use wamn_control_provision::SystemReader;
use wamn_control_provision::session_target::{SessionTarget, validate_session_tenant_id};
use wamn_control_provision::tenant_key::tenant_key;
use wamn_control_provision::{
    APP_ROLE, CredentialGeneration, DB_OWNER_ROLE, EffectWriterCredentialScope,
    EffectWriterCredentialValidity, INSTANCE_SUFFIX_LEN, PLATFORM_GROUP_ROLE, WorkloadRoleFamily,
    WorkloadRoleScope, WorkloadRoleScopeKind, WorkloadSecretBody, WorkloadSecretBodyKind,
    compose_url, effect_writer_credential, legacy_effect_writer_generation_role,
    project_env_database_name, project_env_namespace, project_env_secret_name,
    render_project_env_database, render_project_env_secret_manifest,
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
use crate::pat_client::{PatClient, PatIssuerArgs};

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
use output::{emit_json, emit_text, ensure_distinct_secret_paths, ensure_secret_path, parse_secret_path};
use pat_secrets::{issue_pat_secrets, parse_pat_prefix, revoke_provisioning_pat};
use registry::{mint_instance_suffix, record_project_env};
use workload::run_workload_action;

pub(crate) use registry::{project_tenant_environment, read_project_env_instance, resolve_cluster};

pub(crate) use output::write_secret_json;

#[cfg(test)]
use output::SECRET_TEMP_SEQUENCE;
#[cfg(test)]
use pat_secrets::{MANAGEMENT_AUTHOR, PAT_TTL, ROUTE_CALLER, render_pat_secret};

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

    /// Tenant identity for tenant-scoped workload credential generations.
    /// It is never inferred from the project or environment.
    #[arg(long)]
    pub tenant: Option<String>,

    /// Mark this environment DISPOSABLE: its admitted component facts may be
    /// REPLACED rather than frozen, so an author's no-op edit does not lock them
    /// out of their own package version (wamn-10yt.38).
    ///
    /// `wamn dev up` provisions its own per-run target with this. Nothing else
    /// passes it, and the default is what keeps every other environment's
    /// admitted fact immutable BY CONSTRUCTION rather than by a caller
    /// remembering to withhold a flag. The marker is recorded on the
    /// environment's registry row and projected into the control store; the
    /// admit path reads THAT, never an argument of its own.
    #[arg(long, default_value_t = false)]
    pub disposable: bool,

    /// Superuser Postgres URL to the T1 system DB (`wamn_system`): read the org's
    /// placement, read-or-mint the stored instance suffix, and record the project
    /// + project-env. Env `WAMN_SYSTEM_ADMIN_URL`.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: Option<String>,

    /// Override the target CNPG `Cluster` name. When omitted, it is read from the
    /// org's placement in the registry.
    #[arg(long)]
    pub cluster: Option<String>,

    /// Per-project-env `CONNECTION LIMIT` (noisy-neighbour governance within a
    /// cluster). Default: no limit (`-1`).
    #[arg(long)]
    pub connection_limit: Option<i64>,

    /// Password carried by the legacy shared-app URL surface. Supply it with
    /// `--app-password` or the env var `WAMN_APP_PASSWORD`.
    ///
    /// `wamn-0h0g.12.140` removes it from role SQL: `wamn_app` is now a stable
    /// passwordless NOLOGIN ACL role. The argument remains until
    /// `wamn-0h0g.12.185` retires the legacy command and URL surface itself.
    ///
    /// **Deliberately has no `default_value`.** A default here provisioned every
    /// project-env with a publicly known password on a `LOGIN` role that
    /// guest-authored SQL executes as; a 2026-08-19 verifier read measured it
    /// live on every cluster the role existed on, because nothing ever
    /// overrode it. Provisioning refuses instead (wamn-0h0g.12.129).
    ///
    /// **Required only where it is consumed** (wamn-0h0g.12.141): the exempt
    /// list is `--emit-secret`'s, member for member, because the credential and
    /// the Secret are wanted by exactly the same invocations. The refusal stays
    /// a parse error — mode-scoping narrows *when* the guard fires, never
    /// weakens it into a runtime check.
    ///
    /// `wamn-0h0g.22.16` names the exemption ONCE, as the derived
    /// [`WORKLOAD_ACTION_GROUP`], instead of listing each family's three flags.
    /// Listing them by hand is what left the guest family out of all three
    /// exempt lists: preparing a guest generation demanded an `--emit-secret`
    /// that its own action then refused, so the mode was unrunnable.
    #[arg(
        long,
        env = "WAMN_APP_PASSWORD",
        value_name = "PASSWORD ($WAMN_APP_PASSWORD)",
        required_unless_present_any = ["revoke_pat_prefix", WORKLOAD_ACTION_GROUP]
    )]
    pub app_password: Option<String>,

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

    /// Explicit target project-database admin URL for the generation actions that
    /// address the project-env database (effect-writer, management-admitter).
    /// Provisioning authority only: never persisted or emitted.
    #[arg(
        long,
        env = "WAMN_TARGET_ADMIN_DATABASE_URL",
        hide_env_values = true,
        value_name = "URL"
    )]
    pub target_admin_database_url: Option<String>,

    /// The workload-generation actions and their credential Secrets, DERIVED
    /// from the closed [`WorkloadRoleFamily`] set rather than written out per
    /// family (`wamn-0h0g.22.16`). See [`WorkloadGenerationArgs`].
    #[command(flatten)]
    pub workload: WorkloadGenerationArgs,

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
        required_unless_present_any = ["revoke_pat_prefix", WORKLOAD_ACTION_GROUP]
    )]
    pub emit_secret: Option<PathBuf>,

    /// Operator authentication for PAT issuance through `wamn-identity`.
    #[command(flatten)]
    pub pat_issuer: PatIssuerArgs,

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
        value_parser = parse_pat_prefix
    )]
    pub revoke_pat_prefix: Option<String>,
}

/// The id of the ONE group every derived workload action flag belongs to.
///
/// This constant is the whole exclusion rule. Before `wamn-0h0g.22.16` the same
/// rule was spelled out as SIXTEEN hand-written `conflicts_with_all` /
/// `required_unless_present_any` arrays, and admitting a family meant
/// remembering to append its three flag names to every one of them. A closed
/// enum that must be appended to by hand in sixteen places is not closed; it is
/// a checklist.
pub const WORKLOAD_ACTION_GROUP: &str = "workload_generation_action";

/// The id of the one group every derived credential-Secret flag belongs to.
const WORKLOAD_SECRET_GROUP: &str = "workload_generation_secret";

/// The three verbs of the `wamn-0h0g.13.59` unified generation lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkloadActionVerb {
    Prepare,
    Retire,
    Abort,
}

impl WorkloadActionVerb {
    const ALL: [Self; 3] = [Self::Prepare, Self::Retire, Self::Abort];

    const fn as_str(self) -> &'static str {
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

/// The workload-generation half of the parser, DERIVED from the closed
/// [`WorkloadRoleFamily`] set (`wamn-0h0g.22.16`).
///
/// [`clap::Args`] is implemented by hand rather than derived because the flag
/// SET is a function of the family set: `#[derive(Args)]` can only name fields
/// that were typed out, which is exactly the hand-maintained list this
/// replaces. Mutual exclusion is one [`clap::ArgGroup`] per concern, so
/// admitting a family joins its flags to those groups by construction.
#[derive(Debug, Default, Clone)]
pub struct WorkloadGenerationArgs {
    /// The single selected action. `multiple(false)` on the action group makes
    /// "single" a parse-time guarantee rather than a convention.
    pub action: Option<WorkloadGenerationAction>,
    /// The credential Secret to write, bound by `requires` to its OWN family's
    /// prepare — so a Secret can accompany neither another family's action nor
    /// a retire or abort.
    pub secret: Option<(WorkloadRoleFamily, PathBuf)>,
}

/// `--<verb>-<family>-generation`, the flag one family's one verb answers to.
fn workload_action_flag(family: WorkloadRoleFamily, verb: WorkloadActionVerb) -> String {
    format!("{}-{}-generation", verb.as_str(), family.cli_stem())
}

fn workload_action_id(family: WorkloadRoleFamily, verb: WorkloadActionVerb) -> String {
    workload_action_flag(family, verb).replace('-', "_")
}

/// `--emit-<family>-secret`, the path one family's prepared credential is
/// written to.
fn workload_secret_flag(family: WorkloadRoleFamily) -> String {
    format!("emit-{}-secret", family.cli_stem())
}

fn workload_secret_id(family: WorkloadRoleFamily) -> String {
    workload_secret_flag(family).replace('-', "_")
}

impl clap::Args for WorkloadGenerationArgs {
    fn augment_args(command: clap::Command) -> clap::Command {
        let mut command = command;
        let mut action_ids: Vec<clap::Id> = Vec::new();
        let mut secret_ids: Vec<clap::Id> = Vec::new();
        for family in WorkloadRoleFamily::ALL {
            let stem = family.cli_stem();
            for verb in WorkloadActionVerb::ALL {
                let id = workload_action_id(family, verb);
                command = command.arg(
                    clap::Arg::new(id.clone())
                        .long(workload_action_flag(family, verb))
                        .value_name("a|b")
                        .value_parser(clap::value_parser!(CredentialGeneration))
                        .help(format!(
                            "{} the {stem} credential generation",
                            verb.as_str()
                        )),
                );
                action_ids.push(clap::Id::from(id));
            }
            let secret = workload_secret_id(family);
            let own_prepare = workload_action_id(family, WorkloadActionVerb::Prepare);
            let mut argument = clap::Arg::new(secret.clone())
                .long(workload_secret_flag(family))
                .value_name("PATH")
                .value_parser(parse_secret_path)
                // Bound to its OWN family's prepare: credentials are never
                // written to stdout and never without a mint.
                .requires(own_prepare.clone())
                .help(format!("write the prepared {stem} credential Secret here"));
            // `requires` alone is not enough. Clap does not report a required
            // argument missing when it CONFLICTS with one that is present, and
            // every action shares the exclusive group above — so a retire, an
            // abort, or another family's prepare would silently satisfy the
            // requirement. Naming the conflict outright is what refuses them,
            // and it is derived here rather than written out per family.
            for other in WorkloadRoleFamily::ALL {
                for verb in WorkloadActionVerb::ALL {
                    let id = workload_action_id(other, verb);
                    if id != own_prepare {
                        argument = argument.conflicts_with(id);
                    }
                }
            }
            command = command.arg(argument);
            secret_ids.push(clap::Id::from(secret));
        }
        command
            .group(
                clap::ArgGroup::new(WORKLOAD_ACTION_GROUP)
                    .args(action_ids)
                    .multiple(false),
            )
            .group(
                clap::ArgGroup::new(WORKLOAD_SECRET_GROUP)
                    .args(secret_ids)
                    .multiple(false),
            )
    }

    fn augment_args_for_update(command: clap::Command) -> clap::Command {
        Self::augment_args(command)
    }
}

impl clap::FromArgMatches for WorkloadGenerationArgs {
    fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        let mut parsed = Self::default();
        parsed.update_from_arg_matches(matches)?;
        Ok(parsed)
    }

    fn update_from_arg_matches(&mut self, matches: &clap::ArgMatches) -> Result<(), clap::Error> {
        self.action = None;
        self.secret = None;
        for family in WorkloadRoleFamily::ALL {
            for verb in WorkloadActionVerb::ALL {
                if let Some(generation) =
                    matches.get_one::<CredentialGeneration>(&workload_action_id(family, verb))
                {
                    self.action = Some(WorkloadGenerationAction {
                        family,
                        verb,
                        generation: *generation,
                    });
                }
            }
            if let Some(path) = matches.get_one::<PathBuf>(&workload_secret_id(family)) {
                self.secret = Some((family, path.clone()));
            }
        }
        Ok(())
    }
}

impl ProvisionProjectEnvArgs {
    /// The credential Secret path this family's prepare named, if any.
    fn workload_secret_path(&self, family: WorkloadRoleFamily) -> Option<&Path> {
        self.workload
            .secret
            .as_ref()
            .filter(|(named, _)| *named == family)
            .map(|(_, path)| path.as_path())
    }
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
/// The app-password parameter remains for the legacy command/URL surface owned
/// by `wamn-0h0g.12.185`, but [`sql::ensure_app_role_sql`] deliberately emits
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

    // ONE dispatch over the closed family set, replacing four hand-written
    // branches that each had to be added beside a new `run_*_action`.
    if let Some(action) = args.workload.action {
        return run_workload_action(&args, action).await;
    }
    anyhow::ensure!(
        args.target_admin_database_url.is_none(),
        "--target-admin-database-url is valid only for a workload generation action"
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
    // Refuse incomplete TLS configuration before registry writes or artifacts.
    let pat_client = issues_pat
        .then(|| PatClient::new(&args.pat_issuer))
        .transpose()?;

    let org = args
        .org
        .as_deref()
        .expect("clap parser invariant: --org is required unless --revoke-pat-prefix is present");
    let project = args.project.as_deref().expect(
        "clap parser invariant: --project is required unless --revoke-pat-prefix is present",
    );
    let env = args
        .env
        .as_deref()
        .expect("clap parser invariant: --env is required unless --revoke-pat-prefix is present");
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
    // `--app-password` is `required_unless_present_any` over the modes that
    // provision nothing, and every one of those has already returned above. A
    // missing credential here is a broken parser contract, not a user error:
    // re-checking it would plant a second, weaker enforcement point and hollow
    // out the parse-time refusal (wamn-0h0g.12.141).
    let app_password = args
        .app_password
        .as_deref()
        .expect("clap requires --app-password on every provisioning invocation");
    let app_url = compose_url(APP_ROLE, app_password, &app_host, args.app_port, &db_name);

    // Render the artifacts the runbook applies.
    let db_cr = render_project_env_database(&triple, &instance, &cluster, args.connection_limit);
    // Ordinary provisioning establishes cluster roles before it creates the
    // database. The shared-login drain is an operator finalizer and is emitted
    // only by a successful App-generation prepare after every carrier has a
    // replacement credential.
    let role_sql = role_posture_sql(app_password);
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

    println!(
        "recorded project {:?} + project-env {} in the registry (wamn_system)",
        project, triple
    );
    println!(
        "environment namespace {:?} (instance suffix {instance:?})",
        project_env_namespace(org, project, env, &instance)
    );

    if let Some(pat_client) = pat_client.as_ref() {
        issue_pat_secrets(
            system_url,
            pat_client,
            &triple,
            &args.namespace,
            args.emit_management_author_pat_secret.as_deref(),
            args.emit_route_caller_pat_secret.as_deref(),
        )
        .await?;
    }

    Ok(())
}

fn provision_summary(triple: &Triple, database: &str, cluster: &str) -> String {
    format!(
        "project-env {triple}: database {database:?} on cluster {cluster:?} (owner {DB_OWNER_ROLE})"
    )
}

#[cfg(test)]
mod tests;
