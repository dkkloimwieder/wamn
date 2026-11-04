use super::{Path, PathBuf, ProductionDevStageError, SHARED_WIT_ROOTS};
use anyhow::Context as _;

/// The inputs of the component builds besides the package roots: the
/// platform crates of the packages' Cargo workspace, `<package>/../platform`,
/// and the shared platform WIT sources.
pub(super) fn component_build_watch_roots(
    repository_root: &Path,
    package_roots: &[PathBuf],
) -> Vec<PathBuf> {
    let mut roots = package_roots
        .iter()
        .filter_map(|root| root.parent())
        .map(|workspace| workspace.join("platform"))
        .chain(SHARED_WIT_ROOTS.map(|root| repository_root.join(root)))
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    roots
}

pub(super) fn finish_with_cleanup<T>(
    result: anyhow::Result<T>,
    cleanup: Result<(), ProductionDevStageError>,
) -> anyhow::Result<T> {
    match (result, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup)) => Err(cleanup).context("clean up local activation"),
        (Err(error), Err(cleanup)) => {
            Err(error.context(format!("local activation cleanup also failed: {cleanup}")))
        }
    }
}
