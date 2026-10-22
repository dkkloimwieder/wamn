//! The OCI reference of a digest-addressed artifact: an explicit
//! `<registry>/<repository>` base and the digest-derived tag. Component and
//! package artifacts, and the readers that pull them, derive their references
//! here, so one rule refuses an ambient or mutable reference.

use std::fmt;

use crate::{HASH_HEX_LEN, HASH_PREFIX};

/// Stable classification for an invalid component artifact reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentArtifactReferenceErrorType {
    InvalidBase,
    InvalidDigest,
}

/// Refusal to derive an immutable component artifact reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentArtifactReferenceError {
    type_: ComponentArtifactReferenceErrorType,
    reason: &'static str,
}

impl ComponentArtifactReferenceError {
    /// Stable refusal class for callers that must not match display text.
    pub fn error_type(&self) -> ComponentArtifactReferenceErrorType {
        self.type_
    }

    fn new(kind: ComponentArtifactReferenceErrorType, reason: &'static str) -> Self {
        Self {
            type_: kind,
            reason,
        }
    }
}

impl fmt::Display for ComponentArtifactReferenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "component artifact reference is invalid: {}",
            self.reason
        )
    }
}

impl std::error::Error for ComponentArtifactReferenceError {}

/// Registry, repository, and immutable digest-derived tag of one component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentArtifactReference {
    registry: Box<str>,
    repository: Box<str>,
    tag: Box<str>,
}

/// Parsed artifact base shared internally by the publisher contract and puller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentArtifactBase {
    registry: Box<str>,
    repository: Box<str>,
}

impl ComponentArtifactBase {
    pub fn registry(&self) -> &str {
        &self.registry
    }

    pub fn repository(&self) -> &str {
        &self.repository
    }

    pub fn reference(
        &self,
        component_digest: &str,
    ) -> Result<ComponentArtifactReference, ComponentArtifactReferenceError> {
        let tag = component_digest_tag(component_digest)?;
        Ok(ComponentArtifactReference {
            registry: self.registry.clone(),
            repository: self.repository.clone(),
            tag: tag.into(),
        })
    }
}

impl ComponentArtifactReference {
    /// Registry host, including an explicit port when configured.
    pub fn registry(&self) -> &str {
        &self.registry
    }

    /// Repository path within the registry.
    pub fn repository(&self) -> &str {
        &self.repository
    }

    /// The component digest's 64 lowercase hexadecimal digits.
    pub fn tag(&self) -> &str {
        &self.tag
    }
}

impl fmt::Display for ComponentArtifactReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}/{}:{}",
            self.registry, self.repository, self.tag
        )
    }
}

/// Derive the one OCI reference used by both component publication and pull.
///
/// `artifact_base` is explicitly `<registry>/<repository>`. A bare repository
/// is refused instead of falling back to Docker Hub. The component digest is
/// carried as the tag without its `sha256:` prefix because OCI tags cannot
/// contain a colon; it is not an OCI manifest digest.
pub fn component_artifact_reference(
    artifact_base: &str,
    component_digest: &str,
) -> Result<ComponentArtifactReference, ComponentArtifactReferenceError> {
    parse_component_artifact_base(artifact_base)?.reference(component_digest)
}

pub fn parse_component_artifact_base(
    artifact_base: &str,
) -> Result<ComponentArtifactBase, ComponentArtifactReferenceError> {
    let invalid_base = |reason| {
        ComponentArtifactReferenceError::new(
            ComponentArtifactReferenceErrorType::InvalidBase,
            reason,
        )
    };
    if artifact_base.contains("://") {
        return Err(invalid_base("a URL scheme is not allowed"));
    }
    let (registry, repository) = artifact_base
        .split_once('/')
        .ok_or_else(|| invalid_base("expected <registry>/<repository>"))?;
    if registry.contains('@')
        || registry.starts_with(':')
        || registry.ends_with(':')
        || registry.chars().any(char::is_whitespace)
    {
        return Err(invalid_base("registry host is malformed"));
    }
    if !(registry.contains('.') || registry.contains(':') || registry == "localhost") {
        return Err(invalid_base(
            "registry must be dotted, port-qualified, or localhost",
        ));
    }
    if repository.is_empty() {
        return Err(invalid_base("repository is empty"));
    }
    if repository.starts_with('/')
        || repository.ends_with('/')
        || repository.contains("//")
        || repository.chars().any(char::is_whitespace)
    {
        return Err(invalid_base("repository path is malformed"));
    }
    if repository.contains([':', '@']) {
        return Err(invalid_base(
            "repository must not carry a tag or manifest digest",
        ));
    }

    Ok(ComponentArtifactBase {
        registry: registry.into(),
        repository: repository.into(),
    })
}

pub fn component_digest_tag(
    component_digest: &str,
) -> Result<&str, ComponentArtifactReferenceError> {
    component_digest
        .strip_prefix(HASH_PREFIX)
        .filter(|hex| {
            hex.len() == HASH_HEX_LEN
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
        .ok_or_else(|| {
            ComponentArtifactReferenceError::new(
                ComponentArtifactReferenceErrorType::InvalidDigest,
                "expected sha256:<64 lowercase hex digits>",
            )
        })
}

/// Require the exact OCI manifest reference selected for a workload start.
///
/// # Errors
///
/// Refuses a missing registry, a tagged repository, or an invalid SHA-256 digest.
pub fn validate_component_start_reference(
    reference: &str,
) -> Result<(), ComponentArtifactReferenceError> {
    let (base, digest) = reference.split_once('@').ok_or_else(|| {
        ComponentArtifactReferenceError::new(
            ComponentArtifactReferenceErrorType::InvalidBase,
            "expected <registry>/<repository>@sha256:<manifest-digest>",
        )
    })?;
    parse_component_artifact_base(base)?;
    component_digest_tag(digest)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_reference_is_derived_only_from_an_explicit_base_and_digest() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let reference = component_artifact_reference(
            "registry.wamn-system.svc.cluster.local:5000/wamn/components",
            &digest,
        )
        .expect("reference derives");

        assert_eq!(
            reference.registry(),
            "registry.wamn-system.svc.cluster.local:5000"
        );
        assert_eq!(reference.repository(), "wamn/components");
        assert_eq!(reference.tag(), "a".repeat(64));
        assert_eq!(
            reference.to_string(),
            format!(
                "registry.wamn-system.svc.cluster.local:5000/wamn/components:{}",
                "a".repeat(64)
            )
        );
    }

    #[test]
    fn ambient_or_mutable_references_refuse() {
        let digest = format!("sha256:{}", "a".repeat(64));
        for base in [
            "wamn/components",
            "registry/components",
            "registry:5000/",
            "registry:5000/wamn/components:dev",
            "registry:5000/wamn/components@sha256:abc",
            "https://registry.example/wamn/components",
            "user:secret@registry.example/wamn/components",
        ] {
            assert_eq!(
                component_artifact_reference(base, &digest)
                    .expect_err("invalid base refuses")
                    .error_type(),
                ComponentArtifactReferenceErrorType::InvalidBase,
                "accepted {base:?}"
            );
        }

        let leaked = component_artifact_reference(
            "user:super-secret@registry.example/wamn/components",
            &digest,
        )
        .expect_err("credentials in a base refuse");
        assert!(!format!("{leaked:?} {leaked}").contains("super-secret"));
    }
}

#[cfg(test)]
mod start_reference_tests {
    use super::validate_component_start_reference;

    #[test]
    fn start_requires_an_explicit_repository_and_manifest_digest() {
        assert!(
            validate_component_start_reference(&format!(
                "registry.example:5000/application/entry@sha256:{}",
                "a".repeat(64)
            ))
            .is_ok()
        );
        for reference in [
            "registry.example/entry:latest",
            "registry.example/entry:abc",
            "file:/tmp/entry.wasm",
            "sha256:abc",
            "entry@sha256:abc",
            "registry.example/entry@sha256:ABC",
        ] {
            assert!(
                validate_component_start_reference(reference).is_err(),
                "{reference}"
            );
        }
    }
}
