//! Fixed metadata statements used by package and release effect shells.

/// Read the immutable package root at one exact coordinate.
pub fn select_package_sql() -> &'static str {
    "SELECT manifest_sha256, predecessor_version FROM catalog.packages \
     WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3"
}

/// Read the complete immutable migration records in authored order.
pub fn select_package_migrations_sql() -> &'static str {
    "SELECT ordinal, relative_path, sha256 FROM catalog.package_migrations \
     WHERE tenant_id = $1 AND package_id = $2 AND package_version = $3 \
     ORDER BY ordinal"
}

/// Record an effective-release deployment attestation in the CONTROL plane.
pub fn register_deployment_attestation_sql() -> &'static str {
    "INSERT INTO catalog.deployment_attestations (\
     tenant_id, environment_instance, effective_release_id, org_id, project_id, \
     environment, deployed_manifest_hash, source_commit, attested_at) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::text::timestamptz) \
     ON CONFLICT (tenant_id, environment_instance, effective_release_id, org_id, project_id, environment) \
     DO NOTHING RETURNING attested_at"
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
