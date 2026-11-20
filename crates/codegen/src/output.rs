//! Where a package's build output lies.

use anyhow::{Context as _, Result, ensure};
use std::path::{Path, PathBuf};

/// The directory below a package's parent that holds one build output
/// directory per package: `apps/target/wamn` for an application.
const BUILD_OUTPUT: &str = "target/wamn";

/// The directory that holds the build output of the package at `package_root`.
///
/// Every reader of build output, and the writer that materializes it, resolves
/// the directory here. A logical path such as `generated/sql/...`, which a
/// contract or a release names, keeps its spelling. Its file lies at the same
/// path below this directory, without the `generated/` prefix.
///
/// The directory is, in order:
/// - `<package_root>/generated` when it exists. An unpacked package artifact
///   keeps its `generated/` layer paths, and so does a test fixture.
/// - `package_root` itself when it is a build output directory, that is when
///   it lies directly below a `target/wamn` directory. A manifest in the build
///   output names its own directory as its package root.
/// - otherwise `<workspace>/target/wamn/<name>`, where `<name>` is the package
///   directory's name. Applications use one workspace. The name equals the package id.
#[must_use]
pub fn output_root(package_root: &Path) -> PathBuf {
    let unpacked = package_root.join("generated");
    if unpacked.is_dir() {
        return unpacked;
    }
    if is_build_output(package_root) {
        return package_root.to_owned();
    }
    match package_root.file_name() {
        Some(name) => package_workspace(package_root)
            .join(BUILD_OUTPUT)
            .join(name),
        None => match package_root.canonicalize() {
            Ok(canonical) if canonical.file_name().is_some() => output_root(&canonical),
            _ => package_root.join("..").join(BUILD_OUTPUT),
        },
    }
}

/// The workspace that contains an application package.
pub fn package_workspace(package_root: &Path) -> PathBuf {
    if package_root.file_name().is_none() {
        if let Ok(canonical) = package_root.canonicalize()
            && canonical.file_name().is_some()
        {
            return package_workspace(&canonical);
        }
        return package_root.join("..");
    }
    package_root.parent().unwrap_or(Path::new("")).to_owned()
}

/// Resolve a base package beside its application consumer.
pub(crate) fn dependency_root(package_root: &Path, package: &wamn_package::PackageName) -> PathBuf {
    package_workspace(package_root).join(package.as_str())
}

/// Whether `directory` lies directly below a `target/wamn` directory.
pub(crate) fn is_build_output(directory: &Path) -> bool {
    let Some(wamn) = directory.parent() else {
        return false;
    };
    wamn.file_name().is_some_and(|name| name == "wamn")
        && wamn
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "target")
}

/// Whether a path belongs to the source tree, excluding generated output and dependencies.
///
/// The path can name a new or deleted file or directory.
#[must_use]
pub fn is_input_path(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root).is_ok_and(|relative| {
        relative.components().all(|component| {
            !["target", "node_modules", "dist"]
                .iter()
                .any(|excluded| component.as_os_str() == *excluded)
        })
    })
}

/// Resolve a workspace build directory to its package source root.
///
/// # Errors
///
/// When the path does not name `<workspace>/target/wamn/<package>`.
pub fn source_root(output: &Path) -> Result<PathBuf> {
    ensure!(
        is_build_output(output),
        "{} is not a workspace package output under target/wamn",
        output.display()
    );
    let workspace = output
        .ancestors()
        .nth(3)
        .filter(|path| !path.as_os_str().is_empty())
        .with_context(|| format!("{} has no package workspace parent", output.display()))?;
    let package = output
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("{} has no UTF-8 package name", output.display()))?
        .parse::<wamn_package::PackageName>()?;
    Ok(dependency_root(&workspace.join(package.as_str()), &package))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::output_root;

    #[test]
    fn a_package_writes_beside_its_parent_and_an_output_directory_is_its_own() {
        assert_eq!(
            output_root(Path::new("/repository/apps/wamn_wms")),
            Path::new("/repository/apps/target/wamn/wamn_wms")
        );
        assert_eq!(
            output_root(Path::new("/repository/apps/target/wamn/wamn_wms")),
            Path::new("/repository/apps/target/wamn/wamn_wms")
        );
    }

    #[test]
    fn an_unpacked_artifact_keeps_its_generated_directory() {
        let root = std::env::temp_dir().join(format!("wamn-output-root-{}", std::process::id()));
        std::fs::create_dir_all(root.join("generated")).expect("create the unpacked tree");
        assert_eq!(output_root(&root), root.join("generated"));
        std::fs::remove_dir_all(&root).expect("remove the unpacked tree");
    }
}
