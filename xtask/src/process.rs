//! Child execution and libtest result validation.

use anyhow::{Context as _, ensure};
use clap::Args;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::Command;
use tokio::time::Instant;

/// Maximum elapsed time for one test selection or required test command.
pub const TEST_RUN_TIMEOUT: Duration = Duration::from_mins(30);

#[derive(Debug, Args)]
pub struct TestArgs {
    #[arg(long)]
    expected_test: Option<String>,
    #[arg(long)]
    each_target: bool,
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

pub async fn require(root: &Path, args: &TestArgs) -> anyhow::Result<()> {
    let mut command = Command::new(&args.command[0]);
    command.current_dir(root).args(&args.command[1..]);
    let text = execute_before(&mut command, Instant::now() + TEST_RUN_TIMEOUT).await?;
    validate(&text, args.expected_test.as_deref(), args.each_target)
}

pub fn validate(text: &str, expected: Option<&str>, each_target: bool) -> anyhow::Result<()> {
    let result = regex::Regex::new(r"(?m)^test result: ok\. (\d+) passed; 0 failed;")?;
    let counts = result
        .captures_iter(text)
        .map(|row| row[1].parse::<u64>())
        .collect::<Result<Vec<_>, _>>()?;
    ensure!(
        counts.iter().any(|count| *count > 0),
        "test command reported no executed passing cases"
    );
    ensure!(
        !each_target || !counts.contains(&0),
        "test target reported no executed passing cases"
    );
    if let Some(expected) = expected {
        let prefix = format!("test {expected} ... ");
        ensure!(
            text.lines()
                .filter_map(|line| line.strip_prefix(&prefix))
                .any(|result| !result.starts_with("ignored")),
            "required test did not pass: {expected}"
        );
    }
    Ok(())
}

pub async fn run(root: &Path, program: &str, args: &[&str]) -> anyhow::Result<()> {
    execute(Command::new(program).current_dir(root).args(args)).await?;
    Ok(())
}

pub async fn output(root: &Path, program: &str, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new(program)
        .current_dir(root)
        .args(args)
        .output()
        .await
        .with_context(|| format!("run {program} {args:?}"))?;
    ensure!(
        output.status.success(),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).context("child output is not UTF-8")
}

pub async fn output_before(
    root: &Path,
    program: &str,
    args: &[&str],
    deadline: Instant,
) -> anyhow::Result<String> {
    let mut command = Command::new(program);
    command.current_dir(root).args(args);
    let output = capture_before(&mut command, deadline)
        .await
        .with_context(|| format!("run {program} {args:?}"))?;
    ensure!(
        output.status.success(),
        "{program} {args:?}: {}",
        output.status
    );
    Ok(output.text)
}

struct Group(rustix::process::Pid);
impl Drop for Group {
    fn drop(&mut self) {
        let _ = rustix::process::kill_process_group(self.0, rustix::process::Signal::TERM);
        let _ = rustix::process::kill_process_group(self.0, rustix::process::Signal::KILL);
    }
}

async fn read(
    mut pipe: impl AsyncRead + Unpin,
    stderr: bool,
    sender: tokio::sync::mpsc::Sender<(bool, Vec<u8>)>,
) -> std::io::Result<()> {
    let mut buffer = [0; 8192];
    loop {
        let count = pipe.read(&mut buffer).await?;
        if count == 0 {
            return Ok(());
        }
        if sender
            .send((stderr, buffer[..count].to_vec()))
            .await
            .is_err()
        {
            return Ok(());
        }
    }
}

/// Captured build output. Cargo can emit runnable artifacts before another target fails.
#[derive(Debug)]
pub struct Captured {
    pub text: String,
    pub status: ExitStatus,
}

pub async fn execute(command: &mut Command) -> anyhow::Result<String> {
    execute_checked(command, None).await
}

pub async fn execute_before(command: &mut Command, deadline: Instant) -> anyhow::Result<String> {
    execute_checked(command, Some(deadline)).await
}

async fn execute_checked(
    command: &mut Command,
    deadline: Option<Instant>,
) -> anyhow::Result<String> {
    let output = execute_inner(command, deadline, true).await?;
    ensure!(
        output.status.success(),
        "task child exited {}",
        output.status
    );
    Ok(output.text)
}

pub async fn capture_before(command: &mut Command, deadline: Instant) -> anyhow::Result<Captured> {
    // Cargo renders diagnostics on stderr. Keep its JSON artifact stream out of the run log.
    execute_inner(command, Some(deadline), false).await
}

async fn execute_inner(
    command: &mut Command,
    deadline: Option<Instant>,
    stream_stdout: bool,
) -> anyhow::Result<Captured> {
    ensure!(
        deadline.is_none_or(|deadline| Instant::now() < deadline),
        "run deadline exceeded"
    );
    // Existing build and operator commands keep their silence budget.
    // Test selection and result validation supply a deadline and use two minutes.
    let default_silence = if deadline.is_some() { 120.0 } else { 1800.0 };
    let seconds = std::env::var("WAMN_TEST_SILENCE_SECONDS")
        .map_or(Ok(default_silence), |value| value.parse::<f64>())?;
    ensure!(
        seconds.is_finite() && seconds > 0.0,
        "WAMN_TEST_SILENCE_SECONDS must be finite and positive"
    );
    let bound = Duration::try_from_secs_f64(seconds)?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0);
    let mut child = command.spawn().context("start task child")?;
    let group = Group(
        rustix::process::Pid::from_raw(i32::try_from(child.id().context("child pid")?)?)
            .context("child pid")?,
    );
    let result: anyhow::Result<Captured> = async {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
        let stdout = tokio::spawn(read(
            child.stdout.take().context("child stdout")?,
            false,
            sender.clone(),
        ));
        let stderr = tokio::spawn(read(
            child.stderr.take().context("child stderr")?,
            true,
            sender,
        ));
        let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut captured = Vec::new();
        loop {
            let item = tokio::select! {
                item = tokio::time::timeout(bound, receiver.recv()) => item.context("child exceeded output silence limit")?,
                () = async {
                    if let Some(deadline) = deadline { tokio::time::sleep_until(deadline).await; }
                    else { std::future::pending::<()>().await; }
                } => anyhow::bail!("run deadline exceeded"),
                signal = tokio::signal::ctrl_c() => { signal?; anyhow::bail!("task interrupted"); }
                _ = terminate.recv() => anyhow::bail!("task terminated"),
            };
            let Some((error, bytes)) = item else {
                break;
            };
            if error {
                tokio::io::stderr().write_all(&bytes).await?;
            } else if stream_stdout {
                tokio::io::stdout().write_all(&bytes).await?;
            }
            // Build artifacts are stdout only; rendered diagnostics need no JSON parsing.
            if stream_stdout || !error {
                captured.extend(bytes);
            }
        }
        stdout.await??;
        stderr.await??;
        let status = tokio::time::timeout_at(deadline.map_or(Instant::now() + bound, |deadline| deadline.min(Instant::now() + bound)), child.wait())
            .await
            .context("child exceeded run deadline or output silence limit")??;
        Ok(Captured { text: String::from_utf8_lossy(&captured).into_owned(), status })
    }.await;
    if result.is_err() || result.as_ref().is_ok_and(|output| !output.status.success()) {
        let _ = rustix::process::kill_process_group(group.0, rustix::process::Signal::TERM);
        let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
    }
    result
}
