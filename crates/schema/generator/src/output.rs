//! Where a package's build output lies.

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
///   directory's name. Both top-level applications and nested platform packages
///   use the application workspace. The name equals the package id.
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

/// The application workspace for a top-level or nested platform package.
pub(crate) fn package_workspace(package_root: &Path) -> PathBuf {
    if package_root.file_name().is_none() {
        if let Ok(canonical) = package_root.canonicalize()
            && canonical.file_name().is_some()
        {
            return package_workspace(&canonical);
        }
        return package_root.join("..");
    }
    let parent = package_root.parent().unwrap_or(Path::new(""));
    if parent.file_name().is_some_and(|name| name == "platform") {
        parent.parent().unwrap_or(Path::new("")).to_owned()
    } else {
        parent.to_owned()
    }
}

/// Resolve a base package in the two supported application locations.
pub(crate) fn dependency_root(package_root: &Path, package: &str) -> PathBuf {
    let workspace = package_workspace(package_root);
    let top = workspace.join(package);
    let nested = workspace.join("platform").join(package);
    if !top.is_dir() && nested.is_dir() {
        nested
    } else {
        top
    }
}

/// The build output root of the host route clients in the repository at
/// `repository_root`: `apps/target/wamn`, the root that `wamn build` writes
/// the applications to. The clients lie in its `wamn_control` directory.
#[must_use]
pub fn host_route_client_output_root(repository_root: &Path) -> PathBuf {
    repository_root.join("apps").join(BUILD_OUTPUT)
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{host_route_client_output_root, output_root};

    /// The host route clients lie where `wamn build` puts a package named
    /// for them, never in the repository's `target` (wamn-89s6l).
    #[test]
    fn the_host_route_clients_follow_the_application_output_root() {
        let repository = Path::new("/repository");
        let package = wamn_catalog::host_route_package();
        assert_eq!(
            host_route_client_output_root(repository).join(package),
            output_root(&repository.join("apps").join(package))
        );
    }

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
    fn nested_platform_packages_share_the_application_workspace() {
        let root = Path::new("/repository/apps/platform/wamn_integration");
        assert_eq!(
            super::package_workspace(root),
            Path::new("/repository/apps")
        );
        assert_eq!(
            output_root(root),
            Path::new("/repository/apps/target/wamn/wamn_integration")
        );
        assert_eq!(
            crate::build::default_output_root(root),
            Path::new("/repository/apps/target/wamn")
        );
        assert_eq!(
            super::package_workspace(Path::new("/repository/apps/rest_connector")),
            Path::new("/repository/apps")
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
