//! The stand-in artifact digest of a fixture tree that a test applies as a
//! release.

use std::path::Path;

use anyhow::Context as _;

/// The stand-in artifact digest of the package tree at `root`.
///
/// No registry records it. It is the SHA-256 of the length-framed `wamn.json`
/// and migration bytes, so a test that changes those bytes under one
/// coordinate presents another digest, as a second push would. It is not the
/// package artifact digest of the tree
/// (`wamn_control::package_artifact::package_artifact_digest`).
///
/// # Errors
///
/// When the tree cannot be read.
pub fn fixture_artifact_digest(root: &Path) -> anyhow::Result<String> {
    wamn_schema_generator::receipt::check_current(root)?;
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    let mut frame = |bytes: &[u8]| {
        context.update(&u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
        context.update(bytes);
    };
    let manifest_path = wamn_schema_generator::package_manifest_path(root);
    frame(
        &std::fs::read(&manifest_path)
            .with_context(|| format!("read {}", manifest_path.display()))?,
    );
    let migrations = root.join("migrations");
    let mut names = Vec::new();
    if migrations.exists() {
        for entry in std::fs::read_dir(&migrations)
            .with_context(|| format!("read {}", migrations.display()))?
        {
            names.push(
                entry?
                    .file_name()
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("package migration file name is not UTF-8"))?,
            );
        }
    }
    names.sort_unstable();
    for name in names {
        frame(format!("migrations/{name}").as_bytes());
        let path = migrations.join(&name);
        frame(&std::fs::read(&path).with_context(|| format!("read {}", path.display()))?);
    }
    Ok(format!("sha256:{}", hex::encode(context.finish())))
}
