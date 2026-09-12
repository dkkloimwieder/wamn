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
use ring::rand::{SecureRandom as _, SystemRandom};
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
mod output;
mod pat_secrets;

use database::{
    connect_config, exact_project_database_config, named_database_config, workload_config,
    workload_url,
};
use output::{emit_json, emit_text, ensure_distinct_secret_paths, ensure_secret_path, parse_secret_path};
use pat_secrets::{issue_pat_secrets, parse_pat_prefix, revoke_provisioning_pat};

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

const WORKLOAD_CREDENTIAL_TTL_DAYS: i64 = 30;

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkloadRoleState {
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
    membership_options_migratable: bool,
    member_roles: Vec<String>,
    member_options_exact: bool,
    generation_children_exact: bool,
    connect_databases: Vec<String>,
    sessions: i64,
    owned_objects: i64,
}

impl WorkloadRoleState {
    fn is_active_for(&self, family: WorkloadRoleFamily, database: &str) -> bool {
        self.has_active_shape_for(database)
            && self.memberships == [family.acl_role()]
            && self.membership_options_exact
    }

    fn is_migratable_active_for(&self, family: WorkloadRoleFamily, database: &str) -> bool {
        let memberships_are_known = self.memberships == [family.acl_role()]
            || (family == WorkloadRoleFamily::EffectWriter
                && self.memberships
                    == [
                        family.acl_role(),
                        wamn_run_state::RUN_PROJECTION_WRITER_ROLE,
                    ]);
        self.has_active_shape_for(database)
            && memberships_are_known
            && self.membership_options_migratable
    }

    fn has_active_shape_for(&self, database: &str) -> bool {
        self.login
            && self.restrictive_attributes()
            && self.inherit
            && self.password_set
            && self.valid_until_finite
            && self.member_roles.is_empty()
            && self.member_options_exact
            && self.connect_databases == [database]
            && self.owned_objects == 0
    }

    fn is_inactive(&self) -> bool {
        self.has_inactive_shape() && self.memberships.is_empty() && self.membership_options_exact
    }

    fn is_migratable_inactive_for(&self, family: WorkloadRoleFamily) -> bool {
        let memberships_are_known = self.memberships.is_empty()
            || (family == WorkloadRoleFamily::EffectWriter
                && self.memberships == [wamn_run_state::RUN_PROJECTION_WRITER_ROLE]);
        self.has_inactive_shape() && memberships_are_known && self.membership_options_migratable
    }

    fn has_inactive_shape(&self) -> bool {
        !self.login
            && self.restrictive_attributes()
            && self.inherit
            && !self.password_set
            && self.valid_until.as_deref() == Some("1970-01-01T00:00:00Z")
            && self.valid_until_finite
            && self.member_roles.is_empty()
            && self.member_options_exact
            && self.connect_databases.is_empty()
            && self.sessions == 0
            && self.owned_objects == 0
    }

    fn is_acl_role(&self, family: WorkloadRoleFamily) -> bool {
        self.has_acl_role_shape(family)
            && self.member_options_exact
            && self.generation_children_exact
    }

    /// THE ONE PARENT EDGE A STABLE ACL ROLE MAY CARRY (`wamn-0h0g.22.17`).
    ///
    /// A platform-grain family's ACL role is a member of
    /// [`PLATFORM_GROUP_ROLE`], and it has to be: the tenant floor is narrowed
    /// `TO wamn_app`, PostgreSQL default-denies when no policy matches the
    /// connected role, and the permissive arm names the group. Nothing else may
    /// appear here — an extra parent is authority this provisioner did not
    /// confer.
    fn expected_acl_parents(family: WorkloadRoleFamily) -> &'static [&'static str] {
        if family.is_platform_grain() {
            &[PLATFORM_GROUP_ROLE]
        } else {
            &[]
        }
    }

    fn has_acl_role_shape(&self, family: WorkloadRoleFamily) -> bool {
        !self.login
            && self.restrictive_attributes()
            && !self.inherit
            && !self.password_set
            && self.valid_until.is_none()
            && !self.valid_until_finite
            && self.memberships == Self::expected_acl_parents(family)
            && self.membership_options_exact
            && self
                .member_roles
                .iter()
                .all(|role| is_workload_generation_role(family, role))
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

fn is_workload_generation_role(family: WorkloadRoleFamily, role: &str) -> bool {
    let prefix = format!("{}_", family.generation_prefix());
    let Some(scoped) = role.strip_prefix(&prefix) else {
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

#[derive(Debug, Clone, Copy)]
struct WorkloadLifecycle<'a> {
    family: WorkloadRoleFamily,
    scope: WorkloadRoleScope<'a>,
    control_tenant: Option<&'a str>,
}

impl<'a> WorkloadLifecycle<'a> {
    fn database(self) -> &'a str {
        self.scope.database()
    }

    fn role(self, generation: CredentialGeneration) -> String {
        workload_generation_role(self.family, self.scope, generation)
            .expect("the lifecycle constructor pairs each family with its exact scope")
    }

    fn family_lock_key(self) -> String {
        format!("wamn.workload-family.v1:{}", self.family.acl_role())
    }

    fn label(self) -> String {
        self.family.label()
    }
}

/// ONE lifecycle constructor, for any family (`wamn-0h0g.22.16`).
///
/// Replaces four copy-pasted constructors that differed only in the scope arm
/// they filled in. The scope GRAIN is the family's own declaration, so pairing a
/// family with the wrong grain is not something a caller can get wrong here.
///
/// The control tenant follows the same derivation: only a control-scoped family
/// records a login-to-tenant mapping row, because that row is the control
/// plane's and a project-environment credential never reaches the control
/// database.
fn workload_lifecycle<'a>(
    family: WorkloadRoleFamily,
    identity: WorkloadActionIdentity<'a>,
    database: &'a str,
) -> WorkloadLifecycle<'a> {
    let WorkloadActionIdentity {
        org,
        project,
        environment,
        tenant,
    } = identity;
    let scope = match family.scope_kind() {
        // Tenant scope for the effect writer and the guest credential alike:
        // the digest in the role name IS the tenant key, so the login the mint
        // issues and the key `wamn_authority.tenant_key` computes are the same
        // string (`wamn-0h0g.22.6.4`).
        WorkloadRoleScopeKind::Tenant => WorkloadRoleScope::Tenant { tenant, database },
        WorkloadRoleScopeKind::ProjectEnvironment => WorkloadRoleScope::ProjectEnvironment {
            org,
            project,
            environment,
            database,
        },
        WorkloadRoleScopeKind::Control => WorkloadRoleScope::Control {
            org,
            project,
            environment,
            database,
        },
    };
    WorkloadLifecycle {
        family,
        scope,
        control_tenant: (family.scope_kind() == WorkloadRoleScopeKind::Control).then_some(tenant),
    }
}

/// The retired project-environment effect-writer identities, migration input
/// only.
///
/// `None` for every other family, including any admitted later: a legacy
/// identity is a fact about one family's history, not a generic property.
fn legacy_generation_roles(
    family: WorkloadRoleFamily,
    identity: WorkloadActionIdentity<'_>,
    database: &str,
    generation: CredentialGeneration,
) -> (Option<String>, Option<String>) {
    if family != WorkloadRoleFamily::EffectWriter {
        return (None, None);
    }
    let WorkloadActionIdentity {
        org,
        project,
        environment,
        ..
    } = identity;
    (
        Some(legacy_effect_writer_generation_role(
            org,
            project,
            environment,
            database,
            generation,
        )),
        Some(legacy_effect_writer_generation_role(
            org,
            project,
            environment,
            database,
            generation.other(),
        )),
    )
}

#[derive(Debug, Clone, Copy)]
struct WorkloadActionIdentity<'a> {
    org: &'a str,
    project: &'a str,
    environment: &'a str,
    tenant: &'a str,
}

fn workload_action_identity<'a>(
    args: &'a ProvisionProjectEnvArgs,
    label: &str,
) -> anyhow::Result<WorkloadActionIdentity<'a>> {
    let org = args
        .org
        .as_deref()
        .expect("clap parser invariant: --org is required unless --revoke-pat-prefix is present");
    let project = args.project.as_deref().expect(
        "clap parser invariant: --project is required unless --revoke-pat-prefix is present",
    );
    let environment = args
        .env
        .as_deref()
        .expect("clap parser invariant: --env is required unless --revoke-pat-prefix is present");
    let tenant = args
        .tenant
        .as_deref()
        .with_context(|| format!("{label} generation actions require --tenant"))?;
    anyhow::ensure!(!tenant.is_empty(), "--tenant must not be empty");
    validate_project_env(org, project, environment)
        .map_err(|error| anyhow::anyhow!("project-env names: {error}"))?;
    Ok(WorkloadActionIdentity {
        org,
        project,
        environment,
        tenant,
    })
}

async fn converge_workload_generation_state(
    client: &(impl GenericClient + Sync),
    lifecycle: WorkloadLifecycle<'_>,
    role: &str,
) -> anyhow::Result<Option<WorkloadRoleState>> {
    let state = read_workload_role_state(client, role, &lifecycle.label()).await?;
    let Some(found) = state.as_ref() else {
        return Ok(None);
    };
    let active =
        if found.is_active_for(lifecycle.family, lifecycle.database()) || found.is_inactive() {
            return Ok(state);
        } else if found.is_migratable_active_for(lifecycle.family, lifecycle.database()) {
            true
        } else if found.is_migratable_inactive_for(lifecycle.family) {
            false
        } else {
            return Ok(state);
        };
    client
        .batch_execute(&sql::normalize_workload_generation_membership_sql(
            lifecycle.family,
            role,
            active,
        ))
        .await
        .with_context(|| {
            format!(
                "normalize legacy {} generation membership",
                lifecycle.label()
            )
        })?;
    read_workload_role_state(client, role, &lifecycle.label()).await
}

async fn converge_stable_workload_memberships(
    client: &(impl GenericClient + Sync),
    admin_config: &PgConfig,
    lifecycle: WorkloadLifecycle<'_>,
) -> anyhow::Result<()> {
    let Some(stable) =
        read_workload_role_state(client, lifecycle.family.acl_role(), &lifecycle.label()).await?
    else {
        return Ok(());
    };
    for role in stable.member_roles {
        anyhow::ensure!(
            is_workload_generation_role(lifecycle.family, &role),
            "stable {} ACL role has a member outside its generation family",
            lifecycle.label()
        );
        client
            .batch_execute(&sql::normalize_workload_generation_membership_sql(
                lifecycle.family,
                &role,
                true,
            ))
            .await
            .with_context(|| {
                format!(
                    "normalize {} stable-role generation member",
                    lifecycle.label()
                )
            })?;
        let child = read_workload_role_state(client, &role, &lifecycle.label())
            .await?
            .with_context(|| {
                format!(
                    "{} stable-role generation member disappeared",
                    lifecycle.label()
                )
            })?;
        let [database] = child.connect_databases.as_slice() else {
            anyhow::bail!(
                "{} stable-role generation member does not carry exactly one direct database CONNECT grant",
                lifecycle.label()
            );
        };
        anyhow::ensure!(
            child.is_active_for(lifecycle.family, database),
            "{} stable-role generation member is not an exact active credential",
            lifecycle.label()
        );
        verify_role_grants(
            admin_config,
            &role,
            RoleAclExpectation::Generation { database },
        )
        .await?;
    }
    Ok(())
}

/// ONE workload generation action, for any family (`wamn-0h0g.22.16`).
///
/// Replaces four copy-pasted `run_*_action` functions and the dispatch chain
/// that chose between them. Everything this needs is DERIVED from the family:
/// its scope grain picks the identity inputs, the admin database and the
/// lifecycle scope; its label names the action in every message; its declared
/// Secret body shape picks what the published Secret carries. What is
/// deliberately NOT derived is the GRANT SET, which is where a family's
/// authority actually lives.
async fn run_workload_action(
    args: &ProvisionProjectEnvArgs,
    action: WorkloadGenerationAction,
) -> anyhow::Result<()> {
    let WorkloadGenerationAction {
        family,
        verb,
        generation,
    } = action;
    let label = family.label();
    let emits_app_retirement_sql =
        family == WorkloadRoleFamily::App && verb == WorkloadActionVerb::Prepare;
    anyhow::ensure!(
        args.cluster.is_none()
            && args.connection_limit.is_none()
            && args.app_host.is_none()
            && args.emit_database.is_none()
            && (args.emit_role_sql.is_none() || emits_app_retirement_sql)
            && args.emit_privilege_sql.is_none()
            && args.emit_secret.is_none()
            && args.emit_management_author_pat_secret.is_none()
            && args.emit_route_caller_pat_secret.is_none(),
        "{label} generation actions cannot render ordinary provisioning or PAT artifacts; only \
         App prepare may emit the canonical shared-login retirement role SQL"
    );
    let identity = workload_action_identity(args, &label)?;
    let WorkloadActionIdentity {
        org,
        project,
        environment,
        tenant,
    } = identity;
    if family == WorkloadRoleFamily::SessionRoleReader {
        validate_session_tenant_id(tenant)?;
    }
    let triple = Triple::new(org, project, environment);
    let system_url = args
        .system_database_url
        .as_deref()
        .with_context(|| format!("{label} generation actions require --system-database-url"))?;

    // A CONTROL-scoped family addresses the control database the system URL
    // already names; every other family addresses the project environment's own
    // database, whose instance suffix is READ from the registry rather than
    // typed. One derivation over the scope grain, not a branch per family.
    let (admin_url, database, admin_config, instance) = if family.scope_kind()
        == WorkloadRoleScopeKind::Control
    {
        anyhow::ensure!(
            args.target_admin_database_url.is_none(),
            "--target-admin-database-url is not a {label} input: this family addresses the control database"
        );
        let config = named_database_config(system_url, &format!("{label} admin"))?;
        let database = config
            .get_dbname()
            .expect("named_database_config requires a database name")
            .to_string();
        (system_url, database, config, None)
    } else {
        let instance = read_project_env_instance(system_url, &triple).await?;
        let database = project_env_database_name(org, project, environment, &instance);
        let admin_url = args.target_admin_database_url.as_deref().with_context(|| {
            format!("{label} generation actions require --target-admin-database-url")
        })?;
        let config = exact_project_database_config(admin_url, &database)?;
        (admin_url, database, config, Some(instance))
    };
    let lifecycle = workload_lifecycle(family, identity, &database);

    match verb {
        WorkloadActionVerb::Prepare => {
            let secret_path = args.workload_secret_path(family).with_context(|| {
                format!(
                    "--{} requires --{} PATH",
                    workload_action_flag(family, verb),
                    workload_secret_flag(family)
                )
            })?;
            ensure_secret_path(secret_path, &format!("--{}", workload_secret_flag(family)))?;
            let validity = workload_validity(Utc::now());
            // The key the RLS predicate computes, taken from the ONE Rust
            // definition rather than re-derived here — the Secret's label must
            // name the same tenant the role name's digest does.
            let key = tenant_key(tenant, &database);
            let scope = EffectWriterCredentialScope {
                tenant: tenant.to_string(),
                org: org.to_string(),
                project: project.to_string(),
                environment: environment.to_string(),
                database: database.clone(),
            };
            let (legacy_desired, legacy_other) =
                legacy_generation_roles(family, identity, &database, generation);
            prepare_workload_generation(
                &admin_config,
                lifecycle,
                legacy_desired.as_deref(),
                legacy_other.as_deref(),
                generation,
                &validity.expires_at,
                |role, password, predecessor_role| {
                    let credential_url = workload_url(admin_url, role, password, &database)?;
                    let secret = match family.secret_body_kind() {
                        WorkloadSecretBodyKind::Url => render_workload_secret_manifest(
                            family,
                            &triple,
                            &args.namespace,
                            WorkloadSecretBody::Url(&credential_url),
                        ),
                        WorkloadSecretBodyKind::TenantUrl => {
                            anyhow::ensure!(
                                role.contains(&key),
                                "the minted {label} login does not carry the tenant key the RLS \
                                 predicate computes, so every guest read would refuse"
                            );
                            render_workload_secret_manifest(
                                family,
                                &triple,
                                &args.namespace,
                                WorkloadSecretBody::TenantUrl {
                                    tenant,
                                    tenant_key: &key,
                                    url: &credential_url,
                                },
                            )
                        }
                        WorkloadSecretBodyKind::EffectWriterCredential => {
                            let credential_id = random_lower_hex(16)?;
                            let credential = effect_writer_credential(
                                &scope,
                                &credential_id,
                                generation,
                                &validity,
                                &credential_url,
                            );
                            let mut secret = render_workload_secret_manifest(
                                family,
                                &triple,
                                &args.namespace,
                                WorkloadSecretBody::EffectWriterCredential(&credential),
                            );
                            if let Some(predecessor_role) = predecessor_role {
                                secret["metadata"]["annotations"]
                                    ["wamn.io/predecessor-database-role"] = json!(predecessor_role);
                            }
                            secret
                        }
                        WorkloadSecretBodyKind::SessionTarget => {
                            let target = SessionTarget::new(
                                &triple,
                                instance.as_deref().expect("session readers use project-environment scope"),
                                tenant,
                                &credential_url,
                            )?;
                            render_workload_secret_manifest(
                                family,
                                &triple,
                                &args.namespace,
                                WorkloadSecretBody::SessionTarget(&target),
                            )
                        }
                    };
                    write_secret_json(secret_path, &secret)
                        .with_context(|| format!("write authenticated {label} Secret"))
                },
            )
            .await?;
            println!(
                "prepared and authenticated {label} credential generation {} for {org}/{project}/{environment}; wrote {}",
                generation.as_str(),
                secret_path.display()
            );
            if family == WorkloadRoleFamily::App && args.emit_role_sql.is_some() {
                emit_text(
                    &args.emit_role_sql,
                    "shared App-login retirement role SQL (apply once after every replacement carrier is verified)",
                    &role_sql(""),
                )?;
            }
        }
        WorkloadActionVerb::Retire => {
            let (legacy_old_role, _) =
                legacy_generation_roles(family, identity, &database, generation);
            retire_workload_generation(
                &admin_config,
                lifecycle,
                legacy_old_role.as_deref(),
                generation,
            )
            .await?;
            println!(
                "retired {label} credential generation {} for {org}/{project}/{environment}",
                generation.as_str()
            );
        }
        WorkloadActionVerb::Abort => {
            abort_workload_generation(&admin_config, lifecycle, generation).await?;
            println!(
                "aborted unpublished {label} credential generation {} for {org}/{project}/{environment}",
                generation.as_str()
            );
        }
    }
    Ok(())
}

/// Prepare one generation, then verify it and publish its Secret.
///
/// **A REFUSED PREPARE IS NOT ATOMIC, deliberately (`wamn-0h0g.12.179`).** The
/// prepare transaction COMMITS before the post-commit checks run, because the
/// generation must be authenticated over a real connection — which no
/// uncommitted role can accept. A refusal after that point therefore leaves,
/// and is contracted to leave, exactly two things behind:
///
/// * the stable ACL role converged to its NOLOGIN, password-free shape by
///   `ensure_workload_acl_role_sql`, which is idempotent and is the shape every
///   subsequent prepare wants anyway; and
/// * the target generation role, rolled back by
///   [`rollback_prepared_workload_generation`] to the INACTIVE shape — no
///   `LOGIN`, no password, no membership, no `CONNECT`, `VALID UNTIL 'epoch'`.
///
/// Nothing else survives, and no Secret is published. A retry meets precisely
/// the inactive target a prepare requires, so the partial state is recoverable
/// rather than wedging; what it is NOT is a clean cluster, and a live arm that
/// assumes a refusal left no role behind will find a healthy object sitting
/// inside `prepare_workload_generation_sql`'s `IF NOT EXISTS` guard. Live arms
/// must drop the roles themselves, not rely on a failed run to have done it.
async fn prepare_workload_generation<F>(
    admin_config: &PgConfig,
    lifecycle: WorkloadLifecycle<'_>,
    legacy_desired_role: Option<&str>,
    legacy_other_role: Option<&str>,
    generation: CredentialGeneration,
    expires_at: &str,
    publish: F,
) -> anyhow::Result<()>
where
    F: FnOnce(&str, &str, Option<&str>) -> anyhow::Result<()>,
{
    let database = lifecycle.database();
    let role = lifecycle.role(generation);
    let mut other_role = lifecycle.role(generation.other());
    let (mut admin, admin_task) = connect_config(admin_config, &lifecycle.label()).await?;
    lock_workload_family(&admin, lifecycle).await?;
    let transaction = admin
        .transaction()
        .await
        .with_context(|| format!("begin {} generation prepare", lifecycle.label()))?;
    transaction
        .batch_execute(sql::revoke_public_connect_floor_sql())
        .await
        .context("converge cluster PUBLIC CONNECT floor")?;
    verify_public_access_floor(&transaction, &lifecycle.label()).await?;
    converge_stable_workload_memberships(&transaction, admin_config, lifecycle).await?;
    let desired = converge_workload_generation_state(&transaction, lifecycle, &role).await?;
    if let Some(legacy_role) = legacy_desired_role {
        if let Some(legacy) =
            converge_workload_generation_state(&transaction, lifecycle, legacy_role).await?
        {
            anyhow::ensure!(
                legacy.is_inactive(),
                "legacy effect-writer migration must prepare the opposite generation"
            );
        }
    }
    let mut other =
        converge_workload_generation_state(&transaction, lifecycle, &other_role).await?;
    if other.as_ref().is_none_or(WorkloadRoleState::is_inactive)
        && let Some(legacy_role) = legacy_other_role
    {
        let legacy =
            converge_workload_generation_state(&transaction, lifecycle, legacy_role).await?;
        if legacy.as_ref().is_some_and(|state| !state.is_inactive()) {
            other_role = legacy_role.to_string();
            other = legacy;
        }
    }
    let recovering_active = match (generation, desired.as_ref(), other.as_ref()) {
        (CredentialGeneration::A, desired, None)
            if desired.is_none_or(WorkloadRoleState::is_inactive) =>
        {
            false
        }
        (CredentialGeneration::A, Some(desired), None)
            if desired.is_active_for(lifecycle.family, database) && desired.sessions == 0 =>
        {
            true
        }
        (_, desired, Some(other))
            if desired.is_none_or(WorkloadRoleState::is_inactive)
                && other.is_active_for(lifecycle.family, database) =>
        {
            false
        }
        (_, Some(desired), Some(other))
            if desired.is_active_for(lifecycle.family, database)
                && desired.sessions == 0
                && other.is_active_for(lifecycle.family, database) =>
        {
            true
        }
        (CredentialGeneration::B, None, None) => {
            anyhow::bail!(
                "initial {} credential generation must be a",
                lifecycle.label()
            )
        }
        _ => anyhow::bail!(
            "{} generation prepare requires an inactive target, or an exact zero-session active target recovered after failed Secret publication",
            lifecycle.label()
        ),
    };
    let desired_acl = if recovering_active {
        RoleAclExpectation::Generation { database }
    } else {
        RoleAclExpectation::None
    };
    verify_role_grants(admin_config, &role, desired_acl).await?;
    if other.is_some() {
        verify_role_grants(
            admin_config,
            &other_role,
            RoleAclExpectation::Generation { database },
        )
        .await?;
    }
    let predecessor_role = other.as_ref().map(|_| other_role.as_str());
    // Pre-checked ONLY for a family whose stable grant set is converged
    // elsewhere (schema control owns the effect writer's, because its grants
    // exist only once the effect-ledger tables do). A family whose grant set
    // THIS batch applies has nothing to assert yet on a first prepare, so the
    // condition is the absence of a stable surface, not a family name.
    if sql::stable_surface_sql(lifecycle.family).is_none()
        && let Some(grant_set) = stable_grant_set(lifecycle.family)
        && read_workload_role_state(
            &transaction,
            lifecycle.family.acl_role(),
            &lifecycle.label(),
        )
        .await?
        .is_some()
    {
        verify_role_grants(
            admin_config,
            lifecycle.family.acl_role(),
            RoleAclExpectation::StableGrantSet {
                grant_set,
                required_database: database,
            },
        )
        .await?;
    }
    let password = random_lower_hex(32)?;
    transaction
        .batch_execute(&sql::prepare_workload_generation_sql(
            lifecycle.family,
            database,
            &role,
            &password,
            expires_at,
        ))
        .await
        .with_context(|| format!("prepare {} credential generation", lifecycle.label()))?;
    if let (
        Some(tenant),
        WorkloadRoleScope::Control {
            org,
            project,
            environment,
            ..
        },
    ) = (lifecycle.control_tenant, lifecycle.scope)
    {
        let mapped: Option<String> = transaction
            .query_opt(
                sql::upsert_control_author_tenant_mapping_sql(),
                &[&role, &tenant, &org, &project, &environment],
            )
            .await
            .context("record control-author login tenant mapping")?
            .map(|row| row.get("tenant_id"));
        anyhow::ensure!(
            mapped.as_deref() == Some(tenant),
            "control-author login identity already maps to a different tenant"
        );
    }
    transaction
        .commit()
        .await
        .with_context(|| format!("commit {} generation prepare", lifecycle.label()))?;

    // App prepare has now made the stable role NOLOGIN, so old credentials
    // cannot reconnect. Do not drain its existing sessions here: they bridge
    // the interval in which the authenticated generation Secret is published
    // and workloads roll. The explicit retirement step owns the final bounded
    // native drain after that cutover.

    let publish_result = async {
        let credential_config = workload_config(admin_config, &role, &password, database);
        authenticate_workload_generation(&credential_config, lifecycle, &role)
            .await
            .with_context(|| format!("authenticate prepared {} generation", lifecycle.label()))?;

        let prepared = read_workload_role_state(&admin, &role, &lifecycle.label())
            .await?
            .with_context(|| format!("prepared {} generation disappeared", lifecycle.label()))?;
        anyhow::ensure!(
            prepared.is_active_for(lifecycle.family, database),
            "prepared {} generation did not have the exact active ACL",
            lifecycle.label()
        );
        anyhow::ensure!(
            prepared.valid_until.as_deref() == Some(expires_at),
            "prepared {} generation VALID UNTIL does not match credential expires-at",
            lifecycle.label()
        );
        verify_role_grants(
            admin_config,
            &role,
            RoleAclExpectation::Generation { database },
        )
        .await?;
        verify_stable_workload_role(&admin, admin_config, lifecycle).await?;
        publish(&role, &password, predecessor_role)?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if let Err(error) = publish_result {
        let rollback =
            rollback_prepared_workload_generation(&admin, admin_config, lifecycle, &role).await;
        drop(admin);
        let _ = admin_task.await;
        if let Err(rollback_error) = rollback {
            anyhow::bail!(
                "{} prepare failed after LOGIN was enabled: {error:#}; rollback also failed: {rollback_error:#}",
                lifecycle.label()
            );
        }
        return Err(error);
    }
    drop(admin);
    let _ = admin_task.await;
    Ok(())
}

/// Undo the authority a committed prepare granted, back to the INACTIVE shape.
///
/// It does NOT drop the role, and does not undo the stable ACL role's
/// convergence — see [`prepare_workload_generation`] for the contract on what a
/// refused prepare leaves behind.
async fn rollback_prepared_workload_generation(
    admin: &(impl GenericClient + Sync),
    admin_config: &PgConfig,
    lifecycle: WorkloadLifecycle<'_>,
    role: &str,
) -> anyhow::Result<()> {
    admin
        .batch_execute(&sql::retire_workload_generation_sql(
            lifecycle.family,
            lifecycle.database(),
            role,
        ))
        .await
        .with_context(|| format!("revoke prepared {} generation authority", lifecycle.label()))?;
    admin
        .batch_execute(&sql::terminate_workload_generation_sessions_sql(role))
        .await
        .with_context(|| {
            format!(
                "terminate prepared {} generation sessions",
                lifecycle.label()
            )
        })?;
    let state = read_workload_role_state(admin, role, &lifecycle.label())
        .await?
        .with_context(|| format!("rolled-back {} generation disappeared", lifecycle.label()))?;
    anyhow::ensure!(
        state.is_inactive(),
        "rolled-back {} generation did not converge to inactive",
        lifecycle.label()
    );
    verify_role_grants(admin_config, role, RoleAclExpectation::None).await
}

async fn retire_workload_generation(
    admin_config: &PgConfig,
    lifecycle: WorkloadLifecycle<'_>,
    legacy_old_role: Option<&str>,
    generation: CredentialGeneration,
) -> anyhow::Result<()> {
    let database = lifecycle.database();
    let mut old_role = lifecycle.role(generation);
    let replacement_role = lifecycle.role(generation.other());
    let (mut admin, admin_task) = connect_config(admin_config, &lifecycle.label()).await?;
    lock_workload_family(&admin, lifecycle).await?;
    let transaction = admin
        .transaction()
        .await
        .with_context(|| format!("begin {} generation retirement", lifecycle.label()))?;
    verify_public_access_floor(&transaction, &lifecycle.label()).await?;
    converge_stable_workload_memberships(&transaction, admin_config, lifecycle).await?;
    let mut old = converge_workload_generation_state(&transaction, lifecycle, &old_role).await?;
    if old.as_ref().is_none_or(WorkloadRoleState::is_inactive)
        && let Some(legacy_role) = legacy_old_role
    {
        let legacy =
            converge_workload_generation_state(&transaction, lifecycle, legacy_role).await?;
        if legacy.as_ref().is_some_and(|state| !state.is_inactive()) {
            old_role = legacy_role.to_string();
            old = legacy;
        }
    }
    let old =
        old.with_context(|| format!("old {} generation does not exist", lifecycle.label()))?;
    let replacement =
        converge_workload_generation_state(&transaction, lifecycle, &replacement_role)
            .await?
            .with_context(|| {
                format!(
                    "replacement {} generation does not exist",
                    lifecycle.label()
                )
            })?;
    anyhow::ensure!(
        old.is_active_for(lifecycle.family, database),
        "old {} generation is not the exact active credential",
        lifecycle.label()
    );
    anyhow::ensure!(
        replacement.is_active_for(lifecycle.family, database),
        "replacement {} generation is not LOGIN-capable with exact ACL",
        lifecycle.label()
    );
    anyhow::ensure!(
        replacement.sessions > 0,
        "replacement {} generation has no verified live private-pool session",
        lifecycle.label()
    );
    verify_role_grants(
        admin_config,
        &old_role,
        RoleAclExpectation::Generation { database },
    )
    .await?;
    verify_role_grants(
        admin_config,
        &replacement_role,
        RoleAclExpectation::Generation { database },
    )
    .await?;
    transaction
        .batch_execute(&sql::retire_workload_generation_sql(
            lifecycle.family,
            database,
            &old_role,
        ))
        .await
        .with_context(|| format!("retire old {} credential generation", lifecycle.label()))?;
    transaction
        .commit()
        .await
        .with_context(|| format!("commit {} generation retirement", lifecycle.label()))?;
    admin
        .batch_execute(&sql::terminate_workload_generation_sessions_sql(&old_role))
        .await
        .with_context(|| {
            format!(
                "terminate retired {} generation sessions",
                lifecycle.label()
            )
        })?;
    let retired = read_workload_role_state(&admin, &old_role, &lifecycle.label())
        .await?
        .with_context(|| format!("retired {} generation disappeared", lifecycle.label()))?;
    anyhow::ensure!(
        retired.is_inactive(),
        "old {} generation did not converge to inactive",
        lifecycle.label()
    );
    drop(admin);
    let _ = admin_task.await;
    Ok(())
}

async fn abort_workload_generation(
    admin_config: &PgConfig,
    lifecycle: WorkloadLifecycle<'_>,
    generation: CredentialGeneration,
) -> anyhow::Result<()> {
    let database = lifecycle.database();
    let role = lifecycle.role(generation);
    let (mut admin, admin_task) = connect_config(admin_config, &lifecycle.label()).await?;
    lock_workload_family(&admin, lifecycle).await?;
    let transaction = admin
        .transaction()
        .await
        .with_context(|| format!("begin {} generation abort", lifecycle.label()))?;
    verify_public_access_floor(&transaction, &lifecycle.label()).await?;
    converge_stable_workload_memberships(&transaction, admin_config, lifecycle).await?;
    let prepared = converge_workload_generation_state(&transaction, lifecycle, &role)
        .await?
        .with_context(|| format!("prepared {} generation does not exist", lifecycle.label()))?;
    let other_role = lifecycle.role(generation.other());
    let _ = converge_workload_generation_state(&transaction, lifecycle, &other_role).await?;
    anyhow::ensure!(
        prepared.is_active_for(lifecycle.family, database),
        "prepared {} generation is not the exact active credential",
        lifecycle.label()
    );
    anyhow::ensure!(
        prepared.sessions == 0,
        "published or in-use {} generation cannot be aborted",
        lifecycle.label()
    );
    verify_role_grants(
        admin_config,
        &role,
        RoleAclExpectation::Generation { database },
    )
    .await?;
    verify_stable_workload_role(&transaction, admin_config, lifecycle).await?;
    transaction
        .batch_execute(&sql::retire_workload_generation_sql(
            lifecycle.family,
            database,
            &role,
        ))
        .await
        .with_context(|| {
            format!(
                "abort unpublished {} credential generation",
                lifecycle.label()
            )
        })?;
    transaction
        .commit()
        .await
        .with_context(|| format!("commit {} generation abort", lifecycle.label()))?;
    admin
        .batch_execute(&sql::terminate_workload_generation_sessions_sql(&role))
        .await
        .with_context(|| {
            format!(
                "terminate aborted {} generation sessions",
                lifecycle.label()
            )
        })?;
    let aborted = read_workload_role_state(&admin, &role, &lifecycle.label())
        .await?
        .with_context(|| format!("aborted {} generation disappeared", lifecycle.label()))?;
    anyhow::ensure!(
        aborted.is_inactive(),
        "aborted {} generation did not converge to inactive",
        lifecycle.label()
    );
    verify_role_grants(admin_config, &role, RoleAclExpectation::None).await?;
    drop(admin);
    let _ = admin_task.await;
    Ok(())
}

fn workload_validity(now: DateTime<Utc>) -> EffectWriterCredentialValidity {
    let expires_at = now + chrono::Duration::days(WORKLOAD_CREDENTIAL_TTL_DAYS);
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

async fn authenticate_workload_generation(
    config: &PgConfig,
    lifecycle: WorkloadLifecycle<'_>,
    role: &str,
) -> anyhow::Result<()> {
    let (client, task) = connect_config(config, &lifecycle.label()).await?;
    let row = client
        .query_one(
            "SELECT current_user::text, current_database()::text, \
                    has_database_privilege(current_user, current_database(), 'TEMPORARY')",
            &[],
        )
        .await
        .with_context(|| format!("probe prepared {} generation", lifecycle.label()))?;
    let current_user: String = row.get(0);
    let current_database: String = row.get(1);
    let can_create_temporary: bool = row.get(2);
    anyhow::ensure!(
        current_user == role,
        "prepared generation authenticated as wrong role"
    );
    anyhow::ensure!(
        current_database == lifecycle.database(),
        "prepared generation authenticated to wrong database"
    );
    anyhow::ensure!(
        !can_create_temporary,
        "prepared generation inherited TEMPORARY on its database"
    );
    drop(client);
    task.await
        .with_context(|| format!("join {} authentication connection", lifecycle.label()))??;
    Ok(())
}

async fn lock_workload_family(
    client: &(impl GenericClient + Sync),
    lifecycle: WorkloadLifecycle<'_>,
) -> anyhow::Result<()> {
    let family_key = lifecycle.family_lock_key();
    client
        .query_one(sql::workload_scope_lock_sql(), &[&family_key])
        .await
        .with_context(|| format!("acquire {} family rotation lock", lifecycle.label()))?;
    Ok(())
}

async fn verify_public_access_floor(
    client: &(impl GenericClient + Sync),
    label: &str,
) -> anyhow::Result<()> {
    let databases: Vec<String> = client
        .query(sql::public_connect_databases_sql(), &[])
        .await
        .context("verify cluster PUBLIC CONNECT floor")?
        .into_iter()
        .map(|row| row.get(0))
        .collect();
    anyhow::ensure!(
        databases.is_empty(),
        "{label} generation actions require PUBLIC CONNECT revoked on every connectable database (template1 included); still granted on {databases:?}"
    );
    let public_temporary: bool = client
        .query_one(sql::public_temporary_on_current_database_sql(), &[])
        .await
        .context("verify target database PUBLIC TEMPORARY floor")?
        .get(0);
    anyhow::ensure!(
        !public_temporary,
        "{label} generation actions require PUBLIC TEMPORARY revoked on the exact database"
    );
    Ok(())
}

async fn verify_stable_workload_role(
    client: &(impl GenericClient + Sync),
    admin_config: &PgConfig,
    lifecycle: WorkloadLifecycle<'_>,
) -> anyhow::Result<()> {
    let role = lifecycle.family.acl_role();
    let state = read_workload_role_state(client, role, &lifecycle.label())
        .await?
        .with_context(|| format!("stable {} ACL role does not exist", lifecycle.label()))?;
    anyhow::ensure!(
        state.is_acl_role(lifecycle.family),
        "stable {} ACL role is not a connection-free NOLOGIN role with exact generation members",
        lifecycle.label()
    );
    if let Some(grant_set) = stable_grant_set(lifecycle.family) {
        verify_role_grants(
            admin_config,
            role,
            RoleAclExpectation::StableGrantSet {
                grant_set,
                required_database: lifecycle.database(),
            },
        )
        .await?;
    }
    Ok(())
}

/// THE GRANT SET, and the one thing that stays per family
/// (`wamn-0h0g.22.16`).
///
/// `None` = this family's stable ACL role holds no direct grants of its own, so
/// there is no denial matrix to assert. The wildcard arm is deliberate: an
/// admitted family reaches every derived flag, action and Secret without an
/// edit anywhere, and acquires an entry HERE only when it acquires authority.
fn stable_grant_set(family: WorkloadRoleFamily) -> Option<StableGrantSet> {
    match family {
        WorkloadRoleFamily::EffectWriter => Some(StableGrantSet::EffectWriter),
        WorkloadRoleFamily::ManagementAdmitter => Some(StableGrantSet::ManagementAdmitter),
        WorkloadRoleFamily::RegistryReader => Some(StableGrantSet::RegistryReader),
        WorkloadRoleFamily::IdentityReader => Some(StableGrantSet::IdentityReader),
        WorkloadRoleFamily::SessionRoleReader => Some(StableGrantSet::SessionRoleReader),
        WorkloadRoleFamily::Retention => Some(StableGrantSet::Retention),
        WorkloadRoleFamily::DispatchReader => Some(StableGrantSet::DispatchReader),
        // `wamn-0h0g.22.37`: both families acquired authority, so both acquire
        // a denial matrix in the SAME edit. A family with one and not the other
        // is exactly the bug
        // `every_family_derives_a_lifecycle_and_only_a_grant_set_stays_per_family`
        // exists to catch.
        WorkloadRoleFamily::ExecutorPlatform => Some(StableGrantSet::ExecutorPlatform),
        WorkloadRoleFamily::HttpAdmitter => Some(StableGrantSet::HttpAdmitter),
        WorkloadRoleFamily::EventMaterializer => Some(StableGrantSet::EventMaterializer),
        _ => None,
    }
}

/// The per-family denial matrices a stable ACL role is measured against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StableGrantSet {
    EffectWriter,
    ManagementAdmitter,
    RegistryReader,
    IdentityReader,
    SessionRoleReader,
    Retention,
    DispatchReader,
    ExecutorPlatform,
    HttpAdmitter,
    EventMaterializer,
}

impl StableGrantSet {
    fn verify(
        self,
        role: &str,
        database: &str,
        required_database: &str,
        grants: &[RoleAcl],
    ) -> anyhow::Result<()> {
        match self {
            Self::EffectWriter => {
                verify_effect_writer_grants(role, database, grants)
            }
            Self::ManagementAdmitter => verify_management_admitter_grants(
                role,
                database,
                required_database,
                grants,
            ),
            Self::RegistryReader => verify_system_reader_grants(
                SystemReader::Registry,
                "registry",
                &sql::REGISTRY_READER_RELATIONS,
                role,
                database,
                required_database,
                grants,
            ),
            Self::IdentityReader => verify_system_reader_grants(
                SystemReader::Identity,
                "identity",
                &sql::IDENTITY_READER_RELATIONS,
                role,
                database,
                required_database,
                grants,
            ),
            Self::SessionRoleReader => verify_session_role_reader_grants(
                role,
                database,
                required_database,
                grants,
            ),
            Self::Retention => verify_retention_grants(role, database, grants),
            Self::DispatchReader => {
                verify_dispatch_reader_grants(role, database, grants)
            }
            Self::ExecutorPlatform => verify_executor_platform_grants(
                role,
                database,
                required_database,
                grants,
            ),
            Self::HttpAdmitter => verify_http_admitter_grants(
                role,
                database,
                required_database,
                grants,
            ),
            Self::EventMaterializer => verify_event_materializer_grants(
                role,
                database,
                required_database,
                grants,
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoleAclExpectation<'a> {
    None,
    Generation {
        database: &'a str,
    },
    StableGrantSet {
        grant_set: StableGrantSet,
        required_database: &'a str,
    },
}

async fn verify_role_grants(
    admin_config: &PgConfig,
    role: &str,
    expectation: RoleAclExpectation<'_>,
) -> anyhow::Result<()> {
    let (catalog, catalog_task) = connect_config(admin_config, "role grants").await?;
    let databases: Vec<String> = catalog
        .query(sql::non_template_databases_sql(), &[])
        .await
        .context("list databases for role grants")?
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
        let (client, task) = connect_config(&config, "cross-database role grants").await?;
        let rows = client
            .query(sql::role_database_grants_sql(), &[&role])
            .await
            .with_context(|| format!("read role grants in database {database:?}"))?;
        let grants: Vec<RoleAcl> = rows
            .into_iter()
            .map(|row| RoleAcl {
                object_kind: row.get("object_kind"),
                schema_name: row.get("schema_name"),
                object_name: row.get("object_name"),
                privilege: row.get("privilege_type"),
                grantable: row.get("is_grantable"),
            })
            .collect();
        for acl in &grants {
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
            RoleAclExpectation::StableGrantSet {
                grant_set,
                required_database,
            } => {
                grant_set.verify(role, &database, required_database, &grants)?;
            }
            expectation => {
                for acl in &grants {
                    let allowed = match expectation {
                        RoleAclExpectation::None => false,
                        RoleAclExpectation::Generation { database: expected } => {
                            acl.object_kind == "database"
                                && database == expected
                                && acl.object_name == expected
                                && acl.privilege == "CONNECT"
                        }
                        RoleAclExpectation::StableGrantSet { .. } => {
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

#[derive(Clone)]
struct RoleAcl {
    object_kind: String,
    schema_name: String,
    object_name: String,
    privilege: String,
    grantable: bool,
}

fn verify_effect_writer_grants(
    role: &str,
    database: &str,
    grants: &[RoleAcl],
) -> anyhow::Result<()> {
    let mut by_schema: BTreeMap<String, BTreeSet<(String, String, String)>> = BTreeMap::new();
    for acl in grants {
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

/// The exact run-retention grant set, measured from the SERVER's ACL catalogs
/// (`wamn-0h0g.12.69`).
///
/// Deliberately the effect writer's shape — iterate whatever schemas the role
/// holds anything in and require each to be EXACTLY this set — because retention
/// is likewise a tenant-scoped family whose grants land inside each project-env
/// database's run-plane schema, and a widened grant in a schema nobody thought
/// to name is exactly the drift a per-schema allow-list would miss.
///
/// The `SELECT` is COLUMN-scoped and the assertion has to keep it that way. The
/// role is a `wamn_platform` member, that group's floor arm on `wamn_run.runs`
/// is `USING (true)`, and PostgreSQL grants are relation- and column-shaped
/// rather than row-shaped — so this column list is the only thing standing
/// between a retention credential and every tenant's run payloads. A
/// `("relation", "runs", "SELECT")` entry appearing here is that regression, and
/// it fails as an unexpected member of the exact set.
fn verify_retention_grants(
    role: &str,
    database: &str,
    grants: &[RoleAcl],
) -> anyhow::Result<()> {
    let mut by_schema: BTreeMap<String, BTreeSet<(String, String, String)>> = BTreeMap::new();
    for acl in grants {
        anyhow::ensure!(
            matches!(acl.object_kind.as_str(), "schema" | "relation" | "column"),
            "stable role {role:?} carries non-retention {} ACL in database {database:?}",
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
            "stable role {role:?} carries retention ACLs in reserved schema {schema:?} in database {database:?}"
        );
        let mut expected =
            BTreeSet::from([("schema".to_string(), schema.clone(), "USAGE".to_string())]);
        expected.insert((
            "relation".to_string(),
            "runs".to_string(),
            "DELETE".to_string(),
        ));
        for column in RETENTION_RUN_READ_COLUMNS {
            expected.insert((
                "column".to_string(),
                format!("runs.{column}"),
                "SELECT".to_string(),
            ));
        }
        anyhow::ensure!(
            actual == expected,
            "stable role {role:?} ACLs in database {database:?} schema {schema:?} are not the exact run-retention grant set"
        );
    }
    Ok(())
}

/// The exact dispatcher read surface, measured from the SERVER's ACL catalogs
/// (`wamn-0h0g.22.24`).
///
/// The dispatcher's whole database surface is two `SELECT`s over
/// [`sql::DISPATCH_READER_RELATIONS`], so the stable ACL role holds schema
/// `USAGE` plus `SELECT` on exactly those two relations. It is asserted PER
/// SCHEMA and exactly, the effect writer's shape, because a dispatch-reader
/// generation now inherits everything this role holds in every database the
/// role has grants in — and until this bead the family had no denial matrix at
/// all, because it had no generations to guard.
fn verify_dispatch_reader_grants(
    role: &str,
    database: &str,
    grants: &[RoleAcl],
) -> anyhow::Result<()> {
    let mut by_schema: BTreeMap<String, BTreeSet<(String, String, String)>> = BTreeMap::new();
    for acl in grants {
        anyhow::ensure!(
            matches!(acl.object_kind.as_str(), "schema" | "relation" | "column"),
            "stable role {role:?} carries non-reader {} ACL in database {database:?}",
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
            "stable role {role:?} carries dispatch-reader ACLs in reserved schema {schema:?} in database {database:?}"
        );
        let mut expected =
            BTreeSet::from([("schema".to_string(), schema.clone(), "USAGE".to_string())]);
        for relation in sql::DISPATCH_READER_RELATIONS {
            expected.insert((
                "relation".to_string(),
                relation.to_string(),
                "SELECT".to_string(),
            ));
        }
        anyhow::ensure!(
            actual == expected,
            "stable role {role:?} ACLs in database {database:?} schema {schema:?} are not the exact dispatch-reader grant set"
        );
    }
    Ok(())
}

/// The only `runs` columns run-history pruning reads: the three its `WHERE`
/// clause names. `run_id` is deliberately absent — the statement never selects
/// it, and the verb reports a COUNT rather than a list.
const RETENTION_RUN_READ_COLUMNS: [&str; 3] = ["tenant_id", "status", "created_at"];

fn verify_management_admitter_grants(
    role: &str,
    database: &str,
    required_database: &str,
    grants: &[RoleAcl],
) -> anyhow::Result<()> {
    if grants.is_empty() {
        anyhow::ensure!(
            database != required_database,
            "stable role {role:?} has no management-admission ACL in required database {database:?}"
        );
        return Ok(());
    }

    let actual = grants
        .iter()
        .map(|acl| {
            (
                acl.object_kind.clone(),
                acl.schema_name.clone(),
                acl.object_name.clone(),
                acl.privilege.clone(),
            )
        })
        .collect::<BTreeSet<_>>();
    let mut expected = BTreeSet::from([
        (
            "schema".to_string(),
            "catalog".to_string(),
            "catalog".to_string(),
            "USAGE".to_string(),
        ),
        (
            "routine".to_string(),
            "wamn_authority".to_string(),
            "tenant_key".to_string(),
            "EXECUTE".to_string(),
        ),
    ]);
    for relation in sql::MANAGEMENT_ADMITTER_CATALOG_RELATIONS {
        expected.insert((
            "relation".to_string(),
            "catalog".to_string(),
            relation.to_string(),
            "SELECT".to_string(),
        ));
    }
    for column in sql::MANAGEMENT_ADMITTER_WIRING_INSERT_COLUMNS {
        expected.insert((
            "column".to_string(),
            "catalog".to_string(),
            format!("wirings.{column}"),
            "INSERT".to_string(),
        ));
    }
    anyhow::ensure!(
        actual == expected,
        "stable role {role:?} ACLs in database {database:?} are not the exact management-admission grant set"
    );
    Ok(())
}

/// The `aclexplode` grants as `(kind, schema, object, privilege)` tuples.
fn acl_tuples(grants: &[RoleAcl]) -> BTreeSet<(String, String, String, String)> {
    grants
        .iter()
        .map(|acl| {
            (
                acl.object_kind.clone(),
                acl.schema_name.clone(),
                acl.object_name.clone(),
                acl.privilege.clone(),
            )
        })
        .collect()
}

/// Check the reader's two column-scoped reads and no other direct grants.
fn verify_session_role_reader_grants(
    role: &str,
    database: &str,
    required_database: &str,
    grants: &[RoleAcl],
) -> anyhow::Result<()> {
    if grants.is_empty() {
        anyhow::ensure!(
            database != required_database,
            "stable role {role:?} has no session-role reader ACL in required database {database:?}"
        );
        return Ok(());
    }
    let mut expected = BTreeSet::from([(
        "schema".to_string(),
        "app_system".to_string(),
        "app_system".to_string(),
        "USAGE".to_string(),
    )]);
    for (relation, columns) in [
        ("users", ["tenant_id", "id", "status"]),
        ("user_roles", ["tenant_id", "user_id", "role_name"]),
    ] {
        for column in columns {
            expected.insert((
                "column".to_string(),
                "app_system".to_string(),
                format!("{relation}.{column}"),
                "SELECT".to_string(),
            ));
        }
    }
    anyhow::ensure!(
        grants.iter().all(|acl| !acl.grantable) && acl_tuples(grants) == expected,
        "stable role {role:?} ACLs in database {database:?} are not the exact session-role reader grant set"
    );
    Ok(())
}

/// THE EXECUTOR-PLATFORM DENIAL MATRIX (`wamn-0h0g.22.37`).
///
/// EQUALITY against the server's own `aclexplode` answer, never containment: a
/// containment check passes a role that has ALSO acquired `INSERT` on `runs`,
/// and this family's credentials match the permissive `TO wamn_platform` floor
/// arm, so any privilege it holds it holds over EVERY tenant's rows.
///
/// The `routine` rows are part of the matrix, not an exemption. The surface
/// grants two function EXECUTEs — its own authority guard and the tenant-key
/// derivation the `runs_tkey` expression index makes load bearing — and
/// `sql::role_database_grants_sql` reports routine ACLs, so omitting
/// them here would refuse a correctly converged role. (The management-admitter
/// matrix above carries its own `routine` row since `wamn-0h0g.22.38`; the
/// omission this note used to record as a defect is fixed, and a routine row
/// is the convention for both families rather than an exemption for one.)
///
/// An empty grant set is acceptable only OUTSIDE the target database: the
/// project-environment database must carry the grant set, and this role must
/// hold nothing anywhere else on the cluster.
fn verify_executor_platform_grants(
    role: &str,
    database: &str,
    required_database: &str,
    grants: &[RoleAcl],
) -> anyhow::Result<()> {
    if grants.is_empty() {
        anyhow::ensure!(
            database != required_database,
            "stable role {role:?} has no executor-platform ACL in required database {database:?}"
        );
        return Ok(());
    }
    let actual = acl_tuples(grants);
    let mut expected = BTreeSet::from([
        (
            "schema".to_string(),
            "catalog".to_string(),
            "catalog".to_string(),
            "USAGE".to_string(),
        ),
        (
            "schema".to_string(),
            "wamn_run".to_string(),
            "wamn_run".to_string(),
            "USAGE".to_string(),
        ),
        (
            "relation".to_string(),
            "wamn_run".to_string(),
            "runs".to_string(),
            "SELECT".to_string(),
        ),
        (
            "relation".to_string(),
            "wamn_run".to_string(),
            "run_queue".to_string(),
            "SELECT".to_string(),
        ),
        (
            "relation".to_string(),
            "wamn_run".to_string(),
            "run_queue".to_string(),
            "DELETE".to_string(),
        ),
        (
            "relation".to_string(),
            "wamn_run".to_string(),
            "effect_attempts".to_string(),
            "SELECT".to_string(),
        ),
        (
            "routine".to_string(),
            "wamn_authority".to_string(),
            "tenant_key".to_string(),
            "EXECUTE".to_string(),
        ),
    ]);
    for relation in sql::EXECUTOR_PLATFORM_CATALOG_RELATIONS {
        expected.insert((
            "relation".to_string(),
            "catalog".to_string(),
            relation.to_string(),
            "SELECT".to_string(),
        ));
    }
    for (relation, columns) in [
        ("runs", &sql::EXECUTOR_PLATFORM_RUN_UPDATE_COLUMNS[..]),
        (
            "run_queue",
            &sql::EXECUTOR_PLATFORM_QUEUE_UPDATE_COLUMNS[..],
        ),
    ] {
        for column in columns {
            expected.insert((
                "column".to_string(),
                "wamn_run".to_string(),
                format!("{relation}.{column}"),
                "UPDATE".to_string(),
            ));
        }
    }
    anyhow::ensure!(
        actual == expected,
        "stable role {role:?} ACLs in database {database:?} are not the exact executor-platform grant set"
    );
    Ok(())
}

/// THE CALLABLE-HTTP ADMITTER DENIAL MATRIX (`wamn-0h0g.22.37`).
///
/// `USAGE` on `catalog` and `app_system`, the exact catalog and fresh permission
/// reads, and NOTHING on the run plane — the
/// disjointness from the executor family is the security property, and it is
/// asserted by equality for the same reason the two T1 readers' is. A `wamn_run`
/// schema `USAGE` alone would fail here, which is what stops this credential
/// being quietly reused for admission work.
fn verify_http_admitter_grants(
    role: &str,
    database: &str,
    required_database: &str,
    grants: &[RoleAcl],
) -> anyhow::Result<()> {
    if grants.is_empty() {
        anyhow::ensure!(
            database != required_database,
            "stable role {role:?} has no callable-HTTP admitter ACL in required database {database:?}"
        );
        return Ok(());
    }
    let actual = acl_tuples(grants);
    let mut expected = BTreeSet::from([
        (
            "schema".to_string(),
            "app_system".to_string(),
            "app_system".to_string(),
            "USAGE".to_string(),
        ),
        (
            "schema".to_string(),
            "catalog".to_string(),
            "catalog".to_string(),
            "USAGE".to_string(),
        ),
        (
            "relation".to_string(),
            "app_system".to_string(),
            "permissions".to_string(),
            "SELECT".to_string(),
        ),
        (
            "relation".to_string(),
            "app_system".to_string(),
            "users".to_string(),
            "SELECT".to_string(),
        ),
        (
            "relation".to_string(),
            "app_system".to_string(),
            "user_roles".to_string(),
            "SELECT".to_string(),
        ),
    ]);
    for relation in sql::HTTP_ADMITTER_CATALOG_RELATIONS {
        expected.insert((
            "relation".to_string(),
            "catalog".to_string(),
            relation.to_string(),
            "SELECT".to_string(),
        ));
    }
    anyhow::ensure!(
        actual == expected,
        "stable role {role:?} ACLs in database {database:?} are not the exact callable-HTTP admission grant set"
    );
    Ok(())
}

/// Exact two-table catalog read surface of the event materializer.
fn verify_event_materializer_grants(
    role: &str,
    database: &str,
    required_database: &str,
    grants: &[RoleAcl],
) -> anyhow::Result<()> {
    if grants.is_empty() {
        anyhow::ensure!(
            database != required_database,
            "stable role {role:?} has no event-materializer ACL in required database {database:?}"
        );
        return Ok(());
    }
    let actual = acl_tuples(grants);
    let mut expected = BTreeSet::from([(
        "schema".to_string(),
        "catalog".to_string(),
        "catalog".to_string(),
        "USAGE".to_string(),
    )]);
    for relation in sql::EVENT_MATERIALIZER_CATALOG_RELATIONS {
        expected.insert((
            "relation".to_string(),
            "catalog".to_string(),
            relation.to_string(),
            "SELECT".to_string(),
        ));
    }
    anyhow::ensure!(
        actual == expected,
        "stable role {role:?} ACLs in database {database:?} are not the exact event-materializer grant set"
    );
    Ok(())
}

/// THE DISJOINTNESS MATRIX for one T1 control-database reader
/// (`wamn-0h0g.12.116`, `wamn-0h0g.12.67`).
///
/// The server's own `aclexplode` answer, compared for EQUALITY against the
/// derived set — never containment. Containment would pass a role that had
/// acquired the OTHER reader's schema, and that union is the exact failure the
/// two families exist to prevent; an added `INSERT` or `UPDATE` fails here for
/// the same reason.
///
/// An empty grant set is only acceptable in a database that is not the target:
/// the control database MUST carry the grant set, and every other database in
/// the cluster must carry nothing at all.
fn verify_system_reader_grants(
    reader: SystemReader,
    schema: &str,
    relations: &[&str],
    role: &str,
    database: &str,
    required_database: &str,
    grants: &[RoleAcl],
) -> anyhow::Result<()> {
    if database != required_database {
        anyhow::ensure!(
            grants.is_empty(),
            "stable role {role:?} carries a {reader} ACL in database {database:?}, \
             which is not the control database"
        );
        return Ok(());
    }
    anyhow::ensure!(
        !grants.is_empty(),
        "stable role {role:?} has no {reader} ACL in required database {database:?}"
    );

    let actual = grants
        .iter()
        .map(|acl| {
            (
                acl.object_kind.clone(),
                acl.schema_name.clone(),
                acl.object_name.clone(),
                acl.privilege.clone(),
            )
        })
        .collect::<BTreeSet<_>>();
    let mut expected = BTreeSet::from([(
        "schema".to_string(),
        schema.to_string(),
        schema.to_string(),
        "USAGE".to_string(),
    )]);
    for relation in relations {
        expected.insert((
            "relation".to_string(),
            schema.to_string(),
            (*relation).to_string(),
            "SELECT".to_string(),
        ));
    }
    anyhow::ensure!(
        actual == expected,
        "stable role {role:?} ACLs in database {database:?} are not the exact {reader} grant set"
    );
    Ok(())
}

async fn read_workload_role_state(
    client: &(impl GenericClient + Sync),
    role: &str,
    label: &str,
) -> anyhow::Result<Option<WorkloadRoleState>> {
    let row = client
        .query_opt(sql::workload_generation_state_sql(), &[&role])
        .await
        .with_context(|| format!("read {label} generation state"))?;
    Ok(row.map(|row| WorkloadRoleState {
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
        membership_options_migratable: row.get("membership_options_migratable"),
        member_roles: row.get("member_roles"),
        member_options_exact: row.get("member_options_exact"),
        generation_children_exact: row.get("generation_children_exact"),
        connect_databases: row.get("connect_databases"),
        sessions: row.get("sessions"),
        owned_objects: row.get("owned_objects"),
    }))
}

fn provision_summary(triple: &Triple, database: &str, cluster: &str) -> String {
    format!(
        "project-env {triple}: database {database:?} on cluster {cluster:?} (owner {DB_OWNER_ROLE})"
    )
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
    ensure_env_policy_durability_schema(client).await?;
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

/// Read one project-env's stored instance suffix from the registry.
pub(crate) async fn read_project_env_instance(
    system_url: &str,
    triple: &Triple,
) -> anyhow::Result<String> {
    let (client, conn) = tokio_postgres::connect(system_url, NoTls)
        .await
        .context("system db connect")?;
    let conn_task = tokio::spawn(conn);
    let result = async {
        client
            .batch_execute("SET ROLE wamn_system")
            .await
            .context("SET ROLE wamn_system")?;
        let env = triple.env.as_str();
        let row = client
            .query_opt(
                &wamn_control_registry::sql::select_project_env_sql(),
                &[&triple.org, &triple.project, &env],
            )
            .await
            .context("read registry.project_envs row")?
            .with_context(|| format!("project-env {triple} is not recorded"))?;
        let stored: String = row.get("instance_suffix");
        validate_instance_suffix(&stored)
            .map_err(|error| anyhow::anyhow!("registry instance suffix: {error}"))?;
        Ok(stored)
    }
    .await;
    drop(client);
    let _ = conn_task.await;
    result
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
    tenant: Option<&str>,
    secret_name: &str,
    secret_namespace: Option<&str>,
    minted: &str,
    disposable: bool,
) -> anyhow::Result<String> {
    let (mut client, conn) = tokio_postgres::connect(system_url, NoTls)
        .await
        .context("system db connect")?;
    let conn_task = tokio::spawn(conn);
    let result = do_record_project_env(
        &mut client,
        triple,
        tenant,
        secret_name,
        secret_namespace,
        minted,
        disposable,
    )
    .await;
    drop(client);
    let _ = conn_task.await;
    result
}

async fn do_record_project_env(
    client: &mut tokio_postgres::Client,
    triple: &Triple,
    tenant: Option<&str>,
    secret_name: &str,
    secret_namespace: Option<&str>,
    minted: &str,
    disposable: bool,
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
                &disposable,
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
    let stored_disposable: bool = row.get(1);
    validate_instance_suffix(&stored)
        .map_err(|error| anyhow::anyhow!("registry instance suffix: {error}"))?;
    project_tenant_environment(client, triple, tenant, &stored, stored_disposable).await?;
    Ok(stored)
}

/// Probe for the control store's environment projection without assuming it.
const CONTROL_PROJECTION_INSTALLED_SQL: &str =
    "SELECT to_regclass('catalog.tenant_environments') IS NOT NULL";

/// Claim the projected tenant for the transaction's RLS policy.
const CLAIM_PROJECTED_TENANT_SQL: &str = "SELECT set_config('app.tenant', $1, true)";

/// Write the projected copy of the row just recorded.
const PROJECT_TENANT_ENVIRONMENT_SQL: &str =
    "SELECT catalog.project_tenant_environment($1, $2, $3, $4, $5, $6)";

/// Project the recorded project-env into the control store, beside the facts its
/// `disposable` marker governs (wamn-10yt.38).
///
/// `registry.project_envs` stays the AUTHORITY. This copy carries that row's
/// identity — the triple plus the STORED instance suffix, read back from the
/// upsert rather than assumed — so the admit path resolves the marker locally
/// and a disagreement between the two planes refuses instead of passing.
///
/// ABSENCE MEANS DURABLE, so a system database with no control store, or a
/// provisioning that names no tenant, records nothing here and admits exactly as
/// it always did. Only a DISPOSABLE environment insists, because for it the
/// missing projection would be the difference between an author's edit landing
/// and an author's edit being refused.
async fn project_tenant_environment(
    client: &mut tokio_postgres::Client,
    triple: &Triple,
    tenant: Option<&str>,
    instance_suffix: &str,
    disposable: bool,
) -> anyhow::Result<()> {
    let installed: bool = client
        .query_one(CONTROL_PROJECTION_INSTALLED_SQL, &[])
        .await
        .context("probe the control store's environment projection")?
        .get(0);
    if !installed {
        anyhow::ensure!(
            !disposable,
            "a disposable project-env needs catalog.tenant_environments: apply \
             wamn_control_provision::CONTROL_PORTABLE_STORE_SQL to the system database first"
        );
        return Ok(());
    }
    let Some(tenant) = tenant else {
        anyhow::ensure!(
            !disposable,
            "a disposable project-env needs --tenant: the admit path resolves the \
             marker by the tenant its component facts are keyed on"
        );
        return Ok(());
    };
    let env = triple.env.as_str();
    let transaction = client
        .transaction()
        .await
        .context("begin the project-env control projection")?;
    transaction
        .query_one(CLAIM_PROJECTED_TENANT_SQL, &[&tenant])
        .await
        .context("claim the projected tenant")?;
    transaction
        .execute(
            PROJECT_TENANT_ENVIRONMENT_SQL,
            &[
                &tenant,
                &triple.org,
                &triple.project,
                &env,
                &instance_suffix,
                &disposable,
            ],
        )
        .await
        .context("project the project-env into the control store")?;
    transaction
        .commit()
        .await
        .context("commit the project-env control projection")
}

#[cfg(test)]
mod tests;
