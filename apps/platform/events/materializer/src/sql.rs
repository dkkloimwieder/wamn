/// Read every package identity applied for the current tenant.
pub fn select_known_package_ids_sql() -> String {
    "SELECT DISTINCT package_id \
       FROM catalog.packages \
      WHERE env_key = current_setting('app.env_key', true) \
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
        assert!(sql.contains("current_setting('app.env_key', true)"));
    }
}
