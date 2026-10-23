//! Publish releases through cluster Jobs and return the lines the owner commits.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use serde_json::json;

use crate::environment::document::{DeclaredRelease, EnvironmentDocument};
use crate::package_artifact::{PackageSource, open_package_source};
use crate::release_chart::Target;
use crate::release_composition::{PackageInput, load_wirings};

use super::install::runner_image;
use super::job::{self, JobRequest};
use super::packages;

pub(super) struct Published {
    pub changed: bool,
    pub releases: Vec<(PathBuf, EnvironmentDocument, String)>,
}

pub(super) async fn apply(
    target: &Target,
    repository: &Path,
    environments: &Path,
    directory: &Path,
    registry: &wamn_package_artifact::PackageRegistry,
) -> anyhow::Result<Published> {
    let image = runner_image(directory)?;
    let mut prepared = Vec::new();
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
        let mut wirings = Vec::new();
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
            for wiring in load_wirings(&[input])? {
                wirings.push(job::input_digest(&wiring.document));
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
        packages::registry_inputs(&mut request, registry)?;
        request.environment.insert(
            "WAMN_RELEASE_ARTIFACT_BASE".into(),
            packages::artifact_base(registry, "releases")?,
        );
        request.environment.insert(
            "WAMN_COMPONENT_ARTIFACT_BASE".into(),
            packages::artifact_base(registry, "components")?,
        );
        if !wirings.is_empty() {
            request.arguments[0] = "publish-wired-release".into();
            // The joined verb reads administrator URLs directly from its environment.
            request.arguments.drain(1..5);
            let principal = request
                .arguments
                .iter()
                .position(|arg| arg == "--verified-publisher-principal")
                .expect("publication arguments name the publisher");
            request.arguments.drain(principal..principal + 2);
            request
                .arguments
                .extend(["--project".into(), environment.project.clone()]);
            request.environment.insert(
                "WAMN_PACKAGE_ARTIFACT_BASE".into(),
                registry.artifact_base.clone(),
            );
            // Reading a package wiring compiles its wamn.k.
            request
                .environment
                .insert("WAMN_KCL".into(), "/usr/local/bin/kcl".into());
        }
        prepared.push((path, environment, request, artifacts, wirings));
    }
    let mut published = Published {
        changed: false,
        releases: Vec::new(),
    };
    for (path, environment, mut request, artifacts, wirings) in prepared {
        let mut secrets = Vec::from(packages::credentials(target, &environment).await?);
        let database = url::Url::parse(&secrets[1].value)
            .map_err(|_| anyhow::anyhow!("parse environment database URL"))?;
        let mut job_target = target.clone();
        let mut inputs = json!({"artifacts": artifacts, "database": database.path()});
        if !wirings.is_empty() {
            job_target.namespace = "identity".into();
            super::wiring_job::configure(&mut request, &environment, &secrets);
            secrets.push(super::wiring_job::identity_reader(target, &environment).await?);
            inputs["wirings"] = json!(wirings);
        }
        published.changed |= job::run(&job_target, directory, &request, &inputs, &secrets).await?;
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
