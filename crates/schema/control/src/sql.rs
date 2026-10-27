//! Fixed metadata statements used by package and release effect shells.

/// Read the immutable package root at one exact coordinate.
pub fn select_package_sql() -> &'static str {
    "SELECT artifact_digest, predecessor_version FROM catalog.packages \
     WHERE env_key = $1 AND package_id = $2 AND package_version = $3"
}

/// Read the complete immutable migration records in authored order.
pub fn select_package_migrations_sql() -> &'static str {
    "SELECT ordinal, relative_path, sha256 FROM catalog.package_migrations \
     WHERE env_key = $1 AND package_id = $2 AND package_version = $3 \
     ORDER BY ordinal"
}
