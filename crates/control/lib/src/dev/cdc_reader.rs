//! The CDC reader the development loop runs beside its host (wamn-hw3n).
//!
//! The reader is the cluster's `wamn-cdc-reader` binary with the registration
//! that `enable-cdc-project-env` recorded, so the loop proves what the cluster
//! runs. The loop applies the verb's CDC SQL to every target it creates, and
//! the target lease drops the slot before it replaces the database.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, bail};
use tokio::process::{Child, Command};
use tokio_postgres::NoTls;

use super::config::{CdcReader, DevConfig};

/// The log line of a reader that streams from its slot.
const STREAMING: &str = "walsender session open";
const STARTUP_BUDGET: Duration = Duration::from_secs(60);

/// Apply the CDC SQL of `enable-cdc-project-env` to the current target. Every
/// statement is idempotent, so a kept target takes it again unchanged.
pub(crate) async fn apply_cdc_sql(config: &DevConfig, reader: &CdcReader) -> anyhow::Result<()> {
    let sql = std::fs::read_to_string(&reader.cdc_sql_file)
        .with_context(|| format!("read {}", reader.cdc_sql_file.display()))?;
    let (client, connection) = tokio_postgres::connect(config.target_database_url(), NoTls)
        .await
        .context("connect to the target for its CDC SQL")?;
    let driver = tokio::spawn(connection);
    // One line is one statement group of the verb's file. Each runs in its own
    // transaction, as `psql` runs the file in the cluster, because a slot
    // cannot be made in a transaction that has written.
    let result = async {
        for line in sql.lines().filter(|line| !line.trim().is_empty()) {
            client
                .batch_execute(line)
                .await
                .context("apply the CDC SQL to the target")?;
        }
        Ok(())
    }
    .await;
    drop(client);
    driver.abort();
    result
}

/// One running reader. Dropping it kills the process.
#[derive(Debug)]
pub(crate) struct CdcReaderProcess {
    child: Child,
    log: PathBuf,
    /// Where this start's lines begin in the shared log.
    offset: usize,
}

impl CdcReaderProcess {
    /// Start the reader and wait until it streams from its slot.
    pub(crate) async fn start(
        config: &DevConfig,
        reader: &CdcReader,
        log: &Path,
    ) -> anyhow::Result<Self> {
        let output = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .with_context(|| format!("open {}", log.display()))?;
        let offset = usize::try_from(output.metadata()?.len())?;
        let identity = config.activation_identity();
        let mut command = Command::new(&reader.binary);
        command
            .env_clear()
            .args(["--org", &identity.org])
            .args(["--project", &identity.project])
            .args(["--env", &identity.environment])
            .args(["--stream-replicas", &config.stream_replicas().to_string()])
            .args(["--dup-window-secs", &config.dup_window_secs().to_string()])
            .env("WAMN_SYSTEM_URL", &reader.system_database_url)
            .env("WAMN_CDC_URL", &reader.cdc_url)
            .env("WAMN_EVT_NATS_URL", config.event_nats_url())
            .env("WAMN_EVT_NATS_USERNAME", &reader.nats_username)
            .env("WAMN_EVT_NATS_PASSWORD_FILE", &reader.nats_password_file)
            .stdin(Stdio::null())
            .stdout(output.try_clone()?)
            .stderr(output)
            .kill_on_drop(true);
        let child = command
            .spawn()
            .with_context(|| format!("start {}", reader.binary.display()))?;
        let mut process = Self {
            child,
            log: log.to_owned(),
            offset,
        };
        process.wait_streaming().await?;
        tracing::info!(log = %log.display(), "CDC reader streaming");
        Ok(process)
    }

    async fn wait_streaming(&mut self) -> anyhow::Result<()> {
        let deadline = tokio::time::Instant::now() + STARTUP_BUDGET;
        loop {
            if let Some(status) = self.child.try_wait()? {
                bail!(
                    "the CDC reader exited with {status} before it streamed; see {}",
                    self.log.display()
                );
            }
            if std::fs::read(&self.log).is_ok_and(|bytes| {
                bytes
                    .get(self.offset..)
                    .is_some_and(|bytes| String::from_utf8_lossy(bytes).contains(STREAMING))
            }) {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                bail!(
                    "the CDC reader did not stream within {STARTUP_BUDGET:?}; see {}",
                    self.log.display()
                );
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// Stop the reader and wait for it to exit.
    pub(crate) async fn stop(mut self) -> anyhow::Result<()> {
        self.child.start_kill().context("stop the CDC reader")?;
        self.child.wait().await.context("wait for the CDC reader")?;
        Ok(())
    }
}
