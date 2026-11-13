//! Child execution and libtest result validation.

use anyhow::{Context as _, ensure};
use clap::Args;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::Command;

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
    let text = execute(&mut command).await?;
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

pub async fn execute(command: &mut Command) -> anyhow::Result<String> {
    let seconds = std::env::var("WAMN_TEST_SILENCE_SECONDS")
        .map_or(Ok(1800.0), |value| value.parse::<f64>())?;
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
    let result: anyhow::Result<String> = async {
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
            signal = tokio::signal::ctrl_c() => { signal?; anyhow::bail!("task interrupted"); }
            _ = terminate.recv() => anyhow::bail!("task terminated"),
        };
        let Some((error, bytes)) = item else {
            break;
        };
        if error {
            tokio::io::stderr().write_all(&bytes).await?;
        } else {
            tokio::io::stdout().write_all(&bytes).await?;
        }
        captured.extend(bytes);
    }
    stdout.await??;
    stderr.await??;
    let status = tokio::time::timeout(bound, child.wait())
        .await
        .context("child exceeded output silence limit")??;
    ensure!(status.success(), "task child exited {status}");
    Ok(String::from_utf8_lossy(&captured).into_owned())
    }.await;
    if result.is_err() {
        let _ = rustix::process::kill_process_group(group.0, rustix::process::Signal::TERM);
        let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
    }
    result
}
