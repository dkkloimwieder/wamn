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
        document.require_valid()?;
        Ok(document)
    }

    /// Resolve applied configuration and the manifests of its selected releases.
    pub async fn read_serving(platform: &super::Platform, triple: &Triple) -> anyhow::Result<Self> {
        let mut document = Self::read(platform, triple).await?;
        document.resolve_releases(platform).await?;
        Ok(document)
    }

    pub(super) async fn resolve_releases(
        &mut self,
        platform: &super::Platform,
    ) -> anyhow::Result<()> {
        for selection in self.applications.values_mut() {
            if let DeclaredRelease::Digest(digest) = &selection.current {
                let manifest = super::analyse::pull_release_manifest(platform, digest).await?;
                selection.packages = manifest
                    .release
                    .packages
                    .iter()
                    .map(|package| package.package_id().to_owned())
                    .collect();
            }
        }
        Ok(())
    }

    pub(super) async fn save(&self, connection: &mut sqlx::PgConnection) -> anyhow::Result<()> {
        ensure!(
            wamn_registry::configuration::write(connection, &self.configuration()).await?,
            "provision the environment before applying configuration"
        );
        Ok(())
    }

    /// Applied settings exclude package membership, which comes from the selected release.
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
                            release: match &application.current {
                                DeclaredRelease::None => None,
                                DeclaredRelease::Digest(digest) => Some(digest.clone()),
                            },
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

    #[tokio::test]
    async fn registry_configuration_round_trip() -> anyhow::Result<()> {
        use wamn_registry::configuration;
        let _lock = wamn_test_postgres::lock();
        let database = wamn_control_provision::test_database::system();
        database.execute(&[
            &wamn_control_provision::test_database::seed_org_project_env_sql("acme", "wms", "prod"),
        ])?;
        let triple = Triple::new("acme", "prod", "wms");
        let mut connection = wamn_postgres::connect(database.url()).await?;
        assert!(
            configuration::read(&mut connection, &triple)
                .await?
                .is_none()
        );
        let mut expected =
            EnvironmentDocument::from_json(&serde_json::to_vec(&configuration())?)?.configuration();
        expected.revocation_seconds = u64::MAX;
        expected
            .applications
            .get_mut("wms")
            .unwrap()
            .revocation_seconds = 0;
        let mut second = expected.applications["wms"].clone();
        second.hostname = "second.example".into();
        second.revocation_seconds = u64::MAX;
        second.connections.clear();
        expected.applications.insert("second".into(), second);
        database.execute(&[
            "CREATE ROLE wamn_apply LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS",
            wamn_control_provision::apply::APPLY_GRANTS_SQL,
        ])?;
        sqlx::raw_sql("SET ROLE wamn_apply")
            .execute(&mut connection)
            .await?;
        let mut transaction =
            crate::provision_project_env::provisioning_transaction(&mut connection).await?;
        assert!(configuration::write(&mut transaction, &expected).await?);
        transaction.commit().await?;
        sqlx::raw_sql("SET ROLE wamn_control")
            .execute(&mut connection)
            .await?;
        assert_eq!(
            configuration::read(&mut connection, &triple)
                .await?
                .as_ref(),
            Some(&expected)
        );
        sqlx::raw_sql("SET ROLE wamn_apply")
            .execute(&mut connection)
            .await?;
        let release = wamn_digest::bytes_sha256(b"selected release");
        let restored = Client {
            package: "wamn_wms".into(),
            web: wamn_digest::bytes_sha256(b"restored web"),
        };
        let mut transaction =
            crate::provision_project_env::provisioning_transaction(&mut connection).await?;
        assert!(
            configuration::write_selection(
                &mut transaction,
                &triple,
                "wms",
                Some(&release),
                Some(&restored)
            )
            .await?
        );
        transaction.commit().await?;
        let application = expected.applications.get_mut("wms").unwrap();
        application.release = Some(release);
        application.client = Some(restored);
        assert_eq!(
            configuration::read(&mut connection, &triple)
                .await?
                .as_ref(),
            Some(&expected)
        );
        // A settings write keeps desired releases and replaces obsolete child rows.
        expected.schema.mode = None;
        expected.applications.remove("second");
        expected
            .applications
            .get_mut("wms")
            .unwrap()
            .connections
            .clear();
        expected.integrations.clear();
        let mut transaction =
            crate::provision_project_env::provisioning_transaction(&mut connection).await?;
        assert!(configuration::write(&mut transaction, &expected).await?);
        transaction.commit().await?;
        assert_eq!(
            configuration::read(&mut connection, &triple)
                .await?
                .as_ref(),
            Some(&expected)
        );
        let mut transaction =
            crate::provision_project_env::provisioning_transaction(&mut connection).await?;
        assert!(
            configuration::write_selection(&mut transaction, &triple, "wms", None, None).await?
        );
        assert!(
            !configuration::write_selection(&mut transaction, &triple, "missing", None, None)
                .await?
        );
        transaction.commit().await?;
        let application = expected.applications.get_mut("wms").unwrap();
        application.release = None;
        application.client = None;
        assert_eq!(
            configuration::read(&mut connection, &triple)
                .await?
                .as_ref(),
            Some(&expected)
        );
        expected.applications.clear();
        let mut transaction =
            crate::provision_project_env::provisioning_transaction(&mut connection).await?;
        assert!(configuration::write(&mut transaction, &expected).await?);
        transaction.commit().await?;
        assert_eq!(
            configuration::read(&mut connection, &triple)
                .await?
                .as_ref(),
            Some(&expected)
        );
        Ok(())
    }

    #[tokio::test]
    async fn registry_configuration_constraints() -> anyhow::Result<()> {
        use wamn_registry::configuration;
        let _lock = wamn_test_postgres::lock();
        let database = wamn_control_provision::test_database::system();
        database.execute(&[
            &wamn_control_provision::test_database::seed_org_project_env_sql("acme", "wms", "prod"),
        ])?;
        let triple = Triple::new("acme", "prod", "wms");
        let mut connection = wamn_postgres::connect(database.url()).await?;
        let expected =
            EnvironmentDocument::from_json(&serde_json::to_vec(&configuration())?)?.configuration();
        let mut transaction =
            crate::provision_project_env::provisioning_transaction(&mut connection).await?;
        assert!(configuration::write(&mut transaction, &expected).await?);
        transaction.commit().await?;
        for change in [
            |c: &mut configuration::EnvironmentConfiguration| c.policy = "missing".into(),
            |c: &mut configuration::EnvironmentConfiguration| c.target = "missing".into(),
            |c: &mut configuration::EnvironmentConfiguration| {
                c.applications.get_mut("wms").unwrap().hostname = "Invalid Host".into()
            },
            |c: &mut configuration::EnvironmentConfiguration| {
                c.applications
                    .get_mut("wms")
                    .unwrap()
                    .client
                    .as_mut()
                    .unwrap()
                    .web = "latest".into()
            },
            |c: &mut configuration::EnvironmentConfiguration| {
                c.applications
                    .get_mut("wms")
                    .unwrap()
                    .client
                    .as_mut()
                    .unwrap()
                    .package
                    .clear()
            },
            |c: &mut configuration::EnvironmentConfiguration| {
                c.applications
                    .get_mut("wms")
                    .unwrap()
                    .connections
                    .insert("".into(), "shared-erp".into());
            },
            |c: &mut configuration::EnvironmentConfiguration| {
                c.applications
                    .get_mut("wms")
                    .unwrap()
                    .connections
                    .insert("erp".into(), "org:other:erp".into());
            },
            |c: &mut configuration::EnvironmentConfiguration| {
                c.integrations.get_mut("shared-erp").unwrap().version = 0
            },
        ] {
            let mut invalid = expected.clone();
            change(&mut invalid);
            let mut transaction =
                crate::provision_project_env::provisioning_transaction(&mut connection).await?;
            if configuration::write(&mut transaction, &invalid)
                .await
                .is_ok()
            {
                assert!(transaction.commit().await.is_err());
            } else {
                transaction.rollback().await?;
            }
        }
        let mut transaction =
            crate::provision_project_env::provisioning_transaction(&mut connection).await?;
        assert!(
            configuration::write_selection(&mut transaction, &triple, "wms", Some("latest"), None)
                .await
                .is_err()
        );
        transaction.rollback().await?;
        // SQL exercises constraints that the Rust input types cannot express.
        for statement in [
            "UPDATE registry.environment_projects SET revocation_seconds=-1 WHERE org='acme'",
            "UPDATE registry.environment_projects SET revocation_seconds=18446744073709551616 WHERE org='acme'",
            "UPDATE registry.environment_projects SET schema_mode='unknown' WHERE org='acme'",
            "UPDATE registry.environment_applications SET revocation_seconds=NULL WHERE org='acme'",
            "UPDATE registry.environment_applications SET client_web=NULL WHERE org='acme'",
            "UPDATE registry.environment_applications SET project='missing' WHERE org='acme'",
            "UPDATE registry.environment_application_connections SET application='missing' WHERE org='acme'",
            "UPDATE registry.environment_integrations SET project='missing' WHERE org='acme'",
            "INSERT INTO registry.environment_applications SELECT * FROM registry.environment_applications WHERE org='acme'",
            "INSERT INTO registry.environment_application_connections SELECT * FROM registry.environment_application_connections WHERE org='acme'",
            "INSERT INTO registry.environment_integrations SELECT * FROM registry.environment_integrations WHERE org='acme'",
        ] {
            let mut transaction =
                crate::provision_project_env::provisioning_transaction(&mut connection).await?;
            assert!(
                sqlx::query(statement)
                    .execute(&mut *transaction)
                    .await
                    .is_err()
            );
            transaction.rollback().await?;
        }
        assert_eq!(
            configuration::read(&mut connection, &triple)
                .await?
                .as_ref(),
            Some(&expected)
        );
        database.execute(&[
            &wamn_control_provision::test_database::seed_org_project_env_sql("acme", "wms", "dev"),
        ])?;
        let mut redirected = expected.clone();
        redirected.policy = "dev".into();
        redirected.revocation_seconds = 0;
        let mut transaction =
            crate::provision_project_env::provisioning_transaction(&mut connection).await?;
        let refusal = configuration::write(&mut transaction, &redirected)
            .await
            .expect_err("configuration must not redirect an existing project database");
        assert!(matches!(refusal, sqlx::Error::InvalidArgument(_)));
        transaction.commit().await?;
        assert_eq!(
            configuration::read(&mut connection, &triple)
                .await?
                .as_ref(),
            Some(&expected)
        );
        let missing = Triple::new("acme", "prod", "missing");
        assert!(
            configuration::read(&mut connection, &missing)
                .await?
                .is_none()
        );
        let mut absent = expected;
        absent.project = missing.project;
        let mut transaction =
            crate::provision_project_env::provisioning_transaction(&mut connection).await?;
        assert!(!configuration::write(&mut transaction, &absent).await?);
        transaction.rollback().await?;
        Ok(())
    }
}
