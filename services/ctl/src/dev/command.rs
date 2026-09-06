//! Thin product-command client over the shared development engine.

use std::error::Error;
use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Component, Path, PathBuf};

use anyhow::Context as _;
use clap::Args;
use serde::Deserialize;
use tokio::process::Command;
use tokio::sync::watch;

use super::config::{DevConfig, parse_config, preflight_config, resolve_dev_packages};
use super::coordinator::{ProductionDevStageError, ProductionDevStageRunner};
use super::read::{DevReadHandle, DevRuntimeEndpoint};
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

    /// Keep the activated release reachable until this process is interrupted.
    ///
    /// Session mode and renderer are independent axes. This is the session
    /// mode the interactive client already uses, offered to the plain
    /// renderer: redundant under --tui, and a no-op under --watch, which
    /// holds by its own nature.
    #[arg(long)]
    hold: bool,

    /// Render the development session in the interactive terminal client.
    #[arg(long)]
    tui: bool,
}

impl DevCommandArgs {
    /// Construct the same strict request used by the CLI parser.
    pub fn new(config: PathBuf, overlay_root: PathBuf, watch: bool) -> Self {
        Self {
            config,
            overlay_root,
            watch,
            hold: false,
            tui: false,
        }
    }

    /// Hold the activated release open after a successful one-shot run.
    #[must_use]
    pub const fn with_hold(mut self, hold: bool) -> Self {
        self.hold = hold;
        self
    }

    /// Select the interactive terminal client.
    #[must_use]
    pub const fn with_tui(mut self, tui: bool) -> Self {
        self.tui = tui;
        self
    }

    const fn hold(&self) -> bool {
        self.hold
    }

    const fn tui(&self) -> bool {
        self.tui
    }
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
    shutdown: watch::Receiver<bool>,
}

impl<S> DevInvalidationSource for CommandInvalidations<S>
where
    S: DevInvalidationSource + Send,
{
    type Error = CommandInvalidationError;

    async fn next(&mut self) -> Result<Option<DevInvalidation>, Self::Error> {
        if *self.shutdown.borrow_and_update() {
            return Ok(None);
        }
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
            () = wait_for_shutdown(&mut self.shutdown) => Ok(None),
        }
    }

    fn try_next(&mut self) -> Result<Option<DevInvalidation>, Self::Error> {
        if *self.shutdown.borrow_and_update() {
            return Ok(None);
        }
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

    /// Print the three held-session lines, in order, before the hold begins.
    ///
    /// A caller reading this stream learns the run finished, where to send a
    /// request, and that the process will now sit there. Stdout is line
    /// buffered, so each line has already left; the explicit flush is for the
    /// reader that is a pipe rather than a terminal, which is every caller
    /// that scripts this.
    fn served(&mut self, receipt: &DevRunReceipt, endpoint: Option<&DevRuntimeEndpoint>) {
        print_receipt("run", receipt);
        if let Some(endpoint) = endpoint {
            print_served(endpoint);
        }
        println!("run holding");
        let _ = io::stdout().flush();
    }
}

struct SilentObserver;

impl DevWatchObserver for SilentObserver {
    fn completed(&mut self, _outcome: DevWatchOutcome) {}
}

/// Cooperative stop handle for an interactive development session.
///
/// A stop request never aborts an effectful stage. The engine observes it at
/// the next stage/watch boundary and still runs the existing exact cleanup.
#[derive(Clone, Debug)]
pub struct DevSessionControl {
    shutdown: watch::Sender<bool>,
}

impl DevSessionControl {
    /// Ask the session to finish its current stage and shut down cleanly.
    pub fn request_shutdown(&self) {
        let _already_requested = self.shutdown.send_replace(true);
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildWatchRoots {
    profile: String,
    workspace_roots: Vec<PathBuf>,
}

/// Prepared development-loop engine shared by CLI and console clients.
#[derive(Debug)]
pub struct DevSession {
    config: DevConfig,
    overlay_root: PathBuf,
    watch: bool,
    runner: ProductionDevStageRunner,
    git: GitSource,
    control: DevSessionControl,
    shutdown: watch::Receiver<bool>,
}

impl DevSession {
    /// Validate every input and connect the read-only observation sources.
    pub async fn prepare(args: DevCommandArgs) -> anyhow::Result<Self> {
        let bytes = fs::read(&args.config)
            .with_context(|| format!("read development config {}", args.config.display()))?;
        let config = parse_config(&bytes).context("validate development config")?;
        preflight_config(&config)
            .await
            .context("reach configured development endpoints")?;

        let git = GitSource::discover(&args.overlay_root)
            .await
            .context("discover the originating Git worktree")?;
        let mut runner =
            ProductionDevStageRunner::new(config.clone(), args.overlay_root.clone(), git.clone())
                .context("construct the production development coordinator")?;
        runner
            .start_observations()
            .await
            .context("start development observation readers")?;
        let (shutdown, shutdown_receiver) = watch::channel(false);

        Ok(Self {
            config,
            overlay_root: args.overlay_root,
            watch: args.watch,
            runner,
            git,
            control: DevSessionControl { shutdown },
            shutdown: shutdown_receiver,
        })
    }

    /// Clone the sole public state seam before running this session.
    pub fn read_handle(&self) -> DevReadHandle {
        self.runner.read_handle()
    }

    /// Clone the cooperative stop handle used by interactive clients.
    pub fn control(&self) -> DevSessionControl {
        self.control.clone()
    }

    /// Run without terminal output; state remains available through the handle.
    pub async fn run(mut self) -> anyhow::Result<Option<DevRunReceipt>> {
        self.run_with_observer(&mut SilentObserver, false).await
    }

    /// Run and retain the activated local environment until the client stops it.
    ///
    /// The one-shot loop holds after activation; watch mode continues receiving
    /// invalidations. Both leave through the same native cleanup path.
    pub async fn run_until_shutdown(mut self) -> anyhow::Result<Option<DevRunReceipt>> {
        self.run_with_observer(&mut SilentObserver, true).await
    }

    async fn run_with_observer<O>(
        &mut self,
        observer: &mut O,
        hold_after_one_shot: bool,
    ) -> anyhow::Result<Option<DevRunReceipt>>
    where
        O: DevWatchObserver + Send,
    {
        let result = if self.watch {
            let shutdown = self.shutdown.clone();
            run_watch_command(
                &self.config,
                &self.overlay_root,
                &mut self.runner,
                &mut self.git,
                observer,
                shutdown,
            )
            .await
            .map(|()| None)
        } else {
            let result = run_once_command(&self.config, &mut self.runner, &mut self.git)
                .await
                .map(Some);
            if hold_after_one_shot {
                // Report before holding, not after: the whole point of the
                // hold is that another process acts on these lines while this
                // one sits still. Printing after the hold ends tells nobody
                // anything.
                if let Ok(Some(receipt)) = &result {
                    let snapshot = self.read_handle().snapshot();
                    observer.served(receipt, snapshot.runtime_endpoint());
                }
                if result.is_ok() {
                    wait_for_shutdown(&mut self.shutdown).await;
                }
            }
            result
        };
        let cleanup = self.runner.shutdown().await;
        finish_with_cleanup(result, cleanup)
    }
}

/// Execute the product development command through the shared stage engine.
pub async fn run(args: DevCommandArgs) -> anyhow::Result<()> {
    if args.tui() {
        // The interactive client holds a whole session future; box it so the
        // one-shot caller does not carry it on the stack.
        return Box::pin(super::tui::run(args)).await;
    }
    let hold = args.hold();
    let mut session = DevSession::prepare(args).await?;
    let mut observer = CommandObserver;
    let receipt = session.run_with_observer(&mut observer, hold).await?;
    // Under --hold the observer already printed, before the hold. Printing
    // here as well would repeat all of it once the interrupt arrives.
    if !hold && let Some(receipt) = receipt {
        print_receipt("run", &receipt);
        print_serving(&session);
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
    observer: &mut (impl DevWatchObserver + Send),
    shutdown: watch::Receiver<bool>,
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
        shutdown,
    };
    run_watch_with_source_state_provider(config, runner, &mut source, observer, git)
        .await
        .context("own the disposable verification database")??;
    Ok(())
}

async fn wait_for_shutdown(receiver: &mut watch::Receiver<bool>) {
    loop {
        if *receiver.borrow_and_update() {
            return;
        }
        if receiver.changed().await.is_err() {
            return;
        }
    }
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

/// Report where the completed run served the activated release.
///
/// Read off the same seam the interactive client reads, so the two cannot
/// disagree about which endpoint the release was reachable on. Past tense on
/// purpose: the one-shot loop has already torn the environment down by the time
/// this prints, while --tui holds it and shows the same fact live.
fn print_serving(session: &DevSession) {
    if let Some(endpoint) = session.read_handle().snapshot().runtime_endpoint() {
        print_served(endpoint);
    }
}

fn print_served(endpoint: &DevRuntimeEndpoint) {
    println!(
        "run served: {} host={}",
        endpoint.base_url(),
        endpoint.route_host()
    );
}

pub(super) fn print_receipt(prefix: &str, receipt: &DevRunReceipt) {
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
        assert!(!parsed.args.tui);
        assert!(!parsed.args.hold);

        let one_shot = TestCli::try_parse_from([
            "wamn-dev",
            "--config",
            "dev.json",
            "--overlay-root",
            "packages/client_acme_receiving",
        ])
        .expect("parse the default one-shot command");
        assert!(!one_shot.args.watch);
        assert!(!one_shot.args.tui);
        assert!(!one_shot.args.hold);

        let tui = TestCli::try_parse_from([
            "wamn-dev",
            "--config",
            "dev.json",
            "--overlay-root",
            "packages/client_acme_receiving",
            "--tui",
        ])
        .expect("parse the interactive terminal client");
        assert!(tui.args.tui);

        let missing = TestCli::try_parse_from(["wamn-dev", "--config", "dev.json"])
            .expect_err("an omitted overlay root must refuse");
        assert_eq!(
            missing.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
    }

    #[test]
    fn hold_is_a_session_mode_that_neither_renderer_flag_conflicts_with() {
        #[derive(Debug, clap::Parser)]
        struct TestCli {
            #[command(flatten)]
            args: DevCommandArgs,
        }

        use clap::Parser as _;
        let parse = |extra: &[&str]| {
            let mut argv = vec![
                "wamn-dev",
                "--config",
                "dev.json",
                "--overlay-root",
                "packages/client_acme_receiving",
            ];
            argv.extend_from_slice(extra);
            TestCli::try_parse_from(argv).map(|parsed| parsed.args)
        };

        let hold = parse(&["--hold"]).expect("parse the held one-shot session");
        assert!(hold.hold);
        assert!(!hold.watch);
        assert!(!hold.tui);

        // Session mode and renderer are independent axes, so clap must accept
        // both pairings rather than declare a conflict. --hold is redundant
        // under --tui, which already holds, and inert under --watch, which
        // never reaches the one-shot hold at all.
        let with_tui = parse(&["--hold", "--tui"]).expect("parse hold beside the terminal client");
        assert!(with_tui.hold);
        assert!(with_tui.tui);

        let with_watch = parse(&["--hold", "--watch"]).expect("parse hold beside watch");
        assert!(with_watch.hold);
        assert!(with_watch.watch);
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
            shutdown: watch::channel(false).1,
        };
        assert_eq!(
            source.next().await.expect("read initial event"),
            Some(initial)
        );
        assert_eq!(source.try_next().expect("read queued event"), Some(queued));
        assert_eq!(source.try_next().expect("source drains"), None);
    }

    #[tokio::test]
    async fn an_interactive_stop_closes_the_watch_source_without_dropping_cleanup() {
        let (shutdown, receiver) = watch::channel(false);
        let mut source = CommandInvalidations {
            initial: None,
            source: QueueSource(VecDeque::new()),
            shutdown: receiver,
        };
        shutdown.send_replace(true);
        assert_eq!(source.next().await.expect("stop is a clean close"), None);
    }
}
