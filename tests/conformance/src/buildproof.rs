//! `buildproof` — independently verify a builder-pushed node artifact.
//!
//! This gate deliberately owns its OCI reader and verification logic. A proof
//! must not import the deployable producer whose output it is checking.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, bail};
use bytes::Bytes;
use clap::Args;
use http_body_util::{BodyExt as _, Full};
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;
use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::net::TcpStream;
use wamn_node_manifest::{ANNOTATION_KEY, NodeManifest};

const OCI_MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
const WASM_LAYER_MEDIA_TYPE: &str = "application/wasm";
const SIGNATURE_ANNOTATION: &str = "wamn.node.signature";
const SBOM_ANNOTATION: &str = "wamn.node.sbom";

#[derive(Args)]
pub struct BuildproofArgs {
    /// The registry `host:port` to fetch from.
    #[arg(long)]
    pub registry: String,
    /// The repository path (for example `wamn/sample-node`).
    #[arg(long)]
    pub repository: String,
    /// The tag or digest reference to verify.
    #[arg(long, default_value = "dev")]
    pub reference: String,
    /// Hex ed25519 public key used to verify `wamn.node.signature`.
    #[arg(long, env = "WAMN_BUILDER_PUBLIC_KEY")]
    pub public_key: Option<String>,
    /// Package names the SBOM must list.
    #[arg(long = "expect-package")]
    pub expect_packages: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Descriptor {
    #[serde(rename = "mediaType")]
    media_type: String,
    digest: String,
    size: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ImageManifest {
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
    #[serde(rename = "mediaType")]
    media_type: String,
    config: Descriptor,
    layers: Vec<Descriptor>,
    #[serde(default)]
    annotations: BTreeMap<String, String>,
}

impl ImageManifest {
    fn wasm_layer(&self) -> Option<&Descriptor> {
        self.layers.first()
    }
}

struct RegistryRef {
    registry: String,
    repository: String,
    reference: String,
}

impl RegistryRef {
    fn host_port(&self) -> anyhow::Result<(&str, u16)> {
        let (host, port) = self
            .registry
            .rsplit_once(':')
            .context("registry must be host:port")?;
        Ok((host, port.parse().context("registry port")?))
    }

    fn image(&self) -> String {
        format!("{}/{}:{}", self.registry, self.repository, self.reference)
    }
}

fn sha256_digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

async fn fetch(target: &RegistryRef, path: &str, accept: Option<&str>) -> anyhow::Result<Vec<u8>> {
    let (host, port) = target.host_port()?;
    let stream = TcpStream::connect((host, port))
        .await
        .with_context(|| format!("connect {host}:{port}"))?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .context("HTTP/1 handshake")?;
    tokio::spawn(async move {
        let _ = connection.await;
    });

    let mut request = Request::builder()
        .method("GET")
        .uri(path)
        .header("Host", format!("{host}:{port}"));
    if let Some(media_type) = accept {
        request = request.header("Accept", media_type);
    }
    let response = sender
        .send_request(request.body(Full::new(Bytes::new()))?)
        .await
        .with_context(|| format!("GET {path}"))?;
    let status = response.status();
    let body = response
        .into_body()
        .collect()
        .await
        .context("read registry response")?
        .to_bytes();
    if status != StatusCode::OK {
        bail!(
            "registry GET {path}: expected 200, got {status} ({})",
            String::from_utf8_lossy(&body)
        );
    }
    Ok(body.to_vec())
}

fn verify_signature(
    manifest: &ImageManifest,
    wasm: &[u8],
    public_key_hex: &str,
) -> Result<(), String> {
    let signature_hex = manifest
        .annotations
        .get(SIGNATURE_ANNOTATION)
        .ok_or_else(|| format!("manifest is missing the {SIGNATURE_ANNOTATION:?} annotation"))?;
    let public_key = hex::decode(public_key_hex.trim())
        .map_err(|error| format!("decode public key: {error}"))?;
    let signature =
        hex::decode(signature_hex.trim()).map_err(|error| format!("decode signature: {error}"))?;
    UnparsedPublicKey::new(&ED25519, public_key)
        .verify(&Sha256::digest(wasm), &signature)
        .map_err(|_| "ed25519 signature verification failed".to_string())
}

fn verify_sbom(manifest: &ImageManifest, expected: &[String]) -> Result<usize, Vec<String>> {
    let Some(sbom) = manifest.annotations.get(SBOM_ANNOTATION) else {
        return Err(vec![format!(
            "manifest is missing the {SBOM_ANNOTATION:?} annotation"
        )]);
    };
    let value: serde_json::Value = match serde_json::from_str(sbom) {
        Ok(value) => value,
        Err(error) => return Err(vec![format!("SBOM does not parse: {error}")]),
    };
    let names: BTreeSet<&str> = value
        .get("components")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|component| component.get("name")?.as_str())
        .collect();
    let mut failures = Vec::new();
    if names.is_empty() {
        failures.push("SBOM lists no components".to_string());
    }
    for package in expected {
        if !names.contains(package.as_str()) {
            failures.push(format!("SBOM does not list expected package {package:?}"));
        }
    }
    if failures.is_empty() {
        Ok(names.len())
    } else {
        Err(failures)
    }
}

fn verify_manifest(manifest: &ImageManifest) -> Result<NodeManifest, Vec<String>> {
    let mut failures = Vec::new();
    let node_manifest = match manifest.annotations.get(ANNOTATION_KEY) {
        Some(json) => match NodeManifest::from_json(json) {
            Ok(manifest) if manifest.is_valid() => Some(manifest),
            Ok(manifest) => {
                failures.push(format!(
                    "{ANNOTATION_KEY:?} annotation does not validate: {:?}",
                    manifest.issues()
                ));
                None
            }
            Err(error) => {
                failures.push(format!(
                    "{ANNOTATION_KEY:?} annotation does not parse: {error}"
                ));
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
            "layers[0] media type {:?} != {WASM_LAYER_MEDIA_TYPE:?} — the node host cannot pull it",
            layer.media_type
        )),
        None => failures.push("manifest has no layers".to_string()),
    }

    match node_manifest {
        Some(manifest) if failures.is_empty() => Ok(manifest),
        _ => Err(failures),
    }
}

pub async fn run(args: BuildproofArgs) -> anyhow::Result<()> {
    let target = RegistryRef {
        registry: args.registry,
        repository: args.repository,
        reference: args.reference,
    };
    println!("# wamn-gates buildproof — verify a node artifact FROM THE REGISTRY");
    println!("# image: {}", target.image());

    let manifest_bytes = fetch(
        &target,
        &format!("/v2/{}/manifests/{}", target.repository, target.reference),
        Some(OCI_MANIFEST_MEDIA_TYPE),
    )
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
            failures
                .iter()
                .for_each(|failure| println!("    FAIL: {failure}"));
            pass = false;
        }
    }

    let layer_bytes = match manifest.wasm_layer() {
        Some(layer) => Some(
            fetch(
                &target,
                &format!("/v2/{}/blobs/{}", target.repository, layer.digest),
                None,
            )
            .await
            .context("fetch wasm layer")?,
        ),
        None => None,
    };

    println!("\n## layer digest integrity");
    if let (Some(layer), Some(bytes)) = (manifest.wasm_layer(), &layer_bytes) {
        let actual = sha256_digest(bytes);
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

    println!("\n## artifact signature");
    match (&args.public_key, &layer_bytes) {
        (Some(public_key), Some(bytes)) => {
            if let Err(error) = verify_signature(&manifest, bytes, public_key) {
                println!("    FAIL: {error}");
                pass = false;
            } else {
                println!("    PASS: wamn.node.signature verifies against the public key");
            }
        }
        (Some(_), None) => {
            println!("    FAIL: no wasm layer to verify the signature over");
            pass = false;
        }
        (None, _) => println!("    SKIP: no --public-key given (v0 posture)"),
    }

    println!("\n## SBOM");
    match verify_sbom(&manifest, &args.expect_packages) {
        Ok(count) => println!("    PASS: SBOM present ({count} components)"),
        Err(failures) => {
            failures
                .iter()
                .for_each(|failure| println!("    FAIL: {failure}"));
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
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair as _};

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
            purity: None,
        }
        .to_json()
    }

    fn manifest(wasm: &[u8], node: Option<&str>) -> ImageManifest {
        let mut annotations = BTreeMap::new();
        if let Some(node) = node {
            annotations.insert(ANNOTATION_KEY.to_string(), node.to_string());
        }
        ImageManifest {
            schema_version: 2,
            media_type: OCI_MANIFEST_MEDIA_TYPE.to_string(),
            config: Descriptor {
                media_type: "application/vnd.wasm.config.v0+json".to_string(),
                digest: sha256_digest(b"{}"),
                size: 2,
            },
            layers: vec![Descriptor {
                media_type: WASM_LAYER_MEDIA_TYPE.to_string(),
                digest: sha256_digest(wasm),
                size: wasm.len() as i64,
            }],
            annotations,
        }
    }

    fn signed_manifest(wasm: &[u8]) -> (ImageManifest, String) {
        let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let key = Ed25519KeyPair::from_pkcs8(document.as_ref()).unwrap();
        let mut manifest = manifest(wasm, Some(&valid_node_manifest_json()));
        manifest.annotations.insert(
            SIGNATURE_ANNOTATION.to_string(),
            hex::encode(key.sign(&Sha256::digest(wasm)).as_ref()),
        );
        (manifest, hex::encode(key.public_key().as_ref()))
    }

    #[test]
    fn manifest_requires_valid_annotation_and_layer_media_type() {
        let wasm = b"node";
        assert!(verify_manifest(&manifest(wasm, Some(&valid_node_manifest_json()))).is_ok());
        assert!(verify_manifest(&manifest(wasm, None)).is_err());

        let mut invalid = manifest(wasm, Some(&valid_node_manifest_json()));
        invalid.layers[0].media_type = "application/octet-stream".to_string();
        assert!(verify_manifest(&invalid).is_err());
    }

    #[test]
    fn signature_binds_exact_artifact_bytes() {
        let wasm = b"the-node";
        let (manifest, public_key) = signed_manifest(wasm);
        assert!(verify_signature(&manifest, wasm, &public_key).is_ok());
        assert!(verify_signature(&manifest, b"tampered", &public_key).is_err());
    }

    #[test]
    fn sbom_requires_expected_packages() {
        let mut manifest = manifest(b"node", Some(&valid_node_manifest_json()));
        manifest.annotations.insert(
            SBOM_ANNOTATION.to_string(),
            r#"{"components":[{"name":"sample-echo","version":"0.1.0"}]}"#.to_string(),
        );
        assert!(verify_sbom(&manifest, &["sample-echo".to_string()]).is_ok());
        assert!(verify_sbom(&manifest, &["serde_json".to_string()]).is_err());
    }
}
