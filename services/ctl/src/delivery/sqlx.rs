//! Shared SQLx verifier commands for local generation and release qualification.

use std::path::Path;

use anyhow::{Context as _, ensure};
use tokio::process::Command;
use wamn_schema_generator::PackageManifest;

/// Use the package's existing SQL search path without changing other connection options.
pub fn package_database_url(
    database_url: &str,
    manifest: &PackageManifest,
) -> anyhow::Result<String> {
    let schemas = wamn_schema_generator::data_access_schemas(&serde_json::to_vec(manifest)?)
        .context("resolve the package SQLx schemas")?;
    let connection = database_url
        .parse::<tokio_postgres::Config>()
        .context("parse SQLx connection options")?;
    let mut url = url::Url::parse(database_url).context("parse the SQLx database URL")?;
    // The schema owner validates these as bare identifiers. The final setting
    // wins over any inherited search_path, like the generator's session SET.
    let options = format!(
        "{} -csearch_path={},public",
        connection.get_options().unwrap_or_default(),
        schemas.join(",")
    );
    // PostgreSQL decodes percent escapes in URLs; '+' is a literal character.
    let encoded = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("options", options.trim())
        .finish()
        .replace('+', "%20");
    let query = match url.query().filter(|query| !query.is_empty()) {
        Some(query) => format!("{query}&{encoded}"),
        None => encoded,
    };
    url.set_query(Some(&query));
    Ok(url.into())
}

/// Return the existing verifier target for an application package.
pub fn verifier_for(package_id: &str) -> Option<&'static str> {
    match package_id {
        "wamn_receiving" => Some("receiving_sqlx_verifier"),
        "client_acme_receiving" => Some("client_acme_sqlx_verifier"),
        _ => None,
    }
}

/// Build the complete command arguments for the selected SQLx verifier.
pub fn prepare_arguments(verifier: &str, check: bool) -> Vec<String> {
    let mut arguments = vec!["cargo".to_owned(), "sqlx".to_owned(), "prepare".to_owned()];
    if check {
        arguments.push("--check".to_owned());
    }
    arguments.extend(
        ["--", "--test", verifier, "--locked", "--offline"]
            .into_iter()
            .map(str::to_owned),
    );
    arguments
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
    test_dir: &Path,
    database_url: &str,
    verifier: &str,
    check: bool,
) -> Command {
    let arguments = prepare_arguments(verifier, check);
    let mut command = Command::new(&arguments[0]);
    command
        .args(&arguments[1..])
        .current_dir(test_dir)
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
        let manifest = PackageManifest::from_slice(include_bytes!(
            "../../../../apps/wamn_receiving/wamn.json"
        ))
        .expect("read the existing package schema owner");
        let database_url = package_database_url(
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
            Some("-cstatement_timeout=2500 -csearch_path=old -csearch_path=receiving,public")
        );
        assert_eq!(connection.get_user(), Some("user"));
        assert_eq!(connection.get_password(), Some(b"password".as_slice()));
        let plain = package_database_url("postgresql://127.0.0.1/test", &manifest)
            .expect("scope the unconfigured local connection");
        assert_eq!(
            plain
                .parse::<tokio_postgres::Config>()
                .unwrap()
                .get_options(),
            Some("-csearch_path=receiving,public")
        );
    }

    #[test]
    fn prepare_and_check_name_each_verifier_and_force_online() {
        for package in ["wamn_receiving", "client_acme_receiving"] {
            let verifier = verifier_for(package).expect("the package has a verifier");
            assert_eq!(
                prepare_arguments(verifier, false),
                [
                    "cargo",
                    "sqlx",
                    "prepare",
                    "--",
                    "--test",
                    verifier,
                    "--locked",
                    "--offline"
                ]
            );
            assert_eq!(
                prepare_arguments(verifier, true),
                [
                    "cargo",
                    "sqlx",
                    "prepare",
                    "--check",
                    "--",
                    "--test",
                    verifier,
                    "--locked",
                    "--offline"
                ]
            );
            let directory = Path::new("/apps/package/tests");
            let command = prepare_command(directory, "postgresql://verify", verifier, false);
            let command = command.as_std();
            assert_eq!(command.get_current_dir(), Some(directory));
            let envs: Vec<_> = command.get_envs().collect();
            assert!(envs.contains(&(
                "DATABASE_URL".as_ref(),
                Some("postgresql://verify".as_ref())
            )));
            assert!(envs.contains(&("SQLX_OFFLINE".as_ref(), Some("false".as_ref()))));
        }
        assert_eq!(verifier_for("wamn_wms"), None);
        let warning = b"warning: potentially unused queries found in .sqlx; you may want to re-run sqlx prepare\n";
        assert!(require_current_metadata(warning).is_err());
        assert!(require_current_metadata(b"").is_ok());
    }
}
