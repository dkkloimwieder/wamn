//! Arguments and output of the `provision-project-env` and `enable-cdc-project-env` verbs.

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use clap::Args;
use serde_json::Value;
use wamn_control::enable_cdc_project_env::{
    self, EnableCdcProjectEnvOutcome, EnableCdcProjectEnvRequest,
};
use wamn_control::pat_client::PatIssuerConfig;
use wamn_control::provision_project_env::{
    self, ProvisionProjectEnvOutcome, ProvisionProjectEnvRequest, WorkloadActionOutcome,
    WorkloadActionRequest, WorkloadActionVerb, WorkloadGenerationAction, ensure_secret_path,
    parse_pat_prefix, workload_action_flag, workload_secret_flag,
};
use wamn_control_provision::{CredentialGeneration, DB_OWNER_ROLE, WorkloadRoleFamily};
use wamn_control_registry::Triple;

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

fn workload_action_id(family: WorkloadRoleFamily, verb: WorkloadActionVerb) -> String {
    workload_action_flag(family, verb).replace('-', "_")
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
    /// **Deliberately has no `default_value`** — the same shape
    /// `--dispatch-reader-password` takes in `provision-project-env`
    /// (wamn-0h0g.12.122). A default here minted a `LOGIN REPLICATION` role
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

    /// Write the CDC SQL (schema guard + publication + failover slot + grants;
    /// psql the PROJECT-ENV database) here; `-` = stdout.
    #[arg(long)]
    pub emit_cdc_sql: Option<PathBuf>,

    /// Write the replication-credential `Secret` (JSON) here; `-` = stdout.
    #[arg(long)]
    pub emit_secret: Option<PathBuf>,
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

    let request = provisioning_request(args)?;
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
            && args.app_host.is_none()
            && args.emit_database.is_none()
            && args.emit_privilege_sql.is_none()
            && args.emit_secret.is_none()
            && args.emit_management_author_pat_secret.is_none()
            && args.emit_route_caller_pat_secret.is_none(),
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
        namespace: args.namespace,
        action,
        secret,
        emit_role_sql: args.emit_role_sql,
    })
}

fn provisioning_request(
    args: ProvisionProjectEnvArgs,
) -> anyhow::Result<ProvisionProjectEnvRequest> {
    let emit_secret = args
        .emit_secret
        .context("--emit-secret PATH is required and must not be '-'")?;
    Ok(ProvisionProjectEnvRequest {
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
        // `--app-password` is `required_unless_present_any` over the modes that
        // provision nothing, and every one of those has already returned above. A
        // missing credential here is a broken parser contract, not a user error:
        // re-checking it would plant a second, weaker enforcement point and hollow
        // out the parse-time refusal (wamn-0h0g.12.141).
        app_password: args
            .app_password
            .expect("clap requires --app-password on every provisioning invocation"),
        app_host: args.app_host,
        app_port: args.app_port,
        namespace: args.namespace,
        secret_namespace: args.secret_namespace,
        emit_database: args.emit_database,
        emit_role_sql: args.emit_role_sql,
        emit_privilege_sql: args.emit_privilege_sql,
        emit_secret,
        pat_issuer: args.pat_issuer.into(),
        emit_management_author_pat_secret: args.emit_management_author_pat_secret,
        emit_route_caller_pat_secret: args.emit_route_caller_pat_secret,
    })
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
    println!(
        "wrote {} (database credential Secret; kubectl apply)",
        request.emit_secret.display()
    );

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
        "CDC SQL (psql the PROJECT-ENV database — publication + slot are database-bound)",
        cdc_sql,
    );
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

#[cfg(test)]
mod tests;
