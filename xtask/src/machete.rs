//! Unused dependency checks for both Cargo workspaces.

use anyhow::{Context as _, Result, ensure};
use cargo_metadata::MetadataCommand;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use tokio::process::Command;

pub(super) async fn run(root: &Path) -> Result<()> {
    let pins: toml::Value = toml::from_str(&fs::read_to_string(root.join("mise.toml"))?)?;
    let pinned = pins
        .get("tools")
        .and_then(|tools| tools.get("http:cargo-machete"))
        .and_then(|tool| tool.get("version"))
        .and_then(toml::Value::as_str)
        .context("mise.toml is missing the cargo-machete version pin")?;
    let version = command(root)
        .arg("--version")
        .output()
        .await
        .with_context(|| format!("cargo-machete {pinned} is required. Run mise install"))?;
    ensure!(
        version.status.success(),
        "cargo-machete {pinned} is required. Run mise install\n{}",
        String::from_utf8_lossy(&version.stderr)
    );
    let installed = String::from_utf8_lossy(&version.stdout);
    ensure!(
        installed.trim() == pinned,
        "mise.toml pins cargo-machete {pinned}, but the installed version is {}. Run mise install",
        installed.trim()
    );

    let root = fs::canonicalize(root)?;
    let workspaces = ["Cargo.toml", "apps/Cargo.toml"]
        .into_iter()
        .map(|manifest| {
            MetadataCommand::new()
                .current_dir(&root)
                .manifest_path(manifest)
                .no_deps()
                .exec()
                .with_context(|| format!("read workspace members from {manifest}"))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut owners = BTreeMap::new();
    let mut workspace_roots = BTreeSet::new();
    for workspace in &workspaces {
        let owner = workspace.workspace_root.as_std_path();
        workspace_roots.insert(owner);
        for package in workspace.workspace_packages() {
            let directory = package
                .manifest_path
                .parent()
                .context("package directory")?;
            owners.insert(directory.as_std_path(), owner);
        }
    }

    let mut misplaced = BTreeSet::new();
    let mut workspace_views = BTreeMap::new();
    for (&directory, &owner) in &owners {
        if directory
            .ancestors()
            .skip(1)
            .find(|ancestor| workspace_roots.contains(ancestor))
            != Some(owner)
        {
            misplaced.insert(directory);
            let parent = directory.parent().context("package parent directory")?;
            ensure!(
                !workspace_roots.contains(parent)
                    && !owners.contains_key(parent)
                    && workspace_views
                        .insert(parent, owner)
                        .is_none_or(|prior| prior == owner),
                "cannot stage {} under a shared workspace directory",
                directory.display()
            );
        }
    }

    // Machete reads the nearest ancestor workspace before asking Cargo for metadata.
    // Supply that ancestor in a scratch tree. Cargo follows explicit paths to
    // the owning workspaces in that tree.
    let scratch = tempfile::tempdir()?;
    let staged = |path: &Path| -> Result<_> { Ok(scratch.path().join(path.strip_prefix(&root)?)) };
    for &directory in owners.keys().chain(workspace_roots.iter()) {
        fs::create_dir_all(staged(directory)?)?;
    }
    for (&directory, &owner) in &workspace_views {
        fs::copy(
            owner.join("Cargo.toml"),
            staged(directory)?.join("Cargo.toml"),
        )?;
    }
    for &owner in &workspace_roots {
        for name in ["Cargo.toml", "Cargo.lock"] {
            fs::copy(owner.join(name), staged(owner)?.join(name))?;
        }
    }
    for (&directory, &owner) in &owners {
        let destination = staged(directory)?;
        let manifest = directory.join("Cargo.toml");
        let target = destination.join("Cargo.toml");
        if directory
            .ancestors()
            .skip(1)
            .find(|parent| workspace_roots.contains(parent) || workspace_views.contains_key(parent))
            == Some(owner)
        {
            symlink(manifest, target)?;
        } else {
            let mut contents: toml::Value = toml::from_str(&fs::read_to_string(manifest)?)?;
            let package = contents
                .get_mut("package")
                .and_then(toml::Value::as_table_mut)
                .context("manifest is missing its package table")?;
            let workspace = staged(owner)?;
            let workspace = workspace.to_str().context("workspace path is not UTF-8")?;
            package.insert(
                "workspace".to_owned(),
                toml::Value::String(workspace.to_owned()),
            );
            fs::write(target, toml::to_string(&contents)?)?;
        }
        stage_contents(directory, &destination, misplaced.contains(directory))?;
    }

    let mut failures = Vec::new();
    for workspace in &workspaces {
        let owner = workspace.workspace_root.as_std_path();
        let manifest = owner.strip_prefix(&root)?.join("Cargo.toml");
        let packages = workspace.workspace_packages();
        let mut command = command(owner);
        command.arg("--with-metadata");
        for package in &packages {
            let path = package.manifest_path.as_std_path();
            if path
                .parent()
                .is_some_and(|parent| misplaced.contains(parent))
            {
                command.arg(staged(path)?);
            } else {
                command.arg(path);
            }
        }
        let output = command
            .output()
            .await
            .with_context(|| format!("run cargo machete for {}", manifest.display()))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if output.status.success()
            && !stdout
                .lines()
                .chain(stderr.lines())
                .any(|line| line.starts_with("error when handling "))
        {
            println!(
                "{}: checked {} workspace members, no unused dependencies",
                manifest.display(),
                packages.len()
            );
        } else {
            failures.push(format!(
                "{}: cargo machete reported findings or scan errors ({})\n{stderr}{stdout}",
                manifest.display(),
                output.status
            ));
        }
    }
    ensure!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}

fn command(directory: &Path) -> Command {
    let mut command = Command::new("cargo");
    // Cargo sets CARGO_PKG_NAME when running xtask. Machete uses its absence
    // to recognize that Cargo invoked it as an external subcommand.
    command
        .current_dir(directory)
        .env_remove("CARGO_PKG_NAME")
        .arg("machete");
    command
}

fn stage_contents(source: &Path, destination: &Path, copy: bool) -> Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        let target = destination.join(&name);
        if name == "target" || name == ".git" || target.exists() {
            continue;
        }
        let path = entry.path();
        let file_type = entry.file_type()?;
        if copy && file_type.is_dir() {
            fs::create_dir(&target)?;
            stage_contents(&path, &target, copy)?;
        } else if copy && file_type.is_file() {
            fs::copy(path, target)?;
        } else {
            symlink(path, target)?;
        }
    }
    Ok(())
}
