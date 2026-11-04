//! Provisioning output files and private credential replacement.

use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

use anyhow::Context as _;

use super::{AtomicU64, File, OpenOptions, Ordering, OsString, Path, PathBuf, Permissions, Value};

pub fn ensure_secret_path(path: &Path, flag: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        path.as_os_str() != "-",
        "{flag} must name a file; '-' and stdout are forbidden for credentials"
    );
    Ok(())
}

pub fn ensure_distinct_secret_paths<const N: usize>(
    paths: [(&str, Option<&Path>); N],
) -> anyhow::Result<()> {
    let mut seen = Vec::with_capacity(N);
    for (flag, path) in paths {
        let Some(path) = path else {
            continue;
        };
        let absolute = std::path::absolute(path)
            .with_context(|| format!("resolve credential output path for {flag}"))?;
        let parent = absolute
            .parent()
            .context("credential output path has no parent directory")?;
        let file_name = absolute
            .file_name()
            .context("credential output path has no file name")?;
        let comparable = std::fs::canonicalize(parent)
            .with_context(|| format!("resolve credential output parent for {flag}"))?
            .join(file_name);
        if let Some((other_flag, _)) = seen
            .iter()
            .find(|(_, other_path)| other_path == &comparable)
        {
            anyhow::bail!("{flag} and {other_flag} must name distinct credential output files");
        }
        seen.push((flag, comparable));
    }
    Ok(())
}

pub(super) static SECRET_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn create_secret_temp(path: &Path) -> anyhow::Result<(PathBuf, File)> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .context("credential output path has no file name")?;

    for _ in 0..128 {
        let sequence = SECRET_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut temp_name = OsString::from(".");
        temp_name.push(file_name);
        temp_name.push(format!(".wamn-tmp-{}-{sequence}", std::process::id()));
        let temp_path = parent.join(temp_name);
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(&temp_path)
        {
            Ok(file) => return Ok((temp_path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("create credential output beside {}", path.display())
                });
            }
        }
    }
    anyhow::bail!(
        "could not allocate a temporary credential output beside {}",
        path.display()
    )
}

pub fn write_secret_json(path: &Path, doc: &Value) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(doc).context("serialize Secret JSON")?;
    bytes.push(b'\n');
    let (temp_path, mut file) = create_secret_temp(path)?;
    let result = (|| -> anyhow::Result<()> {
        file.set_permissions(Permissions::from_mode(0o600))
            .with_context(|| format!("set credential output mode on {}", temp_path.display()))?;
        file.write_all(&bytes)
            .with_context(|| format!("write credential output {}", temp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("sync credential output {}", temp_path.display()))?;
        drop(file);
        std::fs::rename(&temp_path, path)
            .with_context(|| format!("install credential output {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

/// Write a rendered artifact to a path. An absent path or `-` writes nothing.
pub(crate) fn write_output(path: Option<&Path>, text: &str) -> anyhow::Result<()> {
    if let Some(path) = path.filter(|path| path.as_os_str() != "-") {
        std::fs::write(path, text).with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
}

#[expect(
    missing_debug_implementations,
    reason = "carries minted PATs and password-bearing URLs; no derived formatter may print them"
)]
pub struct ProvisionedRoute {
    pub database_url: String,
    pub token: String,
    pub token_prefix: String,
    pub principal_subject: String,
    pub management_token: Option<String>,
    pub management_principal_subject: Option<String>,
}

pub fn read_json(path: &Path) -> anyhow::Result<Value> {
    serde_json::from_slice(
        &std::fs::read(path).with_context(|| format!("read {}", path.display()))?,
    )
    .with_context(|| format!("parse {}", path.display()))
}

pub fn secret_value(path: &Path, key: &str) -> anyhow::Result<String> {
    read_json(path)?["stringData"][key]
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("{} carries stringData.{key}", path.display()))
}

pub fn secret_annotation(path: &Path, key: &str) -> anyhow::Result<String> {
    read_json(path)?["metadata"]["annotations"][key]
        .as_str()
        .map(str::to_owned)
        .with_context(|| format!("{} carries annotation {key}", path.display()))
}
