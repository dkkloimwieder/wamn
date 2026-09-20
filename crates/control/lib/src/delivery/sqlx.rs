//! Shared SQLx verifier commands for local generation and release qualification.

use std::path::Path;

use anyhow::ensure;
use tokio::process::Command;
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

/// Build the platform verifier command for one package.
pub fn prepare_arguments(repository_root: &Path, package_root: &Path, check: bool) -> Vec<String> {
    vec![
        "cargo".to_owned(),
        "run".to_owned(),
        "--manifest-path".to_owned(),
        repository_root.join("Cargo.toml").display().to_string(),
        "--locked".to_owned(),
        "--offline".to_owned(),
        "-p".to_owned(),
        "wamn-schema-generator".to_owned(),
        "--example".to_owned(),
        "sqlx_metadata".to_owned(),
        "--".to_owned(),
        if check { "check" } else { "prepare" }.to_owned(),
        package_root.display().to_string(),
    ]
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

/// Refuse committed metadata that holds query files no query uses.
///
/// The pinned CLI fails `prepare --check` for a missing or changed query file,
/// but it only warns about an unused one.
pub fn require_current_metadata(stdout: &[u8]) -> anyhow::Result<()> {
    ensure!(
        !String::from_utf8_lossy(stdout).contains("potentially unused queries found in .sqlx"),
        "committed SQLx metadata has unused queries; re-run cargo sqlx prepare"
    );
    Ok(())
}

/// Configure preparation while leaving execution and cleanup to the caller.
pub fn prepare_command(
    repository_root: &Path,
    package_root: &Path,
    database_url: &str,
    check: bool,
) -> Command {
    let arguments = prepare_arguments(repository_root, package_root, check);
    let mut command = Command::new(&arguments[0]);
    command
        .args(&arguments[1..])
        .current_dir(repository_root)
        .env("DATABASE_URL", database_url)
        .env("SQLX_OFFLINE", "false")
        .env("CARGO_NET_OFFLINE", "true");
    command
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
    fn prepare_and_check_use_the_platform_verifier_for_any_sql_package() {
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
        let repository = Path::new("/repository");
        let package = Path::new("/application");
        for check in [false, true] {
            let arguments = prepare_arguments(repository, package, check);
            assert_eq!(
                arguments,
                [
                    "cargo",
                    "run",
                    "--manifest-path",
                    "/repository/Cargo.toml",
                    "--locked",
                    "--offline",
                    "-p",
                    "wamn-schema-generator",
                    "--example",
                    "sqlx_metadata",
                    "--",
                    if check { "check" } else { "prepare" },
                    "/application"
                ]
            );
            let command = prepare_command(repository, package, "postgresql://verify", check);
            let command = command.as_std();
            assert_eq!(command.get_current_dir(), Some(repository));
            let envs: Vec<_> = command.get_envs().collect();
            assert!(envs.contains(&(
                "DATABASE_URL".as_ref(),
                Some("postgresql://verify".as_ref())
            )));
            assert!(envs.contains(&("SQLX_OFFLINE".as_ref(), Some("false".as_ref()))));
        }
        let warning = b"warning: potentially unused queries found in .sqlx; you may want to re-run sqlx prepare\n";
        assert!(require_current_metadata(warning).is_err());
        assert!(require_current_metadata(b"").is_ok());
    }
}
