//! 5.5e — build the `wamn.node.manifest` from the node crate's metadata.
//!
//! The manifest is the canonical [`wamn_node_manifest::NodeManifest`] model
//! (design-note 8: registry-scannable node metadata; capability GRANTS are NOT
//! here — they are derived from the WIT imports, never declared twice). It is
//! assembled from the node crate's `[package.metadata.wamn-node]` table + the
//! package name/version, then VALIDATED (`is_valid`) before it can be pushed.

use anyhow::{Context as _, bail};
use serde::Deserialize;
use serde_json::Value;
use wamn_node_manifest::{NodeManifest, OrderingPolicy, SCHEMA_VERSION};

/// The `[package.metadata.wamn-node]` table a node crate may carry (all fields
/// optional — sensible defaults come from the package). kebab-case to mirror the
/// manifest schema.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", default)]
struct NodeMeta {
    node_type: Option<String>,
    name: Option<String>,
    description: Option<String>,
    contract: Option<String>,
    ordering: Option<Vec<OrderingPolicy>>,
    output_ports: Option<Vec<String>>,
    config_schema: Option<Value>,
    input_schema: Option<Value>,
    output_schema: Option<Value>,
}

// The slices of cargo metadata this stage needs: the root package's version +
// its `metadata` blob (where `[package.metadata.wamn-node]` lands).
#[derive(Deserialize)]
struct Metadata {
    packages: Vec<MetaPkg>,
}
#[derive(Deserialize)]
struct MetaPkg {
    name: String,
    version: String,
    #[serde(default)]
    metadata: Value,
}

/// The contract version defaulted when `[package.metadata.wamn-node]` omits it —
/// the frozen `wamn:node` 0.1 contract (`docs/wamn-node.wit`).
pub const DEFAULT_CONTRACT: &str = "0.1.0";

/// Build a validated [`NodeManifest`] for `package` from a `cargo metadata` JSON
/// document. node-type / name default to the package name; version to the crate
/// version; contract to [`DEFAULT_CONTRACT`]; ordering / ports to the manifest
/// defaults. Refuses if the assembled manifest does not validate.
pub fn manifest_from_metadata(metadata_json: &str, package: &str) -> anyhow::Result<NodeManifest> {
    let meta: Metadata =
        serde_json::from_str(metadata_json).context("parse cargo metadata JSON")?;
    let pkg = meta
        .packages
        .iter()
        .find(|p| p.name == package)
        .with_context(|| format!("package {package:?} not found in cargo metadata"))?;

    let node_meta: NodeMeta = match pkg.metadata.get("wamn-node") {
        Some(v) => {
            serde_json::from_value(v.clone()).context("parse [package.metadata.wamn-node]")?
        }
        None => NodeMeta::default(),
    };

    let manifest = NodeManifest {
        schema_version: SCHEMA_VERSION.to_string(),
        node_type: node_meta.node_type.unwrap_or_else(|| package.to_string()),
        name: node_meta.name.unwrap_or_else(|| package.to_string()),
        description: node_meta.description,
        version: pkg.version.clone(),
        contract: node_meta
            .contract
            .unwrap_or_else(|| DEFAULT_CONTRACT.to_string()),
        config_schema: node_meta.config_schema,
        input_schema: node_meta.input_schema,
        output_schema: node_meta.output_schema,
        ordering: node_meta.ordering.unwrap_or_else(|| {
            vec![
                OrderingPolicy::Strict,
                OrderingPolicy::Partitioned,
                OrderingPolicy::Unordered,
            ]
        }),
        output_ports: node_meta
            .output_ports
            .unwrap_or_else(|| vec!["main".to_string()]),
    };

    if let Err(issues) = manifest.validate() {
        bail!("assembled node manifest does not validate: {issues:?}");
    }
    Ok(manifest)
}

/// Build a minimal validated [`NodeManifest`] from explicit fields (the jco path,
/// which has no cargo metadata): node-type + name + version + contract.
pub fn minimal_manifest(
    node_type: &str,
    name: &str,
    version: &str,
    contract: &str,
) -> anyhow::Result<NodeManifest> {
    let manifest = NodeManifest {
        schema_version: SCHEMA_VERSION.to_string(),
        node_type: node_type.to_string(),
        name: name.to_string(),
        description: None,
        version: version.to_string(),
        contract: contract.to_string(),
        config_schema: None,
        input_schema: None,
        output_schema: None,
        ordering: vec![
            OrderingPolicy::Strict,
            OrderingPolicy::Partitioned,
            OrderingPolicy::Unordered,
        ],
        output_ports: vec!["main".to_string()],
    };
    if let Err(issues) = manifest.validate() {
        bail!("assembled node manifest does not validate: {issues:?}");
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata_with_wamn_node(meta: &str) -> String {
        format!(r#"{{"packages":[{{"name":"sample-node","version":"0.1.0","metadata":{meta}}}]}}"#)
    }

    #[test]
    fn manifest_from_metadata_reads_wamn_node_table() {
        let json = metadata_with_wamn_node(
            r#"{"wamn-node":{"node-type":"sample-echo","name":"Sample Echo","ordering":["unordered"]}}"#,
        );
        let m = manifest_from_metadata(&json, "sample-node").unwrap();
        assert_eq!(m.schema_version, "0.1");
        assert_eq!(m.node_type, "sample-echo");
        assert_eq!(m.name, "Sample Echo");
        assert_eq!(m.version, "0.1.0");
        assert_eq!(m.contract, "0.1.0");
        assert_eq!(m.ordering, vec![OrderingPolicy::Unordered]);
        assert!(m.is_valid());
    }

    #[test]
    fn manifest_from_metadata_defaults_from_the_package() {
        // No [package.metadata.wamn-node] at all: node-type/name default to the
        // package name, contract to the frozen 0.1.0.
        let json = r#"{"packages":[{"name":"my-node","version":"2.3.4","metadata":null}]}"#;
        let m = manifest_from_metadata(json, "my-node").unwrap();
        assert_eq!(m.node_type, "my-node");
        assert_eq!(m.name, "my-node");
        assert_eq!(m.version, "2.3.4");
        assert_eq!(m.contract, DEFAULT_CONTRACT);
        assert!(m.is_valid());
    }

    #[test]
    fn manifest_from_metadata_refuses_an_invalid_node_type() {
        // An uppercase node-type is not a slug -> validation refuses it.
        let json = metadata_with_wamn_node(r#"{"wamn-node":{"node-type":"Not A Slug"}}"#);
        assert!(manifest_from_metadata(&json, "sample-node").is_err());
    }

    #[test]
    fn minimal_manifest_is_valid() {
        let m = minimal_manifest("node-ts", "Node TS", "0.1.0", "0.1.0").unwrap();
        assert!(m.is_valid());
        assert_eq!(m.node_type, "node-ts");
    }
}
