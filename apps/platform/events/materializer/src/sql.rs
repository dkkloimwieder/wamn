/// Read every package identity applied for the current tenant.
pub fn select_known_package_ids_sql() -> String {
    "SELECT DISTINCT package_id \
       FROM catalog.packages \
      WHERE tenant_id = current_setting('app.tenant', true) \
      ORDER BY package_id"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_identity_resolution_uses_applied_package_roots() {
        let sql = select_known_package_ids_sql();
        assert!(sql.contains("catalog.packages"));
        assert!(sql.contains("current_setting('app.tenant', true)"));
    }
}
