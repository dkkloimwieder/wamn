//! Product command-line client for the local development loop.

#[cfg(all(test, target_os = "linux"))]
#[path = "../../../cli_help_tests.rs"]
mod help_env_tests;

#[cfg(target_os = "linux")]
use anyhow::Context as _;
#[cfg(target_os = "linux")]
use clap::{Parser, Subcommand};

#[cfg(target_os = "linux")]
#[derive(Debug, Parser)]
#[command(name = "wamn", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Subcommand)]
enum Command {
    #[command(subcommand)]
    Env(wamn_cli::env_requests::Command),
    RevokePat(wamn_cli::env_requests::RevokePat),
    RecoverCaptureGap(wamn_cli::provisioning_verbs::RecoverCaptureGapArgs),
    CloseCaptureGap(wamn_cli::provisioning_verbs::CloseCaptureGapArgs),
    GrantProjectEnvMembership(wamn_cli::identity_verbs::ProjectEnvMembershipArgs),
    RevokeProjectEnvMembership(wamn_cli::identity_verbs::ProjectEnvMembershipArgs),
    GrantRole(wamn_cli::role_verbs::UserRoleArgs),
    RevokeRole(wamn_cli::role_verbs::UserRoleArgs),
    CreateRole(wamn_cli::role_verbs::RoleArgs),
    DeactivateRole(wamn_cli::role_verbs::RoleArgs),
    ActivateRole(wamn_cli::role_verbs::RoleArgs),
    GrantPermission(wamn_cli::role_verbs::PermissionArgs),
    RevokePermission(wamn_cli::role_verbs::PermissionArgs),
    /// Create an API service and save its private PAT.
    CreateApiService(wamn_cli::identity_verbs::ApiServiceArgs),
    /// Manage connections through control routes.
    Connection(wamn_cli::component_verbs::ConnectionArgs),
    /// Manage project external systems.
    ExternalSystem(wamn_cli::component_verbs::ExternalSystemArgs),
    /// Read published integration types.
    IntegrationType(wamn_cli::component_verbs::IntegrationTypeArgs),
    /// Manage workflows through authenticated routes.
    #[command(subcommand)]
    Workflow(wamn_cli::workflow_verbs::WorkflowCommand),
    /// Build source or select published bytes, then deploy one application release.
    Deploy(wamn_cli::app_verbs::DeployArgs),
    /// Restore an earlier application's content revision without reversing SQL.
    Rollback(wamn_cli::app_verbs::RollbackArgs),
    /// Show authored selections and observed application state.
    Status(wamn_cli::app_verbs::StatusArgs),
    /// Inspect or apply application SQL independently of serving code.
    #[command(subcommand)]
    Schema(wamn_cli::schema_verbs::SchemaCommand),
    /// Inspect or apply platform SQL.
    #[command(subcommand)]
    Platform(wamn_cli::platform_verbs::ProductPlatformCommand),
    /// Manage organizations.
    #[command(subcommand)]
    Org(OrgCommand),
    /// Build packages: generate every output from wamn.k into target/wamn.
    Build(wamn_cli::build_verbs::BuildArgs),
    /// Publish existing source build output without rebuilding.
    Publish(wamn_cli::dev::command::PublishArgs),
    /// Copy a generated default into application source.
    Eject(wamn_cli::build_verbs::EjectArgs),
    /// Build wasm connector artifacts or manage project integrations.
    Integration(wamn_cli::integration_verbs::IntegrationArgs),
    /// Watch source and deploy through the development platform.
    Dev(wamn_cli::dev::command::DevCommandArgs),
    /// Build, push, or upload a web client.
    Web(wamn_cli::web::WebArgs),
    /// Print layer 0, the canonical wamn.json, of a pushed package artifact.
    Inspect(wamn_cli::artifact_verbs::InspectArgs),
    /// Print where layer 0 of two pushed package artifacts differs.
    Diff(wamn_cli::artifact_verbs::DiffArgs),
}

#[cfg(target_os = "linux")]
#[derive(Debug, Subcommand)]
enum OrgCommand {
    /// Install an org and invite its first owner using the active kubeconfig.
    Create {
        org: wamn_registry::OrgId,
        #[arg(long)]
        owner: String,
    },
}

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // As wash_runtime::init_crypto does for the host. An earlier install
    // leaves its provider in place.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    if std::env::var_os("RUST_LOG").is_some() {
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .context("read the development diagnostics filter")?;
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .log_internal_errors(false)
            .init();
    }
    match Cli::parse().command {
        Command::Env(command) => wamn_cli::env_requests::run(command).await,
        Command::RevokePat(args) => wamn_cli::env_requests::revoke_pat(args).await,
        Command::RecoverCaptureGap(args) => {
            wamn_cli::provisioning_verbs::recover_capture_gap(args).await
        }
        Command::CloseCaptureGap(args) => {
            wamn_cli::provisioning_verbs::close_capture_gap(args).await
        }

        Command::GrantProjectEnvMembership(args) => wamn_cli::identity_verbs::grant(args).await,
        Command::RevokeProjectEnvMembership(args) => wamn_cli::identity_verbs::revoke(args).await,
        Command::GrantRole(args) => wamn_cli::role_verbs::grant(args).await,
        Command::RevokeRole(args) => wamn_cli::role_verbs::revoke(args).await,
        Command::CreateRole(args) => wamn_cli::role_verbs::create_role(args).await,
        Command::DeactivateRole(args) => wamn_cli::role_verbs::deactivate_role(args).await,
        Command::ActivateRole(args) => wamn_cli::role_verbs::activate_role(args).await,
        Command::GrantPermission(args) => wamn_cli::role_verbs::grant_permission(args).await,
        Command::RevokePermission(args) => wamn_cli::role_verbs::revoke_permission(args).await,

        Command::CreateApiService(args) => wamn_cli::identity_verbs::create_api_service(args).await,
        Command::Connection(args) => wamn_cli::component_verbs::connection(args).await,
        Command::ExternalSystem(args) => wamn_cli::component_verbs::external_system(args).await,
        Command::IntegrationType(args) => wamn_cli::component_verbs::integration_type(args).await,
        Command::Workflow(command) => wamn_cli::workflow_verbs::run(command).await,
        Command::Deploy(args) => wamn_cli::app_verbs::deploy(args).await,
        Command::Rollback(args) => wamn_cli::app_verbs::rollback(args).await,
        Command::Status(args) => wamn_cli::app_verbs::status(args).await,
        Command::Schema(command) => wamn_cli::schema_verbs::run(command).await,
        Command::Platform(command) => wamn_cli::platform_verbs::run_product(command).await,
        Command::Org(OrgCommand::Create { org, owner }) => {
            wamn_identity::principal::require_user_contact(&owner, "Org owner")?;
            let installer = wamn_platform_install::org_install::OrgInstaller::developer().await?;
            wamn_platform_install::org::create(&org, &owner, &installer).await
        }
        Command::Eject(args) => wamn_cli::build_verbs::eject(args),
        Command::Build(args) => wamn_cli::build_verbs::run(args).await,
        Command::Publish(args) => wamn_cli::dev::command::publish(args).await,
        Command::Integration(args) => wamn_cli::integration_verbs::run(args).await,
        Command::Web(args) => wamn_cli::web::run(args).await,
        Command::Inspect(args) => wamn_cli::artifact_verbs::inspect(args).await,
        Command::Diff(args) => wamn_cli::artifact_verbs::diff(args).await,
        Command::Dev(args) => wamn_cli::dev::command::run(args).await,
    }
}

#[cfg(not(target_os = "linux"))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("wamn dev requires Linux filesystem notifications")
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn source_publication_refuses_a_caller_supplied_publisher() {
        for arguments in [
            vec![
                "wamn",
                "deploy",
                "apps/orders",
                "--app",
                "orders",
                "--env",
                "env.yaml",
            ],
            vec![
                "wamn",
                "schema",
                "publish",
                "apps/orders",
                "--env",
                "env.yaml",
            ],
        ] {
            assert!(Cli::try_parse_from(&arguments).is_ok());
            let mut impersonated = arguments;
            impersonated.extend(["--verified-publisher-principal", "another-user"]);
            assert_eq!(
                Cli::try_parse_from(impersonated).unwrap_err().kind(),
                clap::error::ErrorKind::UnknownArgument
            );
        }
    }

    #[test]
    fn schema_selection_is_independent_of_code_selection() {
        let digest = format!("sha256:{}", "a".repeat(64));
        for verb in ["diff", "apply"] {
            let command = Cli::try_parse_from([
                "wamn",
                "schema",
                verb,
                "orders",
                "--env",
                "env.yaml",
                "--release",
                &digest,
            ])
            .unwrap()
            .command;
            let Command::Schema(command) = command else {
                panic!("schema")
            };
            let (wamn_cli::schema_verbs::SchemaCommand::Diff(args)
            | wamn_cli::schema_verbs::SchemaCommand::Apply(args)) = command
            else {
                panic!("schema input")
            };
            assert_eq!(args.app, "orders".parse().unwrap());
            assert_eq!(args.env, std::path::Path::new("env.yaml"));
            assert_eq!(
                args.release.as_ref(),
                Some(&digest.parse::<wamn_digest::Digest>().unwrap())
            );
            assert!(Cli::try_parse_from(["wamn", "schema", verb, "orders"]).is_err());
            assert!(
                Cli::try_parse_from(["wamn", "schema", verb, "orders", "--env", "env.yaml"])
                    .is_ok()
            );
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod platform_tests {
    use super::*;

    #[test]
    fn platform_schema_refuses_a_system_env() {
        for action in ["diff", "apply"] {
            assert!(
                Cli::try_parse_from([
                    "wamn", "platform", "schema", action, "--target", "system", "--env", "env.yaml"
                ])
                .is_err()
            );
            assert!(
                Cli::try_parse_from(["wamn", "platform", "schema", action, "--target", "project"])
                    .is_err()
            );
        }
    }
}
