//! Bounded child-process execution shared by repository command owners.

pub(super) mod plan;
use plan::diagnostic_tail;

use std::os::unix::process::CommandExt as _;
use std::process::{Output, Stdio};
use std::time::Duration;

use anyhow::Context as _;
use rustix::process::{Pid, Signal, kill_process_group};
use tokio::io::AsyncReadExt as _;
use tokio::process::Command;
use tokio::signal::unix::{SignalKind, signal};

const REAP_TIMEOUT: Duration = Duration::from_secs(5);
const DIAGNOSTIC_BYTES: usize = 16 * 1024;

struct ProcessGroup(Pid);

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        // Retain the group identity after the leader exits: descendants may
        // still be running or holding its output pipes open.
        let _ = kill_process_group(self.0, Signal::KILL);
    }
}

/// Execute a child with a deadline, signal cancellation, and owned descendants.
pub async fn execute(
    command: &mut Command,
    run_timeout: Duration,
    termination_grace: Duration,
) -> anyhow::Result<Output> {
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    command.as_std_mut().process_group(0);
    command
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .context("start the owned repository command")?;
    let group = ProcessGroup(
        Pid::from_raw(i32::try_from(
            child.id().context("the command has a process ID")?,
        )?)
        .context("the command process ID is positive")?,
    );
    let mut stdout = child.stdout.take().context("the command has stdout")?;
    let mut stderr = child.stderr.take().context("the command has stderr")?;
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    let (finished, leader_finished) = tokio::sync::oneshot::channel();
    let wait_for_leader = async {
        let (completion, stopped) = 'wait: {
            let waited = child.wait();
            tokio::pin!(waited);
            let cause = tokio::select! {
                status = &mut waited => break 'wait (status, None),
                _ = interrupt.recv() => "the owned repository command was interrupted",
                _ = terminate.recv() => "the owned repository command was terminated",
                _ = hangup.recv() => "the owned repository command lost its session",
                () = tokio::time::sleep(run_timeout) => "the owned repository command exceeded its time limit",
            };
            let _ = kill_process_group(group.0, Signal::TERM);
            if let Ok(status) = tokio::time::timeout(termination_grace, &mut waited).await {
                break 'wait (status, Some(cause));
            }
            let _ = kill_process_group(group.0, Signal::KILL);
            let status = tokio::time::timeout(REAP_TIMEOUT, &mut waited)
                .await
                .unwrap_or_else(|_| {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "the killed command did not finish reaping within five seconds",
                    ))
                });
            (status, Some(cause))
        };
        // A successful leader can leave inherited pipes open in descendants.
        // Stop those descendants before waiting for diagnostic EOF.
        drop(group);
        let _ = finished.send(());
        (completion, stopped)
    };
    let drain = async {
        let reading = async {
            tokio::try_join!(
                stdout.read_to_end(&mut stdout_bytes),
                stderr.read_to_end(&mut stderr_bytes),
            )?;
            Ok::<_, std::io::Error>(())
        };
        tokio::pin!(reading);
        tokio::select! {
            result = &mut reading => result,
            _ = leader_finished => tokio::time::timeout(REAP_TIMEOUT, &mut reading)
                .await.unwrap_or_else(|_| Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "the owned output pipes did not finish draining within five seconds",
                ))),
        }
    };
    let ((completion, stopped), drained) = tokio::join!(wait_for_leader, drain);
    if let Some(cause) = stopped {
        let cleanup = completion
            .err()
            .or_else(|| drained.err())
            .map(|error| format!("; cleanup: {error}"))
            .unwrap_or_default();
        anyhow::bail!(
            "{cause}{cleanup}\nstdout tail:\n{}\nstderr tail:\n{}",
            diagnostic_tail(&stdout_bytes),
            diagnostic_tail(&stderr_bytes),
        );
    }
    let status = completion.with_context(|| {
        format!(
            "wait for the owned repository command\nstdout tail:\n{}\nstderr tail:\n{}",
            diagnostic_tail(&stdout_bytes),
            diagnostic_tail(&stderr_bytes),
        )
    })?;
    drained.with_context(|| {
        format!(
            "drain the owned repository command\nstdout tail:\n{}\nstderr tail:\n{}",
            diagnostic_tail(&stdout_bytes),
            diagnostic_tail(&stderr_bytes),
        )
    })?;
    Ok(Output {
        status,
        stdout: stdout_bytes,
        stderr: stderr_bytes,
    })
}
