//! Fixed metadata statements used by package and release effect shells.

/// Lock the recorded schema source and baseline for a package mutation.
pub fn select_package_sql() -> String {
    format!("{} FOR UPDATE", select_package_snapshot_sql())
}

/// Read package SQL records without taking a write lock.
pub fn select_package_snapshot_sql() -> &'static str {
    "SELECT artifact_digest, package_version FROM catalog.packages \
     WHERE project_env_key = $1 AND package_id = $2"
}
