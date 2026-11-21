//! Select test packages from Git paths and the resolved Cargo graphs.

use anyhow::{Context as _, Result, ensure};
use cargo_metadata::{CargoOpt, DependencyKind, Metadata, MetadataCommand, Package};
use std::collections::{BTreeMap, BTreeSet, HashMap, btree_map::Entry};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

const MANIFESTS: [&str; 2] = ["Cargo.toml", "apps/Cargo.toml"];

pub(super) fn run(root: &Path, base: &str) -> Result<()> {
    let paths = changed_paths(root, base)?;
    if paths.is_empty() {
        return Ok(());
    }
    let workspaces = MANIFESTS
        .iter()
        .map(|manifest| {
            MetadataCommand::new()
                .current_dir(root)
                .manifest_path(manifest)
                .features(CargoOpt::AllFeatures)
                .other_options(vec!["--locked".into()])
                .exec()
                .with_context(|| format!("read Cargo metadata for {manifest}"))
        })
        .collect::<Result<Vec<_>>>()?;
    let members: Vec<_> = workspaces
        .iter()
        .map(Metadata::workspace_packages)
        .collect();
    let mut selected = BTreeMap::new();
    for path in &paths {
        let owner = members
            .iter()
            .enumerate()
            .flat_map(|(workspace, packages)| {
                packages.iter().map(move |&package| (workspace, package))
            })
            .filter(|(_, package)| path.starts_with(package_root(package)))
            .max_by_key(|(_, package)| package_root(package).components().count());
        let workspace_file = matches!(
            path.file_name().and_then(OsStr::to_str),
            Some("Cargo.toml" | "Cargo.lock")
        );
        if let Some((_, package)) = owner
            && !workspace_file
        {
            selected
                .entry(package.manifest_path.as_std_path())
                .or_insert_with(|| reason("changed", root, path));
            continue;
        }
        // A file no package owns can be read by a package of either workspace.
        let scope = if workspace_file {
            owner.map(|(workspace, _)| workspace).or_else(|| {
                workspaces
                    .iter()
                    .enumerate()
                    .filter(|(_, metadata)| path.starts_with(metadata.workspace_root.as_std_path()))
                    .max_by_key(|(_, metadata)| metadata.workspace_root.components().count())
                    .map(|(workspace, _)| workspace)
            })
        } else {
            None
        };
        for (workspace, packages) in members.iter().enumerate() {
            if scope.is_none_or(|scope| scope == workspace) {
                for package in packages {
                    selected
                        .entry(package.manifest_path.as_std_path())
                        .or_insert_with(|| reason("workspace change", root, path));
                }
            }
        }
    }
    add_dependents(root, &workspaces, &mut selected)?;
    for (manifest, mut packages) in MANIFESTS.into_iter().zip(members) {
        packages.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        for package in packages {
            let Some(reason) = selected.get(package.manifest_path.as_std_path()) else {
                continue;
            };
            let test = format!("cargo test --manifest-path {manifest} -p {}", package.name);
            if cluster_only(package) {
                println!("{manifest}\t{}\t{reason}\t{test}", package.name);
            } else {
                println!(
                    "{manifest}\t{}\t{reason}\t{test}\t{test} -- --ignored",
                    package.name
                );
            }
        }
    }
    Ok(())
}

fn changed_paths(root: &Path, base: &str) -> Result<BTreeSet<PathBuf>> {
    ensure!(
        !base.is_empty() && !base.starts_with('-'),
        "invalid base ref {base:?}"
    );
    let range = format!("{base}...HEAD");
    let commands: [&[&str]; 4] = [
        &["diff", "--name-only", "--no-renames", "-z", &range, "--"],
        &[
            "diff",
            "--name-only",
            "--no-renames",
            "-z",
            "--cached",
            "--",
        ],
        &["diff", "--name-only", "--no-renames", "-z", "--"],
        &["ls-files", "-z", "--others", "--exclude-standard"],
    ];
    let mut paths = BTreeSet::new();
    for args in commands {
        let output = Command::new("git")
            .arg("--no-optional-locks")
            .current_dir(root)
            .args(args)
            .output()
            .context("run Git to list changed files")?;
        ensure!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
        paths.extend(
            output
                .stdout
                .split(|&byte| byte == 0)
                .filter(|path| !path.is_empty())
                .map(|path| root.join(OsStr::from_bytes(path))),
        );
    }
    Ok(paths)
}

fn add_dependents<'a>(
    root: &Path,
    workspaces: &'a [Metadata],
    selected: &mut BTreeMap<&'a Path, String>,
) -> Result<()> {
    let mut reverse = BTreeMap::<_, BTreeSet<_>>::new();
    let mut development = BTreeMap::<_, BTreeSet<_>>::new();
    for metadata in workspaces {
        let manifests: HashMap<_, _> = metadata
            .packages
            .iter()
            .map(|package| (&package.id, package.manifest_path.as_std_path()))
            .collect();
        let resolve = metadata
            .resolve
            .as_ref()
            .context("missing Cargo dependency graph")?;
        for node in &resolve.nodes {
            let dependent = manifests[&node.id];
            for dependency in &node.deps {
                let manifest = manifests[&dependency.pkg];
                if dependency
                    .dep_kinds
                    .iter()
                    .any(|edge| matches!(edge.kind, DependencyKind::Normal | DependencyKind::Build))
                {
                    reverse.entry(manifest).or_default().insert(dependent);
                } else if dependency
                    .dep_kinds
                    .iter()
                    .any(|edge| edge.kind == DependencyKind::Development)
                {
                    development.entry(manifest).or_default().insert(dependent);
                }
            }
        }
    }
    let mut closure: Vec<_> = selected.keys().copied().collect();
    let mut next = 0;
    while let Some(&dependency) = closure.get(next) {
        next += 1;
        for &dependent in reverse.get(dependency).into_iter().flatten() {
            if let Entry::Vacant(entry) = selected.entry(dependent) {
                entry.insert(reason("depends on", root, dependency));
                closure.push(dependent);
            }
        }
    }
    for dependency in closure {
        for &dependent in development.get(dependency).into_iter().flatten() {
            selected
                .entry(dependent)
                .or_insert_with(|| reason("test dependency", root, dependency));
        }
    }
    Ok(())
}

fn package_root(package: &Package) -> &Path {
    package
        .manifest_path
        .as_std_path()
        .parent()
        .expect("a Cargo manifest has a parent directory")
}

#[expect(
    clippy::unnecessary_debug_formatting,
    reason = "escape tabs and newlines so each package takes one row"
)]
fn reason(prefix: &str, root: &Path, path: &Path) -> String {
    let path = path.strip_prefix(root).unwrap_or(path);
    format!("{prefix} {path:?}")
}

fn cluster_only(package: &Package) -> bool {
    // The Kubernetes test targets use the existing `_kind.rs` file suffix.
    let mut tests = package
        .targets
        .iter()
        .filter(|target| target.test)
        .peekable();
    tests.peek().is_some()
        && tests.all(|target| {
            target
                .src_path
                .file_name()
                .is_some_and(|file| file.ends_with("_kind.rs"))
        })
}
