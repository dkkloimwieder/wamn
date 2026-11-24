use super::{BTreeMap, Path, ensure};
use anyhow::Context as _;

/// The package id and version of a package manifest.
pub(super) fn package_coordinate(
    manifest: &[u8],
) -> anyhow::Result<(
    wamn_package::PackageName,
    wamn_package::package_version::PackageVersion,
)> {
    let document: wamn_package::manifest::OperationOwners = serde_json::from_slice(manifest)?;
    Ok((document.package.id, document.package.version))
}

/// One deterministic tar of `entries`, keyed by package-relative path.
pub(super) fn tar_layer(
    title: &str,
    entries: &BTreeMap<String, Vec<u8>>,
) -> anyhow::Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, data) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        builder
            .append_data(&mut header, name, data.as_slice())
            .with_context(|| format!("add {name} to the {title} layer"))?;
    }
    builder
        .into_inner()
        .with_context(|| format!("finish the {title} layer"))
}

/// Check the tar path before comparing or unpacking its bytes.
pub(super) fn migration_path<'entry, R: std::io::Read>(
    entry: &'entry tar::Entry<'_, R>,
) -> anyhow::Result<std::borrow::Cow<'entry, Path>> {
    let path = entry.path().context("read the migration path")?;
    ensure!(
        path.components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
            && super::MIGRATION_PATHS.iter().any(|directory| {
                path.strip_prefix(directory)
                    .is_ok_and(|file| !file.as_os_str().is_empty())
            }),
        "migration entry {} must be inside migrations/ or history/",
        path.display()
    );
    Ok(path)
}

/// Preserve every previous filename and byte. New SQLx migration versions must sort
/// above the highest version from either directory.
pub(super) fn validate_append(
    previous: &[u8],
    current: &BTreeMap<String, Vec<u8>>,
) -> anyhow::Result<()> {
    use std::io::Read as _;
    let mut archive = tar::Archive::new(previous);
    let mut existing = std::collections::BTreeSet::new();
    let mut highest = None;
    let mut buffer = [0; 4096];
    for entry in archive
        .entries()
        .context("read the predecessor migration tar")?
    {
        let mut entry = entry.context("read a predecessor migration")?;
        ensure!(
            entry.header().entry_type() == tar::EntryType::Regular,
            "predecessor entry is not a regular file"
        );
        let path = migration_path(&entry)?;
        let name = path
            .to_str()
            .context("predecessor migration name is not UTF-8")?
            .to_owned();
        let bytes = current
            .get(&name)
            .with_context(|| format!("removed migration {name}"))?;
        ensure!(
            entry.size() == u64::try_from(bytes.len())?,
            "changed migration {name}"
        );
        for chunk in bytes.chunks(buffer.len()) {
            entry.read_exact(&mut buffer[..chunk.len()])?;
            ensure!(buffer[..chunk.len()] == *chunk, "changed migration {name}");
        }
        if let Some(version) = migration_version(&name)? {
            highest = highest.max(Some(version));
        }
        ensure!(
            existing.insert(name),
            "predecessor migration tar repeats an entry"
        );
    }
    for name in current.keys().filter(|name| !existing.contains(*name)) {
        let version = migration_version(name)?
            .with_context(|| format!("new migration {name} needs a numbered SQLx filename"))?;
        if let Some(highest) = highest {
            ensure!(
                version > highest,
                "new migration {name} must have a version above {highest}"
            );
        }
    }
    Ok(())
}

fn migration_version(path: &str) -> anyhow::Result<Option<i64>> {
    // SQLx reads the files directly inside migrations/ and history/.
    let Some((_, name)) = path.split_once('/') else {
        return Ok(None);
    };
    let Some((version, description)) = name.split_once('_') else {
        return Ok(None);
    };
    if name.contains('/') || description.strip_suffix(".sql").is_none() {
        return Ok(None);
    }
    Ok(Some(version.parse().with_context(|| {
        format!("read migration version from {path}")
    })?))
}

/// Refuse package declarations before the reader drops retired fields.
pub(super) fn refuse_workflow_declarations(bytes: &[u8]) -> anyhow::Result<()> {
    let manifest: serde_json::Value = serde_json::from_slice(bytes)?;
    ensure!(
        manifest.get("workflows").is_none(),
        "a package cannot declare workflows; author workflows in the environment"
    );
    if let Some(operations) = manifest
        .get("custom_operations")
        .and_then(serde_json::Value::as_object)
    {
        for (name, operation) in operations {
            ensure!(
                operation.get("registration").is_none(),
                "operation {name:?} cannot declare a registration; author an event workflow in the environment"
            );
        }
    }
    Ok(())
}
