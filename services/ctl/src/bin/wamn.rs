//! Product command-line client for the local development loop.

#[cfg(all(test, target_os = "linux"))]
#[path = "../../../cli_help_tests.rs"]
mod help_env_tests;

#[cfg(target_os = "linux")]
use anyhow::Context as _;
#[cfg(target_os = "linux")]
use clap::{Args, Parser, Subcommand};

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
    Env(wamn_ctl::environment_requests::Command),
    RevokePat(wamn_ctl::environment_requests::RevokePat),
    RecoverCaptureGap(wamn_ctl::provisioning_verbs::RecoverCaptureGapArgs),
    CloseCaptureGap(wamn_ctl::provisioning_verbs::CloseCaptureGapArgs),
    GrantProjectEnvMembership(wamn_ctl::identity_verbs::ProjectEnvMembershipArgs),
    RevokeProjectEnvMembership(wamn_ctl::identity_verbs::ProjectEnvMembershipArgs),
    GrantRole(wamn_ctl::role_verbs::UserRoleArgs),
    RevokeRole(wamn_ctl::role_verbs::UserRoleArgs),
    CreateRole(wamn_ctl::role_verbs::RoleArgs),
    DeleteRole(wamn_ctl::role_verbs::RoleArgs),
    GrantPermission(wamn_ctl::role_verbs::PermissionArgs),
    RevokePermission(wamn_ctl::role_verbs::PermissionArgs),
    /// Create an API service and save its private PAT.
    CreateApiService(wamn_ctl::identity_verbs::ApiServiceArgs),
    /// Manage connections through control routes.
    Connection(wamn_ctl::component_verbs::ConnectionArgs),
    /// Manage project external systems.
    ExternalSystem(wamn_ctl::component_verbs::ExternalSystemArgs),
    /// Read published integration types.
    IntegrationType(wamn_ctl::component_verbs::IntegrationTypeArgs),
    /// Manage workflows through authenticated routes.
    #[command(subcommand)]
    Workflow(wamn_ctl::workflow_verbs::WorkflowCommand),
    /// Build source or select published bytes, then deploy one application release.
    Deploy(wamn_ctl::application_verbs::DeployArgs),
    /// Restore one application's last successful release, or an explicit target, without reversing SQL.
    Rollback(wamn_ctl::application_verbs::RollbackArgs),
    /// Show authored selections and observed application state.
    Status(wamn_ctl::application_verbs::StatusArgs),
    /// Inspect or apply application SQL independently of serving code.
    #[command(subcommand)]
    Schema(wamn_ctl::schema_verbs::SchemaCommand),
    /// Deploy or inspect platform configuration.
    #[command(subcommand)]
    Platform(wamn_ctl::platform_verbs::ProductPlatformCommand),
    /// Build packages: generate every output from wamn.k into target/wamn.
    Build(wamn_ctl::build_verbs::BuildArgs),
    /// Copy a generated default into application source.
    Eject(wamn_ctl::build_verbs::EjectArgs),
    /// Build or publish a wasm connector.
    Integration(wamn_ctl::integration_verbs::IntegrationArgs),
    /// Run the package development loop, or stand up the environment it needs.
    #[command(args_conflicts_with_subcommands = true)]
    Dev(Box<DevArgs>),
    /// Build, push, or upload a web client.
    Web(wamn_ctl::web::WebArgs),
    /// Print layer 0, the canonical wamn.json, of a pushed package artifact.
    Inspect(wamn_ctl::artifact_verbs::InspectArgs),
    /// Print where layer 0 of two pushed package artifacts differs.
    Diff(wamn_ctl::artifact_verbs::DiffArgs),
}

/// `wamn dev` either runs the loop from its own flags or takes `up` and stands
/// the environment up. The two are exclusive: `up` MINTS the configuration the
/// bare form's `--config` reads, so one invocation is never both.
///
/// The loop's inputs are flattened as an `Option` group rather than negated by
/// the subcommand. `subcommand_negates_reqs` is the setting that reads like the
/// right one, and it does not work here — measured on clap 4.6.6, it leaves
/// `--config` required under `wamn dev up`, whether the requirement is
/// flattened in or declared directly. The `Option` group is what actually holds
/// both halves of the rule: `wamn dev` with no subcommand still refuses without
/// `--config`, and a partly-given loop invocation still names the flag it
/// lacks.
#[cfg(target_os = "linux")]
#[derive(Debug, Args)]
struct DevArgs {
    #[command(subcommand)]
    environment: Option<DevEnvironmentCommand>,

    #[command(flatten)]
    run: Option<wamn_ctl::dev::command::DevCommandArgs>,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Subcommand)]
enum DevEnvironmentCommand {
    /// Stand up the disposable environment the loop runs against, and hold it.
    Up(Box<wamn_ctl::dev::up::DevUpArgs>),
    /// Stop the identity process owned by this development environment.
    Down(wamn_ctl::dev::up::DevDownArgs),
    /// Reset this environment's idle disposable application target.
    Reset(wamn_ctl::dev::target_database::DevResetArgs),
    /// Write the edge release bundle of the release that the loop published.
    EdgeBundle(wamn_ctl::dev::edge_bundle::DevEdgeBundleArgs),
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
        Command::Env(command) => wamn_ctl::environment_requests::run(command).await,
        Command::RevokePat(args) => wamn_ctl::environment_requests::revoke_pat(args).await,
        Command::RecoverCaptureGap(args) => {
            wamn_ctl::provisioning_verbs::recover_capture_gap(args).await
        }
        Command::CloseCaptureGap(args) => {
            wamn_ctl::provisioning_verbs::close_capture_gap(args).await
        }

        Command::GrantProjectEnvMembership(args) => wamn_ctl::identity_verbs::grant(args).await,
        Command::RevokeProjectEnvMembership(args) => wamn_ctl::identity_verbs::revoke(args).await,
        Command::GrantRole(args) => wamn_ctl::role_verbs::grant(args).await,
        Command::RevokeRole(args) => wamn_ctl::role_verbs::revoke(args).await,
        Command::CreateRole(args) => wamn_ctl::role_verbs::create_role(args).await,
        Command::DeleteRole(args) => wamn_ctl::role_verbs::delete_role(args).await,
        Command::GrantPermission(args) => wamn_ctl::role_verbs::grant_permission(args).await,
        Command::RevokePermission(args) => wamn_ctl::role_verbs::revoke_permission(args).await,

        Command::CreateApiService(args) => wamn_ctl::identity_verbs::create_api_service(args).await,
        Command::Connection(args) => wamn_ctl::component_verbs::connection(args).await,
        Command::ExternalSystem(args) => wamn_ctl::component_verbs::external_system(args).await,
        Command::IntegrationType(args) => wamn_ctl::component_verbs::integration_type(args).await,
        Command::Workflow(command) => wamn_ctl::workflow_verbs::run(command).await,
        Command::Deploy(args) => wamn_ctl::application_verbs::deploy(args).await,
        Command::Rollback(args) => wamn_ctl::application_verbs::rollback(args).await,
        Command::Status(args) => wamn_ctl::application_verbs::status(args).await,
        Command::Schema(command) => wamn_ctl::schema_verbs::run(command).await,
        Command::Platform(command) => {
            wamn_ctl::platform_verbs::run_product(command, std::io::stdout().lock()).await
        }
        Command::Eject(args) => wamn_ctl::build_verbs::eject(args),
        Command::Build(args) => wamn_ctl::build_verbs::run(args).await,
        Command::Integration(args) => wamn_ctl::integration_verbs::run(args).await,
        Command::Web(args) => wamn_ctl::web::run(args).await,
        Command::Inspect(args) => wamn_ctl::artifact_verbs::inspect(args).await,
        Command::Diff(args) => wamn_ctl::artifact_verbs::diff(args).await,
        Command::Dev(dev) => match dev.environment {
            Some(DevEnvironmentCommand::Up(args)) => wamn_ctl::dev::up::run(*args).await,
            Some(DevEnvironmentCommand::Down(args)) => wamn_ctl::dev::up::down(args).await,
            Some(DevEnvironmentCommand::Reset(args)) => {
                wamn_ctl::dev::target_database::reset(args).await
            }
            Some(DevEnvironmentCommand::EdgeBundle(args)) => wamn_ctl::dev::edge_bundle::run(&args),
            None => {
                let run = dev
                    .run
                    .context("wamn dev needs --config and --overlay-root, or the up subcommand")?;
                wamn_ctl::dev::command::run(run).await
            }
        },
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
                "--application",
                "orders",
                "--env",
                "environment.k",
            ],
            vec![
                "wamn",
                "schema",
                "publish",
                "apps/orders",
                "--env",
                "environment.k",
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
    fn application_commands_require_an_authored_file_and_target() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let Command::Deploy(deploy) = Cli::try_parse_from([
            "wamn",
            "deploy",
            "--application",
            "orders",
            "--release",
            &digest,
            "--env",
            "environment.k",
        ])
        .unwrap()
        .command
        else {
            panic!("deploy");
        };
        assert_eq!(deploy.release.as_deref(), Some(digest.as_str()));
        assert_eq!(deploy.env, std::path::Path::new("environment.k"));
        let Command::Rollback(rollback) =
            Cli::try_parse_from(["wamn", "rollback", "orders", "--env", "environment.k"])
                .unwrap()
                .command
        else {
            panic!("rollback");
        };
        assert_eq!(rollback.application, "orders");
        assert_eq!(rollback.env, deploy.env);
        assert!(rollback.release.is_none());
        let Command::Rollback(rollback) = Cli::try_parse_from([
            "wamn",
            "rollback",
            "orders",
            "--env",
            "environment.k",
            "--release",
            &digest,
        ])
        .unwrap()
        .command
        else {
            panic!("rollback with explicit release");
        };
        assert_eq!(rollback.release.as_deref(), Some(digest.as_str()));
        let Command::Status(status) =
            Cli::try_parse_from(["wamn", "status", "--env", "environment.k"])
                .unwrap()
                .command
        else {
            panic!("status");
        };
        assert_eq!(status.env, deploy.env);
        let Command::Deploy(source) = Cli::try_parse_from([
            "wamn",
            "deploy",
            "apps/orders",
            "--application",
            "orders",
            "--env",
            "environment.k",
        ])
        .unwrap()
        .command
        else {
            panic!("deploy source");
        };
        assert_eq!(
            source.package.as_deref(),
            Some(std::path::Path::new("apps/orders"))
        );
        assert!(source.release.is_none());
        for arguments in [
            vec!["wamn", "deploy", "--env", "environment.k"],
            vec![
                "wamn",
                "deploy",
                "apps/orders",
                "--release",
                &digest,
                "--env",
                "environment.k",
            ],
            vec!["wamn", "rollback", "--env", "environment.k"],
            vec!["wamn", "status"],
        ] {
            assert!(Cli::try_parse_from(arguments).is_err());
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
                "environment.k",
                "--release",
                &digest,
            ])
            .unwrap()
            .command;
            let Command::Schema(command) = command else {
                panic!("schema")
            };
            let args = match command {
                wamn_ctl::schema_verbs::SchemaCommand::Diff(args) => args,
                wamn_ctl::schema_verbs::SchemaCommand::Apply(args) => args,
                _ => panic!("schema input"),
            };
            assert_eq!(args.application, "orders");
            assert_eq!(args.env, std::path::Path::new("environment.k"));
            assert_eq!(args.release.as_deref(), Some(digest.as_str()));
            assert!(Cli::try_parse_from(["wamn", "schema", verb, "orders"]).is_err());
            assert!(
                Cli::try_parse_from(["wamn", "schema", verb, "orders", "--env", "environment.k"])
                    .is_ok()
            );
        }
    }

    #[test]
    fn reset_parses_without_run_inputs() {
        let reset =
            Cli::try_parse_from(["wamn", "dev", "reset", "--config", "/tmp/dev.json"]).unwrap();
        assert!(
            matches!(reset.command, Command::Dev(dev) if matches!(dev.environment, Some(DevEnvironmentCommand::Reset(_))))
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod platform_tests {
    use super::*;
    use wamn_ctl::platform_verbs::ProductPlatformCommand;
    use wamn_environment::platform::document::UpgradeTarget;

    #[test]
    fn platform_schema_refuses_a_system_environment() {
        for action in ["diff", "apply"] {
            assert!(
                Cli::try_parse_from([
                    "wamn",
                    "platform",
                    "schema",
                    action,
                    "--target",
                    "system",
                    "--env",
                    "environment.k"
                ])
                .is_err()
            );
            assert!(
                Cli::try_parse_from(["wamn", "platform", "schema", action, "--target", "project"])
                    .is_err()
            );
        }
    }

    #[test]
    fn platform_deploy_accepts_an_optional_chart_and_explicit_targets() {
        let cli = Cli::try_parse_from(["wamn", "platform", "deploy", "cluster.k"]).unwrap();
        let Command::Platform(ProductPlatformCommand::Deploy(args)) = cli.command else {
            panic!("expected platform deploy");
        };
        assert!(args.chart.is_none());
        let cli = Cli::try_parse_from([
            "wamn",
            "platform",
            "deploy",
            "cluster.k",
            "--chart",
            "/prepared/chart",
        ])
        .unwrap();
        let Command::Platform(wamn_ctl::platform_verbs::ProductPlatformCommand::Deploy(args)) =
            cli.command
        else {
            panic!("expected platform deploy");
        };
        assert_eq!(args.file, std::path::Path::new("cluster.k"));
        assert_eq!(
            args.chart.as_deref(),
            Some(std::path::Path::new("/prepared/chart"))
        );
        assert!(args.only.is_none());
        for (argument, target) in [
            ("control", UpgradeTarget::Control),
            ("identity", UpgradeTarget::Identity),
            (
                "host:alpha/alpha",
                UpgradeTarget::Host {
                    release: "alpha".into(),
                    group: "alpha".into(),
                },
            ),
            (
                "host:shared/events",
                UpgradeTarget::Host {
                    release: "shared".into(),
                    group: "events".into(),
                },
            ),
        ] {
            let cli = Cli::try_parse_from([
                "wamn",
                "platform",
                "deploy",
                "cluster.k",
                "--chart",
                "/prepared/chart",
                "--only",
                argument,
            ])
            .unwrap();
            let Command::Platform(ProductPlatformCommand::Deploy(args)) = cli.command else {
                panic!("expected platform deploy");
            };
            assert_eq!(args.only, Some(target));
        }
        for target in [
            "host",
            "all",
            "application:alpha",
            "control,identity",
            "host:/alpha",
            "host:alpha/",
            "host:alpha/*",
            "host:alpha/group/extra",
            "host:Alpha/group",
        ] {
            assert!(
                Cli::try_parse_from([
                    "wamn",
                    "platform",
                    "deploy",
                    "cluster.k",
                    "--chart",
                    "/prepared/chart",
                    "--only",
                    target,
                ])
                .is_err(),
                "{target}"
            );
        }
        assert!(Cli::try_parse_from(["wamn", "platform", "apply", "cluster.k"]).is_err());
    }

    #[test]
    fn platform_status_needs_only_the_document() {
        let cli = Cli::try_parse_from(["wamn", "platform", "status", "cluster.k"]).unwrap();
        let Command::Platform(ProductPlatformCommand::Status(args)) = cli.command else {
            panic!("expected platform status");
        };
        assert_eq!(args.file, std::path::Path::new("cluster.k"));
        assert!(Cli::try_parse_from(["wamn", "platform", "status"]).is_err());
    }

    #[tokio::test]
    async fn platform_missing_document_refuses_before_provider_setup() {
        let directory = std::env::temp_dir().join(format!(
            "wamn-platform-missing-inputs-{}",
            std::process::id()
        ));
        std::fs::create_dir(&directory).unwrap();
        let missing = directory.join("missing");
        let cli = Cli::try_parse_from([
            "wamn".as_ref(),
            "platform".as_ref(),
            "deploy".as_ref(),
            missing.as_os_str(),
        ])
        .unwrap();
        let Command::Platform(command) = cli.command else {
            panic!("expected platform deploy");
        };
        let mut output = Vec::new();
        let error = wamn_ctl::platform_verbs::run_product(command, &mut output)
            .await
            .unwrap_err();
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .unwrap_or_else(|| panic!("unexpected input refusal: {error:#}"))
                .kind(),
            std::io::ErrorKind::NotFound
        );
        assert!(output.is_empty());
        std::fs::remove_dir(directory).unwrap();
    }
}
