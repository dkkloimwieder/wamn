//! The environment document, `environment.k`
//! (docs/architecture/overview.md#environment-lifecycle).
//!
//! The registry owns configuration. Release selections are transient execution inputs.
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
        let bytes = if file
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            std::fs::read(file)?
        } else {
            crate::kcl::compile(file, "environment")?
        };
        Self::from_json(&bytes)
            .with_context(|| format!("{} is not a valid environment", file.display()))
    }
}

/// Write declared credential revocation bounds before starting an authenticated host.
/// The caller binds the provisioning actor in its project transaction.
///
/// # Errors
/// Returns an error if the project configuration cannot be written.
pub async fn write_revocation_configuration(
    client: &mut sqlx::PgConnection,
    document: &EnvironmentDocument,
) -> Result<u64, wamn_platform_identity::IdentityError> {
    use wamn_platform_identity::configuration::{
        PlatformConfiguration, Setting, write_configuration,
    };
    let settings = std::iter::once(Setting {
        target_type: "project".into(),
        target_id: document.project.clone(),
        key: "revocation-seconds".into(),
        value: document.revocation_seconds.into(),
        locked: true,
    })
    .chain(
        document
            .applications
            .iter()
            .map(|(name, selection)| Setting {
                target_type: "application".into(),
                target_id: name.clone(),
                key: "revocation-seconds".into(),
                value: selection.revocation_seconds.into(),
                locked: true,
            }),
    )
    .collect();
    write_configuration(
        client,
        &PlatformConfiguration {
            settings,
            ..Default::default()
        },
    )
    .await
}

/// A request to change the release of one application.
#[derive(Debug, Clone, Copy)]
pub(super) enum ReleaseChange<'a> {
    Deploy(&'a str),
    Configure,
    Stop,
    Rollback(Option<&'a str>),
}

impl EnvironmentDocument {
    /// Read applied configuration by coordinate from the registry.
    pub async fn read(platform: &super::Platform, triple: &Triple) -> anyhow::Result<Self> {
        let mut system = wamn_postgres::connect(&platform.system_database_url).await?;
        let configuration = wamn_registry::configuration::read(&mut system, triple)
            .await?
            .with_context(|| format!("environment {triple} has no applied configuration"))?;
        let document = Self::from_configuration(configuration);
        ensure!(
            document.triple() == *triple,
            "stored environment coordinate differs"
        );
        document.require_valid()?;
        Ok(document)
    }

    /// Resolve applied configuration and the releases currently served by Helm.
    pub async fn read_serving(platform: &super::Platform, triple: &Triple) -> anyhow::Result<Self> {
        let mut document = Self::read(platform, triple).await?;
        document.resolve_releases(platform).await?;
        Ok(document)
    }

    pub(super) async fn resolve_releases(
        &mut self,
        platform: &super::Platform,
    ) -> anyhow::Result<()> {
        for name in self.applications.keys().cloned().collect::<Vec<_>>() {
            let release_name = super::analyse::application_release_name(self, &name)?;
            if let Some(revision) = super::analyse::read_revision(platform, &release_name).await? {
                let digest = revision
                    .manifest_digest()
                    .context("Helm revision has no release digest")?;
                self.resolve_release(platform, &name, digest).await?;
                self.applications
                    .get_mut(&name)
                    .expect("application was resolved")
                    .client = revision.client()?;
            }
        }
        Ok(())
    }

    pub(super) async fn resolve_release(
        &mut self,
        platform: &super::Platform,
        application: &str,
        digest: &str,
    ) -> anyhow::Result<()> {
        wamn_package_artifact::ensure_digest(digest)?;
        let manifest = super::analyse::pull_release_manifest(platform, digest).await?;
        let selection = self
            .applications
            .get_mut(application)
            .with_context(|| format!("application {application} is not declared"))?;
        selection.current = DeclaredRelease::Digest(digest.to_owned());
        selection.packages = manifest
            .release
            .packages
            .iter()
            .map(|package| package.package_id().to_owned())
            .collect();
        Ok(())
    }

    pub(super) async fn save(&self, connection: &mut sqlx::PgConnection) -> anyhow::Result<()> {
        ensure!(
            wamn_registry::configuration::write(connection, &self.configuration()).await?,
            "provision the environment before applying configuration"
        );
        Ok(())
    }

    /// Configuration excludes runtime release and package selections.
    pub fn configuration(&self) -> wamn_registry::configuration::EnvironmentConfiguration {
        use wamn_registry::configuration::{ApplicationConfiguration, EnvironmentConfiguration};
        EnvironmentConfiguration {
            org: self.org.clone(),
            project: self.project.clone(),
            env: self.env.clone(),
            target: self.target.clone(),
            policy: self.policy.clone(),
            revocation_seconds: self.revocation_seconds,
            schema: self.schema,
            applications: self
                .applications
                .iter()
                .map(|(name, application)| {
                    (
                        name.clone(),
                        ApplicationConfiguration {
                            hostname: application.hostname.clone(),
                            revocation_seconds: application.revocation_seconds,
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
        configuration: wamn_registry::configuration::EnvironmentConfiguration,
    ) -> Self {
        Self {
            org: configuration.org,
            project: configuration.project,
            env: configuration.env,
            target: configuration.target,
            policy: configuration.policy,
            revocation_seconds: configuration.revocation_seconds,
            schema: configuration.schema,
            applications: configuration
                .applications
                .into_iter()
                .map(|(name, application)| {
                    (
                        name,
                        ApplicationSelection {
                            hostname: application.hostname,
                            revocation_seconds: application.revocation_seconds,
                            client: application.client,
                            connections: application.connections,
                            current: DeclaredRelease::None,
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
            "applications":{"wms":{"hostname":"wms.example","revocation_seconds":5,
                "connections":{"erp":"shared-erp"},
                "client":{"package":"wamn_wms","web":format!("sha256:{:064x}",1)}}},
            "integrations":{"shared-erp":{"version":1}}})
    }

    #[test]
    fn refuses_obsolete_ownership_and_invalid_configuration() {
        let parse =
            |value: Value| EnvironmentDocument::from_json(&serde_json::to_vec(&value).unwrap());
        assert!(parse(configuration()).is_ok());
        for (field, value) in [
            ("release", json!("none")),
            ("route_host", json!("wms.example")),
            ("client", json!({})),
            ("connections", json!({})),
            ("image", json!("host")),
            ("target", json!("  ")),
            ("revocation_seconds", json!(-1)),
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
            ("mode", json!("http")),
            ("connections", json!({"erp":"org:other:erp"})),
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
        for invalid in ["", "latest", "sha256:", "sha256:ABC"] {
            let mut input = configuration();
            input["applications"]["wms"]["client"]["web"] = invalid.into();
            assert!(parse(input).is_err());
        }
    }

    #[test]
    fn configuration_round_trips_without_runtime_selections() -> anyhow::Result<()> {
        let mut document = EnvironmentDocument::from_json(&serde_json::to_vec(&configuration())?)?;
        let application = document.applications.get_mut("wms").unwrap();
        application.current = DeclaredRelease::Digest(format!("sha256:{:064x}", 2));
        application.packages.insert("wamn_wms".to_owned());
        let stored = serde_json::to_vec(&document.configuration())?;
        let read = EnvironmentDocument::from_json(&stored)?;
        assert_eq!(read.configuration(), document.configuration());
        assert!(matches!(
            read.applications["wms"].current,
            DeclaredRelease::None
        ));
        assert!(read.applications["wms"].packages.is_empty());
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
