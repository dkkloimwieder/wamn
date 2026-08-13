//! Process-boundary adapter for integration proofs that drive `wamn-ctl`.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::Output;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Context as _;
use tokio::process::Command;

const CTL_BINARY_ENV: &str = "WAMN_CTL_BIN";
const CTL_OPS_BINARY_ENV: &str = "WAMN_CTL_OPS_BIN";
static INPUT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Run `wamn-ctl` with `args`, returning its captured process output.
pub async fn run<I, S>(args: I) -> anyhow::Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let binary = ctl_binary();
    run_binary("wamn-ctl", &binary, args).await
}

/// Run `wamn-ctl-ops` and require a successful exit status.
pub async fn run_ops_checked<I, S>(args: I) -> anyhow::Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let binary = ctl_ops_binary();
    let output = run_binary("wamn-ctl-ops", &binary, args).await?;
    require_success("wamn-ctl-ops", output)
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
pub async fn run_checked<I, S>(args: I) -> anyhow::Result<Output>
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

/// Reconcile replica identity through the public ctl command.
pub async fn reconcile_replica_identity(
    admin_url: &str,
    catalog: &wamn_schema_model::Catalog,
    schema: &str,
) -> anyhow::Result<Output> {
    let sequence = INPUT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "wamn-ctl-ri-{}-{sequence}.json",
        std::process::id()
    ));
    std::fs::write(&path, catalog.to_json())
        .with_context(|| format!("write ctl catalog input {}", path.display()))?;
    let output = run_checked([
        OsString::from("reconcile-replica-identity"),
        OsString::from("--admin-database-url"),
        OsString::from(admin_url),
        OsString::from("--catalog"),
        path.as_os_str().to_owned(),
        OsString::from("--schema"),
        OsString::from(schema),
    ])
    .await;
    let cleanup = std::fs::remove_file(&path)
        .with_context(|| format!("remove ctl catalog input {}", path.display()));
    match (output, cleanup) {
        (Ok(output), Ok(())) => Ok(output),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(cleanup_error)) => Err(error).context(format!(
            "remove ctl catalog input also failed: {cleanup_error:#}"
        )),
    }
}

fn ctl_binary() -> OsString {
    binary(CTL_BINARY_ENV, "wamn-ctl")
}

fn ctl_ops_binary() -> OsString {
    binary(CTL_OPS_BINARY_ENV, "wamn-ctl-ops")
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
    sibling
        .map(PathBuf::into_os_string)
        .unwrap_or_else(|| OsString::from(name))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_preserves_the_public_cli_boundary() {
        let command = ctl_command(
            OsStr::new("/proof/wamn-ctl"),
            ["migrate-catalog", "--tenant", "tenant-a"],
        );
        let command = command.as_std();
        assert_eq!(command.get_program(), "/proof/wamn-ctl");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                "--log-level",
                "error",
                "migrate-catalog",
                "--tenant",
                "tenant-a",
            ]
            .map(OsStr::new)
        );
    }

    #[test]
    fn command_preserves_the_ops_cli_boundary() {
        let command = ctl_command(
            OsStr::new("/proof/wamn-ctl-ops"),
            ["prune-run-history", "--tenant", "tenant-a"],
        );
        let command = command.as_std();
        assert_eq!(command.get_program(), "/proof/wamn-ctl-ops");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                "--log-level",
                "error",
                "prune-run-history",
                "--tenant",
                "tenant-a",
            ]
            .map(OsStr::new)
        );
    }
}
