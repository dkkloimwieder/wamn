//! Where a package's build output lies.

use std::path::{Path, PathBuf};

/// The directory that holds the build output of the package at `package_root`.
///
/// Every reader of build output, and the writer that materializes it, resolves
/// the directory here. A logical path such as `generated/sql/...`, which a
/// contract or a release names, keeps its spelling. Its file lies at the same
/// path below this directory, without the `generated/` prefix.
#[must_use]
pub fn output_root(package_root: &Path) -> PathBuf {
    package_root.join("generated")
}
