//! The environment document, `environment.k`
//! (docs/architecture/overview.md#environment-lifecycle).
//!
//! The registry owns applied settings and desired release selections.
//! Runtime hosts receive only the declared packages and connections.
//! KCL compiles the document to JSON. Its canonical form is RFC 8785 JSON.

pub(super) mod plan;

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use wamn_catalog::EnvironmentApplications;
use wamn_registry::Triple;

pub use plan::{ApplicationSelection, EnvironmentDocument};

pub use plan::Client;

pub use plan::Integration;

pub use plan::DeclaredRelease;
pub use plan::{Schema, SchemaMode};

impl EnvironmentDocument {
    /// Read compiled JSON or compile an `environment.k` file.
    ///
    /// # Errors
    ///
    /// When KCL refuses the file, or when the JSON is not a valid document.
    pub fn compile(file: &Path) -> anyhow::Result<Self> {
        Self::from_json(&std::fs::read(file)?)
            .with_context(|| format!("{} is not a valid environment", file.display()))
    }
}

/// Read protected configuration as the provisioning component.
pub(super) async fn read_configuration(
    connection: &mut sqlx::PgConnection,
    triple: &Triple,
) -> Result<Option<wamn_catalog::configuration::EnvironmentConfiguration>, sqlx::Error> {
    let principal = wamn_provision::PlatformComponent::Provisioning.principal_id();
    let mut transaction =
        wamn_identity::transaction::read_connection(connection, &principal).await?;
    let configuration = wamn_catalog::configuration::read(&mut transaction, triple).await?;
    transaction.commit().await?;
    Ok(configuration)
}

impl EnvironmentDocument {
    /// Read applied configuration by coordinate from the registry.
    pub async fn read(platform: &super::Platform, triple: &Triple) -> anyhow::Result<Self> {
        let mut system = wamn_postgres::connect(&platform.system_database_url).await?;
        let configuration = read_configuration(&mut system, triple)
            .await?
            .with_context(|| format!("environment {triple} has no applied configuration"))?;
        let document = Self::from_configuration(configuration);
        document.require_valid()?;
        Ok(document)
    }

    /// Resolve applied configuration and the manifests of its selected releases.
    pub async fn read_serving(platform: &super::Platform, triple: &Triple) -> anyhow::Result<Self> {
        let document = Self::read(platform, triple).await?;
        Ok(document)
    }


    pub(super) async fn save(&self, connection: &mut sqlx::PgConnection) -> anyhow::Result<()> {
        ensure!(
            wamn_catalog::configuration::write(connection, &self.configuration()).await?,
            "provision the environment before applying configuration"
        );
        Ok(())
    }

    /// Applied settings exclude package membership, which comes from the selected release.
    pub fn configuration(&self) -> wamn_catalog::configuration::EnvironmentConfiguration {
        use wamn_catalog::configuration::{ApplicationConfiguration, EnvironmentConfiguration};
        EnvironmentConfiguration {
            org: self.org.clone(),
            project: self.project.clone(),
            env: self.env.clone(),
            target: self.target.clone(),
            policy: self.policy.clone(),
            schema: self.schema,
            applications: self
                .applications
                .iter()
                .map(|(name, application)| {
                    (
                        name.clone(),
                        ApplicationConfiguration {
                            release: match &application.current {
                                DeclaredRelease::None => None,
                                DeclaredRelease::Digest(digest) => Some(*digest),
                            },
                            hostname: application.hostname.clone(),
                            client: application.client.clone(),
                            connections: application.connections.clone(),
                        },
                    )
                })
                .collect(),
            integrations: self.integrations.clone(),
        }
    }

    fn from_configuration(
        configuration: wamn_catalog::configuration::EnvironmentConfiguration,
    ) -> Self {
        Self {
            org: configuration.org,
            project: configuration.project,
            env: configuration.env,
            target: configuration.target,
            policy: configuration.policy,
            revocation_seconds: None,
            schema: configuration.schema,
            applications: configuration
                .applications
                .into_iter()
                .map(|(name, application)| {
                    (
                        name,
                        ApplicationSelection {
                            hostname: application.hostname,
                            client: application.client,
                            connections: application.connections,
                            current: application
                                .release
                                .map_or(DeclaredRelease::None, DeclaredRelease::Digest),
                            packages: std::collections::BTreeSet::new(),
                        },
                    )
                })
                .collect(),
            integrations: configuration.integrations,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn configuration() -> Value {
        json!({"org":"acme","project":"wms","env":"prod","target":"cloud",
            "policy":"prod","revocation_seconds":60,"schema":{"mode":"reset"},
            "applications":{"wms":{"hostname":"wms.example",
                "connections":{"erp":"shared-erp"},
                "client":{"package":"wamn_wms","web":format!("sha256:{:064x}",1)}}},
            "integrations":{"shared-erp":{"version":1}}})
    }

    #[test]
    fn refuses_obsolete_ownership_and_invalid_configuration() {
        let parse =
            |value: Value| EnvironmentDocument::from_json(&serde_json::to_vec(&value).unwrap());
        assert!(parse(configuration()).is_ok());
        for version in [1, i32::MAX] {
            let mut input = configuration();
            input["revocation_seconds"] = json!(3600);
            input["applications"]["wms"]["connections"]["erp"] =
                json!(format!("shared-erp@{version}"));
            let document = parse(input).unwrap();
            assert_eq!(
                document.applications["wms"].connections["erp"].version,
                Some(version)
            );
            let restored =
                EnvironmentDocument::from_json(&document.canonical_json().unwrap()).unwrap();
            assert_eq!(
                restored.applications["wms"].connections["erp"].version,
                Some(version)
            );
        }
        for (field, value) in [
            ("release", json!("none")),
            ("route_host", json!("wms.example")),
            ("client", json!({})),
            ("connections", json!({})),
            ("image", json!("host")),
            ("target", json!("  ")),
            ("revocation_seconds", json!(-1)),
            ("revocation_seconds", json!(3601)),
        ] {
            let mut input = configuration();
            input[field] = value;
            assert!(parse(input).is_err());
        }
        for field in ["target", "applications", "revocation_seconds"] {
            let mut input = configuration();
            input.as_object_mut().unwrap().remove(field);
            assert!(parse(input).is_err());
        }
        for (field, value) in [
            ("packages", json!(["wamn_wms"])),
            ("current", json!("none")),
            ("previous", json!(null)),
            ("hostname", json!("Invalid Host")),
            ("revocation_seconds", json!(-1)),
            ("revocation_seconds", json!(3601)),
            ("mode", json!("http")),
            ("connections", json!({"erp":"org:other:erp"})),
            ("connections", json!({"erp":"shared-erp@0"})),
            ("connections", json!({"erp":"shared-erp@2147483648"})),
            ("connections", json!({"erp":"shared-erp@01"})),
            ("connections", json!({"erp":"shared-erp@1@2"})),
            (
                "connections",
                json!({"erp":{"type":"http","definition":{}}}),
            ),
        ] {
            let mut input = configuration();
            input["applications"]["wms"][field] = value;
            assert!(parse(input).is_err());
        }
        for invalid in ["", "unknown", "RESET"] {
            let mut input = configuration();
            input["schema"]["mode"] = invalid.into();
            assert!(parse(input).is_err());
        }
    }

    #[test]
    fn omitted_schema_mode_uses_the_disposable_designation() -> anyhow::Result<()> {
        let read = EnvironmentDocument::from_json(&serde_json::to_vec(&configuration())?)?;
        for (disposable, expected) in [
            (Some(true), SchemaMode::Reset),
            (Some(false), SchemaMode::Migrate),
            (None, SchemaMode::Migrate),
        ] {
            let mut read = read.clone();
            read.schema.mode = None;
            assert_eq!(read.schema_mode(disposable), expected);
        }
        Ok(())
    }

}
