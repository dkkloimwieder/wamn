//! Thin product-command client over the shared development engine.

use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::Context as _;
use clap::Args;
use serde::Deserialize;
use tokio::process::Command;

use super::config::{DevConfig, parse_config, preflight_config, resolve_dev_packages};
use super::coordinator::{ProductionDevStageError, ProductionDevStageRunner};
use super::watch::{FilesystemInvalidationSource, GitSource};
use super::{
    DevInvalidation, DevInvalidationSource, DevRunReceipt, DevStage, DevWatchObserver,
    DevWatchOutcome, run_once_with_source_state_provider, run_watch_with_source_state_provider,
};

const BUILD_COMPONENTS_TOOL: &str = "tools/build-components";
const BUILD_PROFILE: &str = "m1";

/// Inputs owned by the literal `wamn dev` product command.
#[derive(Clone, Debug, Args)]
pub struct DevCommandArgs {
    /// Strict deployment configuration document.
    #[arg(long, value_name = "FILE")]
    config: PathBuf,

    /// Package root for the application overlay under development.
    #[arg(long, value_name = "DIRECTORY")]
    overlay_root: PathBuf,

    /// Keep the disposable verification session open and rerun affected suffixes.
    #[arg(long)]
    watch: bool,
}

#[derive(Debug)]
struct CommandInvalidationError {
    operation: &'static str,
    source: Box<dyn Error + Send + Sync>,
}

impl CommandInvalidationError {
    fn new(operation: &'static str, source: impl Error + Send + Sync + 'static) -> Self {
        Self {
            operation,
            source: Box::new(source),
        }
    }
}

impl fmt::Display for CommandInvalidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.source)
    }
}

impl Error for CommandInvalidationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.source.as_ref())
    }
}

struct CommandInvalidations<S> {
    initial: Option<DevInvalidation>,
    source: S,
}

impl<S> DevInvalidationSource for CommandInvalidations<S>
where
    S: DevInvalidationSource + Send,
{
    type Error = CommandInvalidationError;

    async fn next(&mut self) -> Result<Option<DevInvalidation>, Self::Error> {
        if let Some(initial) = self.initial.take() {
            return Ok(Some(initial));
        }
        tokio::select! {
            result = self.source.next() => result.map_err(|source| {
                CommandInvalidationError::new("read a filesystem invalidation", source)
            }),
            result = tokio::signal::ctrl_c() => {
                result.map_err(|source| {
                    CommandInvalidationError::new("wait for the shutdown signal", source)
                })?;
                Ok(None)
            }
        }
    }

    fn try_next(&mut self) -> Result<Option<DevInvalidation>, Self::Error> {
        if let Some(initial) = self.initial.take() {
            return Ok(Some(initial));
        }
        self.source.try_next().map_err(|source| {
            CommandInvalidationError::new("drain a filesystem invalidation", source)
        })
    }
}

struct CommandObserver;

impl DevWatchObserver for CommandObserver {
    fn completed(&mut self, outcome: DevWatchOutcome) {
        match outcome.into_result() {
            Ok(receipt) => print_receipt("watch", &receipt),
            Err(error) => eprintln!("{error}"),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildWatchRoots {
    profile: String,
    workspace_roots: Vec<PathBuf>,
}

/// Execute the product development command through the shared stage engine.
pub async fn run(args: DevCommandArgs) -> anyhow::Result<()> {
    let bytes = fs::read(&args.config)
        .with_context(|| format!("read development config {}", args.config.display()))?;
    let config = parse_config(&bytes).context("validate development config")?;
    preflight_config(&config)
        .await
        .context("reach configured development endpoints")?;

    let mut git = GitSource::discover(&args.overlay_root)
        .await
        .context("discover the originating Git worktree")?;
    let mut runner =
        ProductionDevStageRunner::new(config.clone(), args.overlay_root.clone(), git.clone())
            .context("construct the production development coordinator")?;

    let result = if args.watch {
        run_watch_command(&config, &args.overlay_root, &mut runner, &mut git)
            .await
            .map(|()| None)
    } else {
        run_once_command(&config, &mut runner, &mut git)
            .await
            .map(Some)
    };
    let cleanup = runner.shutdown().await;
    if let Some(receipt) = finish_with_cleanup(result, cleanup)? {
        print_receipt("run", &receipt);
    }
    Ok(())
}

async fn run_once_command(
    config: &DevConfig,
    runner: &mut ProductionDevStageRunner,
    git: &mut GitSource,
) -> anyhow::Result<DevRunReceipt> {
    let receipt = run_once_with_source_state_provider(config, runner, git)
        .await
        .context("own the disposable verification database")??;
    Ok(receipt)
}

async fn run_watch_command(
    config: &DevConfig,
    overlay_root: &std::path::Path,
    runner: &mut ProductionDevStageRunner,
    git: &mut GitSource,
) -> anyhow::Result<()> {
    let packages = resolve_dev_packages(config, overlay_root)
        .context("resolve the manifest-declared package closure")?;
    let package_roots = std::iter::once(packages.overlay_root().to_owned())
        .chain(
            packages
                .base_packages()
                .iter()
                .map(|package| package.root().to_owned()),
        )
        .collect::<Vec<_>>();
    let component_roots = component_build_watch_roots(git.repository_root()).await?;
    let filesystem = FilesystemInvalidationSource::new(package_roots, component_roots, git.clone())
        .context("watch package and component inputs")?;
    let source_state = git
        .snapshot()
        .await
        .context("read the initial Git source state")?
        .state();
    let mut source = CommandInvalidations {
        initial: Some(DevInvalidation::Rerun {
            from: DevStage::Migrate,
            source_state,
        }),
        source: filesystem,
    };
    let mut observer = CommandObserver;
    run_watch_with_source_state_provider(config, runner, &mut source, &mut observer, git)
        .await
        .context("own the disposable verification database")??;
    Ok(())
}

async fn component_build_watch_roots(repository_root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let tool = repository_root.join(BUILD_COMPONENTS_TOOL);
    let output = Command::new(&tool)
        .args(["watch-roots", BUILD_PROFILE])
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("start production build owner {}", tool.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "production build owner {} exited with {}: {}",
            tool.display(),
            output.status,
            stderr.trim()
        );
    }
    let roots: BuildWatchRoots = serde_json::from_slice(&output.stdout)
        .context("decode production component-build watch roots")?;
    anyhow::ensure!(
        roots.profile == BUILD_PROFILE,
        "production build owner returned profile {:?} instead of {BUILD_PROFILE:?}",
        roots.profile
    );
    anyhow::ensure!(
        !roots.workspace_roots.is_empty(),
        "production build owner returned no component watch roots"
    );
    roots
        .workspace_roots
        .into_iter()
        .map(|root| {
            anyhow::ensure!(
                !root.is_absolute()
                    && root
                        .components()
                        .all(|component| matches!(component, Component::Normal(_))),
                "production build owner returned unsafe component watch root {}",
                root.display()
            );
            Ok(repository_root.join(root))
        })
        .collect()
}

fn finish_with_cleanup<T>(
    result: anyhow::Result<T>,
    cleanup: Result<(), ProductionDevStageError>,
) -> anyhow::Result<T> {
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup)) => Err(cleanup).context("clean up local activation"),
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("local activation cleanup also failed: {cleanup}")))
        }
    }
}

fn print_receipt(prefix: &str, receipt: &DevRunReceipt) {
    let completed = receipt
        .completed()
        .iter()
        .map(|stage| stage.as_str())
        .collect::<Vec<_>>()
        .join(",");
    println!("{prefix} completed: {completed}");
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::convert::Infallible;

    use super::*;

    struct QueueSource(VecDeque<DevInvalidation>);

    impl DevInvalidationSource for QueueSource {
        type Error = Infallible;

        async fn next(&mut self) -> Result<Option<DevInvalidation>, Self::Error> {
            Ok(self.0.pop_front())
        }

        fn try_next(&mut self) -> Result<Option<DevInvalidation>, Self::Error> {
            Ok(self.0.pop_front())
        }
    }

    #[test]
    fn command_arguments_require_explicit_deployment_and_package_inputs() {
        #[derive(Debug, clap::Parser)]
        struct TestCli {
            #[command(flatten)]
            args: DevCommandArgs,
        }

        use clap::Parser as _;
        let parsed = TestCli::try_parse_from([
            "wamn-dev",
            "--config",
            "dev.json",
            "--overlay-root",
            "packages/client_acme_receiving",
            "--watch",
        ])
        .expect("parse complete command inputs");
        assert_eq!(parsed.args.config, PathBuf::from("dev.json"));
        assert_eq!(
            parsed.args.overlay_root,
            PathBuf::from("packages/client_acme_receiving")
        );
        assert!(parsed.args.watch);

        let one_shot = TestCli::try_parse_from([
            "wamn-dev",
            "--config",
            "dev.json",
            "--overlay-root",
            "packages/client_acme_receiving",
        ])
        .expect("parse the default one-shot command");
        assert!(!one_shot.args.watch);

        let missing = TestCli::try_parse_from(["wamn-dev", "--config", "dev.json"])
            .expect_err("an omitted overlay root must refuse");
        assert_eq!(
            missing.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }

    #[tokio::test]
    async fn an_initial_full_run_precedes_queued_watch_events() {
        let initial = DevInvalidation::Rerun {
            from: DevStage::Migrate,
            source_state: super::super::DevSourceState::Clean,
        };
        let queued = DevInvalidation::Rerun {
            from: DevStage::Generate,
            source_state: super::super::DevSourceState::Dirty,
        };
        let mut source = CommandInvalidations {
            initial: Some(initial),
            source: QueueSource(VecDeque::from([queued])),
        };
        assert_eq!(
            source.next().await.expect("read initial event"),
            Some(initial)
        );
        assert_eq!(source.try_next().expect("read queued event"), Some(queued));
        assert_eq!(source.try_next().expect("source drains"), None);
    }
}
