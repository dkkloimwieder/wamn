//! Publish releases without wirings and return the lines the owner commits.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use serde_json::json;

use crate::environment::document::{DeclaredRelease, EnvironmentDocument};
use crate::package_artifact::{PackageSource, open_package_source};
use crate::release_chart::Target;
use crate::release_composition::{PackageInput, load_wirings};

use super::document::ClusterDocument;
use super::install::runner_image;
use super::job::{self, JobRequest};
use super::packages;

pub(super) struct Published {
    pub changed: bool,
    pub releases: Vec<(PathBuf, EnvironmentDocument, String)>,
}

pub(super) async fn apply(
    target: &Target,
    document: &ClusterDocument,
    repository: &Path,
    environments: &Path,
    directory: &Path,
) -> anyhow::Result<Published> {
    let registry = packages::registry(document)?;
    let image = runner_image(directory)?;
    let mut prepared = Vec::new();
    // Refuse every wiring before starting any publication Job.
    for (path, environment) in super::broker::documents(environments)? {
        let tenant = format!(
            "{}--{}--{}",
            environment.org, environment.project, environment.env
        );
        let mut request = JobRequest {
            name: format!("platform-release-{tenant}"),
            image: image.clone(),
            capture_output: true,
            arguments: vec![
                "publish-release".into(),
                "--database-url".into(),
                "$(WAMN_PG_ADMIN_URL)".into(),
                "--control-database-url".into(),
                "$(WAMN_SYSTEM_ADMIN_URL)".into(),
                "--org".into(),
                environment.org.clone(),
                "--tenant".into(),
                tenant.clone(),
                "--environment".into(),
                environment.env.clone(),
                "--verified-publisher-principal".into(),
                format!("wamn-management-author-{tenant}"),
                "--run-schema".into(),
                "wamn_run".into(),
                "--artifact-base".into(),
                registry.artifact_base.clone(),
            ],
            ..JobRequest::default()
        };
        let mut artifacts = Vec::new();
        for package in packages::read(repository, &path)? {
            let identity = &package.manifest.package;
            let opened = open_package_source(PackageSource::Artifact {
                tag: format!("{}-{}", identity.id, identity.version),
                registry: registry.clone(),
            })
            .await?;
            let input = PackageInput {
                root: opened.root().to_owned(),
                manifest: wamn_schema_generator::PackageManifest::from_slice(&opened.manifest()?)?,
            };
            if let Some(wiring) = load_wirings(&[input])?.first() {
                anyhow::bail!(
                    "release for {} names wiring {}::{}; wamn-wgpm9.4 must supply the in-cluster wiring gate before step 4.14",
                    path.display(),
                    wiring.package_id,
                    wiring.wiring.wiring_id
                );
            }
            artifacts.push(opened.package_root().artifact_digest);
            request.arguments.extend([
                "--package".into(),
                format!("{}@{}", identity.id, identity.version),
            ]);
        }
        ensure!(
            !artifacts.is_empty(),
            "environment {} names no package",
            path.display()
        );
        packages::registry_inputs(&mut request, &registry)?;
        request.environment.insert(
            "WAMN_RELEASE_ARTIFACT_BASE".into(),
            format!("{}/releases", document.registry),
        );
        prepared.push((path, environment, request, artifacts));
    }
    let mut published = Published {
        changed: false,
        releases: Vec::new(),
    };
    for (path, environment, request, artifacts) in prepared {
        let secrets = packages::credentials(target, &environment).await?;
        published.changed |= job::run(
            target,
            directory,
            &request,
            &json!({"artifacts": artifacts}),
            &secrets,
        )
        .await?;
        let output = job::output(target, &request).await?;
        let digest = output.trim();
        let release: DeclaredRelease = serde_json::from_value(json!(digest))
            .context("publish-release returned no valid release digest")?;
        ensure!(
            matches!(release, DeclaredRelease::Digest(_)),
            "publish-release returned no release digest"
        );
        published.releases.push((path, environment, digest.into()));
    }
    Ok(published)
}
