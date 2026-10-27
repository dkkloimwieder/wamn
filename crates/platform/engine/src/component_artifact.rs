//! Shared wire contract for digest-addressed component artifacts.

use serde::{Deserialize, Serialize};
use wamn_catalog::AdmittedComponent;

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
/// import list and fingerprint are derived from the exact component bytes
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
    use std::collections::BTreeMap;

    use serde_json::json;
    use wamn_catalog::{
        ComponentDeclaration, ComponentOperationDeclaration, ComponentPackageScope,
        ComponentPortDeclaration, normalize_component_fact,
    };

    use super::*;

    fn admitted() -> AdmittedComponent {
        normalize_component_fact(
            ComponentDeclaration {
                scope: ComponentPackageScope {
                    env_key: "tenant-a".to_owned(),
                    package_id: "orders".to_owned(),
                    package_version: "1.0.0".to_owned(),
                },
                component: "transform".to_owned(),
                interface_version: "0.1.0".to_owned(),
                operations: BTreeMap::from([(
                    "map".to_owned(),
                    ComponentOperationDeclaration {
                        committed_result_schema: None,
                        registered_operation: None,
                        input_ports: vec![ComponentPortDeclaration {
                            name: "input".to_owned(),
                            schema: json!({"type": "object"}),
                        }],
                        output_ports: Vec::new(),
                        parameters: Vec::new(),
                    },
                )]),
                connections: Vec::new(),
            },
            format!("sha256:{}", "a".repeat(64)),
            ["wasi:io/streams@0.2.3".to_owned()],
            Vec::new(),
        )
        .expect("fixture admits")
        .component
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
