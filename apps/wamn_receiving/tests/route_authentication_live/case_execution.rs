//! Execution and response helpers shared by local and deployed cases.
use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use tokio::process::Command;

pub(super) async fn with_signals(
    evidence: &Path,
    operation: impl std::future::Future<Output = anyhow::Result<()>>,
) -> anyhow::Result<()> {
    use futures_util::FutureExt as _;
    use std::os::unix::fs::DirBuilderExt as _;
    use tokio::signal::unix::{SignalKind, signal};
    fs::DirBuilder::new().mode(0o700).create(evidence)?;
    let test_log = evidence.join("test.log");
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(fs::File::create(&test_log)?)
        .finish();
    let _subscriber = tracing::subscriber::set_default(subscriber);
    tracing::info!("Receiving test started");
    wash_runtime::init_crypto();
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut hangup = signal(SignalKind::hangup())?;
    let result = {
        let operation = std::panic::AssertUnwindSafe(operation).catch_unwind();
        tokio::select! {
            result = operation => result.unwrap_or_else(|_| Err(anyhow::anyhow!("the Receiving test panicked"))),
            _ = interrupt.recv() => Err(anyhow::anyhow!("the Receiving test received SIGINT")),
            _ = terminate.recv() => Err(anyhow::anyhow!("the Receiving test received SIGTERM")),
            _ = hangup.recv() => Err(anyhow::anyhow!("the Receiving test received SIGHUP")),
        }
    };
    let result = result.with_context(|| {
        format!(
            "test log: {}; identity log: {}",
            test_log.display(),
            evidence.join("identity.log").display()
        )
    });
    if let Err(error) = &result {
        tracing::error!(failure = %format!("{error:#}"), "Receiving test failed");
    } else {
        tracing::info!("Receiving test passed");
    }
    if let Err(error) = &result
        && evidence.is_dir()
    {
        fs::write(
            evidence.join("failure.json"),
            serde_json::to_vec_pretty(&json!({
                "verdict":"fail", "failure":format!("{error:#}"),
            }))?,
        )?;
    }
    result
}

pub(super) fn validate_response(
    status: u16,
    content_type: &str,
    body: &[u8],
) -> anyhow::Result<()> {
    ensure!(
        status == 404
            && content_type == "application/json"
            && body == br#"{"error":{"code":"route-not-found"}}"#,
        "the owned NodePort must return the released route's exact HTTP 404 response"
    );
    Ok(())
}

pub(super) async fn checked(command: &mut Command) -> anyhow::Result<Vec<u8>> {
    let program = command
        .as_std()
        .get_program()
        .to_string_lossy()
        .into_owned();
    let output = command
        .kill_on_drop(true)
        .output()
        .await
        .context("run the Receiving test command")?;
    // The error reaches failure.json in the evidence directory, so a failed
    // command keeps the end of its stderr there (wamn-ld93.33.8).
    let stderr = String::from_utf8_lossy(&output.stderr);
    let tail = &stderr[stderr.floor_char_boundary(stderr.len().saturating_sub(STDERR_TAIL))..];
    ensure!(
        output.status.success(),
        "Receiving test command {program} failed with {}: {}",
        output.status,
        tail.trim()
    );
    Ok(output.stdout)
}

/// The bytes of stderr that a failed command keeps in its error.
const STDERR_TAIL: usize = 4096;
