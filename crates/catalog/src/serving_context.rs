//! Manifest context used by the edge runtime and test fixtures.

use crate::{AdmittedComponent, ServingManifest};
use std::sync::Arc;

/// The project, environment and application a host serves, from the
/// host configuration.
///
/// The serving manifest carries none of them (R1). The host derives the
/// project from its `(org, project, environment)` coordinate, takes the
/// targeted application that `environment.k` declares, and every reader takes it
/// from here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseTarget {
    /// The composed `org--env--project` key of the project environment.
    pub project_env_key: wamn_registry::ProjectEnvKey,
    /// The short environment name, such as `dev`.
    pub environment: wamn_registry::EnvName,
    /// The single application name and its packages
    /// (docs/architecture/execution.md#workflows).
    pub applications: crate::EnvironmentApplications,
}

impl ReleaseTarget {
    /// A target of one project and environment that declares no application.
    pub fn new(
        project_env_key: wamn_registry::ProjectEnvKey,
        environment: wamn_registry::EnvName,
    ) -> Self {
        Self {
            project_env_key,
            environment,
            applications: crate::EnvironmentApplications::new(),
        }
    }

    /// This target with the targeted application membership.
    #[must_use]
    pub fn with_applications(mut self, applications: crate::EnvironmentApplications) -> Self {
        self.applications = applications;
        self
    }
}

/// Manifest facts retained until their owning readers leave the release document.
#[derive(Debug)]
pub struct ServingContext {
    manifest: ServingManifest,
    target: ReleaseTarget,
    components: Arc<[AdmittedComponent]>,
    release: CarriedRelease,
}

/// The content identity of the serving manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CarriedRelease {
    pub manifest_digest: wamn_digest::Digest,
}

impl ServingContext {
    /// Bind an admitted manifest to its deployment coordinate.
    pub fn new(manifest: ServingManifest, target: ReleaseTarget) -> anyhow::Result<Self> {
        anyhow::ensure!(
            target.applications.len() <= 1,
            "a loaded release serves at most one application"
        );
        if let Some((application, membership)) = target.applications.first_key_value() {
            anyhow::ensure!(
                manifest
                    .release
                    .packages
                    .iter()
                    .all(|package| membership.packages.contains(package.package_id())),
                "release carries a package outside application {application}"
            );
        }
        let components = manifest.admitted_components(&target.project_env_key)?;
        for component in &components {
            crate::verify_stored_effect_projection(component)?;
        }
        let release = CarriedRelease {
            manifest_digest: manifest.digest(),
        };
        Ok(Self {
            manifest,
            target,
            components: components.into(),
            release,
        })
    }

    /// The manifest retained by this reader.
    pub fn manifest(&self) -> &ServingManifest {
        &self.manifest
    }
    /// The deployment coordinate and application membership.
    pub fn target(&self) -> &ReleaseTarget {
        &self.target
    }
    /// The component facts admitted once for this context.
    pub fn components(&self) -> &Arc<[AdmittedComponent]> {
        &self.components
    }
    /// The manifest content identity retained by deferred readers.
    pub fn release(&self) -> &CarriedRelease {
        &self.release
    }

    /// Bind the canonical manifest bytes used by host service fixtures.
    #[cfg(feature = "test-util")]
    pub fn load_canonical_bytes(
        bytes: &[u8],
        origin: &str,
        target: ReleaseTarget,
    ) -> anyhow::Result<Self> {
        let (manifest, _) = ServingManifest::from_canonical_bytes(bytes)
            .map_err(|error| anyhow::anyhow!("serving manifest {origin} refused: {error}"))?;
        Self::new(manifest, target)
    }
}
