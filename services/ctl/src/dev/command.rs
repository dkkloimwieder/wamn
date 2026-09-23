//! Command-line parsing, rendering, and process signals for development sessions.

use anyhow::Context as _;
use clap::Args;
use std::io::{self, Write as _};
use std::path::PathBuf;
use tokio::signal::unix::{SignalKind, signal};
use tokio::task::JoinSet;
use wamn_control::dev::read::{DevReadHandle, DevRuntimeEndpoint};
use wamn_control::dev::session::{DevSession, DevSessionControl, DevSessionRequest};
use wamn_control::dev::{DevRunResult, DevWatchObserver, DevWatchOutcome};

/// Inputs owned by the literal `wamn dev` product command.
#[derive(Clone, Debug, Args)]
pub struct DevCommandArgs {
    /// Strict deployment configuration document.
    #[arg(long, value_name = "FILE")]
    config: PathBuf,

    /// Package root for the application overlay under development.
    #[arg(long, value_name = "DIRECTORY")]
    overlay_root: PathBuf,

    /// Keep the session open and rerun affected suffixes.
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

    /// Open the developer console, or the named component's operator terminal.
    #[arg(long, num_args = 0..=1, value_name = "COMPONENT")]
    #[allow(
        clippy::option_option,
        reason = "clap distinguishes omitted --tui, bare --tui, and --tui COMPONENT"
    )]
    tui: Option<Option<String>>,
}

impl DevCommandArgs {
    /// Construct the same strict request used by the CLI parser.
    pub fn new(config: PathBuf, overlay_root: PathBuf, watch: bool) -> Self {
        Self {
            config,
            overlay_root,
            watch,
            hold: false,
            tui: None,
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
    pub fn with_tui(mut self, tui: bool) -> Self {
        self.tui = if tui { Some(None) } else { None };
        self
    }

    const fn hold(&self) -> bool {
        self.hold
    }

    const fn tui(&self) -> bool {
        matches!(self.tui, Some(None))
    }

    /// Select the generated terminal owned by this declared component.
    #[must_use]
    pub fn with_component_tui(mut self, component: String) -> Self {
        self.tui = Some(Some(component));
        self
    }

    fn operator_component(&self) -> Option<&str> {
        self.tui.as_ref().and_then(Option::as_deref)
    }
}

/// Prints every watch and held run, reading the endpoint off the session seam.
///
/// The engine hands `completed` a result and nothing else, so a watch run has
/// to read where it served from the same handle the interactive client reads.
/// Without it a `--watch` session printed only that stages finished, and the
/// operator had no way to learn the port each rerun had just bound
/// (wamn-10yt.55).
struct CommandObserver {
    read: DevReadHandle,
}

impl DevWatchObserver for CommandObserver {
    fn completed(&mut self, outcome: DevWatchOutcome) {
        match outcome.into_result() {
            Ok(result) => {
                print_result("watch", &result);
                // Absent whenever the run stopped before Activate, which the
                // read handle reports at the actual shutdown boundary.
                let snapshot = self.read.snapshot();
                if let Some(endpoint) = snapshot.runtime_endpoint() {
                    print_served(endpoint);
                }
                // The reader of a watch session is a person or a script sitting
                // on a pipe, and it acts on the endpoint line while this process
                // goes back to waiting. Line buffering never leaves a terminal
                // short, but a pipe holds the line until the buffer fills.
                let _ = io::stdout().flush();
            }
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
    fn served(&mut self, result: &DevRunResult, endpoint: Option<&DevRuntimeEndpoint>) {
        print_result("run", result);
        if let Some(endpoint) = endpoint {
            print_served(endpoint);
        }
        println!("run holding");
        let _ = io::stdout().flush();
    }
}

// A failed initial run has no operator terminal to show its failure. Keep
// errors visible there without writing over an existing operator session.
struct OperatorObserver {
    read: DevReadHandle,
}

impl DevWatchObserver for OperatorObserver {
    fn completed(&mut self, outcome: DevWatchOutcome) {
        if let Err(error) = outcome.into_result() {
            if self.read.snapshot().runtime_endpoint().is_none() {
                eprintln!("{error}");
            } else {
                tracing::warn!(%error, "watch run failed; the previous operator target remains active");
            }
        }
    }
}

fn shutdown_signals(control: DevSessionControl) -> io::Result<JoinSet<()>> {
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let mut tasks = JoinSet::new();
    tasks.spawn(async move {
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
            _ = hangup.recv() => {}
        }
        // Latch the request even while an effectful stage is running. The
        // existing engine owns when that work completes and cleanup begins.
        control.request_shutdown();
    });
    Ok(tasks)
}

/// Prepare the library session while the CLI owns its signal handlers.
pub(super) async fn prepare(args: DevCommandArgs) -> anyhow::Result<(DevSession, JoinSet<()>)> {
    let control = DevSessionControl::default();
    let signals = shutdown_signals(control.clone())
        .context("install development session shutdown signals")?;
    let operator_component = args.operator_component().map(str::to_owned);
    let session = DevSession::prepare(
        DevSessionRequest {
            config: args.config,
            overlay_root: args.overlay_root,
            watch: args.watch,
            operator_component,
        },
        control,
    )
    .await?;
    Ok((session, signals))
}

/// Execute the product development command through the shared stage engine.
pub async fn run(args: DevCommandArgs) -> anyhow::Result<()> {
    if args.tui() {
        // The interactive client holds a whole session future; box it so the
        // one-shot caller does not carry it on the stack.
        return Box::pin(super::tui::run(args)).await;
    }
    if args.operator_component().is_some() {
        let (mut session, _signals) = prepare(args).await?;
        let mut observer = OperatorObserver {
            read: session.read_handle(),
        };
        session.run_with_observer(&mut observer, true).await?;
        return Ok(());
    }
    let hold = args.hold();
    let (mut session, _signals) = prepare(args).await?;
    let mut observer = CommandObserver {
        read: session.read_handle(),
    };
    let result = session.run_with_observer(&mut observer, hold).await?;
    // Under --hold the observer already printed, before the hold. Printing
    // here as well would repeat all of it once the interrupt arrives.
    if !hold && let Some(result) = result {
        print_result("run", &result);
        print_serving(&session);
    }
    Ok(())
}

/// Report where the completed run served the activated release.
///
/// Read off the same seam the interactive client reads, so the two cannot
/// disagree about which endpoint the release was reachable on. Past tense on
/// purpose: the one-shot loop has already stopped the application by the time
/// this prints, while --tui holds it and shows the same fact live.
fn print_serving(session: &DevSession) {
    if let Some(endpoint) = session.last_served() {
        print_served(endpoint);
    }
}

fn print_served(endpoint: &DevRuntimeEndpoint) {
    println!(
        "run served: {} host={} target_instance={}",
        endpoint.base_url(),
        endpoint.route_host(),
        endpoint.target_instance()
    );
}

pub(super) fn print_result(prefix: &str, result: &DevRunResult) {
    let completed = result
        .completed()
        .iter()
        .map(|stage| stage.as_str())
        .collect::<Vec<_>>()
        .join(",");
    println!("{prefix} completed: {completed}");
    // A separate line on purpose. Scripted callers read the completed line and
    // the served line by shape, so a skip must not change either of them.
    if !result.skipped().is_empty() {
        let skipped = result
            .skipped()
            .iter()
            .map(|stage| stage.as_str())
            .collect::<Vec<_>>()
            .join(",");
        println!("{prefix} skipped: unchanged {skipped}");
    }
    let timings = result
        .timings()
        .iter()
        .map(|(stage, elapsed)| format!("{}={}ms", stage.as_str(), elapsed.as_millis()))
        .collect::<Vec<_>>()
        .join(" ");
    println!(
        "{prefix} stage-ms: prepare={}ms {timings}",
        result.prepared().as_millis()
    );
    // One line per notice, after the timings, for the same reason the skip line
    // is separate: a scripted caller reads the completed and served lines by
    // shape. A notice never refuses, so it must never change either of them.
    for notice in result.notices() {
        println!("{prefix} {}: {}", notice.code(), notice.detail());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::process::Command;
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
            "apps/platform_fixture_overlay",
            "--watch",
        ])
        .expect("parse complete command inputs");
        assert_eq!(parsed.args.config, PathBuf::from("dev.json"));
        assert_eq!(
            parsed.args.overlay_root,
            PathBuf::from("apps/platform_fixture_overlay")
        );
        assert!(parsed.args.watch);
        assert!(parsed.args.tui.is_none());
        assert!(!parsed.args.hold);

        let one_shot = TestCli::try_parse_from([
            "wamn-dev",
            "--config",
            "dev.json",
            "--overlay-root",
            "apps/platform_fixture_overlay",
        ])
        .expect("parse the default one-shot command");
        assert!(!one_shot.args.watch);
        assert!(one_shot.args.tui.is_none());
        assert!(!one_shot.args.hold);

        let tui = TestCli::try_parse_from([
            "wamn-dev",
            "--config",
            "dev.json",
            "--overlay-root",
            "apps/platform_fixture_overlay",
            "--tui",
        ])
        .expect("parse the interactive terminal client");
        assert!(tui.args.tui());

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
                "apps/platform_fixture_overlay",
            ];
            argv.extend_from_slice(extra);
            TestCli::try_parse_from(argv).map(|parsed| parsed.args)
        };

        let hold = parse(&["--hold"]).expect("parse the held one-shot session");
        assert!(hold.hold);
        assert!(!hold.watch);
        assert!(hold.tui.is_none());

        // Session mode and renderer are independent axes, so clap must accept
        // both pairings rather than declare a conflict. --hold is redundant
        // under --tui, which already holds, and inert under --watch, which
        // never reaches the one-shot hold at all.
        let with_tui = parse(&["--hold", "--tui"]).expect("parse hold beside the terminal client");
        assert!(with_tui.hold);
        assert!(with_tui.tui());

        let operator = parse(&["--watch", "--tui", "fixture"])
            .expect("parse generated operator session without hold");
        assert_eq!(operator.operator_component(), Some("fixture"));
        assert!(!operator.tui());
        assert!(!operator.hold());
        assert!(operator.watch);

        let with_watch = parse(&["--hold", "--watch"]).expect("parse hold beside watch");
        assert!(with_watch.hold);
        assert!(with_watch.watch);
    }

    #[tokio::test]
    async fn unix_signals_are_remembered_until_active_work_finishes() {
        if let Ok(signal) = std::env::var("WAMN_DEV_SIGNAL_TEST") {
            signal_child_finishes_work_and_closes_watch(&signal).await;
            return;
        }
        // Keep process-wide handlers and real signals out of the test runner.
        for signal in ["TERM", "INT", "HUP"] {
            let mut command = Command::new(std::env::current_exe().expect("locate test binary"));
            command
                .args([
                    "--exact",
                    "dev::command::tests::unix_signals_are_remembered_until_active_work_finishes",
                ])
                .env("WAMN_DEV_SIGNAL_TEST", signal)
                .kill_on_drop(true);
            let output = tokio::time::timeout(std::time::Duration::from_secs(15), command.output())
                .await
                .expect("signal child must finish cooperative shutdown")
                .expect("run isolated signal child");
            assert!(
                output.status.success(),
                "{signal} child failed: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        }
    }

    async fn signal_child_finishes_work_and_closes_watch(signal: &str) {
        use rustix::process::{Signal, getpid, kill_process};
        use std::convert::Infallible;
        use wamn_control::dev::{
            DevInvalidation, DevInvalidationSource, DevStage, DevStageRunner, run_watch,
        };

        struct ActiveWork {
            signal: Signal,
            control: DevSessionControl,
            completed: bool,
        }
        impl DevStageRunner for ActiveWork {
            type Error = Infallible;
            async fn run(&mut self, stage: DevStage) -> Result<(), Self::Error> {
                assert_eq!(stage, DevStage::Activate);
                assert!(!self.completed);
                kill_process(getpid(), self.signal).expect("signal isolated active stage");
                self.control.wait_for_shutdown().await;
                tokio::task::yield_now().await;
                self.completed = true;
                Ok(())
            }
        }
        struct WaitingSource {
            initial: bool,
            control: DevSessionControl,
        }
        impl DevInvalidationSource for WaitingSource {
            type Error = Infallible;
            async fn next(&mut self) -> Result<Option<DevInvalidation>, Self::Error> {
                if std::mem::take(&mut self.initial) {
                    return Ok(Some(DevInvalidation::Rerun {
                        from: DevStage::Activate,
                    }));
                }
                self.control.wait_for_shutdown().await;
                Ok(None)
            }
            fn try_next(&mut self) -> Result<Option<DevInvalidation>, Self::Error> {
                Ok(None)
            }
        }
        struct Observer;
        impl DevWatchObserver for Observer {
            fn completed(&mut self, outcome: DevWatchOutcome) {
                outcome.into_result().expect("active work completes");
            }
        }
        let selected = match signal {
            "TERM" => Signal::TERM,
            "INT" => Signal::INT,
            "HUP" => Signal::HUP,
            other => panic!("unexpected test signal: {other}"),
        };
        let control = DevSessionControl::default();
        let _signals = shutdown_signals(control.clone()).expect("install CLI signals");
        let mut runner = ActiveWork {
            signal: selected,
            control: control.clone(),
            completed: false,
        };
        let mut source = WaitingSource {
            initial: true,
            control: control.clone(),
        };
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_watch(&mut runner, &mut source, &mut Observer),
        )
        .await
        .expect("remembered signal closes the watch loop")
        .expect("normal watch shutdown");
        assert!(runner.completed, "active work finishes before cleanup");
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            control.wait_for_shutdown(),
        )
        .await
        .expect("a later waiter observes the latched signal");
    }
}
