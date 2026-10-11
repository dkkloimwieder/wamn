//! Fixed metadata statements used by package and release effect shells.

/// Read the immutable package root at one exact coordinate.
pub fn select_package_sql() -> &'static str {
    "SELECT artifact_digest, predecessor_version FROM catalog.packages \
     WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3"
}

/// Read the complete immutable migration records in authored order.
pub fn select_package_migrations_sql() -> &'static str {
    "SELECT ordinal, relative_path, sha256 FROM catalog.package_migrations \
     WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3 \
     ORDER BY ordinal"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_reads_use_exact_coordinate_and_ordered_migrations() {
        assert!(select_package_sql().contains("package_version = $3"));
        assert!(select_package_migrations_sql().ends_with("ORDER BY ordinal"));
    }
}
