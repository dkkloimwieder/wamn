//! Bounded child-process execution shared by repository command owners.

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

fn diagnostic_tail(bytes: &[u8]) -> std::borrow::Cow<'_, str> {
    String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(DIAGNOSTIC_BYTES)..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stuck_children_cancel_and_reap_descendants_with_diagnostics() {
        if let Ok(mode) = std::env::var("WAMN_OWNED_COMMAND_TEST") {
            stuck_child(&mode).await;
            return;
        }
        for mode in ["deadline", "TERM"] {
            let mut command = Command::new(std::env::current_exe().expect("locate test binary"));
            command
                .args([
                    "--exact",
                    "--nocapture",
                    "owned_command::tests::stuck_children_cancel_and_reap_descendants_with_diagnostics",
                ])
                .env("WAMN_OWNED_COMMAND_TEST", mode)
                .kill_on_drop(true);
            let output = tokio::time::timeout(Duration::from_secs(15), command.output())
                .await
                .expect("isolated child must finish bounded cleanup")
                .expect("run isolated child");
            assert!(
                output.status.success(),
                "{mode}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        }
    }

    async fn stuck_child(mode: &str) {
        use rustix::process::{WaitOptions, getpid, kill_process, set_child_subreaper, waitpid};

        // Only this isolated test process adopts the exiting leader's child.
        set_child_subreaper(Some(getpid())).expect("own the orphaned fixture descendant");

        assert!(matches!(mode, "deadline" | "TERM"));
        let path =
            std::env::temp_dir().join(format!("wamn-owned-command-{}.pids", std::process::id()));
        assert!(!path.exists(), "fixture path is unused");
        let mut command = Command::new("sh");
        command.args(["-c", r#"
trap 'kill -KILL "$worker"; wait "$worker"; printf "descendant reaped\n" >&2; while :; do :; done' TERM
sh -c 'trap "" TERM; exec sleep 600' &
worker=$!
printf '%s %s\n' "$$" "$worker" > "$1"
printf 'before cancellation stdout\n'
printf 'before cancellation stderr\n' >&2
wait "$worker"
"#, "owned-command"]).arg(&path);
        let signal_task = if mode == "TERM" {
            let ready = path.clone();
            Some(tokio::spawn(async move {
                tokio::time::timeout(Duration::from_secs(3), async {
                    while !ready.exists() {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("child publishes owned process IDs");
                kill_process(getpid(), Signal::TERM).expect("signal isolated test process");
            }))
        } else {
            None
        };
        let error = execute(
            &mut command,
            if mode == "deadline" {
                Duration::from_secs(1)
            } else {
                Duration::from_secs(10)
            },
            Duration::from_millis(100),
        )
        .await
        .expect_err("a stuck command cannot report success");
        if let Some(task) = signal_task {
            task.await.expect("join signal sender");
        }
        let diagnostic = error.to_string();
        assert!(
            diagnostic.contains(if mode == "deadline" {
                "exceeded its time limit"
            } else {
                "was terminated"
            }),
            "{diagnostic}"
        );
        assert!(
            diagnostic.contains("before cancellation stdout"),
            "{diagnostic}"
        );
        assert!(
            diagnostic.contains("before cancellation stderr"),
            "{diagnostic}"
        );
        assert!(diagnostic.contains("descendant reaped"), "{diagnostic}");
        let pids = std::fs::read_to_string(&path).expect("read owned process IDs");
        for pid in pids.split_whitespace() {
            assert!(
                !std::path::Path::new("/proc").join(pid).exists(),
                "owned process {pid} was not reaped"
            );
        }
        std::fs::remove_file(&path).expect("remove exact fixture file");

        let output = tokio::time::timeout(
            Duration::from_secs(2),
            execute(
                Command::new("sh")
                    .args([
                        "-c",
                        "sleep 600 & printf '%s\\n' \"$!\" > \"$1\"; printf leader-exited",
                        "owned-command",
                    ])
                    .arg(&path),
                Duration::from_secs(10),
                Duration::from_millis(100),
            ),
        )
        .await
        .expect("leader exit must clean descendants before waiting for pipe EOF")
        .expect("successful leader retains successful output");
        assert!(output.status.success());
        assert_eq!(output.stdout, b"leader-exited");
        let descendant = std::fs::read_to_string(&path).expect("read descendant ID");
        let descendant = Pid::from_raw(descendant.trim().parse().unwrap()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some((pid, _)) =
                    waitpid(Some(descendant), WaitOptions::NOHANG).expect("reap adopted descendant")
                {
                    assert_eq!(pid, descendant);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the leftover descendant must have been killed");
        std::fs::remove_file(&path).expect("remove exact descendant fixture file");

        let output = execute(
            Command::new("sh").args(["-c", "printf stdout; printf stderr >&2"]),
            Duration::from_secs(1),
            Duration::from_millis(100),
        )
        .await
        .expect("ordinary completion remains available");
        assert!(output.status.success());
        assert_eq!(output.stdout, b"stdout");
        assert_eq!(output.stderr, b"stderr");
    }
}
