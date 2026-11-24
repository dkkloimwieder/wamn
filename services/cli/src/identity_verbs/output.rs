//! Provisioning output files and private credential replacement.

mod plan;
pub(super) use plan::ensure_secret_path;

#[cfg(test)]
mod tests;

use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

use anyhow::Context as _;

use serde_json::Value;
use std::ffi::OsString;
use std::fs::{File, OpenOptions, Permissions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SECRET_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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

pub(super) fn write_secret_json(path: &Path, doc: &Value) -> anyhow::Result<()> {
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
