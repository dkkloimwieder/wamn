//! MVP one-shot control-plane verbs.

use std::str::FromStr as _;

mod platform_verbs;
mod print_platform_principals;
mod project_verbs;

use clap::{Parser, Subcommand};
use wamn_ctl::{
    component_verbs, delivery_verbs, env_verbs, identity_verbs, package_verbs, provisioning_verbs,
    release_verbs, role_verbs, workflow_verbs,
};

#[derive(Parser)]
#[command(name = "wamn-ctl", version, about)]
struct Cli {
    /// Log level (the chart passes this before the subcommand)
    #[arg(long = "log-level", global = true, default_value = "info")]
    log_level: String,

    #[command(subcommand)]
    command: Command,
}

/// The verbs the shipped `wamn-ctl` binary carries.
///
/// DO NOT gate a variant here on the `ops` feature. The `build-ctl` stage in
/// `Dockerfile` builds this binary with NO features (`cargo build --locked
/// --release -p wamn-ctl`) and passes `--features ops` only to the separate
/// `wamn-ctl-ops` binary on the next line. A variant added here behind
/// `#[cfg(feature = "ops")]` compiles locally and passes tests, and is then
/// absent from the deployed image. Put an ops-only verb on `wamn-ctl-ops`
/// instead.
#[derive(Subcommand)]
enum Command {
    /// Build and push the platform images and stamp them into the release
    /// chart's defaults.
    BuildPlatform(delivery_verbs::BuildPlatformArgs),
    /// Qualify one published release, by its digest, on the release chart's
    /// platform image set, and push the qualification beside the release.
    QualifyRelease(delivery_verbs::QualifyReleaseArgs),
    /// Take one installed environment to the bytes of one commit.
    UpgradeEnvironment(delivery_verbs::UpgradeEnvironmentArgs),
    /// Install the control store into an empty system database and record its platform domain
    ProvisionSystem(provisioning_verbs::ProvisionSystemArgs),
    /// Render a dedicated org's CNPG Cluster set (one per recovery domain, sized by env policy) + record it in the T1 registry (wamn-q3n.6 / D18)
    ProvisionOrg(provisioning_verbs::ProvisionOrgArgs),
    /// Render a per-project-env database (CNPG Database CRD) + privilege step + record it in the T1 registry (wamn-q3n.7)
    ProvisionProjectEnv(provisioning_verbs::ProvisionProjectEnvArgs),
    /// Provision the scoped database credential for the identity service.
    ProvisionIdentityIssuer(identity_verbs::IdentityIssuerArgs),
    /// Grant one user access to one project environment.
    GrantProjectEnvMembership(identity_verbs::ProjectEnvMembershipArgs),
    /// Revoke one user's access to one project environment.
    RevokeProjectEnvMembership(identity_verbs::ProjectEnvMembershipArgs),
    /// Ask identity to mail an invitation to one user principal.
    Invite(identity_verbs::InviteArgs),
    /// Give a user or a service one existing role in one environment.
    GrantRole(role_verbs::UserRoleArgs),
    /// Take one role from a user or a service in one environment.
    RevokeRole(role_verbs::UserRoleArgs),
    /// Create an empty authored role in one environment.
    CreateRole(role_verbs::RoleArgs),
    /// Delete an authored role with its assignments and permissions.
    DeleteRole(role_verbs::RoleArgs),
    /// Grant an operation to an authored role.
    GrantPermission(role_verbs::PermissionArgs),
    /// Remove the selection of an operation from an authored role.
    RevokePermission(role_verbs::PermissionArgs),
    /// Overlay CDC capture onto a provisioned project-env: publication + failover slot + replication role/Secret + reader registration (wamn-l5i9.9, D19 v3)
    EnableCdcProjectEnv(provisioning_verbs::EnableCdcProjectEnvArgs),
    /// Drop a lost CDC slot, create it again under the same name, and record the capture gap (wamn-59z6)
    RecoverCaptureGap(provisioning_verbs::RecoverCaptureGapArgs),
    /// Set resync_at on the newest capture gap of a CDC reader, which lets it resume (wamn-59z6)
    CloseCaptureGap(provisioning_verbs::CloseCaptureGapArgs),
    /// Apply the pending platform schema migrations to one installed database (wamn-o8b9)
    UpgradeSchema(provisioning_verbs::UpgradeSchemaArgs),
    /// Apply one exact package-owned migration stream to a project database.
    ApplyPackage(package_verbs::ApplyPackageArgs),
    /// Push one authored package as a registry artifact and record its digest (wamn-zua8.3)
    PushPackage(package_verbs::PushPackageArgs),
    /// Prove a package successor on a copy of the serving predecessor database.
    QualifyUpgrade(package_verbs::QualifyUpgradeArgs),
    /// Reconcile generated package data privileges after apply-package.
    ReconcilePackageDataAccess(package_verbs::ReconcilePackageDataAccessArgs),
    /// Submit one authored wiring document as an immutable gated wiring version (wamn-1xb5)
    /// Bind one admitted component's declared connection alias to an
    /// environment-owned instance carrying a host-held credential handle.
    BindConnection(component_verbs::BindConnectionArgs),
    /// Manage org connection declarations.
    Connection(component_verbs::ConnectionArgs),
    /// Publish one immutable release from exact package-owned facts and push
    /// its manifest to the release repository.
    ///
    /// PRECONDITION: run `env apply` for this environment FIRST; it reconciles the run plane `wamn_run`. This verb reads the tenant's `environment_policies` row before it commits and refuses when the row is absent (`environment-policy-not-converged`) as well as when it names another environment than the release carries (`environment-policy-environment-mismatch`), so publishing into a never-reconciled run plane fails rather than passing unchecked.
    PublishRelease(release_verbs::PublishReleaseArgs),
    /// Print the SQL that creates the platform principal rows of one tenant.
    PrintPlatformPrincipals(print_platform_principals::PrintPlatformPrincipalsArgs),
    /// Detect or repair REPLICA IDENTITY drift on captured models.
    ReconcileReplicaIdentity(package_verbs::ReconcileReplicaIdentityArgs),
    /// Terminalize one effect-uncertain run from explicit external evidence.
    TerminalizeEffectUncertain(release_verbs::TerminalizeEffectUncertainArgs),
    /// Author, enable, disable and list workflows, and start, park, release and list runs.
    #[command(subcommand)]
    Workflow(workflow_verbs::WorkflowCommand),
    /// Apply, show and dry-run one environment document.
    #[command(subcommand)]
    Env(env_verbs::EnvCommand),
    /// Read a cluster document.
    #[command(subcommand)]
    Platform(platform_verbs::PlatformCommand),
    /// Render a project document for OpenTofu.
    #[command(subcommand)]
    Project(project_verbs::ProjectCommand),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let level = tracing::Level::from_str(&cli.log_level)
        .map_err(|_| anyhow::anyhow!("invalid log level: {}", cli.log_level))?;
    // Same shape as the pre-split no-OTEL path of
    // wash_runtime::observability::initialize_observability: stderr fmt layer,
    // RUST_LOG overriding --log-level. The verbs report via stdout; this
    // carries dep diagnostics only.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level.as_str()));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    match cli.command {
        Command::BuildPlatform(args) => delivery_verbs::build_platform(args).await,
        Command::QualifyRelease(args) => delivery_verbs::qualify(args).await,
        Command::UpgradeEnvironment(args) => delivery_verbs::upgrade_environment(args).await,
        Command::ProvisionSystem(args) => provisioning_verbs::provision_system(args).await,
        Command::ProvisionOrg(args) => provisioning_verbs::provision_org(args).await,
        Command::ProvisionProjectEnv(args) => provisioning_verbs::provision(args).await,
        Command::ProvisionIdentityIssuer(args) => identity_verbs::provision_issuer(args).await,
        Command::GrantProjectEnvMembership(args) => identity_verbs::grant(args).await,
        Command::RevokeProjectEnvMembership(args) => identity_verbs::revoke(args).await,
        Command::Invite(args) => identity_verbs::invite(args).await,
        Command::GrantRole(args) => role_verbs::grant(args).await,
        Command::RevokeRole(args) => role_verbs::revoke(args).await,
        Command::CreateRole(args) => role_verbs::create_role(args).await,
        Command::DeleteRole(args) => role_verbs::delete_role(args).await,
        Command::GrantPermission(args) => role_verbs::grant_permission(args).await,
        Command::RevokePermission(args) => role_verbs::revoke_permission(args).await,
        Command::EnableCdcProjectEnv(args) => provisioning_verbs::enable_cdc(args).await,
        Command::RecoverCaptureGap(args) => provisioning_verbs::recover_capture_gap(args).await,
        Command::CloseCaptureGap(args) => provisioning_verbs::close_capture_gap(args).await,
        Command::UpgradeSchema(args) => provisioning_verbs::upgrade_schema(args).await,
        Command::ApplyPackage(args) => package_verbs::apply(args).await,
        Command::PushPackage(args) => package_verbs::push(args).await,
        Command::QualifyUpgrade(args) => package_verbs::qualify_upgrade(args).await,
        Command::BindConnection(args) => component_verbs::bind(args).await,
        Command::Connection(args) => component_verbs::connection(args).await,
        Command::ReconcilePackageDataAccess(args) => {
            package_verbs::reconcile_data_access(args).await
        }
        Command::PublishRelease(args) => release_verbs::publish(args).await,

        Command::PrintPlatformPrincipals(args) => print_platform_principals::run(&args),
        Command::ReconcileReplicaIdentity(args) => {
            package_verbs::reconcile_replica_identity(args).await
        }
        Command::TerminalizeEffectUncertain(args) => release_verbs::terminalize(args).await,
        Command::Workflow(command) => workflow_verbs::run(command).await,
        Command::Env(command) => Box::pin(env_verbs::run(command)).await,
        Command::Platform(command) => platform_verbs::run(command, std::io::stdout().lock()).await,
        Command::Project(command) => project_verbs::run(command),
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::{Cli, Command};

    #[test]
    fn connection_declare_parses_without_a_secret_argument() {
        let args = [
            "wamn-ctl",
            "connection",
            "declare",
            "--org",
            "acme",
            "--name",
            "erp",
            "--requirement-type",
            "http",
            "--definition",
            "/tmp/erp.json",
            "--system-database-url",
            "postgres://localhost/control",
        ];
        let parsed = Cli::try_parse_from(args).unwrap();
        let Command::Connection(connection) = parsed.command else {
            panic!("connection verb")
        };
        let wamn_ctl::component_verbs::ConnectionCommand::Declare(declare) = connection.command;
        assert_eq!(declare.org, "acme");
        assert_eq!(declare.name, "erp");
        assert_eq!(
            declare.requirement_type,
            wamn_ctl::component_verbs::RequirementTypeArg::Http
        );
        assert_eq!(declare.definition, std::path::Path::new("/tmp/erp.json"));
        assert!(Cli::try_parse_from(args.into_iter().chain(["--secret", "forbidden"])).is_err());
    }

    #[test]
    fn project_environment_membership_verbs_parse_exact_scope() {
        for verb in [
            "grant-project-env-membership",
            "revoke-project-env-membership",
        ] {
            let cli = Cli::try_parse_from([
                "wamn-ctl",
                verb,
                "--org",
                "acme",
                "--project",
                "billing",
                "--env",
                "dev",
                "--principal-id",
                "00112233-4455-6677-8899-aabbccddeeff",
                "--system-database-url",
                "postgres://admin:secret@localhost/wamn_system",
            ])
            .unwrap();
            let args = match cli.command {
                Command::GrantProjectEnvMembership(args) => {
                    assert_eq!(verb, "grant-project-env-membership");
                    args
                }
                Command::RevokeProjectEnvMembership(args) => {
                    assert_eq!(verb, "revoke-project-env-membership");
                    args
                }
                _ => panic!("expected a project environment membership command"),
            };
            assert_eq!(args.org, "acme");
            assert_eq!(args.project, "billing");
            assert_eq!(args.env, "dev");
            assert_eq!(args.principal_id, "00112233-4455-6677-8899-aabbccddeeff");
        }
    }

    const PREDECESSOR: &str =
        "sha256:1111111111111111111111111111111111111111111111111111111111111111";

    /// `qualify-upgrade` needs `--predecessor-release` and passes it on.
    #[test]
    fn qualify_upgrade_requires_the_predecessor_release() {
        let mut values = vec![
            "wamn-ctl",
            "qualify-upgrade",
            "--package",
            "orders-2.0.0",
            "--presented-package",
            "orders-2.0.0",
            "--artifact-base",
            "registry.example/wamn/packages",
            "--registry-auth-file",
            "/run/registry/config.json",
            "--database-url",
            "postgres://project.invalid/project",
            "--org",
            "acme",
            "--project",
            "plant",
            "--environment",
            "dev",
            "--result",
            "/tmp/qualification.json",
        ];
        let Err(missing) = Cli::try_parse_from(values.clone()) else {
            panic!("qualify-upgrade without a predecessor release refuses");
        };
        assert_eq!(
            missing.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
        assert!(missing.to_string().contains("--predecessor-release"));
        values.extend(["--predecessor-release", PREDECESSOR]);
        let Command::QualifyUpgrade(args) = Cli::try_parse_from(values).unwrap().command else {
            panic!("expected qualify-upgrade");
        };
        assert_eq!(args.predecessor_release, PREDECESSOR);
    }

    /// `upgrade-environment` takes `--predecessor-release` as an optional
    /// digest; the package stage refuses an installed upgrade without it.
    #[test]
    fn upgrade_environment_takes_an_optional_predecessor_release() {
        let commit = "a".repeat(40);
        let values = [
            "wamn-ctl",
            "upgrade-environment",
            "--org",
            "acme",
            "--project",
            "plant",
            "--env",
            "dev",
            "--commit",
            commit.as_str(),
        ];
        let Command::UpgradeEnvironment(args) = Cli::try_parse_from(values).unwrap().command else {
            panic!("expected upgrade-environment");
        };
        assert_eq!(args.predecessor_release, None);
        let with = values
            .into_iter()
            .chain(["--predecessor-release", PREDECESSOR]);
        let Command::UpgradeEnvironment(args) = Cli::try_parse_from(with).unwrap().command else {
            panic!("expected upgrade-environment");
        };
        assert_eq!(args.predecessor_release.as_deref(), Some(PREDECESSOR));
    }

    #[test]
    fn workflow_verbs_parse_their_scope_after_the_verb() {
        let cli = Cli::try_parse_from([
            "wamn-ctl",
            "workflow",
            "park",
            "--admin-database-url",
            "postgres://admin:secret@localhost/project",
            "--org",
            "acme",
            "--project",
            "plant",
            "--environment",
            "dev",
            "--run-id",
            "run-1",
        ])
        .unwrap();
        let Command::Workflow(wamn_ctl::workflow_verbs::WorkflowCommand::Park(args)) = cli.command
        else {
            panic!("expected workflow park");
        };
        assert_eq!(args.scope.org, "acme");
        assert_eq!(args.scope.project, "plant");
        assert_eq!(args.scope.scope.environment, "dev");
        assert_eq!(args.scope.scope.schema, "wamn_run");
        assert_eq!(args.run_id, "run-1");
    }

    #[test]
    fn workflow_enable_names_one_version_under_a_coordinate() {
        let cli = Cli::try_parse_from([
            "wamn-ctl",
            "workflow",
            "enable",
            "--admin-database-url",
            "postgres://admin:secret@localhost/project",
            "--environment",
            "dev",
            "--coordinate",
            "acme/wms/dev",
            "--application",
            "wamn_wms",
            "--workflow-id",
            "label",
            "--version",
            "2",
            "--principal-id",
            "42b638ce-4892-52a4-95a9-316c4c0e820d",
        ])
        .unwrap();
        let Command::Workflow(wamn_ctl::workflow_verbs::WorkflowCommand::Enable(args)) =
            cli.command
        else {
            panic!("expected workflow enable");
        };
        assert_eq!(args.version.coordinate, "acme/wms/dev");
        assert_eq!(args.version.workflow_id, "label");
        assert_eq!(args.version.version, 2);
    }
}
