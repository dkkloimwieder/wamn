//! Arguments and output of the `provision-system`, `provision-org`,
//! `provision-project-env`, `enable-cdc-project-env`, `recover-capture-gap`, and
//! `close-capture-gap` verbs.

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::{Args, ValueEnum};
use serde_json::Value;
use wamn_control::capture_gap::{
    self, CloseCaptureGapRequest, ClosedCaptureGap, RecoverCaptureGapRequest, RecoveredCaptureGap,
};
use wamn_control::delete_project_env::DeleteProjectEnvRequest;
use wamn_control::enable_cdc_project_env::{
    self, EnableCdcProjectEnvOutcome, EnableCdcProjectEnvRequest,
};
use wamn_control::pat_client::PatIssuerConfig;
use wamn_control::provision_org::{self, ProvisionOrgRequest, ProvisionedOrg};
use wamn_control::provision_project_env::{
    self, OrgWorkloadActionRequest, ProvisionProjectEnvOutcome, ProvisionProjectEnvRequest,
    WorkloadActionOutcome, WorkloadActionRequest, WorkloadActionVerb, WorkloadGenerationAction,
    ensure_secret_path, parse_pat_prefix, workload_action_flag, workload_secret_flag,
};
use wamn_control::provision_system::{self, EmitProvisionerRequest, ProvisionSystemRequest};
use wamn_control_provision::{CredentialGeneration, DB_OWNER_ROLE, WorkloadRoleFamily};
use wamn_control_registry::{Template, Triple};

#[derive(Debug, Args)]
pub struct ProvisionSystemArgs {
    /// Superuser Postgres URL to the empty system database (`wamn_system`),
    /// whose owner role `wamn_system` already exists.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_url: String,

    /// Email domain of the platform principal rows, written to
    /// `registry.meta.platform_domain`.
    #[arg(long, required_unless_present = "emit_secret")]
    pub platform_domain: Option<String>,

    /// Install nothing. Write the Secret manifest `wamn-provisioner` of the
    /// `platform` namespace here, mode 0600, with a new password.
    #[arg(
        long,
        requires_all = ["emit_provisioner_sql", "db_host"],
        conflicts_with = "platform_domain"
    )]
    pub emit_secret: Option<PathBuf>,

    /// Write the role statement of `wamn_provisioner` here, with the SCRAM
    /// verifier of the password in `--emit-secret`. The operator applies it
    /// once as superuser.
    #[arg(long, requires = "emit_secret")]
    pub emit_provisioner_sql: Option<PathBuf>,

    /// Host the URL in the emitted Secret names.
    #[arg(long, requires = "emit_secret")]
    pub db_host: Option<String>,

    /// Port the URL in the emitted Secret names.
    #[arg(long, default_value_t = 5432)]
    pub db_port: u16,
}

/// Install the control store into an empty system database, once. With the
/// emit flags, write only the provisioner's Secret manifest and role statement.
pub async fn provision_system(args: ProvisionSystemArgs) -> anyhow::Result<()> {
    if let (Some(emit_secret), Some(emit_provisioner_sql), Some(db_host)) =
        (args.emit_secret, args.emit_provisioner_sql, args.db_host)
    {
        provision_system::emit_provisioner_credential(&EmitProvisionerRequest {
            system_database_url: args.system_url,
            emit_secret,
            emit_provisioner_sql,
            db_host,
            db_port: args.db_port,
        })?;
        println!("provision-system: provisioner Secret and role statement written");
        return Ok(());
    }
    provision_system::provision_system(&ProvisionSystemRequest {
        system_database_url: args.system_url,
        platform_domain: args
            .platform_domain
            .context("--platform-domain is required")?,
    })
    .await?;
    println!("provision-system: control store installed");
    Ok(())
}

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

    /// Namespace of the CNPG `Cluster`. Everything that lives beside it (the
    /// `Database`, the `ObjectStore`, the `ScheduledBackup`) shares it.
    #[arg(long, env = "WAMN_CLUSTER_NAMESPACE", default_value = "wamn-system")]
    pub cluster_namespace: String,

    /// Host every emitted credential URL names. Defaults to the target
    /// cluster's read-write service `<cluster>-rw`, never the admin URL's host.
    #[arg(long)]
    pub db_host: Option<String>,

    /// Port every emitted credential URL names.
    #[arg(long, default_value_t = 5432)]
    pub db_port: u16,

    /// Namespace the emitted credential `Secret` is applied to.
    #[arg(long, env = "WAMN_NAMESPACE", default_value = "wamn-system")]
    pub namespace: String,

    /// Secret namespace to RECORD in the registry `SecretRef`. Omit to record
    /// `NULL` (the resolving component's own namespace).
    #[arg(long)]
    pub secret_namespace: Option<String>,

    /// Explicit target project-database admin URL for the generation actions that
    /// address the project-env database (for example, management-admitter).
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

    /// Write the JSON merge patch that puts the prepared administration login
    /// into the control host's Secret `wamn-control-administration-<org>`.
    /// Every administration prepare, the first one and each rotation,
    /// requires it, because the control host holds the same login.
    #[arg(long, value_name = "PATH", value_parser = parse_secret_path)]
    pub emit_control_administration_patch: Option<PathBuf>,

    /// Write the privilege SQL (`ALTER DATABASE … OWNER TO wamn_db_owner`, then
    /// `REVOKE CONNECT,TEMPORARY FROM PUBLIC` and `REVOKE CONNECT` from
    /// `wamn_app`; apply AFTER the database is
    /// ready) here; `-` = stdout.
    #[arg(long)]
    pub emit_privilege_sql: Option<PathBuf>,

    /// Write the database credential `Secret` (JSON) here. Required for
    /// provisioning unless the run only issues a PAT, and must name a file;
    /// credentials are never written to stdout.
    #[arg(
        long,
        value_name = "PATH",
        value_parser = parse_secret_path,
        required_unless_present_any = [
            "revoke_pat_prefix",
            WORKLOAD_ACTION_GROUP,
            "emit_management_author_pat_secret",
            "emit_operator_pat_secret"
        ]
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

    /// Issue an operator PAT and write its Kubernetes `Secret` JSON here.
    #[arg(
        long,
        value_name = "PATH",
        value_parser = parse_secret_path,
        conflicts_with = "revoke_pat_prefix"
    )]
    pub emit_operator_pat_secret: Option<PathBuf>,

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

/// A closed set of families whose generation flags one verb carries.
pub trait WorkloadFamilySet: fmt::Debug + Default + Clone + Send + Sync + 'static {
    /// The id of the group every action flag of the set belongs to.
    const ACTION_GROUP: &'static str;
    /// The id of the group every credential-Secret flag of the set belongs to.
    const SECRET_GROUP: &'static str;
    /// The families of the set, in declaration order.
    fn families() -> impl Iterator<Item = WorkloadRoleFamily>;
}

/// The families `provision-project-env` mints.
#[derive(Debug, Default, Clone)]
pub struct ProjectEnvFamilies;

impl WorkloadFamilySet for ProjectEnvFamilies {
    const ACTION_GROUP: &'static str = WORKLOAD_ACTION_GROUP;
    const SECRET_GROUP: &'static str = WORKLOAD_SECRET_GROUP;
    fn families() -> impl Iterator<Item = WorkloadRoleFamily> {
        WorkloadRoleFamily::project_env_families()
    }
}

/// The org-scoped families `provision-org` mints (`wamn-a40n.2`).
#[derive(Debug, Default, Clone)]
pub struct OrgFamilies;

impl WorkloadFamilySet for OrgFamilies {
    const ACTION_GROUP: &'static str = "org_generation_action";
    const SECRET_GROUP: &'static str = "org_generation_secret";
    fn families() -> impl Iterator<Item = WorkloadRoleFamily> {
        WorkloadRoleFamily::ALL
            .into_iter()
            .filter(|family| !family.is_project_env_provisioned())
    }
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
pub struct WorkloadGenerationArgs<S: WorkloadFamilySet = ProjectEnvFamilies> {
    /// The single selected action. `multiple(false)` on the action group makes
    /// "single" a parse-time guarantee rather than a convention.
    pub action: Option<WorkloadGenerationAction>,
    /// The credential Secret to write, bound by `requires` to its OWN family's
    /// prepare — so a Secret can accompany neither another family's action nor
    /// a retire or abort.
    pub secret: Option<(WorkloadRoleFamily, PathBuf)>,
    set: std::marker::PhantomData<S>,
}

fn workload_action_id(family: WorkloadRoleFamily, verb: WorkloadActionVerb) -> String {
    workload_action_flag(family, verb).replace('-', "_")
}

fn workload_secret_id(family: WorkloadRoleFamily) -> String {
    workload_secret_flag(family).replace('-', "_")
}

impl<S: WorkloadFamilySet> clap::Args for WorkloadGenerationArgs<S> {
    fn augment_args(command: clap::Command) -> clap::Command {
        let mut command = command;
        let mut action_ids: Vec<clap::Id> = Vec::new();
        let mut secret_ids: Vec<clap::Id> = Vec::new();
        for family in S::families() {
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
            for other in S::families() {
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
                clap::ArgGroup::new(S::ACTION_GROUP)
                    .args(action_ids)
                    .multiple(false),
            )
            .group(
                clap::ArgGroup::new(S::SECRET_GROUP)
                    .args(secret_ids)
                    .multiple(false),
            )
    }

    fn augment_args_for_update(command: clap::Command) -> clap::Command {
        Self::augment_args(command)
    }
}

impl<S: WorkloadFamilySet> clap::FromArgMatches for WorkloadGenerationArgs<S> {
    fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        let mut parsed = Self::default();
        parsed.update_from_arg_matches(matches)?;
        Ok(parsed)
    }

    fn update_from_arg_matches(&mut self, matches: &clap::ArgMatches) -> Result<(), clap::Error> {
        self.action = None;
        self.secret = None;
        for family in S::families() {
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

#[derive(Debug, Args)]
pub struct EnableCdcProjectEnvArgs {
    /// Org id (the project-env must already be provisioned and recorded).
    #[arg(long)]
    pub org: String,

    /// Project id.
    #[arg(long)]
    pub project: String,

    /// Environment slug.
    #[arg(long)]
    pub env: String,

    /// The application DATA schema the publication covers (not the
    /// `app_system` auth schema).
    #[arg(long, default_value = "public")]
    pub schema: String,

    /// Superuser Postgres URL to the T1 system DB (`wamn_system`): derive the
    /// target cluster, resolve the stored project-env instance suffix, and record
    /// the `registry.event_readers` registration. Env `WAMN_SYSTEM_ADMIN_URL`.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: Option<String>,

    /// Override the target CNPG `Cluster` name. When omitted, it is derived from
    /// the org's placement in the registry.
    #[arg(long)]
    pub cluster: Option<String>,

    /// Password for the per-project-env replication role (embedded in the
    /// emitted URL + role SQL). Supply it with `--replication-password` or the
    /// env var `WAMN_REPLICATION_PASSWORD`.
    ///
    /// **Deliberately has no `default_value`.** A default here would mint a
    /// `LOGIN REPLICATION` role
    /// with a publicly known password, and `REPLICATION` authority is
    /// cluster-wide: it can open a replication session against any database on
    /// the cluster and decode co-tenant WAL. Provisioning refuses instead
    /// (wamn-0h0g.12.134).
    #[arg(
        long,
        env = "WAMN_REPLICATION_PASSWORD",
        value_name = "PASSWORD ($WAMN_REPLICATION_PASSWORD)"
    )]
    pub replication_password: String,

    /// Host the reader reaches the project-env database at. Defaults to the
    /// target cluster's read-write service `<cluster>-rw`.
    #[arg(long)]
    pub db_host: Option<String>,

    /// Port the reader reaches the database at.
    #[arg(long, default_value_t = 5432)]
    pub db_port: u16,

    /// Namespace the emitted `Secret` is applied to.
    #[arg(long, env = "WAMN_NAMESPACE", default_value = "wamn-system")]
    pub namespace: String,

    /// Secret namespace to RECORD in the registration's replication `SecretRef`.
    /// Omit to record `NULL` (the resolving service's own namespace).
    #[arg(long)]
    pub secret_namespace: Option<String>,

    /// Exact environment source stream. Must match the declared coordinates.
    #[arg(long)]
    pub stream: Option<String>,

    /// Event broker managed by this environment's provisioning credential.
    #[arg(long, env = "WAMN_EVT_NATS_URL")]
    pub nats_url: String,

    /// Provisioning username. Runtime uses a separate restricted credential.
    #[arg(long, env = "WAMN_EVT_NATS_USERNAME")]
    pub nats_username: String,

    /// Private file containing the provisioning password.
    #[arg(long, env = "WAMN_EVT_NATS_PASSWORD_FILE")]
    pub nats_password_file: PathBuf,

    /// NATS stream copies, separate from workload instances.
    #[arg(long, env = "WAMN_EVT_STREAM_REPLICAS")]
    pub stream_replicas: usize,

    /// Declared duplicate detection window in seconds.
    #[arg(long, env = "WAMN_EVT_DUP_WINDOW_SECS")]
    pub dup_window_secs: u64,

    /// One native NATS pull consumer configuration as JSON. Repeat as needed.
    #[arg(long, value_name = "JSON")]
    pub consumer_config: Vec<String>,

    /// Write the replication-role SQL (psql the TARGET cluster first — roles are
    /// cluster-global) here; `-` = stdout.
    #[arg(long)]
    pub emit_role_sql: Option<PathBuf>,

    /// Write the CDC SQL (schema guard + failover slot + grants; psql the
    /// PROJECT-ENV database) here; `-` = stdout.
    #[arg(long)]
    pub emit_cdc_sql: Option<PathBuf>,

    /// Write the CNPG `Publication` CR (JSON) here; `-` = stdout. Apply it
    /// after the CDC SQL and wait for `status.applied` before any write.
    #[arg(long)]
    pub emit_publication: Option<PathBuf>,

    /// Namespace of the CNPG `Cluster`, which the `Publication` shares.
    #[arg(long, env = "WAMN_CLUSTER_NAMESPACE", default_value = "wamn-system")]
    pub cluster_namespace: String,

    /// Write the replication-credential `Secret` (JSON) here; `-` = stdout.
    #[arg(long)]
    pub emit_secret: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct RecoverCaptureGapArgs {
    /// Org id.
    #[arg(long)]
    pub org: String,

    /// Project id.
    #[arg(long)]
    pub project: String,

    /// Environment slug.
    #[arg(long)]
    pub env: String,

    /// Superuser Postgres URL to the T1 system DB (`wamn_system`): the instance
    /// suffix, the reader registration, and the gap row.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: String,

    /// Superuser connection to the project database, which holds the slot.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,

    /// Event broker of the environment.
    #[arg(long, env = "WAMN_EVT_NATS_URL")]
    pub nats_url: String,

    /// Observer username of the environment. Reading the stream is its job.
    #[arg(long, env = "WAMN_EVT_NATS_USERNAME")]
    pub nats_username: String,

    /// Private file containing the observer password.
    #[arg(long, env = "WAMN_EVT_NATS_PASSWORD_FILE")]
    pub nats_password_file: PathBuf,
}

#[derive(Debug, Args)]
pub struct CloseCaptureGapArgs {
    /// Org id.
    #[arg(long)]
    pub org: String,

    /// Project id.
    #[arg(long)]
    pub project: String,

    /// Environment slug.
    #[arg(long)]
    pub env: String,

    /// Superuser Postgres URL to the T1 system DB (`wamn_system`).
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: String,
}

#[derive(Debug, Args)]
pub struct DeleteProjectEnvArgs {
    /// Org id.
    #[arg(long)]
    pub org: String,

    /// Project id.
    #[arg(long)]
    pub project: String,

    /// Environment slug.
    #[arg(long)]
    pub env: String,

    /// Superuser Postgres URL to the T1 system DB (`wamn_system`): the registry
    /// row, the tenant row and the control rows.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: String,

    /// Superuser connection to the `postgres` database of the target cluster:
    /// the slot, the database and the roles.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,

    /// Event broker of the environment.
    #[arg(long, env = "WAMN_EVT_NATS_URL")]
    pub nats_url: String,

    /// Provisioning username of the stream, which may delete it.
    #[arg(long, env = "WAMN_EVT_NATS_USERNAME")]
    pub nats_username: String,

    /// Private file containing the provisioning password.
    #[arg(long, env = "WAMN_EVT_NATS_PASSWORD_FILE")]
    pub nats_password_file: PathBuf,

    /// Run the deletes. Without it, the verb prints the plan and changes nothing.
    #[arg(long)]
    pub confirm: bool,

    /// Write the JSON merge patch that removes the environment's key from the
    /// control host's Secret `wamn-control-administration-<org>`. Apply it
    /// before the run, as the plan says.
    #[arg(long, value_name = "PATH")]
    pub emit_control_administration_patch: Option<PathBuf>,
}

/// Arguments of `upgrade-schema` (docs/plan/schema-upgrade.md).
#[derive(Debug, Args)]
pub struct UpgradeSchemaArgs {
    /// Superuser Postgres URL to the T1 system DB (`wamn_system`). Alone, it
    /// names the database to upgrade. With `--admin-database-url`, the verb
    /// reads `registry.project_envs` through it.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL", hide_env_values = true)]
    pub system_database_url: String,

    /// Superuser Postgres URL to one project-environment database. With it,
    /// the verb upgrades that database.
    #[arg(long, env = "WAMN_PG_ADMIN_URL", hide_env_values = true)]
    pub admin_database_url: Option<String>,

    /// First run on a database installed before its record table: the last
    /// file that the database already holds. The verb records the files up to
    /// it without running them. Refused once the record table has a row.
    #[arg(long)]
    pub baseline: Option<i32>,

    /// Apply the pending files. Without it, the verb prints them and changes nothing.
    #[arg(long)]
    pub confirm: bool,

    /// Also write the one statement that records a password fingerprint for
    /// every generation prepared before system migration 0009. The operator
    /// applies it once by hand, as a superuser, connected to the system
    /// database (`docs/operations/gcp.md` §7).
    #[arg(long, value_name = "PATH")]
    pub emit_fingerprint_sql: Option<PathBuf>,
}

/// TLS credentials and the identity service endpoint for operator PAT issuance.
#[derive(Clone, Default, Args)]
pub struct PatIssuerArgs {
    /// HTTPS base URL of the identity service. Its path is preserved.
    #[arg(long = "pat-issuer", env = "WAMN_PAT_ISSUER", hide_env_values = true)]
    pub endpoint: Option<String>,

    /// PEM certificate chain for the provisioning operator.
    #[arg(
        long = "pat-client-cert",
        env = "WAMN_PAT_CLIENT_CERT",
        hide_env_values = true
    )]
    pub client_cert: Option<PathBuf>,

    /// PEM private key for the provisioning operator.
    #[arg(
        long = "pat-client-key",
        env = "WAMN_PAT_CLIENT_KEY",
        hide_env_values = true
    )]
    pub client_key: Option<PathBuf>,

    /// PEM roots that replace the default trust roots for this identity service.
    #[arg(
        long = "pat-server-ca",
        env = "WAMN_PAT_SERVER_CA",
        hide_env_values = true
    )]
    pub server_ca: Option<PathBuf>,
}

impl fmt::Debug for PatIssuerArgs {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PatIssuerArgs")
            .field("endpoint", &self.endpoint.as_ref().map(|_| "<redacted>"))
            .field(
                "client_cert",
                &self.client_cert.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "client_key",
                &self.client_key.as_ref().map(|_| "<redacted>"),
            )
            .field("server_ca", &self.server_ca.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl From<PatIssuerArgs> for PatIssuerConfig {
    fn from(args: PatIssuerArgs) -> Self {
        Self {
            endpoint: args.endpoint,
            client_cert: args.client_cert,
            client_key: args.client_key,
            server_ca: args.server_ca,
        }
    }
}

fn parse_secret_path(value: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    ensure_secret_path(&path, "secret output").map_err(|error| error.to_string())?;
    Ok(path)
}

/// Provision one project-env, run one workload-generation action, or revoke one
/// PAT, and print what it did.
pub async fn provision(args: ProvisionProjectEnvArgs) -> anyhow::Result<()> {
    if let Some(prefix) = args.revoke_pat_prefix.as_deref() {
        let system_url = args
            .system_database_url
            .as_deref()
            .context("--revoke-pat-prefix requires --system-database-url")?;
        provision_project_env::revoke_provisioning_pat(system_url, prefix).await?;
        println!("revoked PAT prefix {prefix}");
        return Ok(());
    }

    // ONE dispatch over the closed family set, replacing four hand-written
    // branches that each had to be added beside a new `run_*_action`.
    if let Some(action) = args.workload.action {
        let request = workload_action_request(args, action)?;
        let outcome = provision_project_env::run_workload_action(&request).await?;
        print_workload_action(&request, &outcome);
        return Ok(());
    }
    anyhow::ensure!(
        args.target_admin_database_url.is_none(),
        "--target-admin-database-url is valid only for a workload generation action"
    );
    anyhow::ensure!(
        args.emit_control_administration_patch.is_none(),
        "--emit-control-administration-patch is valid only for an administration prepare"
    );

    let request = provisioning_request(args);
    let outcome = provision_project_env::provision_project_env(&request).await?;
    print_provisioned(&request, &outcome)
}

fn workload_action_request(
    args: ProvisionProjectEnvArgs,
    action: WorkloadGenerationAction,
) -> anyhow::Result<WorkloadActionRequest> {
    let label = action.family.label();
    anyhow::ensure!(
        args.cluster.is_none()
            && args.connection_limit.is_none()
            && args.emit_database.is_none()
            && args.emit_privilege_sql.is_none()
            && args.emit_secret.is_none()
            && args.emit_management_author_pat_secret.is_none()
            && args.emit_operator_pat_secret.is_none(),
        "{label} generation actions cannot render ordinary provisioning or PAT artifacts; only \
         App prepare may emit the canonical shared-login retirement role SQL"
    );
    let secret = args
        .workload_secret_path(action.family)
        .map(Path::to_path_buf);
    Ok(WorkloadActionRequest {
        org: args.org.expect(
            "clap parser invariant: --org is required unless --revoke-pat-prefix is present",
        ),
        project: args.project.expect(
            "clap parser invariant: --project is required unless --revoke-pat-prefix is present",
        ),
        env: args.env.expect(
            "clap parser invariant: --env is required unless --revoke-pat-prefix is present",
        ),
        tenant: args.tenant,
        system_database_url: args.system_database_url,
        target_admin_database_url: args.target_admin_database_url,
        cluster: args.cluster,
        db_host: args.db_host,
        db_port: args.db_port,
        namespace: args.namespace,
        action,
        secret,
        emit_role_sql: args.emit_role_sql,
        control_administration_patch: args.emit_control_administration_patch,
    })
}

fn provisioning_request(args: ProvisionProjectEnvArgs) -> ProvisionProjectEnvRequest {
    ProvisionProjectEnvRequest {
        org: args.org.expect(
            "clap parser invariant: --org is required unless --revoke-pat-prefix is present",
        ),
        project: args.project.expect(
            "clap parser invariant: --project is required unless --revoke-pat-prefix is present",
        ),
        env: args.env.expect(
            "clap parser invariant: --env is required unless --revoke-pat-prefix is present",
        ),
        tenant: args.tenant,
        disposable: args.disposable,
        system_database_url: args.system_database_url,
        cluster: args.cluster,
        connection_limit: args.connection_limit,
        cluster_namespace: args.cluster_namespace,
        namespace: args.namespace,
        secret_namespace: args.secret_namespace,
        emit_database: args.emit_database,
        emit_role_sql: args.emit_role_sql,
        emit_privilege_sql: args.emit_privilege_sql,
        emit_secret: args.emit_secret,
        pat_issuer: args.pat_issuer.into(),
        emit_management_author_pat_secret: args.emit_management_author_pat_secret,
        emit_operator_pat_secret: args.emit_operator_pat_secret,
    }
}

/// Print the lines that report one project-env provisioning.
pub fn print_provisioned(
    request: &ProvisionProjectEnvRequest,
    outcome: &ProvisionProjectEnvOutcome,
) -> anyhow::Result<()> {
    println!(
        "{}",
        provision_summary(&outcome.triple, &outcome.database, &outcome.cluster)
    );

    emit_json(
        request.emit_database.as_deref(),
        "Database CR (kubectl apply)",
        &outcome.database_cr,
    )?;
    emit_text(
        request.emit_role_sql.as_deref(),
        "role SQL (psql the TARGET cluster BEFORE the Database CR)",
        &outcome.role_sql,
    );
    emit_text(
        request.emit_privilege_sql.as_deref(),
        "privilege SQL (psql the TARGET cluster AFTER the Database is ready)",
        &outcome.privilege_sql,
    );
    if let Some(path) = &request.emit_secret {
        println!(
            "wrote {} (database credential Secret; kubectl apply)",
            path.display()
        );
    }

    println!(
        "recorded project {:?} + project-env {} in the registry (wamn_system)",
        request.project, outcome.triple
    );
    println!(
        "environment namespace {:?} (instance suffix {:?})",
        outcome.namespace, outcome.instance
    );

    for secret in &outcome.pat_secrets {
        println!(
            "wrote {} ({} PAT Secret; kubectl apply)",
            secret.path.display(),
            secret.purpose
        );
    }
    Ok(())
}

/// Print the line that reports one org credential action.
fn print_org_workload_action(request: &OrgWorkloadActionRequest, outcome: &WorkloadActionOutcome) {
    let WorkloadGenerationAction {
        family, generation, ..
    } = request.action;
    let (label, generation, org) = (family.label(), generation.as_str(), &request.org);
    match outcome {
        WorkloadActionOutcome::Prepared { secret, .. } => println!(
            "prepared and authenticated {label} credential generation {generation} for org {org}; wrote {}",
            secret.display()
        ),
        WorkloadActionOutcome::Retired => {
            println!("retired {label} credential generation {generation} for org {org}");
        }
        WorkloadActionOutcome::Aborted => {
            println!("aborted {label} credential generation {generation} for org {org}");
        }
    }
}

/// Print the lines that report one workload-generation action.
pub fn print_workload_action(request: &WorkloadActionRequest, outcome: &WorkloadActionOutcome) {
    let WorkloadGenerationAction {
        family, generation, ..
    } = request.action;
    let label = family.label();
    let (org, project, environment) = (&request.org, &request.project, &request.env);
    match outcome {
        WorkloadActionOutcome::Prepared {
            secret,
            app_retirement_role_sql,
        } => {
            println!(
                "prepared and authenticated {label} credential generation {} for {org}/{project}/{environment}; wrote {}",
                generation.as_str(),
                secret.display()
            );
            if let Some(retirement_sql) = app_retirement_role_sql {
                emit_text(
                    request.emit_role_sql.as_deref(),
                    "shared App-login retirement role SQL (apply once after every replacement carrier is verified)",
                    retirement_sql,
                );
            }
        }
        WorkloadActionOutcome::Retired => {
            println!(
                "retired {label} credential generation {} for {org}/{project}/{environment}",
                generation.as_str()
            );
        }
        WorkloadActionOutcome::Aborted => {
            println!(
                "aborted unpublished {label} credential generation {} for {org}/{project}/{environment}",
                generation.as_str()
            );
        }
    }
}

/// Recover one capture gap and print the row it wrote.
pub async fn recover_capture_gap(args: RecoverCaptureGapArgs) -> anyhow::Result<()> {
    let RecoveredCaptureGap {
        triple,
        slot,
        reason,
        start_lsn,
        start_at,
        end_lsn,
        created_at,
    } = capture_gap::recover_capture_gap(&RecoverCaptureGapRequest {
        org: args.org,
        project: args.project,
        env: args.env,
        system_database_url: args.system_database_url,
        admin_database_url: args.admin_database_url,
        nats_url: args.nats_url,
        nats_username: args.nats_username,
        nats_password_file: args.nats_password_file,
    })
    .await?;
    println!(
        "capture gap of {triple} recorded at {created_at}: slot {slot:?} created again, \
         reason {reason:?}, start_lsn {}, start_at {start_at}, end_lsn {end_lsn}",
        start_lsn.as_deref().unwrap_or("null"),
    );
    Ok(())
}

/// Delete one project environment. The library prints each step.
pub async fn delete_project_env(args: DeleteProjectEnvArgs) -> anyhow::Result<()> {
    wamn_control::delete_project_env::delete_project_env(&DeleteProjectEnvRequest {
        org: args.org,
        project: args.project,
        env: args.env,
        system_database_url: args.system_database_url,
        admin_database_url: args.admin_database_url,
        nats_url: args.nats_url,
        nats_username: args.nats_username,
        nats_password_file: args.nats_password_file,
        confirm: args.confirm,
        control_administration_patch: args.emit_control_administration_patch,
    })
    .await
}

/// Upgrade one installed database. The library prints each file.
pub async fn upgrade_schema(args: UpgradeSchemaArgs) -> anyhow::Result<()> {
    if let Some(path) = &args.emit_fingerprint_sql {
        std::fs::write(
            path,
            wamn_control_provision::sql::backfill_generation_passwords_sql(),
        )
        .with_context(|| format!("write {}", path.display()))?;
    }
    wamn_control::upgrade_schema::upgrade_schema(
        &wamn_control::upgrade_schema::UpgradeSchemaRequest {
            system_database_url: args.system_database_url,
            admin_database_url: args.admin_database_url,
            baseline: args.baseline,
            confirm: args.confirm,
        },
    )
    .await
}

/// Close the newest capture gap of one reader and print it.
pub async fn close_capture_gap(args: CloseCaptureGapArgs) -> anyhow::Result<()> {
    let ClosedCaptureGap {
        triple,
        slot,
        created_at,
        resync_at,
    } = capture_gap::close_capture_gap(&CloseCaptureGapRequest {
        org: args.org,
        project: args.project,
        env: args.env,
        system_database_url: args.system_database_url,
    })
    .await?;
    println!(
        "capture gap of {triple} recorded at {created_at} for slot {slot:?} closed: resync_at {resync_at}"
    );
    Ok(())
}

/// Overlay CDC capture onto one provisioned project-env and print what it did.
pub async fn enable_cdc(args: EnableCdcProjectEnvArgs) -> anyhow::Result<()> {
    let request = EnableCdcProjectEnvRequest {
        org: args.org,
        project: args.project,
        env: args.env,
        schema: args.schema,
        system_database_url: args.system_database_url,
        cluster: args.cluster,
        replication_password: args.replication_password,
        db_host: args.db_host,
        db_port: args.db_port,
        namespace: args.namespace,
        cluster_namespace: args.cluster_namespace,
        secret_namespace: args.secret_namespace,
        stream: args.stream,
        nats_url: args.nats_url,
        nats_username: args.nats_username,
        nats_password_file: args.nats_password_file,
        stream_replicas: args.stream_replicas,
        dup_window_secs: args.dup_window_secs,
        consumer_config: args.consumer_config,
        emit_role_sql: args.emit_role_sql,
        emit_cdc_sql: args.emit_cdc_sql,
        emit_publication: args.emit_publication,
        emit_secret: args.emit_secret,
    };
    let outcome = enable_cdc_project_env::enable_cdc_project_env(&request).await?;
    let EnableCdcProjectEnvOutcome {
        triple,
        cdc_name,
        cluster,
        stream,
        secret_name,
        role_sql,
        cdc_sql,
        publication,
        secret,
    } = &outcome;

    println!(
        "cdc for project-env {triple}: publication/slot/role {cdc_name:?} over schema {:?} \
         on cluster {cluster:?}; stream {stream:?}; replication secret {secret_name:?}",
        request.schema,
    );

    emit_text(
        request.emit_role_sql.as_deref(),
        "replication-role SQL (psql the TARGET cluster — roles are cluster-global)",
        role_sql,
    );
    emit_text(
        request.emit_cdc_sql.as_deref(),
        "CDC SQL (psql the PROJECT-ENV database — the slot is database-bound)",
        cdc_sql,
    );
    emit_json(
        request.emit_publication.as_deref(),
        "Publication CR (kubectl apply after the CDC SQL)",
        publication,
    )?;
    emit_json(
        request.emit_secret.as_deref(),
        "replication-credential Secret (kubectl apply)",
        secret,
    )?;

    println!("recorded event-reader registration for {triple} in the registry (wamn_system)");

    Ok(())
}

fn provision_summary(triple: &Triple, database: &str, cluster: &str) -> String {
    format!(
        "project-env {triple}: database {database:?} on cluster {cluster:?} (owner {DB_OWNER_ROLE})"
    )
}

/// Print that a JSON document was written to a path, or print it with a labeled
/// header when the path is absent (`-` also means stdout).
fn emit_json(path: Option<&Path>, label: &str, doc: &Value) -> anyhow::Result<()> {
    emit_text(path, label, &serde_json::to_string_pretty(doc)?);
    Ok(())
}

fn emit_text(path: Option<&Path>, label: &str, text: &str) {
    match path {
        Some(p) if p.as_os_str() != "-" => println!("wrote {} ({label})", p.display()),
        _ => println!("--- {label} ---\n{text}"),
    }
}

/// The named org preset `provision-org` stamps (the `Tier` successor —
/// [`wamn_control_registry::Template`]).
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum TemplateArg {
    /// Pre-contract: placed on the shared `--pool` cluster (owns no clusters;
    /// the RLS floor is load-bearing there); stamps `dev` + `prod`.
    Trials,
    /// Standard paying tier: owns per-recovery-domain clusters; stamps `dev` /
    /// `prod` (own) + `canary` sharing prod's recovery domain (T2).
    Standard,
    /// Regulated tier: like standard, but `canary` owns its recovery domain — a
    /// third cluster (T4).
    Dedicated,
}

impl TemplateArg {
    pub(crate) fn template(self) -> Template {
        match self {
            TemplateArg::Trials => Template::trials(),
            TemplateArg::Standard => Template::standard(),
            TemplateArg::Dedicated => Template::dedicated(),
        }
    }
}

#[derive(Debug, Args)]
pub struct ProvisionOrgArgs {
    /// Org id: a lowercase slug `[a-z0-9-]` (start/end alphanumeric). Names the
    /// derived `<org>-<owner>` clusters; the reserved `wamn` prefix is rejected.
    #[arg(long)]
    pub org: String,

    /// The template preset to stamp: `trials` (pooled, record-only), `standard`
    /// (dedicated, canary shared-with prod), or `dedicated` (canary own).
    #[arg(long, value_enum)]
    pub template: TemplateArg,

    /// The shared pool cluster a `trials` org is placed on. Ignored for
    /// dedicated templates. Default: the shipped `wamn-pg` pool.
    #[arg(long, default_value = "wamn-pg")]
    pub pool: String,

    /// Superuser Postgres URL to the T1 system DB (`wamn_system`), where the org
    /// and its policy rows are recorded and read back (for cluster sizing). Env
    /// `WAMN_SYSTEM_ADMIN_URL`. Omit to render/plan only (with template policies).
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: Option<String>,

    /// The email of an existing user principal who owns the org. The run
    /// writes that user's active org membership and `org-admin` row. It is
    /// required with `--system-database-url` and refused without it, because
    /// a render-only run cannot look the email up or write a row.
    #[arg(long)]
    pub owner_email: Option<String>,

    /// Namespace of the CNPG `Cluster`. Everything that lives beside it (the
    /// `Database`, the `ObjectStore`, the `ScheduledBackup`) shares it.
    #[arg(long, env = "WAMN_CLUSTER_NAMESPACE", default_value = "wamn-system")]
    pub cluster_namespace: String,

    /// Write the rendered `Cluster` CRs (a JSON `List`) here; `-` = stdout
    /// (default). Empty for a pooled org (no owned clusters).
    #[arg(long)]
    pub emit_clusters: Option<PathBuf>,

    /// Write the WAL/PITR `ObjectStore` CRs (a JSON `List`, wamn-e1g) here; `-` =
    /// stdout (default). Apply these **before** the clusters — the Barman plugin
    /// references them.
    #[cfg(feature = "ops")]
    #[arg(long)]
    pub emit_object_store: Option<PathBuf>,

    /// Write the WAL/PITR `ScheduledBackup` CRs (a JSON `List`, wamn-e1g) here;
    /// `-` = stdout (default). Apply these **after** the clusters exist.
    #[cfg(feature = "ops")]
    #[arg(long)]
    pub emit_scheduled_backup: Option<PathBuf>,

    /// The generation actions of the org's own credentials and their Secrets,
    /// derived from the org-scoped families: today `control`, the login of the
    /// org's control host (`wamn-a40n.2`). An action needs
    /// `--system-database-url`.
    #[command(flatten)]
    pub credential: WorkloadGenerationArgs<OrgFamilies>,

    /// Host the emitted credential URL names: the system database's
    /// read-write service as a pod reaches it. A prepare requires it.
    #[arg(long, requires = "system_database_url")]
    pub db_host: Option<String>,

    /// Port the emitted credential URL names.
    #[arg(long, default_value_t = 5432)]
    pub db_port: u16,

    /// Namespace the emitted credential `Secret` is applied to: the namespace
    /// of the org's control host.
    #[arg(long, env = "WAMN_NAMESPACE", default_value = "hosts")]
    pub namespace: String,

    /// Write the empty Secret `wamn-control-administration-<org>` here. Each
    /// `provision-project-env` of the org adds the administration login of
    /// its environment to it with a patch, and the control host mounts it.
    #[arg(long, value_name = "PATH")]
    pub emit_control_administration_secret: Option<PathBuf>,
}

/// Stamp one org, then print what was recorded and write the CRs it owns.
pub async fn provision_org(args: ProvisionOrgArgs) -> anyhow::Result<()> {
    anyhow::ensure!(
        args.credential.action.is_none() || args.system_database_url.is_some(),
        "an org credential action requires --system-database-url"
    );
    anyhow::ensure!(
        args.owner_email.is_none() || args.system_database_url.is_some(),
        "--owner-email needs --system-database-url"
    );
    anyhow::ensure!(
        args.owner_email.is_some() || args.system_database_url.is_none(),
        "--system-database-url needs --owner-email"
    );
    let system_database_url = args.system_database_url.clone();
    let provisioned = provision_org::provision_org(ProvisionOrgRequest {
        org: args.org,
        template: args.template.template(),
        pool: args.pool,
        system_database_url: args.system_database_url,
        cluster_namespace: args.cluster_namespace,
        owner_email: args.owner_email,
    })
    .await?;
    print_provisioned_org(&provisioned);
    if let Some(path) = &args.emit_control_administration_secret {
        let secret = wamn_control_provision::render_control_administration_secret_manifest(
            &provisioned.org.id,
            &args.namespace,
        );
        write_json(path, &secret).context("emit the control administration Secret")?;
    }
    if let Some(action) = args.credential.action {
        let system_database_url = system_database_url
            .context("an org credential action requires --system-database-url")?;
        let secret = args.credential.secret.map(|(_, path)| path);
        let request = OrgWorkloadActionRequest {
            org: provisioned.org.id.clone(),
            system_database_url,
            db_host: args.db_host,
            db_port: args.db_port,
            namespace: args.namespace,
            action,
            secret,
        };
        let outcome = provision_project_env::run_org_workload_action(&request).await?;
        print_org_workload_action(&request, &outcome);
    }
    let Some(set) = &provisioned.clusters else {
        return Ok(());
    };
    let emit_clusters = args.emit_clusters.unwrap_or_else(|| PathBuf::from("-"));
    #[cfg(feature = "ops")]
    let emit_os = args.emit_object_store.unwrap_or_else(|| PathBuf::from("-"));
    #[cfg(feature = "ops")]
    let emit_sb = args
        .emit_scheduled_backup
        .unwrap_or_else(|| PathBuf::from("-"));
    write_json(&emit_clusters, &k8s_list(&set.clusters)).context("emit Cluster CRs")?;
    #[cfg(feature = "ops")]
    write_json(&emit_os, &k8s_list(&set.object_stores)).context("emit ObjectStore CRs")?;
    #[cfg(feature = "ops")]
    write_json(&emit_sb, &k8s_list(&set.scheduled_backups)).context("emit ScheduledBackup CRs")?;
    Ok(())
}

/// Arguments of `recover-org-cluster` — render the CNPG `Cluster` that restores one
/// org recovery domain from its WAL/PITR object store (wamn-fibe).
#[cfg(feature = "ops")]
#[derive(Debug, Args)]
pub struct RecoverOrgClusterArgs {
    /// Org id whose cluster is recovered. With `--owner` it names the SOURCE
    /// cluster `<org>-<owner>`, the one `provision-org` created.
    #[arg(long)]
    pub org: String,

    /// The template preset the org was stamped from — it supplies the policy the
    /// restored cluster is sized by, so use the one `provision-org` was run with.
    #[arg(long, value_enum)]
    pub template: TemplateArg,

    /// The shared pool cluster a `trials` org sits on. Ignored for dedicated
    /// templates. A pooled org owns no cluster and cannot be recovered here.
    #[arg(long, default_value = "wamn-pg")]
    pub pool: String,

    /// The recovery-domain owner env, e.g. `prod`. CNPG recovery is
    /// whole-cluster, so this is the unit that is restored.
    #[arg(long)]
    pub owner: String,

    /// Name of the NEW cluster the restore lands in. Default
    /// `<org>-<owner>-restore`. It must differ from the source: the live cluster
    /// is never the target of a recovery.
    #[arg(long)]
    pub target: Option<String>,

    /// Stop the replay at this RFC3339 timestamp (`recoveryTarget.targetTime`) —
    /// the instant just before the loss. Omit to replay every archived WAL
    /// segment, which is what a lost-cluster restore wants.
    #[arg(long = "at")]
    pub target_time: Option<String>,

    /// Namespace of the CNPG `Cluster`. Everything that lives beside it (the
    /// `Database`, the `ObjectStore`, the `ScheduledBackup`) shares it.
    #[arg(long, env = "WAMN_CLUSTER_NAMESPACE", default_value = "wamn-system")]
    pub cluster_namespace: String,

    /// Write the recovery `Cluster` CR here; `-` = stdout (default).
    #[arg(long)]
    pub emit_cluster: Option<PathBuf>,

    /// Write the RESTORED cluster's own `ObjectStore` CR here; `-` = stdout
    /// (default). Apply it **before** the cluster. It is a different WAL prefix
    /// from the source's, so the restore never writes into the stream it read.
    #[arg(long)]
    pub emit_object_store: Option<PathBuf>,

    /// Write the RESTORED cluster's own `ScheduledBackup` CR here; `-` = stdout
    /// (default). Apply it **after** the restored cluster is ready.
    #[arg(long)]
    pub emit_scheduled_backup: Option<PathBuf>,
}

/// Render one recovery: the `Cluster` bootstrapped from the source's object
/// store, plus the backup resources the restored cluster needs for its own
/// future backups. Pure — this reads no database and applies nothing.
#[cfg(feature = "ops")]
pub fn recover_org_cluster(args: RecoverOrgClusterArgs) -> anyhow::Result<()> {
    let template = args.template.template();
    let (org, _) = template.stamp(&args.org, &args.pool);
    let owner = wamn_control_registry::Env::new(&args.owner);
    let source = format!("{}-{}", org.id, owner);
    let target = args
        .target
        .unwrap_or_else(|| format!("{}-{}-restore", org.id, owner));
    let recovered = wamn_control_provision::render_recovery_cluster(
        &org,
        &owner,
        &template.policies,
        &target,
        &args.cluster_namespace,
        args.target_time.as_deref(),
    )?;

    match &args.target_time {
        Some(at) => println!(
            "recovery of cluster {source:?} into {target:?}, replayed to {at} \
             (whole-cluster point-in-time recovery)"
        ),
        None => println!(
            "recovery of cluster {source:?} into {target:?}, replaying every archived WAL segment"
        ),
    }
    println!(
        "  apply order: the ObjectStore, then the Cluster, then the ScheduledBackup once the \
         restored cluster is ready"
    );
    if recovered.object_store.is_none() {
        println!(
            "  note: env {owner:?} has no backup cadence in this template, so it has no WAL \
             stream to restore from and the restore renders no backup resources"
        );
    }

    let emit_cluster = args.emit_cluster.unwrap_or_else(|| PathBuf::from("-"));
    let emit_os = args.emit_object_store.unwrap_or_else(|| PathBuf::from("-"));
    let emit_sb = args
        .emit_scheduled_backup
        .unwrap_or_else(|| PathBuf::from("-"));
    if let Some(store) = &recovered.object_store {
        write_json(&emit_os, store).context("emit the restored cluster's ObjectStore CR")?;
    }
    write_json(&emit_cluster, &recovered.cluster).context("emit the recovery Cluster CR")?;
    if let Some(sb) = &recovered.scheduled_backup {
        write_json(&emit_sb, sb).context("emit the restored cluster's ScheduledBackup CR")?;
    }
    Ok(())
}

/// Print the lines that report one org provisioning run.
fn print_provisioned_org(provisioned: &ProvisionedOrg) {
    let id = &provisioned.org.id;
    let tpl = provisioned.template_name;
    match provisioned.stamped_policies {
        Some(n) => println!(
            "recorded org {id:?} (template {tpl:?}) in registry.orgs + {n} env \
             policy row(s) stamped insert-if-absent (wamn_system)"
        ),
        None => println!("(no --system-database-url: org not recorded; template policies used)"),
    }
    if let Some(set) = &provisioned.clusters {
        let names: Vec<String> = set
            .clusters
            .iter()
            .map(|c| c["metadata"]["name"].as_str().unwrap_or("?").to_string())
            .collect();
        println!(
            "org {id:?} (template {tpl:?}, dedicated): {n} cluster(s) [{names}], \
             sized by the org's env policies",
            n = set.clusters.len(),
            names = names.join(", "),
        );
        #[cfg(feature = "ops")]
        if !set.object_stores.is_empty() {
            println!(
                "  WAL/PITR: {} backed cluster(s); apply the ObjectStore(s) before the clusters, the ScheduledBackup(s) after",
                set.object_stores.len(),
            );
        }
    } else if let wamn_control_registry::Placement::Pooled { pool } = &provisioned.org.placement {
        println!(
            "org {id:?} (template {tpl:?}, pooled): placed on the shared pool {pool:?} \
             (owns no clusters)"
        );
    }
}

/// Wrap CRs in a Kubernetes `v1` `List` so `kubectl apply -f` accepts the whole
/// set from one file/stream. An empty items list is a valid, harmless no-op apply.
fn k8s_list(items: &[Value]) -> Value {
    serde_json::json!({
        "apiVersion": "v1",
        "kind": "List",
        "items": items,
    })
}

fn write_json(path: &PathBuf, doc: &Value) -> anyhow::Result<()> {
    let text = serde_json::to_string_pretty(doc)?;
    if path.as_os_str() == "-" {
        println!("{text}");
    } else {
        std::fs::write(path, text).with_context(|| format!("write {}", path.display()))?;
        println!("wrote {}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
