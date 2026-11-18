//! Completion marker written after all package output is in place.

use std::collections::BTreeMap;
use std::fs;
use std::io::{ErrorKind, Write as _};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use wamn_digest::{Digest, bytes_sha256};

/// A finished build's input key and the files produced by that build.
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fingerprint {
    pub input: Digest,
    pub outputs: BTreeMap<PathBuf, Digest>,
}

fn path(output: &Path) -> Result<PathBuf> {
    let name = output
        .file_name()
        .context("build output has no directory name")?;
    let mut file = name.to_os_string();
    file.push(".fingerprint");
    Ok(output.with_file_name(file))
}

/// Serialize writers sharing the build output and the source history they materialize.
pub fn lock(output_root: &Path) -> Result<fs::File> {
    fs::create_dir_all(output_root)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(output_root.join(".build.lock"))?;
    lock.lock()?;
    Ok(lock)
}

/// Read the completion marker and validate the recorded paths and their presence.
pub fn read(output: &Path) -> Result<Fingerprint> {
    let marker = path(output)?;
    let fingerprint: Fingerprint = serde_json::from_slice(
        &fs::read(&marker).with_context(|| format!("read {}; run wamn build", marker.display()))?,
    )
    .with_context(|| format!("decode {}", marker.display()))?;
    ensure!(
        fingerprint.outputs.contains_key(Path::new("wamn.json")),
        "build has no wamn.json"
    );
    for file in fingerprint.outputs.keys() {
        ensure!(
            file.components()
                .all(|part| matches!(part, Component::Normal(_)))
                && fs::symlink_metadata(output.join(file)).is_ok_and(|metadata| metadata.is_file()),
            "missing or invalid build output {}; run wamn build",
            output.join(file).display()
        );
    }
    Ok(fingerprint)
}

/// Read and hash the exact bytes of a recorded artifact.
pub fn read_artifact(
    output: &Path,
    fingerprint: &Fingerprint,
    relative: &Path,
) -> Result<(Vec<u8>, Digest)> {
    ensure!(
        fingerprint.outputs.contains_key(relative),
        "{} is absent from the finished build",
        relative.display()
    );
    let path = output.join(relative);
    let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let digest = bytes_sha256(&bytes);
    verify_digest(output, fingerprint, relative, digest)?;
    Ok((bytes, digest))
}

/// Verify a digest computed by a consumer against the finished build.
pub fn verify_digest(
    output: &Path,
    fingerprint: &Fingerprint,
    relative: &Path,
    digest: Digest,
) -> Result<()> {
    ensure!(
        fingerprint.outputs.get(relative) == Some(&digest),
        "{} differs from the finished build; run wamn build",
        output.join(relative).display()
    );
    Ok(())
}

/// Remove the marker before changing any build output.
pub fn invalidate(output: &Path) -> Result<()> {
    match fs::remove_file(path(output)?) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Publish a successfully built output in one atomic write.
pub fn write(output: &Path, fingerprint: &Fingerprint) -> Result<()> {
    write_atomic(&path(output)?, &serde_json::to_vec(fingerprint)?)
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("atomic output has no parent")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    Ok(())
}
