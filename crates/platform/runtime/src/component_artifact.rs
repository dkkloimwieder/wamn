//! Shared wire contract for digest-addressed component artifacts.

use std::fmt;

use serde::{Deserialize, Serialize};
use wamn_catalog::AdmittedComponent;

const HASH_PREFIX: &str = "sha256:";
const HASH_HEX_LEN: usize = 64;

/// Media type of the single layer carrying exact component bytes.
pub const COMPONENT_LAYER_MEDIA_TYPE: &str = "application/vnd.wamn.component.v1+wasm";

/// Media type of the config carrying canonical byte-derived admission facts.
pub const COMPONENT_CONFIG_MEDIA_TYPE: &str = "application/vnd.wamn.component.config.v1+json";

/// Format version of [`ComponentArtifactConfig`].
pub const COMPONENT_CONFIG_FORMAT_VERSION: &str = "0.1";

/// Exact publisher/puller layout for one component artifact.
///
/// The layout owns no OCI-client types so callers cannot accidentally fork the
/// wire contract through an upstream constructor. The publisher translates
/// these facts into OCI descriptors; the puller verifies those same facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentArtifactLayout<'a> {
    component_bytes: &'a [u8],
    config_bytes: &'a [u8],
}

impl<'a> ComponentArtifactLayout<'a> {
    /// Exact component bytes carried by the artifact's sole layer.
    pub const fn component_bytes(&self) -> &'a [u8] {
        self.component_bytes
    }

    /// Exact canonical admission-fact bytes carried by the config descriptor.
    pub const fn config_bytes(&self) -> &'a [u8] {
        self.config_bytes
    }

    /// OCI image-manifest schema version accepted by both transfer directions.
    pub const fn manifest_schema_version(&self) -> u8 {
        2
    }

    /// Exact number of component layers accepted by both transfer directions.
    pub const fn layer_count(&self) -> usize {
        1
    }

    /// Media type of the sole component layer.
    pub const fn layer_media_type(&self) -> &'static str {
        COMPONENT_LAYER_MEDIA_TYPE
    }

    /// Media type of the canonical admission-fact config.
    pub const fn config_media_type(&self) -> &'static str {
        COMPONENT_CONFIG_MEDIA_TYPE
    }
}

/// Bind exact component and config bytes to the shared OCI layout contract.
pub const fn component_artifact_layout<'a>(
    component_bytes: &'a [u8],
    config_bytes: &'a [u8],
) -> ComponentArtifactLayout<'a> {
    ComponentArtifactLayout {
        component_bytes,
        config_bytes,
    }
}

/// Byte-derived admission facts carried beside the component layer.
///
/// Catalog scope, names, operations, and typed declarations deliberately stay
/// out of this document: one component digest may be admitted by several
/// catalogs, while one digest-derived OCI tag can carry only one config. The
/// import inventory and fingerprint are derived from the exact component bytes
/// and therefore remain stable for that tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ComponentArtifactConfig {
    /// Exact config layout version.
    pub format_version: String,
    /// SHA-256 identity of the component layer bytes.
    pub component_digest: String,
    /// Sorted, deduplicated imports extracted from the component bytes.
    pub imports: Vec<String>,
    /// RFC 8785 SHA-256 identity of `imports`.
    pub imports_fingerprint: String,
}

/// Stable classification for an invalid component artifact reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentArtifactReferenceErrorKind {
    InvalidBase,
    InvalidDigest,
}

/// Refusal to derive an immutable component artifact reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentArtifactReferenceError {
    kind: ComponentArtifactReferenceErrorKind,
    reason: &'static str,
}

impl ComponentArtifactReferenceError {
    /// Stable refusal class for callers that must not match display text.
    pub fn kind(&self) -> ComponentArtifactReferenceErrorKind {
        self.kind
    }

    fn new(kind: ComponentArtifactReferenceErrorKind, reason: &'static str) -> Self {
        Self { kind, reason }
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
pub(crate) struct ComponentArtifactBase {
    registry: Box<str>,
    repository: Box<str>,
}

impl ComponentArtifactBase {
    pub(crate) fn registry(&self) -> &str {
        &self.registry
    }

    pub(crate) fn repository(&self) -> &str {
        &self.repository
    }

    pub(crate) fn reference(
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

pub(crate) fn parse_component_artifact_base(
    artifact_base: &str,
) -> Result<ComponentArtifactBase, ComponentArtifactReferenceError> {
    let invalid_base = |reason| {
        ComponentArtifactReferenceError::new(
            ComponentArtifactReferenceErrorKind::InvalidBase,
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

fn component_digest_tag(component_digest: &str) -> Result<&str, ComponentArtifactReferenceError> {
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
                ComponentArtifactReferenceErrorKind::InvalidDigest,
                "expected sha256:<64 lowercase hex digits>",
            )
        })
}

/// Canonical OCI config bytes for an admitted component.
///
/// The config is a byte-derived supply-chain fact, not a second declaration.
/// The pull side compares its digest and import-policy facts with the catalog
/// row before exposing component bytes.
pub fn component_artifact_config_bytes(component: &AdmittedComponent) -> Vec<u8> {
    let config = ComponentArtifactConfig {
        format_version: COMPONENT_CONFIG_FORMAT_VERSION.to_owned(),
        component_digest: component.component_digest.clone(),
        imports: component.imports.clone(),
        imports_fingerprint: component.imports_fingerprint.clone(),
    };
    let value = serde_json::to_value(config).expect("a component artifact config serializes");
    wamn_execution_contract::canonical_json_bytes(&value)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wamn_catalog::{
        ComponentCatalogScope, ComponentDeclaration, ComponentPortDeclaration,
        normalize_component_fact,
    };

    use super::*;

    fn admitted() -> AdmittedComponent {
        normalize_component_fact(
            ComponentDeclaration {
                scope: ComponentCatalogScope {
                    tenant_id: "tenant-a".to_owned(),
                    catalog_id: "orders".to_owned(),
                    catalog_version: 3,
                },
                component: "transform".to_owned(),
                interface_version: "0.1.0".to_owned(),
                operation: "map".to_owned(),
                input_ports: vec![ComponentPortDeclaration {
                    name: "input".to_owned(),
                    schema: json!({"type": "object"}),
                }],
                output_ports: Vec::new(),
                parameters: Vec::new(),
            },
            format!("sha256:{}", "a".repeat(64)),
            ["wasi:io/streams@0.2.3".to_owned()],
        )
        .expect("fixture admits")
    }

    #[test]
    fn component_artifact_wire_literals_are_pinned() {
        let layout = component_artifact_layout(b"component", b"config");
        assert_eq!(
            COMPONENT_LAYER_MEDIA_TYPE,
            "application/vnd.wamn.component.v1+wasm"
        );
        assert_eq!(
            COMPONENT_CONFIG_MEDIA_TYPE,
            "application/vnd.wamn.component.config.v1+json"
        );
        assert_eq!(layout.manifest_schema_version(), 2);
        assert_eq!(layout.layer_count(), 1);
        assert_eq!(layout.component_bytes(), b"component");
        assert_eq!(layout.config_bytes(), b"config");
        assert_eq!(layout.layer_media_type(), COMPONENT_LAYER_MEDIA_TYPE);
        assert_eq!(layout.config_media_type(), COMPONENT_CONFIG_MEDIA_TYPE);
    }

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
                    .kind(),
                ComponentArtifactReferenceErrorKind::InvalidBase,
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

    #[test]
    fn canonical_config_carries_only_digest_stable_admission_facts() {
        let admitted = admitted();
        let bytes = component_artifact_config_bytes(&admitted);
        let decoded: ComponentArtifactConfig =
            serde_json::from_slice(&bytes).expect("canonical config decodes");

        assert_eq!(decoded.format_version, COMPONENT_CONFIG_FORMAT_VERSION);
        assert_eq!(decoded.component_digest, admitted.component_digest);
        assert_eq!(decoded.imports, admitted.imports);
        assert_eq!(decoded.imports_fingerprint, admitted.imports_fingerprint);
        let text = String::from_utf8(bytes).expect("config is UTF-8");
        assert!(!text.contains("tenant-a"));
        assert!(!text.contains("transform"));
    }
}
