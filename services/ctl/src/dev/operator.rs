//! Acknowledged process supervision for one generated operator terminal.

use std::error::Error;
use std::fmt;
use std::os::unix::process::ExitStatusExt as _;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use rustix::process::{Pid, Signal, kill_process};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use wamn_client_terminal::operator::SUPERVISOR_STOP_EXIT_CODE;

use super::command::DevSessionControl;

/// The terminal gets five seconds to restore itself before forced cleanup.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Deployment facts for one fresh process, with no submitted command state.
#[derive(Clone)]
pub(super) struct LaunchSpec {
    pub executable: PathBuf,
    pub base_url: String,
    pub route_host: String,
    pub target_instance: String,
    pub operator_token: String,
}

impl fmt::Debug for LaunchSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LaunchSpec")
            .field("executable", &self.executable)
            .field("base_url", &self.base_url)
            .field("route_host", &self.route_host)
            .field("target_instance", &self.target_instance)
            .field("operator_token", &"[REDACTED]")
            .finish()
    }
}

/// Failure to start or finish an operator process at an activation boundary.
#[derive(Debug)]
pub(super) struct OperatorError {
    operation: &'static str,
    detail: String,
    source: Option<Box<dyn Error + Send + Sync>>,
    process_stopped: bool,
}

impl OperatorError {
    /// Whether a successful wait means that the failed process is reaped.
    pub const fn process_stopped(&self) -> bool {
        self.process_stopped
    }

    fn exited(status: ExitStatus) -> Self {
        let mut error = Self::new(
            "run operator terminal",
            format!("the process exited with {status}"),
        );
        error.process_stopped = true;
        error
    }

    fn ended(process_stopped: bool) -> Self {
        let mut error = Self::new("start operator terminal", "the operator session ended");
        error.process_stopped = process_stopped;
        error
    }

    fn new(operation: &'static str, detail: impl Into<String>) -> Self {
        Self {
            operation,
            detail: detail.into(),
            source: None,
            process_stopped: false,
        }
    }

    fn with_source(
        operation: &'static str,
        detail: impl Into<String>,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            operation,
            detail: detail.into(),
            source: Some(Box::new(source)),
            process_stopped: false,
        }
    }
}

impl fmt::Display for OperatorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.detail)
    }
}

impl Error for OperatorError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

type Reply = oneshot::Sender<Result<(), OperatorError>>;

#[derive(Debug)]
enum Request {
    Start(LaunchSpec, Reply),
    Stop(&'static str, Reply),
}

/// Cloned callers share the same serialized process owner.
#[derive(Clone, Debug)]
pub(super) struct OperatorControl {
    requests: mpsc::Sender<Request>,
}

impl OperatorControl {
    /// Reap any previous process, then start a fresh terminal session.
    pub async fn start(&self, spec: LaunchSpec) -> Result<(), OperatorError> {
        let (reply, response) = oneshot::channel();
        self.request(Request::Start(spec, reply), response).await
    }

    /// Return only after the old process exits and the supervisor reaps it.
    pub async fn stop(&self, reason: &'static str) -> Result<(), OperatorError> {
        let (reply, response) = oneshot::channel();
        self.request(Request::Stop(reason, reply), response).await
    }

    async fn request(
        &self,
        request: Request,
        response: oneshot::Receiver<Result<(), OperatorError>>,
    ) -> Result<(), OperatorError> {
        self.requests.send(request).await.map_err(|_| {
            OperatorError::new("contact operator supervisor", "the process owner stopped")
        })?;
        response.await.map_err(|_| {
            OperatorError::new("wait for operator supervisor", "the process owner stopped")
        })?
    }
}

/// Start the process owner without entering a terminal in the parent process.
pub(super) fn spawn(session: DevSessionControl) -> OperatorControl {
    start_supervisor(move || session.request_shutdown(), SHUTDOWN_TIMEOUT)
}

fn start_supervisor(
    request_shutdown: impl Fn() + Send + 'static,
    shutdown_timeout: Duration,
) -> OperatorControl {
    // A boundary needs one acknowledged request, so one queued request suffices.
    let (requests, receiver) = mpsc::channel(1);
    tokio::spawn(supervise(receiver, request_shutdown, shutdown_timeout));
    OperatorControl { requests }
}

async fn supervise(
    mut requests: mpsc::Receiver<Request>,
    request_shutdown: impl Fn() + Send,
    shutdown_timeout: Duration,
) {
    let mut child = None;
    let mut session_ended = false;
    let mut exit_error = None;
    loop {
        tokio::select! {
            biased;
            request = requests.recv() => match request {
                Some(Request::Start(spec, reply)) => {
                    let result = if session_ended {
                        Err(exit_error.take().unwrap_or_else(|| OperatorError::ended(child.is_none())))
                    } else {
                        match stop_child(&mut child, "replace operator terminal", shutdown_timeout).await {
                            Ok(Some(status)) => {
                                session_ended = true;
                                let error = natural_exit(Ok(status), &request_shutdown)
                                    .unwrap_or_else(|| OperatorError::ended(true));
                                Err(error)
                            }
                            Ok(None) => launch(&spec).map(|launched| child = Some(launched)),
                            Err(error) => Err(error),
                        }
                    };
                    let _closed_caller = reply.send(result);
                }
                Some(Request::Stop(reason, reply)) => {
                    let result = stop_child(&mut child, reason, shutdown_timeout).await;
                    let result = result.and_then(|status| {
                        if !session_ended && let Some(status) = status {
                            session_ended = true;
                            exit_error = natural_exit(Ok(status), &request_shutdown);
                        }
                        exit_error.take().map_or(Ok(()), |mut error| {
                            // Even an earlier wait error no longer prevents target
                            // cleanup once this acknowledged stop has reaped the child.
                            error.process_stopped = true;
                            Err(error)
                        })
                    });
                    let _closed_caller = reply.send(result);
                }
                None => {
                    if let Err(error) = stop_child(&mut child, "close operator supervisor", shutdown_timeout).await {
                        tracing::error!(error = %error, "operator terminal cleanup failed");
                    }
                    break;
                }
            },
            status = wait_child(&mut child), if child.is_some() && !session_ended => {
                if status.is_ok() {
                    child = None;
                }
                session_ended = true;
                exit_error = natural_exit(status, &request_shutdown);
            }
        }
    }
}

fn natural_exit(
    status: std::io::Result<ExitStatus>,
    request_shutdown: &impl Fn(),
) -> Option<OperatorError> {
    request_shutdown();
    match status {
        Ok(status) if status.success() => None,
        Ok(status) => Some(OperatorError::exited(status)),
        Err(source) => Some(OperatorError::with_source(
            "wait for operator terminal",
            "cannot reap the process",
            source,
        )),
    }
}

fn launch(spec: &LaunchSpec) -> Result<Child, OperatorError> {
    for (name, value) in [
        ("WAMN_BASE_URL", spec.base_url.as_str()),
        ("WAMN_HOST", spec.route_host.as_str()),
        ("WAMN_TARGET_INSTANCE", spec.target_instance.as_str()),
        ("WAMN_TOKEN", spec.operator_token.as_str()),
    ] {
        if value.is_empty() {
            return Err(OperatorError::new(
                "start operator terminal",
                format!("{name} is empty"),
            ));
        }
    }
    Command::new(&spec.executable)
        .env("WAMN_BASE_URL", &spec.base_url)
        .env("WAMN_HOST", &spec.route_host)
        .env("WAMN_TARGET_INSTANCE", &spec.target_instance)
        .env("WAMN_TOKEN", &spec.operator_token)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| {
            OperatorError::with_source(
                "start operator terminal",
                format!("cannot spawn {}", spec.executable.display()),
                source,
            )
        })
}

async fn wait_child(child: &mut Option<Child>) -> std::io::Result<ExitStatus> {
    match child {
        Some(child) => child.wait().await,
        None => std::future::pending().await,
    }
}

async fn stop_child(
    child: &mut Option<Child>,
    reason: &'static str,
    shutdown_timeout: Duration,
) -> Result<Option<ExitStatus>, OperatorError> {
    let Some(active) = child.as_mut() else {
        return Ok(None);
    };
    if let Some(status) = active.try_wait().map_err(|source| {
        OperatorError::with_source(reason, "cannot read the operator exit status", source)
    })? {
        *child = None;
        return Ok(Some(status));
    }
    let pid = active
        .id()
        .and_then(|raw| i32::try_from(raw).ok())
        .and_then(Pid::from_raw)
        .ok_or_else(|| OperatorError::new(reason, "the operator process has no valid PID"))?;
    // A process can exit between try_wait and kill. Waiting still reaps it.
    if let Err(source) = kill_process(pid, Signal::TERM)
        && source != rustix::io::Errno::SRCH
    {
        tracing::warn!(error = %source, "operator termination failed, attempting forced cleanup");
    }
    match timeout(shutdown_timeout, active.wait()).await {
        Ok(Ok(status)) => {
            *child = None;
            return Ok(natural_status(status, false));
        }
        Ok(Err(source)) => {
            return Err(OperatorError::with_source(
                reason,
                "cannot reap the operator process",
                source,
            ));
        }
        Err(_) => {}
    }
    active.start_kill().map_err(|source| {
        OperatorError::with_source(
            reason,
            "cannot kill the unresponsive operator process",
            source,
        )
    })?;
    let status = timeout(shutdown_timeout, active.wait())
        .await
        .map_err(|source| {
            OperatorError::with_source(reason, "the operator reap deadline expired", source)
        })?
        .map_err(|source| {
            OperatorError::with_source(reason, "cannot reap the killed operator process", source)
        })?;
    *child = None;
    Ok(natural_status(status, true))
}

fn natural_status(status: ExitStatus, forced: bool) -> Option<ExitStatus> {
    // An operator quit can race either signal. Preserve its actual wait status.
    // TERM before the handler is installed and our forced KILL also acknowledge stop.
    let intentional = status.code() == Some(i32::from(SUPERVISOR_STOP_EXIT_CODE))
        || status.signal() == Some(Signal::TERM.as_raw())
        || (forced && status.signal() == Some(Signal::KILL.as_raw()));
    (!intentional).then_some(status)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    static SEQUENCE: AtomicUsize = AtomicUsize::new(0);

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir()
                .join(format!("wamn-operator-{}-{sequence}", std::process::id()));
            fs::create_dir(&root).expect("create isolated operator fixture");
            Self { root }
        }

        fn script(&self, name: &str, body: &str) -> PathBuf {
            let path = self.root.join(name);
            // A concurrent fork can inherit a writable script descriptor before
            // CLOEXEC closes it, so keep all executable writes in a child process.
            let status = std::process::Command::new("/bin/sh")
                .args([
                    "-c",
                    "umask 077; printf '%s' \"$2\" > \"$1\" && chmod 700 \"$1\"",
                    "operator-fixture",
                ])
                .arg(&path)
                .arg(format!(
                    "#!/bin/sh\ncd '{}' || exit 1\n{body}\n",
                    self.root.display()
                ))
                .status()
                .expect("start the isolated fixture writer");
            assert!(status.success(), "write the executable operator fixture");
            path
        }
    }

    fn spec(executable: PathBuf, instance: &str) -> LaunchSpec {
        LaunchSpec {
            executable,
            base_url: "http://127.0.0.1:31001".to_owned(),
            route_host: "receiving.localhost".to_owned(),
            target_instance: instance.to_owned(),
            operator_token: "fixture-operator-secret".to_owned(),
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _removed = fs::remove_dir_all(&self.root);
        }
    }

    fn supervisor() -> (OperatorControl, Arc<AtomicUsize>) {
        let stopped = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&stopped);
        let control = start_supervisor(
            move || {
                observed.fetch_add(1, Ordering::SeqCst);
            },
            Duration::from_millis(200),
        );
        (control, stopped)
    }

    async fn wait_for(path: &Path) {
        timeout(Duration::from_secs(3), async {
            while !fs::metadata(path).is_ok_and(|metadata| metadata.len() > 0) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("fixture reaches its process boundary");
    }

    async fn wait_for_shutdown(stopped: &AtomicUsize) {
        timeout(Duration::from_secs(3), async {
            while stopped.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("operator exit requests cooperative shutdown");
    }

    #[test]
    fn launch_debug_redacts_the_operator_credential() {
        let spec = spec(PathBuf::from("/fixture/operator"), "instance-a");
        let rendered = format!("{spec:?}");
        assert!(!rendered.contains(&spec.operator_token));
        assert!(rendered.contains("[REDACTED]"));
        assert!(rendered.contains("instance-a"));
    }

    #[tokio::test]
    async fn launch_supplies_exact_bound_facts_without_command_arguments() {
        let fixture = Fixture::new();
        let executable = fixture.script("print-facts", "printf '%s\\n' \"$WAMN_BASE_URL\" \"$WAMN_HOST\" \"$WAMN_TARGET_INSTANCE\" \"$WAMN_TOKEN\" \"$#\" > facts\nexit 0");
        let (control, stopped) = supervisor();
        control
            .start(spec(executable, "instance-a"))
            .await
            .expect("launch the bound process");
        wait_for_shutdown(&stopped).await;
        assert_eq!(
            fs::read_to_string(fixture.root.join("facts")).expect("read child facts"),
            "http://127.0.0.1:31001\nreceiving.localhost\ninstance-a\nfixture-operator-secret\n0\n"
        );
        control
            .stop("finish the session")
            .await
            .expect("reap a normal exit");
    }

    #[test]
    fn stop_status_preserves_natural_exits_even_after_forced_cleanup() {
        for forced in [false, true] {
            for code in [0, 7, 137] {
                let status = ExitStatus::from_raw(code << 8);
                assert_eq!(natural_status(status, forced), Some(status));
            }
            assert!(natural_status(ExitStatus::from_raw(143 << 8), forced).is_none());
            assert!(natural_status(ExitStatus::from_raw(Signal::TERM.as_raw()), forced).is_none());
        }
        let killed = ExitStatus::from_raw(Signal::KILL.as_raw());
        assert_eq!(natural_status(killed, false), Some(killed));
        assert!(natural_status(killed, true).is_none());
    }

    #[tokio::test]
    async fn natural_exit_after_stop_begins_cannot_launch_a_replacement() {
        for code in [0, 7] {
            let fixture = Fixture::new();
            // This child cannot exit before stop_child's TERM. Its normal exit
            // therefore lands after try_wait, at the q/stop acknowledgement race.
            let executable = fixture.script(
                "operator",
                &format!("trap 'exit {code}' TERM\nprintf '%s' \"$$\" > pid\nwhile :; do sleep 0.02; done"),
            );
            let replacement = fixture.script(
                "replacement",
                "printf launched > replacement-launched\nexit 0",
            );
            let (control, stopped) = supervisor();
            control.start(spec(executable, "old")).await.expect("launch");
            wait_for(&fixture.root.join("pid")).await;
            let pid = fs::read_to_string(fixture.root.join("pid")).expect("read child PID");
            let error = control
                .start(spec(replacement.clone(), "new"))
                .await
                .expect_err("natural exit during stop ends the operator session");
            assert!(error.process_stopped());
            if code == 0 {
                assert!(error.to_string().contains("the operator session ended"));
            } else {
                assert!(error.to_string().contains("exit status: 7"));
            }
            assert_eq!(stopped.load(Ordering::SeqCst), 1);
            assert!(!Path::new("/proc").join(pid).exists());
            assert!(!fixture.root.join("replacement-launched").exists());
            control
                .start(spec(replacement, "later"))
                .await
                .expect_err("the natural exit remains latched");
            control
                .stop("clean the ended activation")
                .await
                .expect("the acknowledged reap permits target cleanup");
        }
    }

    #[tokio::test]
    async fn replacement_reaps_the_old_child_without_ending_the_session() {
        let fixture = Fixture::new();
        let executable = fixture.script("operator", "trap 'printf stopped > \"$WAMN_TARGET_INSTANCE.stopped\"; exit 143' TERM\nprintf '%s' \"$$\" > \"$WAMN_TARGET_INSTANCE.pid\"\nwhile [ ! -f \"$WAMN_TARGET_INSTANCE.quit\" ]; do sleep 0.02; done");
        let (control, stopped) = supervisor();
        control
            .start(spec(executable.clone(), "old"))
            .await
            .expect("launch old session");
        wait_for(&fixture.root.join("old.pid")).await;
        let old_pid = fs::read_to_string(fixture.root.join("old.pid")).expect("read old PID");
        control
            .start(spec(executable, "new"))
            .await
            .expect("replace the old process");
        assert!(
            fixture.root.join("old.stopped").exists(),
            "replacement acknowledges graceful exit first"
        );
        assert!(
            !Path::new("/proc").join(&old_pid).exists(),
            "the old process is reaped before replacement returns"
        );
        wait_for(&fixture.root.join("new.pid")).await;
        assert_eq!(
            stopped.load(Ordering::SeqCst),
            0,
            "the old exit cannot end the new session"
        );
        fs::write(fixture.root.join("new.quit"), "quit").expect("ask the new operator to exit");
        wait_for_shutdown(&stopped).await;
        assert_eq!(stopped.load(Ordering::SeqCst), 1);
        control
            .stop("finish the session")
            .await
            .expect("finish cleanup");
    }

    #[tokio::test]
    async fn stop_acknowledges_terminal_cleanup_before_the_caller_continues() {
        let fixture = Fixture::new();
        let executable = fixture.script("operator", "trap 'sleep 0.04; printf restored > restored; exit 143' TERM\nprintf ready > ready\nwhile :; do sleep 0.02; done");
        let (control, stopped) = supervisor();
        control
            .start(spec(executable, "instance-a"))
            .await
            .expect("launch the operator");
        wait_for(&fixture.root.join("ready")).await;
        control
            .stop("invalidate target")
            .await
            .expect("wait for terminal cleanup");
        assert_eq!(
            fs::read_to_string(fixture.root.join("restored")).expect("read restoration marker"),
            "restored"
        );
        assert_eq!(stopped.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn an_unresponsive_terminal_is_killed_and_reaped_before_acknowledgement() {
        let fixture = Fixture::new();
        let executable = fixture.script(
            "operator",
            "trap '' TERM\nprintf '%s' \"$$\" > pid\nwhile :; do sleep 0.02; done",
        );
        let (control, stopped) = supervisor();
        control
            .start(spec(executable, "instance-a"))
            .await
            .expect("launch the operator");
        wait_for(&fixture.root.join("pid")).await;
        let pid = fs::read_to_string(fixture.root.join("pid")).expect("read child PID");
        control
            .stop("invalidate target")
            .await
            .expect("kill and reap the process");
        assert!(!Path::new("/proc").join(pid).exists());
        assert_eq!(stopped.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn closing_the_control_channel_stops_the_owned_process() {
        let fixture = Fixture::new();
        let executable = fixture.script("operator", "trap 'printf restored > restored; exit 143' TERM\nprintf ready > ready\nwhile :; do sleep 0.02; done");
        let (control, stopped) = supervisor();
        control
            .start(spec(executable, "instance-a"))
            .await
            .expect("launch the operator");
        wait_for(&fixture.root.join("ready")).await;
        drop(control);
        wait_for(&fixture.root.join("restored")).await;
        assert_eq!(stopped.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn an_abnormal_exit_stops_the_session_and_reaches_cleanup() {
        let fixture = Fixture::new();
        let executable = fixture.script("operator", "exit 7");
        let (control, stopped) = supervisor();
        control
            .start(spec(executable, "instance-a"))
            .await
            .expect("spawn the process");
        wait_for_shutdown(&stopped).await;
        let error = control
            .stop("finish the session")
            .await
            .expect_err("report the failed operator");
        assert!(error.to_string().contains("exit status: 7"));
        assert!(error.process_stopped());
    }

    /// Keep the current-thread supervisor unpolled until the OS has exited its child.
    /// This makes the queued request win the biased select after a natural exit.
    fn exit_before_supervisor_poll(fixture: &Fixture) {
        let pid = fs::read_to_string(fixture.root.join("pid")).expect("read child PID");
        fs::write(fixture.root.join("quit"), "quit").expect("release the child");
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            let stat =
                fs::read_to_string(format!("/proc/{pid}/stat")).expect("child is not reaped yet");
            if stat
                .rsplit_once(')')
                .is_some_and(|(_, suffix)| suffix.trim_start().starts_with('Z'))
            {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "child exits while supervisor is unpolled"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn queued_stop_observes_natural_exit_instead_of_restarting_after_quit() {
        let fixture = Fixture::new();
        let executable = fixture.script(
            "operator",
            "printf '%s' \"$$\" > pid\nwhile [ ! -f quit ]; do sleep 0.02; done\nexit 0",
        );
        let (control, stopped) = supervisor();
        control
            .start(spec(executable.clone(), "old"))
            .await
            .expect("launch");
        wait_for(&fixture.root.join("pid")).await;
        exit_before_supervisor_poll(&fixture);
        control
            .stop("replace target")
            .await
            .expect("natural exit is reaped");
        assert_eq!(stopped.load(Ordering::SeqCst), 1);
        let error = control
            .start(spec(executable, "new"))
            .await
            .expect_err("q ended the session");
        assert!(error.process_stopped());
        control
            .stop("final cleanup")
            .await
            .expect("cleanup remains acknowledged");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn queued_replacement_reports_abnormal_exit_and_keeps_cleanup_safe() {
        let fixture = Fixture::new();
        let executable = fixture.script(
            "operator",
            "printf '%s' \"$$\" > pid\nwhile [ ! -f quit ]; do sleep 0.02; done\nexit 7",
        );
        let replacement = fixture.script(
            "replacement",
            "printf launched > replacement-launched\nexit 0",
        );
        let (control, stopped) = supervisor();
        control
            .start(spec(executable, "old"))
            .await
            .expect("launch");
        wait_for(&fixture.root.join("pid")).await;
        exit_before_supervisor_poll(&fixture);
        let error = control
            .start(spec(replacement, "new"))
            .await
            .expect_err("natural failure precedes replacement");
        assert!(error.to_string().contains("exit status: 7"));
        assert!(
            error.process_stopped(),
            "the old process was reaped before the refusal"
        );
        assert_eq!(stopped.load(Ordering::SeqCst), 1);
        assert!(!fixture.root.join("replacement-launched").exists());
        control
            .stop("clean the failed activation")
            .await
            .expect("already reported failure does not block target cleanup");
    }

    #[tokio::test]
    async fn launch_failure_acknowledges_cleanup_and_allows_a_later_watch_activation() {
        let fixture = Fixture::new();
        let (control, stopped) = supervisor();
        control
            .start(spec(fixture.root.join("missing-binary"), "failed"))
            .await
            .expect_err("missing operator binary cannot launch");
        control
            .stop("clean failed activation")
            .await
            .expect("no operator remains before target teardown");
        assert_eq!(stopped.load(Ordering::SeqCst), 0);
        let executable = fixture.script("operator", "exit 0");
        control
            .start(spec(executable, "replacement"))
            .await
            .expect("watch can try a fresh activation");
        wait_for_shutdown(&stopped).await;
        control
            .stop("finish recovered session")
            .await
            .expect("reap recovered process");
    }
}
