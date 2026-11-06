//! Fixed metadata statements used by package and release effect shells.

/// Read initialization provenance and physical SQL identity for one installed package.
pub fn select_package_sql() -> &'static str {
    "SELECT artifact_digest, predecessor_version, package_version, physical_schema_fingerprint FROM catalog.packages \
     WHERE env_key = $1 AND package_id = $2 FOR UPDATE"
}

/// Read the complete immutable migration records in authored order.
pub fn select_package_migrations_sql() -> &'static str {
    "SELECT ordinal, relative_path, sha256 FROM catalog.package_migrations \
     WHERE env_key = $1 AND package_id = $2 \
     ORDER BY ordinal"
}
