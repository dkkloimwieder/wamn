//! MVP one-shot control-plane verbs.

#[cfg(test)]
#[path = "../../cli_help_tests.rs"]
mod help_env_tests;

use std::str::FromStr as _;

use wamn_ctl::platform_verbs;
mod print_platform_principals;
mod project_verbs;

use clap::{Parser, Subcommand};
use wamn_ctl::{
    component_verbs, env_verbs, identity_verbs, package_verbs, provisioning_verbs, release_verbs,
    role_verbs, workflow_verbs,
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
/// DO NOT condition a variant here on the `ops` feature. The `build-ctl` stage in
/// `Dockerfile` builds this binary with NO features (`cargo build --locked
/// --release -p wamn-ctl`) and passes `--features ops` only to the separate
/// `wamn-ctl-ops` binary on the next line. A variant added here behind
/// `#[cfg(feature = "ops")]` compiles locally and passes tests, and is then
/// absent from the deployed image. Put an ops-only verb on `wamn-ctl-ops`
/// instead.
#[derive(Subcommand)]
enum Command {
    /// Execute one server-authorized application operation in a worker Job.
    #[command(hide = true)]
    Operation(OperationArgs),
    /// Install the control store into an empty system database and record its platform domain
    ProvisionSystem(provisioning_verbs::ProvisionSystemArgs),
    /// Inspect or apply the system SQL embedded in this image.
    #[command(subcommand)]
    SystemSchema(platform_verbs::SystemSchemaCommand),
    /// Render a dedicated org's CNPG Cluster set (one per recovery domain, sized by env policy) + record it in the T1 registry (wamn-q3n.6 / D18)
    ProvisionOrg(provisioning_verbs::ProvisionOrgArgs),
    /// Render a per-project-env database (CNPG Database CRD) + privilege step + record it in the T1 registry (wamn-q3n.7)
    ProvisionProjectEnv(provisioning_verbs::ProvisionProjectEnvArgs),
    /// Provision the targeted database credential for the identity service.
    ProvisionIdentityIssuer(identity_verbs::IdentityIssuerArgs),
    /// Create an API service principal, authored grants, and its PAT.
    CreateApiService(identity_verbs::ApiServiceArgs),
    /// Grant one user access to one project environment.
    GrantProjectEnvMembership(identity_verbs::ProjectEnvMembershipArgs),
    /// Revoke one user's access to one project environment.
    RevokeProjectEnvMembership(identity_verbs::ProjectEnvMembershipArgs),
    /// Provision the first org admin through SQL as the Provisioning platform principal.
    ///
    /// Refuse an org that already has an active org admin. Use user.invite afterward.
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
    /// Apply one exact package-owned migration stream to a project database.
    ApplyPackage(package_verbs::ApplyPackageArgs),
    /// Push one authored package as a registry artifact and record its digest (wamn-zua8.3)
    PushPackage(package_verbs::PushPackageArgs),
    /// Manage environment connections through control routes.
    Connection(component_verbs::ConnectionArgs),
    /// Manage project external systems through control routes.
    ExternalSystem(component_verbs::ExternalSystemArgs),
    /// Read published integration types through a control route.
    IntegrationType(component_verbs::IntegrationTypeArgs),
    /// Build, publish, and administer project integrations.
    Integration(wamn_ctl::integration_verbs::IntegrationArgs),
    /// Publish one application's immutable release from verified registry artifacts.
    /// No destination database or provisioned environment is required.
    PublishRelease(release_verbs::PublishReleaseArgs),
    /// Print the SQL that creates the platform principal rows of one project.
    PrintPlatformPrincipals(print_platform_principals::PrintPlatformPrincipalsArgs),
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

#[derive(clap::Args)]
struct OperationArgs {
    #[arg(value_parser = ["deploy", "rollback", "diff", "apply", "reset"])]
    operation: String,
    #[arg(long)]
    org: String,
    #[arg(long)]
    project: String,
    #[arg(long)]
    env: String,
    #[arg(long, required_if_eq_any = [("operation", "diff"), ("operation", "apply")])]
    application: Option<String>,
    #[arg(long)]
    release: Option<String>,
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
        .log_internal_errors(false)
        .init();

    match cli.command {
        Command::Operation(args) => {
            let result = wamn_environment_control::environment::operation::run(
                wamn_environment_control::environment::operation::Request {
                    operation: &args.operation,
                    org: &args.org,
                    project: &args.project,
                    env: &args.env,
                    application: args.application.as_deref(),
                    release: args.release.as_deref(),
                },
            )
            .await?;
            println!("{}", serde_json::to_string(&result)?);
            Ok(())
        }
        Command::ProvisionSystem(args) => provisioning_verbs::provision_system(args).await,
        Command::SystemSchema(command) => {
            platform_verbs::run_system_schema(command, std::io::stdout().lock()).await
        }
        Command::ProvisionOrg(args) => provisioning_verbs::provision_org(args).await,
        Command::ProvisionProjectEnv(args) => provisioning_verbs::provision(args).await,
        Command::ProvisionIdentityIssuer(args) => identity_verbs::provision_issuer(args).await,
        Command::CreateApiService(args) => identity_verbs::create_api_service(args).await,
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
        Command::ApplyPackage(args) => package_verbs::apply(args).await,
        Command::PushPackage(args) => package_verbs::push(args).await,
        Command::Connection(args) => component_verbs::connection(args).await,
        Command::ExternalSystem(args) => component_verbs::external_system(args).await,
        Command::IntegrationType(args) => component_verbs::integration_type(args).await,
        Command::Integration(args) => wamn_ctl::integration_verbs::run(args).await,
        Command::PublishRelease(args) => release_verbs::publish(args).await,

        Command::PrintPlatformPrincipals(args) => print_platform_principals::run(&args),
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
    fn system_schema_is_available_in_the_shipped_ctl() {
        for action in ["diff", "apply"] {
            let cli = Cli::try_parse_from([
                "wamn-ctl",
                "system-schema",
                action,
                "--system-url",
                "postgres://admin/system",
                "--image",
                "registry/ctl@sha256:abc",
            ])
            .unwrap();
            assert!(matches!(cli.command, Command::SystemSchema(_)));
        }
    }

    #[test]
    fn platform_rejects_removed_apply_and_product_deploy() {
        for verb in ["apply", "deploy"] {
            let error = Cli::try_parse_from(["wamn-ctl", "platform", verb, "cluster.k"])
                .err()
                .expect("reject product deployment in the low-level tool");
            assert_eq!(error.kind(), clap::error::ErrorKind::InvalidSubcommand);
        }
    }

    #[test]
    fn connection_write_calls_a_host_with_a_separate_credential_file() {
        let args = [
            "wamn-ctl",
            "connection",
            "write",
            "--file",
            "/tmp/request.json",
            "--host-url",
            "https://control.example",
            "--token",
            "session",
            "--credential-file",
            "/tmp/private",
        ];
        let Command::Connection(connection) = Cli::try_parse_from(args).unwrap().command else {
            panic!("connection verb")
        };
        let wamn_ctl::component_verbs::ConnectionCommand::Write(write) = connection.command else {
            panic!("write verb")
        };
        assert_eq!(
            write.credential_file.unwrap(),
            std::path::Path::new("/tmp/private")
        );
        assert!(
            Cli::try_parse_from(
                args.into_iter()
                    .chain(["--system-database-url", "postgres://invalid"])
            )
            .is_err()
        );
    }

    #[test]
    fn project_environment_membership_verbs_parse_exact_target() {
        for verb in [
            "grant-project-env-membership",
            "revoke-project-env-membership",
        ] {
            let cli = Cli::try_parse_from([
                "wamn-ctl",
                verb,
                "--host-url",
                "https://control.example",
                "--project",
                "billing",
                "--env",
                "dev",
                "--principal-id",
                "00112233-4455-6677-8899-aabbccddeeff",
                "--token",
                "session",
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
            assert_eq!(args.host_url, "https://control.example");
            assert_eq!(args.token, "session");
            assert_eq!(args.project, "billing");
            assert_eq!(args.env, "dev");
            assert_eq!(args.principal_id, "00112233-4455-6677-8899-aabbccddeeff");
        }
    }

    #[test]
    fn workflow_verbs_parse_their_target_after_the_verb() {
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
        assert_eq!(args.target.org, "acme");
        assert_eq!(args.target.project, "plant");
        assert_eq!(args.target.target.environment, "dev");
        assert_eq!(args.target.target.schema, "wamn_run");
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
