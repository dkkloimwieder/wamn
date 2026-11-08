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
    /// Build source or select published bytes, then deploy one application release.
    Deploy(wamn_ctl::application_verbs::DeployArgs),
    /// Apply one application's previous authored release.
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
        Command::Deploy(args) => wamn_ctl::application_verbs::deploy(args).await,
        Command::Rollback(args) => wamn_ctl::application_verbs::rollback(args).await,
        Command::Status(args) => wamn_ctl::application_verbs::status(args).await,
        Command::Schema(command) => wamn_ctl::schema_verbs::run(command).await,
        Command::Platform(command) => {
            wamn_ctl::platform_verbs::run_product(command, std::io::stdout().lock()).await
        }
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
    fn application_commands_require_an_authored_file_and_target() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let Command::Deploy(deploy) = Cli::try_parse_from([
            "wamn",
            "deploy",
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
            "--env",
            "environment.k",
            "--verified-publisher-principal",
            "publisher",
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
            vec!["wamn", "deploy", "apps/orders", "--env", "environment.k"],
            vec![
                "wamn",
                "deploy",
                "apps/orders",
                "--release",
                &digest,
                "--env",
                "environment.k",
                "--verified-publisher-principal",
                "publisher",
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
                wamn_ctl::schema_verbs::SchemaCommand::Apply(args) => args.schema,
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
        let Command::Schema(wamn_ctl::schema_verbs::SchemaCommand::Apply(args)) =
            Cli::try_parse_from([
                "wamn",
                "schema",
                "apply",
                "orders",
                "--env",
                "environment.k",
                "--resolve",
                "migrations/0002_index.sql=retry",
            ])
            .unwrap()
            .command
        else {
            panic!("schema apply")
        };
        let resolution = args.resolve.unwrap();
        assert_eq!(resolution.path, "migrations/0002_index.sql");
        assert_eq!(
            resolution.action,
            wamn_package_control::apply_package::Resolution::Retry
        );
        let Command::Schema(wamn_ctl::schema_verbs::SchemaCommand::Publish(args)) =
            Cli::try_parse_from([
                "wamn",
                "schema",
                "publish",
                "apps/orders",
                "--env",
                "environment.k",
                "--verified-publisher-principal",
                "publisher",
            ])
            .unwrap()
            .command
        else {
            panic!("schema publication")
        };
        assert_eq!(args.package, std::path::Path::new("apps/orders"));
        assert_eq!(args.verified_publisher_principal, "publisher");
    }

    #[test]
    fn integration_build_and_publish_parse() {
        assert!(matches!(
            Cli::try_parse_from(["wamn", "integration", "build", "fixture"])
                .unwrap()
                .command,
            Command::Integration(_)
        ));
        assert!(matches!(
            Cli::try_parse_from([
                "wamn",
                "integration",
                "publish",
                "fixture",
                "--artifact-base",
                "localhost/integrations",
                "--registry-auth-file",
                "/tmp/auth.json",
                "--database-url",
                "postgresql://control/system",
                "--version",
                "1",
                "--parameter-schema",
                "/tmp/schema.json",
                "--operations",
                "example:fixture/client",
            ])
            .unwrap()
            .command,
            Command::Integration(_)
        ));
        assert!(Cli::try_parse_from(["wamn", "integration", "build"]).is_err());
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

    #[test]
    fn platform_deploy_requires_an_explicit_chart() {
        assert!(Cli::try_parse_from(["wamn", "platform", "deploy", "cluster.k"]).is_err());
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
        assert_eq!(args.chart, std::path::Path::new("/prepared/chart"));
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
    async fn platform_missing_inputs_refuse_before_provider_setup() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let directory = std::env::temp_dir().join(format!(
            "wamn-platform-missing-inputs-{}",
            std::process::id()
        ));
        std::fs::create_dir(&directory).unwrap();
        let missing = directory.join("missing");
        let document = root.join("deploy/kind/cluster.k");
        let chart = root.join("deploy/platform/release");
        for (document, chart) in [(&missing, &chart), (&document, &missing)] {
            let cli = Cli::try_parse_from([
                "wamn".as_ref(),
                "platform".as_ref(),
                "deploy".as_ref(),
                document.as_os_str(),
                "--chart".as_ref(),
                chart.as_os_str(),
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
        }
        std::fs::remove_dir(directory).unwrap();
    }
}
