//! Child execution and libtest result validation.

use anyhow::{Context as _, ensure};
use clap::Args;
use rustix::process::{Pid, Signal, WaitOptions};
use std::ffi::OsString;
use std::io::Write as _;
use std::os::fd::AsFd as _;
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt as _};
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
    output_command_before(&mut command, deadline, None).await
}

pub async fn output_command_before(
    command: &mut Command,
    deadline: Instant,
    label: Option<&str>,
) -> anyhow::Result<String> {
    let output = execute_inner(command, Some(deadline), false, label)
        .await
        .with_context(|| crate::tasks::describe(command))?;
    ensure!(
        output.status.success(),
        "{}: {}\n{}{}",
        crate::tasks::describe(command),
        output.status,
        output.text,
        output.diagnostics
    );
    Ok(output.text)
}

// A separate subreaper for every command keeps parallel targets' descendants apart.
// Closing its input requests cleanup, even when the calling future or process dies.
pub async fn supervise(arguments: &[OsString]) -> anyhow::Result<i32> {
    rustix::process::set_child_subreaper(Some(rustix::process::getpid()))?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut exited = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::child())?;
    let input = std::io::stdin().as_fd().try_clone_to_owned()?;
    let input = std::os::unix::net::UnixStream::from(input);
    input.set_nonblocking(true)?;
    let mut input = tokio::net::UnixStream::from_std(input)?;
    let mut buffer = [0_u8];
    let mut child = std::process::Command::new(&arguments[0])
        .args(&arguments[1..])
        .stdin(Stdio::null())
        .process_group(0)
        .spawn()
        .context("start supervised child")?;
    let result: anyhow::Result<i32> = async {
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)));
            }
            tokio::select! {
                count = input.read(&mut buffer) => { count?; return Ok(1); }
                signal = tokio::signal::ctrl_c() => { signal?; return Ok(130); }
                _ = terminate.recv() => return Ok(143),
                _ = exited.recv() => {}
            }
        }
    }
    .await;
    // std::process::Child does not reap on drop. This subreaper owns all wait calls.
    drop(child);
    reap_descendants().await?;
    result
}

fn children() -> anyhow::Result<Vec<Pid>> {
    let mut children = Vec::new();
    for task in std::fs::read_dir("/proc/self/task")? {
        let path = task?.path().join("children");
        match std::fs::read_to_string(path) {
            Ok(text) => {
                for pid in text.split_whitespace() {
                    children.push(Pid::from_raw(pid.parse()?).context("child pid")?);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(children)
}

async fn reap_descendants() -> anyhow::Result<()> {
    for pid in children()? {
        let _ = rustix::process::kill_process(pid, Signal::TERM);
    }
    let grace = Instant::now() + Duration::from_secs(1);
    loop {
        loop {
            match rustix::process::wait(WaitOptions::NOHANG) {
                Ok(Some(_)) | Err(rustix::io::Errno::INTR) => {}
                Ok(None) => break,
                Err(rustix::io::Errno::CHILD) => return Ok(()),
                Err(error) => return Err(error.into()),
            }
        }
        if Instant::now() >= grace {
            // A killed parent's children become ours, regardless of their groups or sessions.
            // Repeat until wait reports no children, rather than trusting a process snapshot.
            for pid in children()? {
                let _ = rustix::process::kill_process(pid, Signal::KILL);
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
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
    pub diagnostics: String,
    pub status: ExitStatus,
}

pub async fn execute(command: &mut Command) -> anyhow::Result<String> {
    execute_checked(command, None, None).await
}

/// Report compiler activity while retaining the command's complete labeled output.
pub async fn execute_labeled(command: &mut Command, label: &str) -> anyhow::Result<String> {
    execute_checked(command, None, Some(label)).await
}

pub async fn execute_before(command: &mut Command, deadline: Instant) -> anyhow::Result<String> {
    execute_checked(command, Some(deadline), None).await
}

pub async fn execute_labeled_before(
    command: &mut Command,
    deadline: Instant,
    label: &str,
) -> anyhow::Result<String> {
    execute_checked(command, Some(deadline), Some(label)).await
}

async fn execute_checked(
    command: &mut Command,
    deadline: Option<Instant>,
    label: Option<&str>,
) -> anyhow::Result<String> {
    let output = execute_inner(command, deadline, true, label).await?;
    ensure!(
        output.status.success(),
        "{}: task child exited {}\n{}",
        label.unwrap_or("command"),
        output.status,
        output.text
    );
    Ok(output.text)
}

pub async fn capture_before(command: &mut Command, deadline: Instant) -> anyhow::Result<Captured> {
    // Cargo renders diagnostics on stderr. Keep its JSON artifact stream out of the run log.
    execute_inner(command, Some(deadline), false, None).await
}

async fn execute_inner(
    command: &mut Command,
    deadline: Option<Instant>,
    combined: bool,
    label: Option<&str>,
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
    let report_activity = deadline.is_none() && label.is_some();
    let label = label.map_or_else(|| crate::tasks::describe(command), str::to_owned);
    let original = command.as_std();
    // The selection rebuilds xtask while it runs, which replaces the file
    // that current_exe names. /proc/self/exe still runs this program.
    let mut supervisor = Command::new("/proc/self/exe");
    supervisor
        .arg("supervise")
        .arg("--")
        .arg(original.get_program())
        .args(original.get_args());
    if let Some(directory) = original.get_current_dir() {
        supervisor.current_dir(directory);
    }
    for (name, value) in original.get_envs() {
        if let Some(value) = value {
            supervisor.env(name, value);
        } else {
            supervisor.env_remove(name);
        }
    }
    let (input, control) = std::os::unix::net::UnixStream::pair()?;
    supervisor
        .stdin(Stdio::from(std::os::fd::OwnedFd::from(control)))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = supervisor.spawn().context("start task supervisor")?;
    let mut captured = Vec::new();
    let mut diagnostics = Vec::new();
    let mut record = |error: bool, bytes: Vec<u8>| {
        if report_activity {
            println!("compiler output received: {label}");
        }
        // Cargo artifacts are stdout only; diagnostics stay separate from its JSON.
        if combined || !error {
            captured.extend(bytes);
        } else {
            diagnostics.extend(bytes);
        }
    };
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
    let result: anyhow::Result<ExitStatus> = async {
        let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
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
            record(error, bytes);
        }
        let status = tokio::time::timeout_at(deadline.map_or(Instant::now() + bound, |deadline| deadline.min(Instant::now() + bound)), child.wait())
            .await
            .context("child exceeded run deadline or output silence limit")??;
        Ok(status)
    }.await;
    drop(input);
    // The supervisor returns only after every descendant exits and is reaped.
    // Dropping this future also closes the input; Tokio reaps the supervisor on exit.
    // Keep reading during cleanup so the failure also carries shutdown diagnostics.
    while let Some((error, bytes)) = receiver.recv().await {
        record(error, bytes);
    }
    let reaped = child.wait().await.context("reap task supervisor");
    let streams = async {
        stdout.await??;
        stderr.await??;
        anyhow::Ok(())
    }
    .await;
    let text = String::from_utf8_lossy(&captured).into_owned();
    let diagnostics = String::from_utf8_lossy(&diagnostics).into_owned();
    let displayed: &str = if combined { &text } else { &diagnostics };
    if !displayed.is_empty() {
        let mut output = std::io::stdout().lock();
        writeln!(output, "--- {label} ---\n{displayed}\n--- end {label} ---")?;
    }
    let status = result.with_context(|| format!("{label}\n{text}{diagnostics}"))?;
    reaped?;
    streams?;
    Ok(Captured {
        text,
        diagnostics,
        status,
    })
}
