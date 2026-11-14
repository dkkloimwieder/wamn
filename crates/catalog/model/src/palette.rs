//! Platform palette descriptors shared by publication and development.
use crate::{ComponentDescriptor, ComponentPackageCoordinate};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The platform owns these components independently of application packages.
pub const PALETTE_PACKAGE_ID: &str = "wamn_palette";
pub const PALETTE_PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const PALETTE_FILE: &str = "palette.json";
pub const PALETTE_DESCRIPTOR_ANNOTATION: &str = "wamn.palette.descriptor";

/// A platform artifact pin and the declaration admitted with its exact bytes.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PalettePin {
    pub artifact: String,
    pub descriptor: ComponentDescriptor,
}

pub type Palette = BTreeMap<String, PalettePin>;

/// Supply only the deployment coordinate when reading a sealed descriptor.
pub fn palette_target(project_env_key: &str) -> ComponentPackageCoordinate {
    ComponentPackageCoordinate {
        project_env_key: project_env_key.to_owned(),
        package_id: PALETTE_PACKAGE_ID.to_owned(),
        package_version: PALETTE_PACKAGE_VERSION.to_owned(),
    }
}

/// Read the current platform document. Refuse unsealed declarations and mutable pins.
pub fn read_palette(bytes: &[u8]) -> anyhow::Result<Palette> {
    let palette: Palette = serde_json::from_slice(bytes)?;
    palette
        .into_iter()
        .map(|(name, pin)| {
            anyhow::ensure!(
                name == pin.descriptor.component,
                "palette component name differs from its descriptor"
            );
            let (_, digest) = pin
                .artifact
                .rsplit_once('@')
                .ok_or_else(|| anyhow::anyhow!("palette artifact is not pinned"))?;
            crate::ArtifactHash::parse(digest.to_owned())?;
            crate::ArtifactHash::parse(pin.descriptor.component_digest.clone())?;
            anyhow::ensure!(
                pin.descriptor.operations.values().all(|operation| operation
                    .registered_operation
                    .is_none()
                    && !operation.host_transaction
                    && operation.statements.is_empty()),
                "palette components cannot own application operations or SQL"
            );
            for connection in &pin.descriptor.connections {
                connection.store_alias.validate()?;
            }
            let facts = pin
                .descriptor
                .into_admitted(palette_target("platform-palette"));
            crate::verify_stored_effect_projection(&facts.component)?;
            let descriptor = ComponentDescriptor::new(facts.component, facts.connections)
                .with_statement_instances(facts.statement_instances);
            Ok((
                name,
                PalettePin {
                    artifact: pin.artifact,
                    descriptor,
                },
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ComponentDeclaration, ComponentOperationDeclaration, ComponentPortDeclaration,
        normalize_component_fact,
    };
    use serde_json::json;

    #[test]
    fn sealed_palette_preserves_ports_and_refuses_mutable_or_unsealed_sources() {
        let facts = normalize_component_fact(
            ComponentDeclaration {
                target: palette_target("platform-palette"),
                component: "transform".into(),
                interface_version: "0.1.0".into(),
                operations: BTreeMap::from([(
                    "wamn:node/handler@0.1.0".into(),
                    ComponentOperationDeclaration {
                        registered_operation: None,
                        committed_result_schema: None,
                        input_ports: vec![ComponentPortDeclaration {
                            name: "input".into(),
                            schema: json!({}),
                        }],
                        output_ports: vec![ComponentPortDeclaration {
                            name: "main".into(),
                            schema: json!({}),
                        }],
                        parameters: vec![],
                    },
                )]),
                connections: vec![],
            },
            format!("sha256:{}", "a".repeat(64)),
            Vec::<String>::new(),
            vec![],
        )
        .unwrap();
        let descriptor = ComponentDescriptor::new(facts.component, facts.connections);
        let mut document = json!({"transform": {"artifact": format!("localhost/palette/transform@sha256:{}", "b".repeat(64)), "descriptor": descriptor}});
        let palette = read_palette(&serde_json::to_vec(&document).unwrap()).unwrap();
        let operation = &palette["transform"].descriptor.operations["wamn:node/handler@0.1.0"];
        assert!(operation.registered_operation.is_none());
        assert!(!operation.input_ports.is_empty());
        assert!(!operation.output_ports.is_empty());
        document["transform"]["artifact"] = json!("localhost/palette/transform:latest");
        assert!(read_palette(&serde_json::to_vec(&document).unwrap()).is_err());
        assert!(
            read_palette(&serde_json::to_vec(&json!({"transform": {"declaration": {}}})).unwrap())
                .is_err()
        );
    }
}
