//! Receiving routes tests and helpers.

use super::*;

/// Copy the package at `source` to `destination`, with its build output.
///
/// The build output lies outside the package, in
/// `<package>/../target/wamn/<package>`, so the copy takes it from the
/// source's output root to the destination's.
///
/// The build's input digest covers the lock file of the enclosing workspace,
/// `<package>/../Cargo.lock`, so the copy takes it too. Without it the copy's
/// tree differs from its `build.json` and push-package refuses it as stale.
pub(super) fn copy_package(source: &Path, destination: &Path) -> anyhow::Result<()> {
    wamn_compiler::build_record::require_current(source)?;
    copy_package_tree(source, destination)?;
    let lock = |root: &Path| root.parent().map(|parent| parent.join("Cargo.lock"));
    if let (Some(from), Some(to)) = (lock(source), lock(destination))
        && from.is_file()
        && !to.exists()
    {
        std::fs::copy(from, to)?;
    }
    let output = wamn_compiler::output_root(source);
    if output.is_dir() {
        let copied = wamn_compiler::output_root(destination);
        std::fs::create_dir_all(copied.parent().context("an output root has a parent")?)?;
        copy_package_tree(&output, &copied)?;
    }
    Ok(())
}

fn copy_package_tree(source: &Path, destination: &Path) -> anyhow::Result<()> {
    std::fs::create_dir(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let type_ = entry.file_type()?;
        let target = destination.join(entry.file_name());
        // Installed web packages are not package sources, and pnpm links them.
        if type_.is_dir() && entry.file_name() == "node_modules" {
            continue;
        }
        if type_.is_dir() {
            copy_package_tree(&entry.path(), &target)?;
        } else {
            anyhow::ensure!(type_.is_file(), "package sources must be regular files");
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
