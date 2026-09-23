//! Digest-verified supply of admitted component bytes.
//!
//! [`ArtifactSource`] is the one way a host gets the bytes of an admitted
//! component. Every source returns only bytes checked against that fact. This
//! module holds the trait, the digest checks, and the local file source. The
//! OCI registry source lives in `wamn-runtime` and implements the same trait.
//! No source owns a catalog read, cache, instance pool, router behavior, or
//! retry policy.

use std::fmt;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use wamn_catalog::AdmittedComponent;

use crate::component_admission::component_digest;
use crate::component_artifact::component_digest_tag;

/// A source that returns only verified component bytes.
#[async_trait]
pub trait ArtifactSource: Send + Sync {
    /// Pull and verify the exact bytes named by one admitted component fact.
    async fn pull_verified(
        &self,
        component: &AdmittedComponent,
    ) -> Result<Vec<u8>, ComponentArtifactFetchError>;
}

/// Stable classification of a refused component artifact pull.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentArtifactFetchErrorKind {
    /// The supplied admitted digest cannot name an immutable artifact.
    InvalidReference,
    /// `oci-client` rejected the transport configuration, so no client exists.
    RegistryClient,
    /// The registry or named artifact is not currently available.
    Unavailable,
    /// The registry answered with bytes or metadata that contradict admission.
    Mismatched,
}

/// Contextual refusal from the component artifact transfer boundary.
pub struct ComponentArtifactFetchError {
    kind: ComponentArtifactFetchErrorKind,
    reference: Option<Box<str>>,
    refusal: &'static str,
}

impl ComponentArtifactFetchError {
    /// Stable refusal class for callers that must not match display text.
    pub fn kind(&self) -> ComponentArtifactFetchErrorKind {
        self.kind
    }

    /// Stable literal naming the exact refused invariant or transfer phase.
    pub fn refusal(&self) -> &'static str {
        self.refusal
    }

    /// The admitted digest cannot name an immutable artifact.
    pub fn invalid_reference() -> Self {
        Self {
            kind: ComponentArtifactFetchErrorKind::InvalidReference,
            reference: None,
            refusal: "component-artifact-digest-invalid",
        }
    }

    /// The bundles were readable but at least one is not usable as a trust root.
    pub fn registry_client() -> Self {
        Self {
            kind: ComponentArtifactFetchErrorKind::RegistryClient,
            reference: None,
            refusal: "component-artifact-registry-client-unusable",
        }
    }

    /// The artifact at `reference` is not currently available.
    pub fn unavailable(reference: &str, refusal: &'static str) -> Self {
        Self {
            kind: ComponentArtifactFetchErrorKind::Unavailable,
            reference: Some(reference.into()),
            refusal,
        }
    }

    /// The artifact at `reference` contradicts admission.
    pub fn mismatched(reference: &str, refusal: &'static str) -> Self {
        Self {
            kind: ComponentArtifactFetchErrorKind::Mismatched,
            reference: Some(reference.into()),
            refusal,
        }
    }
}

impl fmt::Debug for ComponentArtifactFetchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The upstream registry error is deliberately omitted: this type is
        // safe to log without emitting response bodies or future auth context.
        formatter
            .debug_struct("ComponentArtifactFetchError")
            .field("kind", &self.kind)
            .field("reference", &self.reference)
            .field("refusal", &self.refusal)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for ComponentArtifactFetchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(reference) = &self.reference {
            write!(
                formatter,
                "component artifact {reference}: {}",
                self.refusal
            )
        } else {
            write!(formatter, "component artifact: {}", self.refusal)
        }
    }
}

impl std::error::Error for ComponentArtifactFetchError {}

/// Digest-addressed local files, read through the same component validation.
#[derive(Debug, Clone)]
pub struct LocalComponentSource {
    directory: PathBuf,
}

impl LocalComponentSource {
    /// Read digest-addressed local files under `directory`.
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }
}

#[async_trait]
impl ArtifactSource for LocalComponentSource {
    async fn pull_verified(
        &self,
        component: &AdmittedComponent,
    ) -> Result<Vec<u8>, ComponentArtifactFetchError> {
        let path = local_component_path(&self.directory, &component.component_digest)?;
        let named = path.display().to_string();
        let bytes = tokio::fs::read(&path).await.map_err(|_| {
            ComponentArtifactFetchError::unavailable(&named, "component-artifact-body-unavailable")
        })?;
        verify_component_body(
            &bytes,
            i64::try_from(bytes.len()).expect("an admitted component body fits a signed length"),
            &component.component_digest,
            &named,
        )?;
        Ok(bytes)
    }
}

/// Refuse a component body its descriptor size or admitted digest contradicts.
pub fn verify_component_body(
    bytes: &[u8],
    descriptor_size: i64,
    expected_digest: &str,
    reference: &str,
) -> Result<(), ComponentArtifactFetchError> {
    if i64::try_from(bytes.len()).unwrap_or(i64::MAX) != descriptor_size {
        return Err(ComponentArtifactFetchError::mismatched(
            reference,
            "component-artifact-body-size-mismatch",
        ));
    }
    if component_digest(bytes) != expected_digest {
        return Err(ComponentArtifactFetchError::mismatched(
            reference,
            "component-artifact-body-digest-mismatch",
        ));
    }
    Ok(())
}

/// Refuse a config body that differs from the admitted config bytes.
pub fn verify_config_body(
    bytes: &[u8],
    descriptor_size: i64,
    expected_digest: &str,
    expected_bytes: &[u8],
    reference: &str,
) -> Result<(), ComponentArtifactFetchError> {
    if i64::try_from(bytes.len()).unwrap_or(i64::MAX) != descriptor_size {
        return Err(ComponentArtifactFetchError::mismatched(
            reference,
            "component-artifact-config-body-size-mismatch",
        ));
    }
    if component_digest(bytes) != expected_digest || bytes != expected_bytes {
        return Err(ComponentArtifactFetchError::mismatched(
            reference,
            "component-artifact-config-body-mismatch",
        ));
    }
    Ok(())
}

/// Exact local filename for an admitted digest, without path interpretation.
pub fn local_component_path(
    directory: &Path,
    digest: &str,
) -> Result<PathBuf, ComponentArtifactFetchError> {
    let tag = component_digest_tag(digest)
        .map_err(|_| ComponentArtifactFetchError::invalid_reference())?;
    Ok(directory.join(format!("{tag}.wasm")))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;
    use wamn_catalog::{
        ComponentDeclaration, ComponentOperationDeclaration, ComponentPackageScope,
        ComponentPortDeclaration, normalize_component_fact,
    };

    use super::*;
    use crate::component_artifact::component_artifact_config_bytes;

    fn admitted(bytes: &[u8]) -> AdmittedComponent {
        normalize_component_fact(
            ComponentDeclaration {
                scope: ComponentPackageScope {
                    tenant_id: "tenant-a".to_owned(),
                    package_id: "orders".to_owned(),
                    package_version: "1.0.0".to_owned(),
                },
                component: "transform".to_owned(),
                interface_version: "0.1.0".to_owned(),
                operations: BTreeMap::from([(
                    "run".to_owned(),
                    ComponentOperationDeclaration {
                        pre_commit: None,
                        committed_result_schema: None,
                        fresh_only: false,
                        registered_operation: None,
                        dependencies: Vec::new(),
                        input_ports: vec![ComponentPortDeclaration {
                            name: "input".to_owned(),
                            schema: json!({}),
                        }],
                        output_ports: Vec::new(),
                        parameters: Vec::new(),
                    },
                )]),
                connections: Vec::new(),
            },
            component_digest(bytes),
            ["wasi:logging/logging@0.1.0".to_owned()],
            Vec::new(),
        )
        .expect("fixture admits")
        .component
    }

    #[tokio::test]
    async fn local_source_accepts_exact_bytes_and_refuses_changed_missing_or_invalid_artifacts() {
        let root = std::env::temp_dir().join(format!("wamn-local-artifact-{}", std::process::id()));
        std::fs::create_dir(&root).expect("create owned local artifact fixture");
        let bytes = b"admitted-local-component";
        let component = admitted(bytes);
        let path = local_component_path(&root, &component.component_digest).unwrap();
        std::fs::write(&path, bytes).unwrap();
        let source = LocalComponentSource::new(root.clone());
        assert_eq!(source.pull_verified(&component).await.unwrap(), bytes);
        std::fs::write(&path, b"changed-local-component").unwrap();
        assert_eq!(
            source.pull_verified(&component).await.unwrap_err().kind(),
            ComponentArtifactFetchErrorKind::Mismatched
        );
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            source.pull_verified(&component).await.unwrap_err().kind(),
            ComponentArtifactFetchErrorKind::Unavailable
        );
        let mut invalid = component;
        invalid.component_digest = "sha256:../../outside".to_owned();
        assert_eq!(
            source.pull_verified(&invalid).await.unwrap_err().kind(),
            ComponentArtifactFetchErrorKind::InvalidReference
        );
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn bodies_are_independently_verified_before_component_bytes_return() {
        let bytes = b"component-bytes";
        let component = admitted(bytes);
        verify_component_body(
            bytes,
            i64::try_from(bytes.len()).unwrap(),
            &component.component_digest,
            "reference",
        )
        .expect("exact body verifies");

        let wrong_size = verify_component_body(
            bytes,
            i64::try_from(bytes.len() + 1).unwrap(),
            &component.component_digest,
            "reference",
        )
        .expect_err("descriptor size drift refuses");
        assert_eq!(
            wrong_size.refusal(),
            "component-artifact-body-size-mismatch"
        );

        let wrong_digest =
            verify_component_body(b"other", 5, &component.component_digest, "reference")
                .expect_err("body digest drift refuses");
        assert_eq!(
            wrong_digest.refusal(),
            "component-artifact-body-digest-mismatch"
        );

        let config = component_artifact_config_bytes(&component);
        verify_config_body(
            &config,
            i64::try_from(config.len()).unwrap(),
            &component_digest(&config),
            &config,
            "reference",
        )
        .expect("exact config verifies");
        let wrong_config =
            verify_config_body(b"{}", 2, &component_digest(b"{}"), &config, "reference")
                .expect_err("different config facts refuse");
        assert_eq!(
            wrong_config.refusal(),
            "component-artifact-config-body-mismatch"
        );
    }
}
