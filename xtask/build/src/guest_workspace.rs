//! Keep every compiled local package below Cargo's build workspace root.
//!
//! Cargo hashes absolute paths for packages outside that root. Rust source path
//! remapping cannot change those crate identities. This source view moves only
//! workspace manifests; Cargo still owns inheritance and dependency resolution.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use toml::Value;

pub(crate) struct GuestWorkspace {
    directory: PathBuf,
    // Other callers share this source view. Hold its lock across Cargo's build.
    _lock: fs::File,
}

impl GuestWorkspace {
    pub(crate) fn new(repository: &Path, metadata: &cargo_metadata::Metadata) -> Result<Self> {
        let target = metadata.target_directory.as_std_path();
        fs::create_dir_all(target)?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(target.join(".wamn-guest.lock"))?;
        lock.lock()?;
        let directory = target.join("wamn-guest");
        if directory.exists() {
            fs::remove_dir_all(&directory)?;
        }
        fs::create_dir(&directory)?;
        let packages = metadata
            .packages
            .iter()
            .filter(|package| package.source.is_none())
            .map(|package| package.manifest_path.as_std_path().to_owned())
            .collect::<BTreeSet<_>>();
        let mut directories = BTreeSet::new();
        let mut manifests = BTreeMap::new();
        let mut owners: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
        for manifest in &packages {
            let relative = manifest
                .strip_prefix(repository)
                .context("guest path dependency must be inside the repository")?;
            directories.extend(relative.ancestors().skip(1).map(Path::to_owned));
            let value: Value = toml::from_str(&fs::read_to_string(manifest)?)?;
            owners
                .entry(owner(manifest, &value)?)
                .or_default()
                .push(manifest.clone());
            manifests.insert(manifest, value);
        }
        link_sources(repository, &directory, Path::new(""), &directories)?;
        for (index, (owner, members)) in owners.into_iter().enumerate() {
            let primary = owner == metadata.workspace_root.as_std_path();
            let workspace_root = if primary {
                directory.clone()
            } else {
                directory.join(".workspaces").join(index.to_string())
            };
            fs::create_dir_all(&workspace_root)?;
            let mut original: toml::Table =
                toml::from_str(&fs::read_to_string(owner.join("Cargo.toml"))?)?;
            let mut workspace = toml::Table::new();
            for key in ["workspace", "profile", "patch", "replace"] {
                if let Some(value) = original.remove(key) {
                    workspace.insert(key.into(), value);
                }
            }
            let table = workspace
                .get_mut("workspace")
                .and_then(Value::as_table_mut)
                .context("Cargo workspace")?;
            table.insert(
                "members".into(),
                Value::Array(
                    members
                        .iter()
                        .map(|manifest| {
                            Ok(Value::String(
                                directory
                                    .join(
                                        manifest
                                            .parent()
                                            .context("package directory")?
                                            .strip_prefix(repository)?,
                                    )
                                    .to_string_lossy()
                                    .into_owned(),
                            ))
                        })
                        .collect::<Result<_>>()?,
                ),
            );
            table.remove("default-members");
            table.insert(
                "exclude".into(),
                Value::Array(
                    packages
                        .iter()
                        .filter(|manifest| !members.contains(manifest))
                        .map(|manifest| {
                            Ok(Value::String(
                                directory
                                    .join(
                                        manifest
                                            .parent()
                                            .context("package directory")?
                                            .strip_prefix(repository)?,
                                    )
                                    .to_string_lossy()
                                    .into_owned(),
                            ))
                        })
                        .collect::<Result<_>>()?,
                ),
            );
            let mut workspace = Value::Table(workspace);
            relocate_dependencies(&mut workspace, &owner, repository, &directory, &packages)?;
            write_manifest(&workspace_root.join("Cargo.toml"), &workspace)?;
            for manifest in members {
                let mut value = manifests
                    .remove(&manifest)
                    .context("local Cargo manifest")?;
                relocate_dependencies(
                    &mut value,
                    manifest.parent().context("package directory")?,
                    repository,
                    &directory,
                    &packages,
                )?;
                let table = value.as_table_mut().context("Cargo manifest")?;
                table.remove("workspace");
                table
                    .get_mut("package")
                    .and_then(Value::as_table_mut)
                    .context("Cargo package")?
                    .insert(
                        "workspace".into(),
                        Value::String(workspace_root.to_string_lossy().into_owned()),
                    );
                write_manifest(&directory.join(manifest.strip_prefix(repository)?), &value)?;
            }
        }
        fs::copy(
            metadata.workspace_root.join("Cargo.lock"),
            directory.join("Cargo.lock"),
        )?;
        Ok(Self {
            directory,
            _lock: lock,
        })
    }

    pub(crate) fn root(&self) -> &Path {
        &self.directory
    }
    pub(crate) fn manifest(&self) -> PathBuf {
        self.root().join("Cargo.toml")
    }
}

fn write_manifest(path: &Path, value: &Value) -> Result<()> {
    if path.is_symlink() {
        fs::remove_file(path)?;
    }
    fs::write(path, toml::to_string(value)?)?;
    Ok(())
}

fn link_sources(
    source: &Path,
    destination: &Path,
    relative: &Path,
    directories: &BTreeSet<PathBuf>,
) -> Result<()> {
    fs::create_dir_all(destination.join(relative))?;
    for entry in fs::read_dir(source.join(relative))? {
        let entry = entry?;
        let path = relative.join(entry.file_name());
        if path == Path::new("Cargo.toml")
            || path == Path::new("Cargo.lock")
            || path == Path::new(".git")
        {
            continue;
        }
        if directories.contains(&path) {
            link_sources(source, destination, &path, directories)?;
        } else {
            std::os::unix::fs::symlink(entry.path(), destination.join(path))?;
        }
    }
    Ok(())
}

fn owner(manifest: &Path, package: &Value) -> Result<PathBuf> {
    let parent = manifest.parent().context("package directory")?;
    if let Some(workspace) = package
        .get("package")
        .and_then(|package| package.get("workspace"))
        .and_then(Value::as_str)
    {
        return Ok(parent.join(workspace).canonicalize()?);
    }
    for directory in parent.ancestors() {
        let path = directory.join("Cargo.toml");
        if path.is_file()
            && toml::from_str::<Value>(&fs::read_to_string(path)?)?
                .get("workspace")
                .is_some()
        {
            return Ok(directory.to_owned());
        }
    }
    anyhow::bail!("Cargo package has no workspace: {}", manifest.display())
}

fn relocate_dependencies(
    value: &mut Value,
    origin: &Path,
    repository: &Path,
    destination: &Path,
    packages: &BTreeSet<PathBuf>,
) -> Result<()> {
    let Some(table) = value.as_table_mut() else {
        return Ok(());
    };
    for (name, value) in table {
        if [
            "dependencies",
            "dev-dependencies",
            "build-dependencies",
            "patch",
            "replace",
        ]
        .contains(&name.as_str())
            && let Some(dependencies) = value.as_table_mut()
        {
            for (_, dependency) in dependencies {
                if let Some(path) = dependency.get("path").and_then(Value::as_str) {
                    let path = origin.join(path).canonicalize()?;
                    let path = if packages.contains(&path.join("Cargo.toml")) {
                        destination.join(path.strip_prefix(repository)?)
                    } else {
                        path
                    };
                    dependency
                        .as_table_mut()
                        .context("path dependency")?
                        .insert(
                            "path".into(),
                            Value::String(path.to_string_lossy().into_owned()),
                        );
                }
            }
        }
        relocate_dependencies(value, origin, repository, destination, packages)?;
    }
    Ok(())
}
