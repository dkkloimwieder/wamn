//! Static conformance for the callable-flow POC RawSql grant.

#[cfg(test)]
mod tests {
    const RUNNER: &str = include_str!("../../../components/execution/flowrunner/src/lib.rs");
    const POC_CONFIG: &str = include_str!("../../../deploy/poc/poc-material-receiving.config.json");
    const POC_PROVISION: &str = include_str!("../../../deploy/poc/f1-provision-job.yaml");
    const PUBLISH_CATALOG: &str = include_str!("../../../services/ctl/src/publish_catalog.rs");

    #[test]
    fn trusted_lookup_is_database_and_tenant_scoped() {
        assert!(RUNNER.contains("FROM app_system.configurations"));
        assert!(RUNNER.contains("tenant_id = NULLIF(current_setting('app.tenant', true), '')"));
        assert!(RUNNER.contains("AND config_key = $1"));
        assert!(RUNNER.contains("LIMIT 2"));
        assert!(RUNNER.contains("raw_sql_enabled_rows(&result.rows)"));
    }

    #[test]
    fn node_authored_values_cannot_forge_the_grant() {
        let context_start = RUNNER
            .find("let mut ctx = wamn_node_guest::caps::CapsCtx")
            .expect("production standard-node context");
        let context_end = RUNNER[context_start..]
            .find("let granted =")
            .map(|offset| context_start + offset)
            .expect("grant calculation follows context construction");
        let construction = &RUNNER[context_start..context_end];
        assert!(construction.contains("&& raw_sql_enabled()"));
        assert!(construction.contains("Capability::RawSql"));
        assert!(!construction.contains("d.config"));
        assert!(!construction.contains("d.payload"));
    }

    #[test]
    fn only_the_callable_poc_fixture_enables_raw_sql() {
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(POC_CONFIG).unwrap(),
            serde_json::json!({"raw_sql_enabled": true})
        );
        assert!(
            POC_PROVISION.contains("\"--project-config\", \"/fixtures/poc-receiving.config.json\"")
        );
        assert!(PUBLISH_CATALOG.contains("INSERT INTO app_system.configurations"));
        assert!(PUBLISH_CATALOG.contains("(tenant_id, config_key) DO UPDATE"));
        assert!(PUBLISH_CATALOG.contains("parse_raw_sql_config(&source)"));
        let deploy_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy");
        let mut enabling_files = Vec::new();
        collect_enabling_files(&deploy_root, &mut enabling_files);
        assert_eq!(
            enabling_files,
            vec![deploy_root.join("poc/poc-material-receiving.config.json")]
        );
    }

    fn collect_enabling_files(path: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
        let mut entries = std::fs::read_dir(path)
            .expect("read deploy tree")
            .collect::<Result<Vec<_>, _>>()
            .expect("read deploy entries");
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                collect_enabling_files(&path, found);
                continue;
            }
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            if path.extension().and_then(std::ffi::OsStr::to_str) == Some("json")
                && serde_json::from_str::<serde_json::Value>(&contents)
                    .ok()
                    .and_then(|value| value.get("raw_sql_enabled").cloned())
                    == Some(serde_json::Value::Bool(true))
            {
                found.push(path);
            }
        }
    }
}
