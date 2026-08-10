//! Process-boundary harness for deterministic dispatcher proofs.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

const DISPATCHER_BINARY_ENV: &str = "WAMN_DISPATCHER_BIN";
static PROJECT_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ProjectSpec {
    #[serde(skip)]
    pub(crate) name: String,
    pub(crate) url: String,
    pub(crate) tenant: String,
    pub(crate) schema: Option<String>,
}

#[derive(Debug)]
pub(crate) struct DispatcherProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    projects_file: PathBuf,
}

impl DispatcherProcess {
    pub(crate) fn spawn(
        specs: &[ProjectSpec],
        nats_url: &str,
        tls: Option<(&PathBuf, &PathBuf, &PathBuf)>,
        min_interval_ms: Option<i64>,
        max_interval_ms: Option<i64>,
        batch: Option<usize>,
    ) -> anyhow::Result<Self> {
        Self::spawn_at_level(
            specs,
            nats_url,
            tls,
            min_interval_ms,
            max_interval_ms,
            batch,
            "error",
        )
    }

    pub(crate) fn spawn_traced(specs: &[ProjectSpec], nats_url: &str) -> anyhow::Result<Self> {
        Self::spawn_at_level(specs, nats_url, None, None, None, None, "info")
    }

    fn spawn_at_level(
        specs: &[ProjectSpec],
        nats_url: &str,
        tls: Option<(&PathBuf, &PathBuf, &PathBuf)>,
        min_interval_ms: Option<i64>,
        max_interval_ms: Option<i64>,
        batch: Option<usize>,
        log_level: &str,
    ) -> anyhow::Result<Self> {
        let projects_file = write_projects_file(specs)?;
        let binary = dispatcher_binary();
        let mut command = dispatcher_command(
            &binary,
            &projects_file,
            nats_url,
            tls,
            min_interval_ms,
            max_interval_ms,
            batch,
            true,
            log_level,
        );
        command
            .kill_on_drop(true)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit());
        let mut child = command
            .spawn()
            .with_context(|| format!("launch dispatcher process {}", binary.to_string_lossy()))?;
        let stdin = child
            .stdin
            .take()
            .context("dispatcher stdin was not piped")?;
        let stdout = child
            .stdout
            .take()
            .context("dispatcher stdout was not piped")?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            projects_file,
        })
    }

    pub(crate) async fn tick_project(&mut self, project: usize, now_ms: i64) -> anyhow::Result<()> {
        let command = serde_json::json!({"command": "tick", "project": project, "now_ms": now_ms});
        self.stdin
            .write_all(command.to_string().as_bytes())
            .await
            .context("write dispatcher step")?;
        self.stdin
            .write_all(b"\n")
            .await
            .context("terminate dispatcher step")?;
        self.stdin.flush().await.context("flush dispatcher step")?;

        let mut line = String::new();
        let bytes = self
            .stdout
            .read_line(&mut line)
            .await
            .context("read dispatcher step response")?;
        if bytes == 0 {
            let status = self.child.try_wait().context("poll dispatcher process")?;
            anyhow::bail!("dispatcher closed its control stream (status {status:?})");
        }
        let response: StepResponse =
            serde_json::from_str(&line).context("parse dispatcher step response")?;
        match response {
            StepResponse::Ok {
                project: response_project,
                now_ms: response_now,
            } => {
                anyhow::ensure!(
                    (response_project, response_now) == (project, now_ms),
                    "dispatcher response ({response_project}, {response_now}) did not match command ({project}, {now_ms})"
                );
                Ok(())
            }
            StepResponse::Error {
                project: response_project,
                now_ms: response_now,
                interval_ms,
                message,
            } => {
                anyhow::ensure!(
                    (response_project, response_now) == (project, now_ms),
                    "dispatcher error response ({response_project}, {response_now}) did not match command ({project}, {now_ms})"
                );
                anyhow::bail!("dispatcher tick failed at interval {interval_ms}ms: {message}")
            }
        }
    }

    pub(crate) async fn emit_trigger_span(
        &mut self,
        run_id: &str,
        flow_id: &str,
        flow_version: i32,
        trigger_source: &str,
        tenant: &str,
    ) -> anyhow::Result<()> {
        let command = serde_json::json!({
            "command": "emit-trigger-span",
            "run_id": run_id,
            "flow_id": flow_id,
            "flow_version": flow_version,
            "trigger_source": trigger_source,
            "tenant": tenant,
        });
        self.stdin
            .write_all(command.to_string().as_bytes())
            .await
            .context("write dispatcher span command")?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        let mut line = String::new();
        anyhow::ensure!(
            self.stdout.read_line(&mut line).await? != 0,
            "dispatcher closed before acknowledging trigger span"
        );
        let response: SpanResponse =
            serde_json::from_str(&line).context("parse dispatcher span response")?;
        anyhow::ensure!(
            response.status == "span-emitted" && response.run_id == run_id,
            "unexpected dispatcher span response: {line}"
        );
        Ok(())
    }
}

impl Drop for DispatcherProcess {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        let _ = std::fs::remove_file(&self.projects_file);
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
enum StepResponse {
    Ok {
        project: usize,
        now_ms: i64,
    },
    Error {
        project: usize,
        now_ms: i64,
        interval_ms: i64,
        message: String,
    },
}

#[derive(Debug, Deserialize)]
struct SpanResponse {
    status: String,
    run_id: String,
}

#[expect(
    clippy::too_many_arguments,
    reason = "the helper mirrors the independent dispatcher CLI controls"
)]
fn dispatcher_command(
    binary: &OsStr,
    projects_file: &std::path::Path,
    nats_url: &str,
    tls: Option<(&PathBuf, &PathBuf, &PathBuf)>,
    min_interval_ms: Option<i64>,
    max_interval_ms: Option<i64>,
    batch: Option<usize>,
    stepped: bool,
    log_level: &str,
) -> Command {
    let mut command = Command::new(binary);
    command
        .arg("--log-level")
        .arg(log_level)
        .arg("--projects-file")
        .arg(projects_file)
        .arg("--nats-url")
        .arg(nats_url);
    if stepped {
        command.arg("--stepped-stdio");
    }
    if let Some((ca, cert, key)) = tls {
        command
            .arg("--nats-tls-ca")
            .arg(ca)
            .arg("--nats-tls-cert")
            .arg(cert)
            .arg("--nats-tls-key")
            .arg(key);
    }
    if let Some(value) = min_interval_ms {
        command.arg("--min-interval-ms").arg(value.to_string());
    }
    if let Some(value) = max_interval_ms {
        command.arg("--max-interval-ms").arg(value.to_string());
    }
    if let Some(value) = batch {
        command.arg("--batch").arg(value.to_string());
    }
    command
}

fn write_projects_file(specs: &[ProjectSpec]) -> anyhow::Result<PathBuf> {
    let sequence = PROJECT_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "wamn-dispatcher-projects-{}-{sequence}.json",
        std::process::id()
    ));
    let projects = specs
        .iter()
        .map(|spec| (spec.name.clone(), spec))
        .collect::<std::collections::BTreeMap<_, _>>();
    let json = serde_json::to_vec(&projects).context("encode dispatcher projects file")?;
    std::fs::write(&path, json)
        .with_context(|| format!("write dispatcher projects file {}", path.display()))?;
    Ok(path)
}

fn dispatcher_binary() -> OsString {
    if let Some(binary) = std::env::var_os(DISPATCHER_BINARY_ENV) {
        return binary;
    }
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("wamn-dispatcher")));
    sibling
        .filter(|path| path.is_file())
        .map(PathBuf::into_os_string)
        .unwrap_or_else(|| OsString::from("wamn-dispatcher"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatcher_command_preserves_the_stepped_process_contract() {
        let ca = PathBuf::from("/tls/ca");
        let cert = PathBuf::from("/tls/cert");
        let key = PathBuf::from("/tls/key");
        let command = dispatcher_command(
            OsStr::new("/proof/wamn-dispatcher"),
            std::path::Path::new("/proof/projects.json"),
            "nats://events",
            Some((&ca, &cert, &key)),
            Some(25),
            Some(2_000),
            Some(17),
            true,
            "error",
        );
        let command = command.as_std();
        assert_eq!(command.get_program(), "/proof/wamn-dispatcher");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                "--log-level",
                "error",
                "--projects-file",
                "/proof/projects.json",
                "--nats-url",
                "nats://events",
                "--stepped-stdio",
                "--nats-tls-ca",
                "/tls/ca",
                "--nats-tls-cert",
                "/tls/cert",
                "--nats-tls-key",
                "/tls/key",
                "--min-interval-ms",
                "25",
                "--max-interval-ms",
                "2000",
                "--batch",
                "17",
            ]
            .map(OsStr::new)
        );
    }

    #[test]
    fn integration_manifest_does_not_link_the_dispatcher_service() {
        let manifest = include_str!("../Cargo.toml");
        assert!(
            !manifest
                .lines()
                .any(|line| { line.trim_start().starts_with("wamn-dispatcher") }),
            "tests/integration must drive wamn-dispatcher through its executable boundary"
        );
    }
}
