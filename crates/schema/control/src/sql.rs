//! Fixed metadata statements used by package and release effect shells.

/// Lock the recorded schema source and baseline for a package mutation.
pub fn select_package_sql() -> String {
    format!("{} FOR UPDATE", select_package_snapshot_sql())
}

/// Read package SQL records without taking a write lock.
pub fn select_package_snapshot_sql() -> &'static str {
    "SELECT artifact_digest, package_version, physical_schema_fingerprint, baseline_fingerprint FROM catalog.packages \
     WHERE env_key = $1 AND package_id = $2"
}

/// Read the complete immutable migration records in authored order.
pub fn select_package_migrations_sql() -> &'static str {
    "SELECT ordinal, relative_path, sha256 FROM catalog.package_migrations \
     WHERE env_key = $1 AND package_id = $2 \
     ORDER BY ordinal"
}
