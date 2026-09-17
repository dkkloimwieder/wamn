//! Development sessions, input watches, and cooperative cleanup.

use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::Context as _;
use serde::Deserialize;
use tokio::process::Command;
use tokio::sync::watch;

use super::config::{DevConfig, parse_config, preflight_config, resolve_dev_packages};
use super::coordinator::{ProductionDevStageError, ProductionDevStageRunner};
use super::read::{DevReadHandle, DevRuntimeEndpoint};
use super::watch::{FilesystemInvalidationSource, GitSource};
use super::{
    DevInvalidation, DevInvalidationSource, DevRunResult, DevStage, DevWatchObserver,
    DevWatchOutcome, run_once, run_watch,
};

/// Inputs for one development session, independent of its command-line renderer.
#[derive(Clone, Debug)]
pub struct DevSessionRequest {
    pub config: PathBuf,
    pub overlay_root: PathBuf,
    pub watch: bool,
    pub operator_component: Option<String>,
}

const BUILD_COMPONENTS_TOOL: &str = "tools/build-components";

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

struct NativeInvalidations {
    filesystem: FilesystemInvalidationSource,
    repository_root: PathBuf,
    package_roots: Vec<PathBuf>,
    config: DevConfig,
}

fn local_configuration_files(config: &DevConfig) -> Vec<PathBuf> {
    let local = config.local_artifacts();
    let mut files = vec![
        config.target_privileges_file().to_owned(),
        config.target_database_acl_file().to_owned(),
        local.flow_http_component.clone(),
    ];
    if let Some(path) = &local.bindings {
        files.push(path.clone());
        // Watches identify input files; the coordinator owns strict interpretation.
        if let Ok(bytes) = std::fs::read(path)
            && let Ok(serde_json::Value::Array(selections)) = serde_json::from_slice(&bytes)
        {
            files.extend(
                selections
                    .iter()
                    .filter_map(|selection| selection["instance"]["definition"].as_str())
                    .map(PathBuf::from)
                    .filter(|path| path.is_absolute()),
            );
        }
    }
    files
}

impl DevInvalidationSource for NativeInvalidations {
    type Error = CommandInvalidationError;

    async fn next(&mut self) -> Result<Option<DevInvalidation>, Self::Error> {
        // The engine calls next only after the prior run finishes. Metadata
        // never competes with the Build stage, and the first run can Generate
        // a missing native manifest before this refresh needs it.
        let packages = super::native_tui::operator_packages(&self.package_roots)
            .map_err(|source| CommandInvalidationError::new("read native package names", source))?;
        if packages
            .iter()
            .all(|package| package.manifest_path.is_file())
        {
            match super::native_tui::native_dependency_roots(&self.repository_root, &packages).await
            {
                Ok(inputs) => {
                    self.filesystem
                        .replace_native_inputs(inputs.directories, inputs.files)
                        .await
                        .map_err(|source| {
                            CommandInvalidationError::new("watch native build dependencies", source)
                        })?;
                }
                Err(error) => {
                    tracing::warn!(%error, "retain the previous native watches until Cargo metadata parses");
                }
            }
        }
        self.filesystem
            .replace_configuration_files(local_configuration_files(&self.config))
            .map_err(|source| {
                CommandInvalidationError::new("watch local configuration files", source)
            })?;
        self.filesystem
            .next()
            .await
            .map_err(|source| CommandInvalidationError::new("read filesystem changes", source))
    }

    fn try_next(&mut self) -> Result<Option<DevInvalidation>, Self::Error> {
        self.filesystem.try_next().map_err(|source| {
            CommandInvalidationError::new("read queued filesystem changes", source)
        })
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

struct SilentObserver;

impl DevWatchObserver for SilentObserver {
    fn completed(&mut self, _outcome: DevWatchOutcome) {}
}

/// Cooperative stop handle for an interactive development session.
///
/// A stop request never aborts an effectful stage. The engine observes it when
/// the current run finishes and still runs the existing exact cleanup.
#[derive(Clone, Debug)]
pub struct DevSessionControl {
    shutdown: watch::Sender<bool>,
}

impl Default for DevSessionControl {
    fn default() -> Self {
        Self {
            shutdown: watch::channel(false).0,
        }
    }
}

impl DevSessionControl {
    /// Wait until the client requests cooperative shutdown.
    pub async fn wait_for_shutdown(&self) {
        wait_for_shutdown(&mut self.shutdown.subscribe()).await;
    }

    /// Ask the session to finish its current run and shut down cleanly.
    pub fn request_shutdown(&self) {
        let _already_requested = self.shutdown.send_replace(true);
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildWatchRoots {
    profile: String,
    roots: Vec<PathBuf>,
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
    last_served: Option<DevRuntimeEndpoint>,
}

impl DevSession {
    /// Validate every input and connect the read-only observation sources.
    pub async fn prepare(
        args: DevSessionRequest,
        control: DevSessionControl,
    ) -> anyhow::Result<Self> {
        let bytes = fs::read(&args.config)
            .with_context(|| format!("read development config {}", args.config.display()))?;
        let config = parse_config(&bytes).context("validate development config")?;
        let operator_component = if let Some(selected) = args.operator_component.as_deref() {
            anyhow::ensure!(
                config.operator_bearer_token().is_some(),
                "--tui <component> requires operator_bearer_token in dev.json"
            );
            let packages = resolve_dev_packages(&config, &args.overlay_root)
                .context("resolve operator package closure")?;
            let roots = packages
                .base_packages()
                .iter()
                .map(|package| package.root().to_owned())
                .chain(std::iter::once(packages.overlay_root().to_owned()))
                .collect::<Vec<_>>();
            Some(super::native_tui::select_component(&roots, selected)?)
        } else {
            None
        };
        preflight_config(&config)
            .await
            .context("reach configured development endpoints")?;

        let git = GitSource::discover(&args.overlay_root)
            .await
            .context("discover the originating Git worktree")?;
        let mut runner =
            ProductionDevStageRunner::new(config.clone(), args.overlay_root.clone(), git.clone());
        let shutdown_receiver = control.shutdown.subscribe();
        runner
            .start_observations()
            .await
            .context("start development observation readers")?;
        if let Some(package) = operator_component {
            runner.configure_operator(package, super::operator::spawn(control.clone()));
        }

        Ok(Self {
            config,
            overlay_root: args.overlay_root,
            watch: args.watch,
            runner,
            git,
            control,
            shutdown: shutdown_receiver,
            last_served: None,
        })
    }

    /// Endpoint served by the most recent run, including after cleanup.
    pub fn last_served(&self) -> Option<&DevRuntimeEndpoint> {
        self.last_served.as_ref()
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
    pub async fn run(mut self) -> anyhow::Result<Option<DevRunResult>> {
        self.run_with_observer(&mut SilentObserver, false).await
    }

    /// Run and retain the activated local environment until the client stops it.
    ///
    /// The one-shot loop holds after activation; watch mode continues receiving
    /// invalidations. Both leave through the same native cleanup path.
    pub async fn run_until_shutdown(mut self) -> anyhow::Result<Option<DevRunResult>> {
        self.run_with_observer(&mut SilentObserver, true).await
    }

    pub async fn run_with_observer<O>(
        &mut self,
        observer: &mut O,
        hold_after_one_shot: bool,
    ) -> anyhow::Result<Option<DevRunResult>>
    where
        O: DevWatchObserver + Send,
    {
        let result = if self.watch {
            let shutdown = self.shutdown.clone();
            run_watch_command(
                &self.config,
                &self.overlay_root,
                &mut self.runner,
                &self.git,
                observer,
                shutdown,
            )
            .await
            .map(|()| None)
        } else {
            let result = run_once(&mut self.runner)
                .await
                .map(Some)
                .map_err(anyhow::Error::from);
            if hold_after_one_shot {
                // Report before holding, not after: the whole point of the
                // hold is that another process acts on these lines while this
                // one sits still. Printing after the hold ends tells nobody
                // anything.
                if let Ok(Some(result)) = &result {
                    let snapshot = self.read_handle().snapshot();
                    observer.served(result, snapshot.runtime_endpoint());
                }
                if result.is_ok() {
                    wait_for_shutdown(&mut self.shutdown).await;
                }
            }
            result
        };
        self.last_served = self.read_handle().snapshot().runtime_endpoint().cloned();
        let cleanup = self.runner.shutdown().await;
        finish_with_cleanup(result, cleanup)
    }
}

async fn run_watch_command(
    config: &DevConfig,
    overlay_root: &std::path::Path,
    runner: &mut ProductionDevStageRunner,
    git: &GitSource,
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
    let component_roots =
        component_build_watch_roots(git.repository_root(), &package_roots).await?;
    let repository_root = git.repository_root().to_owned();
    let native_files = [
        "Cargo.toml",
        "Cargo.lock",
        ".cargo/config",
        ".cargo/config.toml",
        "rust-toolchain",
        "rust-toolchain.toml",
    ]
    .map(|file| repository_root.join(file))
    .to_vec();
    let mut filesystem = FilesystemInvalidationSource::with_native_inputs(
        package_roots.clone(),
        component_roots,
        [repository_root.join("crates/client")],
        native_files,
        git.clone(),
    )
    .await
    .context("watch package, component and native client inputs")?;
    filesystem
        .replace_configuration_files(local_configuration_files(config))
        .context("watch local configuration files")?;
    runner.configure_generated_native_outputs(
        filesystem
            .watch_generated_native_outputs()
            .context("watch generated native outputs at their emission boundary")?,
    );
    let native = NativeInvalidations {
        filesystem,
        repository_root,
        package_roots,
        config: config.clone(),
    };
    let mut source = CommandInvalidations {
        initial: Some(DevInvalidation::Rerun {
            from: DevStage::Migrate,
        }),
        source: native,
        shutdown,
    };
    run_watch(runner, &mut source, observer).await?;
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

async fn component_build_watch_roots(
    repository_root: &Path,
    package_roots: &[PathBuf],
) -> anyhow::Result<Vec<PathBuf>> {
    let tool = repository_root.join(BUILD_COMPONENTS_TOOL);
    let output = super::execute_preparation(
        Command::new(&tool)
            .args(["watch-roots", "app"])
            .args(package_roots),
        super::INPUT_COMMAND_TIMEOUT,
    )
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
        roots.profile == "app",
        "production build owner returned profile {:?} instead of app",
        roots.profile
    );
    anyhow::ensure!(
        !roots.roots.is_empty(),
        "production build owner returned no component watch roots"
    );
    roots
        .roots
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

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::convert::Infallible;

    use super::*;

    struct QueueSource(VecDeque<DevInvalidation>);

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "`DevInvalidationSource` declares `async fn` because the production source \
                  waits on a watcher; this test source answers from a queue and still \
                  has to match the trait"
    )]
    impl DevInvalidationSource for QueueSource {
        type Error = Infallible;

        async fn next(&mut self) -> Result<Option<DevInvalidation>, Self::Error> {
            Ok(self.0.pop_front())
        }

        fn try_next(&mut self) -> Result<Option<DevInvalidation>, Self::Error> {
            Ok(self.0.pop_front())
        }
    }

    #[tokio::test]
    async fn an_initial_full_run_precedes_queued_watch_events() {
        let initial = DevInvalidation::Rerun {
            from: DevStage::Migrate,
        };
        let queued = DevInvalidation::Rerun {
            from: DevStage::Generate,
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
