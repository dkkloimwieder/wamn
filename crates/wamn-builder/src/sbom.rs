//! 5.5d — a minimal CycloneDX SBOM (name / version / purl per component)
//! derived from `cargo metadata`, attached as an OCI annotation at push.
//!
//! ATTACHMENT CHOICE: an OCI ANNOTATION (`wamn.node.sbom`), not a layer blob —
//! a node SBOM (its transitive closure, ~50 components ≈ a few KB) is small, and
//! an annotation keeps the manifest a SINGLE `application/wasm` layer (matching
//! the live wash-pushed shape exactly, so pullability is maximally certain) and
//! lets `buildproof` read it without a second blob fetch. A large SBOM → an
//! additional layer blob is a deferral.

use std::collections::BTreeMap;

use anyhow::Context as _;
use serde::Deserialize;
use serde_json::json;

/// The OCI annotation carrying the CycloneDX SBOM JSON.
pub const SBOM_ANNOTATION: &str = "wamn.node.sbom";

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<MetaPkg>,
    resolve: Resolve,
}
#[derive(Deserialize)]
struct MetaPkg {
    id: String,
    name: String,
    version: String,
}
#[derive(Deserialize)]
struct Resolve {
    nodes: Vec<ResolveNode>,
    #[serde(default)]
    root: Option<String>,
}
#[derive(Deserialize)]
struct ResolveNode {
    id: String,
    deps: Vec<ResolveDep>,
}
#[derive(Deserialize)]
struct ResolveDep {
    pkg: String,
}

/// Build a minimal CycloneDX 1.5 SBOM over `package`'s transitive closure
/// (INCLUDING the node itself), one component per crate (`type`/`name`/`version`/
/// `purl pkg:cargo/<name>@<version>`), sorted by name for reproducibility.
pub fn cyclonedx_from_metadata(metadata_json: &str, package: &str) -> anyhow::Result<String> {
    let meta: Metadata =
        serde_json::from_str(metadata_json).context("parse cargo metadata JSON for SBOM")?;
    let root_id = meta
        .resolve
        .root
        .clone()
        .or_else(|| {
            meta.packages
                .iter()
                .find(|p| p.name == package)
                .map(|p| p.id.clone())
        })
        .with_context(|| format!("package {package:?} not found in cargo metadata"))?;

    let nodes: std::collections::HashMap<&str, &ResolveNode> = meta
        .resolve
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n))
        .collect();

    // Reachable closure (including the root).
    let mut seen = std::collections::BTreeSet::new();
    let mut stack = vec![root_id];
    while let Some(cur) = stack.pop() {
        if !seen.insert(cur.clone()) {
            continue;
        }
        if let Some(node) = nodes.get(cur.as_str()) {
            for dep in &node.deps {
                stack.push(dep.pkg.clone());
            }
        }
    }

    let mut components: Vec<(&str, &str)> = meta
        .packages
        .iter()
        .filter(|p| seen.contains(&p.id))
        .map(|p| (p.name.as_str(), p.version.as_str()))
        .collect();
    components.sort();
    components.dedup();

    let components: Vec<_> = components
        .iter()
        .map(|(name, version)| {
            json!({
                "type": "library",
                "name": name,
                "version": version,
                "purl": format!("pkg:cargo/{name}@{version}"),
            })
        })
        .collect();

    let bom = json!({
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "version": 1,
        "components": components,
    });
    Ok(serde_json::to_string(&bom).expect("SBOM serializes"))
}

/// A minimal single-component CycloneDX SBOM for the jco path (no cargo graph):
/// just the node itself. A richer npm SBOM is a deferral.
pub fn cyclonedx_single(node_type: &str, version: &str) -> String {
    let bom = json!({
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "version": 1,
        "components": [{
            "type": "application",
            "name": node_type,
            "version": version,
            "purl": format!("pkg:generic/{node_type}@{version}"),
        }],
    });
    serde_json::to_string(&bom).expect("SBOM serializes")
}

/// The set of component NAMES an SBOM JSON lists — the `buildproof` cross-check
/// that the SBOM covers the expected package set.
pub fn sbom_component_names(sbom_json: &str) -> anyhow::Result<BTreeMap<String, String>> {
    let value: serde_json::Value = serde_json::from_str(sbom_json).context("parse SBOM JSON")?;
    let mut names = BTreeMap::new();
    if let Some(components) = value.get("components").and_then(|c| c.as_array()) {
        for c in components {
            if let (Some(name), Some(version)) = (
                c.get("name").and_then(|v| v.as_str()),
                c.get("version").and_then(|v| v.as_str()),
            ) {
                names.insert(name.to_string(), version.to_string());
            }
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(root_deps: &str) -> String {
        format!(
            r#"{{
              "packages":[
                {{"id":"root 0.1.0 (path)","name":"sample-node","version":"0.1.0"}},
                {{"id":"serde_json 1.0.150 (reg)","name":"serde_json","version":"1.0.150"}},
                {{"id":"serde 1.0.228 (reg)","name":"serde","version":"1.0.228"}}
              ],
              "resolve":{{
                "root":"root 0.1.0 (path)",
                "nodes":[
                  {{"id":"root 0.1.0 (path)","deps":{root_deps}}},
                  {{"id":"serde_json 1.0.150 (reg)","deps":[{{"pkg":"serde 1.0.228 (reg)"}}]}},
                  {{"id":"serde 1.0.228 (reg)","deps":[]}}
                ]
              }}
            }}"#
        )
    }

    #[test]
    fn sbom_lists_the_closure_including_the_node_with_purls() {
        let json = metadata(r#"[{"pkg":"serde_json 1.0.150 (reg)"}]"#);
        let sbom = cyclonedx_from_metadata(&json, "sample-node").unwrap();
        let names = sbom_component_names(&sbom).unwrap();
        assert_eq!(names.get("sample-node").map(String::as_str), Some("0.1.0"));
        assert_eq!(names.get("serde_json").map(String::as_str), Some("1.0.150"));
        assert_eq!(names.get("serde").map(String::as_str), Some("1.0.228"));
        // purl form.
        assert!(sbom.contains("pkg:cargo/serde_json@1.0.150"));
        // CycloneDX envelope.
        assert!(sbom.contains("\"bomFormat\":\"CycloneDX\""));
    }

    #[test]
    fn single_component_sbom_for_jco() {
        let sbom = cyclonedx_single("node-ts", "0.1.0");
        let names = sbom_component_names(&sbom).unwrap();
        assert_eq!(names.len(), 1);
        assert_eq!(names.get("node-ts").map(String::as_str), Some("0.1.0"));
    }
}
