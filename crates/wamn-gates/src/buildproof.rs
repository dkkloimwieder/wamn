//! buildproof — verify a 5.5-built node artifact FROM THE REGISTRY.
//!
//! The out-of-cluster gate for the builder pipeline (5.5e): fetch the pushed
//! manifest over plain HTTP (the wamn-builder registry client), then assert the
//! properties a runnable, trustworthy node artifact must carry —
//! - the `wamn.node.manifest` annotation parses via [`NodeManifest::from_json`]
//!   and validates (`is_valid`);
//! - `layers[0]` is the pullable `application/wasm` layer the wash-runtime host
//!   expects, and its bytes hash to the descriptor digest (integrity).
//!
//! The 5.5d signature + SBOM checks (verify the detached ed25519 signature
//! against the public key; assert the SBOM lists the expected package set) are
//! added by the wamn-0si.4 commit, which extends this gate.

use anyhow::{Context as _, bail};
use clap::Args;

use wamn_builder::registry::{
    self, ImageManifest, RegistryRef, WASM_LAYER_MEDIA_TYPE, sha256_digest,
};
use wamn_node_manifest::{ANNOTATION_KEY, NodeManifest};

#[derive(Args)]
pub struct BuildproofArgs {
    /// The registry `host:port` to fetch from (e.g.
    /// `registry.wamn-system.svc.cluster.local:5000`).
    #[arg(long)]
    pub registry: String,

    /// The repository path (e.g. `wamn/sample-node`).
    #[arg(long)]
    pub repository: String,

    /// The tag or `sha256:…` digest reference to verify. Default `dev`.
    #[arg(long, default_value = "dev")]
    pub reference: String,
}

/// Verify the pushed manifest's node-facing invariants (5.5e), independent of
/// the registry: the `wamn.node.manifest` annotation parses + validates, and
/// `layers[0]` is the pullable `application/wasm` layer. Returns the parsed
/// [`NodeManifest`] or the list of failures. Unit-testable over a synthetic
/// manifest.
pub fn verify_manifest(manifest: &ImageManifest) -> Result<NodeManifest, Vec<String>> {
    let mut failures = Vec::new();

    let node_manifest = match manifest.annotations.get(ANNOTATION_KEY) {
        Some(json) => match NodeManifest::from_json(json) {
            Ok(m) if m.is_valid() => Some(m),
            Ok(m) => {
                failures.push(format!(
                    "{ANNOTATION_KEY:?} annotation does not validate: {:?}",
                    m.issues()
                ));
                None
            }
            Err(e) => {
                failures.push(format!("{ANNOTATION_KEY:?} annotation does not parse: {e}"));
                None
            }
        },
        None => {
            failures.push(format!(
                "manifest is missing the {ANNOTATION_KEY:?} annotation"
            ));
            None
        }
    };

    match manifest.wasm_layer() {
        Some(layer) if layer.media_type == WASM_LAYER_MEDIA_TYPE => {}
        Some(layer) => failures.push(format!(
            "layers[0] media type {:?} != {WASM_LAYER_MEDIA_TYPE:?} — the wash-runtime host \
             cannot pull it",
            layer.media_type
        )),
        None => failures.push("manifest has no layers".to_string()),
    }

    match node_manifest {
        Some(m) if failures.is_empty() => Ok(m),
        _ => Err(failures),
    }
}

pub async fn run(args: BuildproofArgs) -> anyhow::Result<()> {
    let target = RegistryRef {
        registry: args.registry.clone(),
        repository: args.repository.clone(),
        reference: args.reference.clone(),
        insecure: true,
    };

    println!("# wamn-gates buildproof — verify the 5.5-built node artifact FROM THE REGISTRY");
    println!("# image: {}", target.image());

    let manifest_bytes = registry::fetch_manifest(&target)
        .await
        .context("fetch manifest from the registry")?;
    let manifest: ImageManifest =
        serde_json::from_slice(&manifest_bytes).context("parse fetched OCI manifest")?;

    let mut pass = true;

    println!("\n## wamn.node.manifest annotation + layer media type");
    match verify_manifest(&manifest) {
        Ok(node) => println!(
            "    PASS: wamn.node.manifest valid (node-type {:?}, contract {}); layers[0] = {}",
            node.node_type, node.contract, WASM_LAYER_MEDIA_TYPE
        ),
        Err(failures) => {
            for f in &failures {
                println!("    FAIL: {f}");
            }
            pass = false;
        }
    }

    println!("\n## layer digest integrity (the exact bytes the host pulls)");
    if let Some(layer) = manifest.wasm_layer() {
        let bytes = registry::fetch_blob(&target, &layer.digest)
            .await
            .context("fetch the wasm layer blob")?;
        let actual = sha256_digest(&bytes);
        if actual == layer.digest {
            println!(
                "    PASS: layer digest {actual} matches ({} bytes)",
                bytes.len()
            );
        } else {
            println!(
                "    FAIL: layer digest mismatch — descriptor {} vs actual {actual}",
                layer.digest
            );
            pass = false;
        }
    }

    println!("\nbuildproof complete — overall PASS: {pass}");
    if !pass {
        bail!("buildproof failed: the pushed artifact does not carry the required node properties");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn manifest_with_annotation(node_manifest_json: Option<&str>) -> ImageManifest {
        let (m, _config) = registry::build_manifest(b"\x00asm\x0d\x00\x01\x00node", {
            let mut a = BTreeMap::new();
            if let Some(j) = node_manifest_json {
                a.insert(ANNOTATION_KEY.to_string(), j.to_string());
            }
            a
        });
        m
    }

    fn valid_node_manifest_json() -> String {
        NodeManifest {
            schema_version: "0.1".to_string(),
            node_type: "sample-echo".to_string(),
            name: "Sample Echo".to_string(),
            description: None,
            version: "0.1.0".to_string(),
            contract: "0.1.0".to_string(),
            config_schema: None,
            input_schema: None,
            output_schema: None,
            ordering: vec![wamn_node_manifest::OrderingPolicy::Unordered],
            output_ports: vec!["main".to_string()],
        }
        .to_json()
    }

    #[test]
    fn verify_manifest_accepts_a_valid_pushed_artifact() {
        let m = manifest_with_annotation(Some(&valid_node_manifest_json()));
        let node = verify_manifest(&m).expect("valid");
        assert_eq!(node.node_type, "sample-echo");
    }

    #[test]
    fn verify_manifest_rejects_a_missing_annotation() {
        let m = manifest_with_annotation(None);
        let failures = verify_manifest(&m).expect_err("must fail");
        assert!(failures.iter().any(|f| f.contains(ANNOTATION_KEY)));
    }

    #[test]
    fn verify_manifest_rejects_an_invalid_node_manifest() {
        // Uppercase node-type is not a slug -> is_valid() is false.
        let bad = r#"{"schema-version":"0.1","node-type":"Bad","name":"x","version":"0.1.0","contract":"0.1.0"}"#;
        let m = manifest_with_annotation(Some(bad));
        let failures = verify_manifest(&m).expect_err("must fail");
        assert!(failures.iter().any(|f| f.contains("does not validate")));
    }

    #[test]
    fn verify_manifest_rejects_a_wrong_layer_media_type() {
        let mut m = manifest_with_annotation(Some(&valid_node_manifest_json()));
        m.layers[0].media_type = "application/octet-stream".to_string();
        let failures = verify_manifest(&m).expect_err("must fail");
        assert!(failures.iter().any(|f| f.contains("cannot pull")));
    }
}
