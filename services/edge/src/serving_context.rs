//! Manifest context used by the edge runtime and test fixtures.

use std::sync::Arc;
use wamn_component_policy::{AdmittedComponent, verify_stored_effect_projection};
use wamn_edge_document::ServingManifest;

/// The project and environment a host serves, from the host configuration.
///
/// The serving manifest carries neither of them (R1). The host derives the
/// project from its `(org, project, environment)` coordinate, and every reader
/// takes it from here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseTarget {
    /// The composed `org--env--project` key of the project environment.
    pub project_env_key: wamn_registry::ProjectEnvKey,
    /// The short environment name, such as `dev`.
    pub env: wamn_registry::EnvName,
}

impl ReleaseTarget {
    /// A target of one project and environment.
    pub fn new(project_env_key: wamn_registry::ProjectEnvKey, env: wamn_registry::EnvName) -> Self {
        Self {
            project_env_key,
            env,
        }
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
        let components = manifest.admitted_components(&target.project_env_key)?;
        for component in &components {
            verify_stored_effect_projection(component)?;
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
    /// The deployment coordinate.
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
}
