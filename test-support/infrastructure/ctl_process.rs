//! Process-boundary adapter for integration tests that drive `wamn-ctl`.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::Output;

use anyhow::Context as _;
use tokio::process::Command;

const CTL_BINARY_ENV: &str = "WAMN_CTL_BIN";

/// Run `wamn-ctl` with `args`, returning its captured process output.
pub async fn run<I, S>(args: I) -> anyhow::Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let binary = ctl_binary();
    run_binary("wamn-ctl", &binary, args).await
}

async fn run_binary<I, S>(name: &str, binary: &OsStr, args: I) -> anyhow::Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    ctl_command(binary, args)
        .output()
        .await
        .with_context(|| format!("launch {name} process {}", binary.to_string_lossy()))
}

/// Run `wamn-ctl` and require a successful exit status.
pub async fn run_resulted<I, S>(args: I) -> anyhow::Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = run(args).await?;
    require_success("wamn-ctl", output)
}

fn require_success(name: &str, output: Output) -> anyhow::Result<Output> {
    if output.status.success() {
        return Ok(output);
    }
    anyhow::bail!(
        "{name} failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    )
}

fn ctl_binary() -> OsString {
    binary(CTL_BINARY_ENV, "wamn-ctl")
}

fn binary(env: &str, name: &str) -> OsString {
    if let Some(binary) = std::env::var_os(env) {
        return binary;
    }

    let sibling = std::env::current_exe().ok().and_then(|exe| {
        let dir = exe.parent()?;
        let direct = dir.join(name);
        if direct.is_file() {
            return Some(direct);
        }
        (dir.file_name().and_then(OsStr::to_str) == Some("deps"))
            .then(|| dir.parent().map(|parent| parent.join(name)))
            .flatten()
            .filter(|path| path.is_file())
    });
    sibling.map_or_else(|| OsString::from(name), PathBuf::into_os_string)
}

fn ctl_command<I, S>(binary: &OsStr, args: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new(binary);
    command.arg("--log-level").arg("error").args(args);
    command
}
