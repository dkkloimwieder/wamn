//! `env show`: an environment document synthesized from the authorities
//! (docs/architecture/overview.md#environment-lifecycle).
//!
//! Each application comes from its own successful Helm revision.
//! The environment row supplies the shared policy.
//! Connection names come from the applied application map, and the
//! document uses the current authorities. `show` takes no lock.

pub(super) mod plan;
use plan::revision_applications;
pub(super) use plan::revision_client;

use std::collections::BTreeMap;

use anyhow::Context as _;
use wamn_control_registry::Triple;

use super::Platform;
use super::analyse::{read_revision, read_row};
use super::document::{ApplicationSelection, Client, DeclaredRelease, EnvironmentDocument};
use wamn_package_control::database::connect;

/// Synthesize the document of an environment.
///
/// # Errors
///
/// When the environment has no row, or an authority cannot be read.
pub async fn show(platform: &Platform, triple: &Triple) -> anyhow::Result<EnvironmentDocument> {
    let mut document = EnvironmentDocument {
        target: "cloud".into(),
        org: triple.org.clone(),
        project: triple.project.clone(),
        env: triple.env.as_str().to_owned(),
        policy: String::new(),
        applications: BTreeMap::new(),
        integrations: BTreeMap::new(),
    };
    let system = connect(&platform.system_database_url).await?;
    let row = read_row(&system, &document)
        .await?
        .with_context(|| format!("environment {triple} has no row"))?;
    document.policy = row.policy_name;
    document.target = system.query_one("SELECT t.name FROM registry.project_envs e JOIN registry.target t ON t.id = e.target_id WHERE e.org = $1 AND e.project = $2 AND e.env = $3", &[&document.org, &document.project, &document.env]).await?.get(0);
    for (application, name) in super::analyse::application_releases(platform, triple).await? {
        let Some(revision) = read_revision(platform, &name).await? else {
            continue;
        };
        let mut membership = revision_applications(&revision.values)?;
        let membership = membership
            .remove(&application)
            .context("application is absent from its Helm revision")?;
        let hostname = revision.values["routeHost"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let current = revision
            .manifest_digest()
            .map_or(DeclaredRelease::None, |digest| {
                DeclaredRelease::Digest(digest.to_owned())
            });
        let client = revision_client(&revision.values, &hostname)?;
        document.applications.insert(
            application.clone(),
            ApplicationSelection {
                current: current.clone(),
                previous: None,
                hostname,
                client,
                packages: membership.packages,
                connections: membership.connections,
            },
        );
        if let DeclaredRelease::Digest(digest) = &current {
            let url = super::ensure::project_url(platform, triple).await?;
            let project = super::analyse::connect_project(&url).await?;
            let env_key = wamn_control_provision::project_env_key(
                &document.org,
                &document.project,
                &document.env,
            );
            for binding in project.query(
                "SELECT b.package_id,b.name,i.name,r.version FROM catalog.connection_binding b JOIN catalog.release_connection r USING(env_key,manifest_digest,connection_id) JOIN platform.connection c ON c.id=b.connection_id JOIN platform.integration i ON i.id=c.integration_id WHERE b.env_key=$1 AND b.manifest_digest=$2",
                &[&env_key,digest],
            ).await? {
                let selection = document.applications.get_mut(&application).expect("observed application");
                let package: String = binding.get(0);
                anyhow::ensure!(selection.packages.contains(&package), "binding outside application {application}");
                let integration: String = binding.get(2);
                selection.connections.insert(binding.get(1), integration.clone());
                document.integrations.insert(integration, super::document::Integration { version: binding.get(3) });
            }
        }
    }
    Ok(document)
}

/// Read the target application's release and membership from the same successful revision.
pub async fn serving_application(
    platform: &Platform,
    triple: &Triple,
    application: &str,
) -> anyhow::Result<(String, wamn_catalog::EnvironmentApplications)> {
    let name = wamn_control_provision::environment_state::application_release_name(
        &triple.org,
        &triple.project,
        triple.env.as_str(),
        application,
    )?;
    let revision = read_revision(platform, &name)
        .await?
        .with_context(|| format!("application {application} in {triple} serves no release"))?;
    let membership = revision_applications(&revision.values)?;
    anyhow::ensure!(
        membership.len() == 1 && membership.contains_key(application),
        "application Helm revision has foreign membership"
    );
    let digest = revision
        .manifest_digest()
        .context("application revision has no release digest")?
        .to_owned();
    Ok((digest, membership))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn revision_client_reads_the_package_and_web_pin_together() {
        let digest = wamn_execution_contract::bytes_sha256(b"web artifact");
        let values = json!({
            "webClients": {"wms.example.com": "wms"},
            "webDigests": {"wms.example.com": digest},
        });
        assert_eq!(
            revision_client(&values, "wms.example.com").unwrap(),
            Some(Client {
                package: "wms".into(),
                web: digest
            })
        );
        assert_eq!(revision_client(&values, "other.example.com").unwrap(), None);
        for key in ["webClients", "webDigests"] {
            let mut incomplete = values.clone();
            incomplete.as_object_mut().unwrap().remove(key);
            assert!(revision_client(&incomplete, "wms.example.com").is_err());
        }
    }
}
