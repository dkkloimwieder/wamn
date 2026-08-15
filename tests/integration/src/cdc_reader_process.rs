//! Process-boundary harness for reader-inclusive integration proofs.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::ExitStatus;
use std::time::Duration;

use anyhow::Context as _;
use tokio::process::{Child, Command};

const READER_BINARY_ENV: &str = "WAMN_CDC_READER_BIN";

#[derive(Debug)]
pub(crate) struct ReaderArgs {
    pub(crate) org: String,
    pub(crate) project: String,
    pub(crate) env: String,
    pub(crate) system_database_url: String,
    pub(crate) cdc_url: String,
    pub(crate) nats_url: String,
    pub(crate) stream_replicas: usize,
}

#[derive(Debug)]
pub(crate) struct ReaderProcess {
    child: Child,
}

impl ReaderProcess {
    pub(crate) fn spawn(args: ReaderArgs) -> anyhow::Result<Self> {
        Self::spawn_with_dup_window(args, 120)
    }

    pub(crate) fn spawn_with_dup_window(
        args: ReaderArgs,
        dup_window_secs: u64,
    ) -> anyhow::Result<Self> {
        let binary = reader_binary();
        let mut command = reader_command_with_dup_window(&binary, &args, dup_window_secs);
        command.kill_on_drop(true);
        let child = command
            .spawn()
            .with_context(|| format!("launch CDC reader process {}", binary.to_string_lossy()))?;
        Ok(Self { child })
    }

    pub(crate) fn is_finished(&mut self) -> anyhow::Result<bool> {
        self.child
            .try_wait()
            .context("poll CDC reader process")
            .map(|status| status.is_some())
    }

    pub(crate) async fn wait(mut self) -> anyhow::Result<ExitStatus> {
        self.child
            .wait()
            .await
            .context("wait for CDC reader process")
    }

    pub(crate) async fn shutdown(mut self, timeout: Duration) -> anyhow::Result<bool> {
        if let Some(status) = self
            .child
            .try_wait()
            .context("poll CDC reader before shutdown")?
        {
            return Ok(status.success());
        }

        let pid = self
            .child
            .id()
            .context("CDC reader has no process id before shutdown")?;
        let pid = libc::pid_t::try_from(pid).context("CDC reader process id exceeds pid_t")?;
        // SAFETY: `kill` does not dereference pointers. The PID comes directly
        // from the still-running child and the signal is the service's existing
        // graceful-shutdown boundary.
        let signal_result = unsafe { libc::kill(pid, libc::SIGTERM) };
        if signal_result != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error).context("send SIGTERM to CDC reader process");
            }
        }

        match tokio::time::timeout(timeout, self.child.wait()).await {
            Ok(status) => Ok(status.context("wait for CDC reader shutdown")?.success()),
            Err(_) => {
                self.child
                    .start_kill()
                    .context("kill CDC reader after shutdown timeout")?;
                let _ = self.child.wait().await;
                Ok(false)
            }
        }
    }
}

fn reader_binary() -> OsString {
    if let Some(binary) = std::env::var_os(READER_BINARY_ENV) {
        return binary;
    }

    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("wamn-cdc-reader")));
    sibling
        .filter(|path| path.is_file())
        .map(PathBuf::into_os_string)
        .unwrap_or_else(|| OsString::from("wamn-cdc-reader"))
}

#[cfg(test)]
fn reader_command(binary: &OsStr, args: &ReaderArgs) -> Command {
    reader_command_with_dup_window(binary, args, 120)
}

fn reader_command_with_dup_window(
    binary: &OsStr,
    args: &ReaderArgs,
    dup_window_secs: u64,
) -> Command {
    let mut command = Command::new(binary);
    command
        .env("WAMN_SYSTEM_URL", &args.system_database_url)
        .env("WAMN_CDC_URL", &args.cdc_url)
        .arg("--log-level")
        .arg("error")
        .arg("--org")
        .arg(&args.org)
        .arg("--project")
        .arg(&args.project)
        .arg("--env")
        .arg(&args.env)
        .arg("--nats-url")
        .arg(&args.nats_url)
        .arg("--sslmode")
        .arg("disable")
        .arg("--stream-replicas")
        .arg(args.stream_replicas.to_string())
        .arg("--dup-window-secs")
        .arg(dup_window_secs.to_string())
        .arg("--feedback-secs")
        .arg("1")
        .arg("--stall-threshold-secs")
        .arg("30")
        .arg("--slot-poll-secs")
        .arg("0")
        .arg("--slot-safe-wal-warn-bytes")
        .arg("268435456");
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_command_preserves_the_proof_runtime_contract() {
        let args = ReaderArgs {
            org: "org".into(),
            project: "project".into(),
            env: "dev".into(),
            system_database_url: "postgres://system".into(),
            cdc_url: "postgres://cdc".into(),
            nats_url: "nats://events".into(),
            stream_replicas: 3,
        };

        let command = reader_command(OsStr::new("/proof/wamn-cdc-reader"), &args);
        let command = command.as_std();
        assert_eq!(command.get_program(), "/proof/wamn-cdc-reader");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                "--log-level",
                "error",
                "--org",
                "org",
                "--project",
                "project",
                "--env",
                "dev",
                "--nats-url",
                "nats://events",
                "--sslmode",
                "disable",
                "--stream-replicas",
                "3",
                "--dup-window-secs",
                "120",
                "--feedback-secs",
                "1",
                "--stall-threshold-secs",
                "30",
                "--slot-poll-secs",
                "0",
                "--slot-safe-wal-warn-bytes",
                "268435456",
            ]
            .map(OsStr::new)
        );
        assert!(command.get_envs().any(|(key, value)| {
            key == "WAMN_SYSTEM_URL" && value == Some(OsStr::new("postgres://system"))
        }));
        assert!(command.get_envs().any(|(key, value)| {
            key == "WAMN_CDC_URL" && value == Some(OsStr::new("postgres://cdc"))
        }));
    }
}
