//! Shared SQLx verifier commands for local generation and release qualification.

use std::path::Path;

use anyhow::{Context as _, ensure};
use wamn_schema_generator::PackageManifest;

/// Select SQL verification from declarations rather than package identity.
pub fn requires_verifier(manifest: &PackageManifest) -> bool {
    manifest
        .models
        .values()
        .any(|model| !model.operations.is_empty())
        || manifest
            .custom_operations
            .values()
            .any(|operation| !operation.statements.is_empty())
}

/// Prepare the SQLx metadata of the package's generated verifier into its
/// build output, against the migrated database at `database_url`.
pub async fn prepare(package_root: &Path, database_url: &str) -> anyhow::Result<()> {
    let package_root = package_root.to_owned();
    let database_url = database_url.to_owned();
    tokio::task::spawn_blocking(move || {
        wamn_schema_generator::prepare_sqlx_metadata(&package_root, &database_url)
    })
    .await
    .context("join the SQLx metadata preparation")?
}

/// Require the pinned CLI version before preparing metadata.
pub fn require_cli_version(output: &[u8]) -> anyhow::Result<()> {
    ensure!(
        matches!(
            String::from_utf8_lossy(output).trim(),
            "sqlx-cli 0.9.0" | "sqlx-cli-sqlx 0.9.0"
        ),
        "SQLx CLI 0.9.0 is required"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_search_path_preserves_other_connection_options() {
        let manifest = manifest();
        let database_url = wamn_schema_generator::package_database_url(
            "postgresql://user:password@127.0.0.1:5432/test?application_name=local%2Bcheck&options=-cstatement_timeout%3D2500%20-csearch_path%3Dold&connect_timeout=5",
            &manifest,
        ).expect("scope SQLx to the package schemas");
        let connection = database_url
            .parse::<tokio_postgres::Config>()
            .expect("the scoped URL remains a PostgreSQL connection");
        assert_eq!(connection.get_dbname(), Some("test"));
        assert_eq!(connection.get_application_name(), Some("local+check"));
        assert_eq!(
            connection.get_connect_timeout(),
            Some(&std::time::Duration::from_secs(5))
        );
        assert_eq!(
            connection.get_options(),
            Some("-cstatement_timeout=2500 -csearch_path=old -csearch_path=inventory,public")
        );
        assert_eq!(connection.get_user(), Some("user"));
        assert_eq!(connection.get_password(), Some(b"password".as_slice()));
        let plain =
            wamn_schema_generator::package_database_url("postgresql://127.0.0.1/test", &manifest)
                .expect("scope the unconfigured local connection");
        assert_eq!(
            plain
                .parse::<tokio_postgres::Config>()
                .unwrap()
                .get_options(),
            Some("-csearch_path=inventory,public")
        );
    }

    fn manifest() -> PackageManifest {
        serde_json::from_value(serde_json::json!({
            "package": {"id": "sqlx_fixture", "version": "1.0.0"},
            "required_platform_policy_contract": {"id": "fixture_access", "state": "unsatisfied"},
            "models": {"widget": {"schema": "inventory", "table": "widget", "owner": "sqlx_fixture",
                "operations": {"get": {"permission": "widget.get", "result": "one"}}}},
            "connections": ["postgres"], "components": {"fixture": {"connections": ["postgres"]}}
        }))
        .unwrap()
    }

    #[test]
    fn every_sql_package_requires_the_platform_verifier() {
        let mut manifest = manifest();
        assert!(requires_verifier(&manifest));
        manifest.package.id = "another_package".to_owned();
        assert!(requires_verifier(&manifest));
        manifest
            .models
            .get_mut("widget")
            .unwrap()
            .operations
            .clear();
        assert!(!requires_verifier(&manifest));
    }
}
