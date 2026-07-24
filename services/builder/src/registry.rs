//! 5.5e — the OCI registry-v2 writer (the repo's first Rust OCI push) + the
//! plain-HTTP fetch helpers the `buildproof` gate reuses.
//!
//! HAND-ROLLED over the hyper 1 stack (already in the graph), NOT `oci-client`:
//! full control of the manifest media types + annotations, no new heavy dep, and
//! a wire shape the local stub test can assert byte-for-byte. The artifact MUST
//! stay pullable by the wash-runtime host, whose fork pull path
//! (`~/.cargo/git/checkouts/wasmcloud-…/eef76cd/crates/wash-runtime/src/oci.rs`,
//! `pull_component` lines 422-452) accepts `[WASM_LAYER_MEDIA_TYPE,
//! WASMCLOUD_MEDIA_TYPE]` and takes `layers.first()`. So the wasm layer is layer
//! `[0]` with media type [`WASM_LAYER_MEDIA_TYPE`] (`application/wasm`) and the
//! manifest / config media types match the LIVE wash-pushed artifact
//! (cross-checked against the in-cluster registry). Annotations are ADDITIVE to
//! that shape (the live manifest carries none).
//!
//! Plain HTTP only (the in-cluster registry:2 is plain HTTP + the host pulls it
//! with `--allow-insecure-registries`); TLS is a deferral.

use std::collections::BTreeMap;

use anyhow::{Context as _, bail};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, StatusCode};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::net::TcpStream;

/// The OCI image-manifest media type (matches the live wash-pushed artifact).
pub const OCI_MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
/// The wasm config-blob media type (oci-wasm `WASM_MANIFEST_CONFIG_MEDIA_TYPE`).
pub const WASM_CONFIG_MEDIA_TYPE: &str = "application/vnd.wasm.config.v0+json";
/// The wasm LAYER media type (oci-wasm `WASM_LAYER_MEDIA_TYPE`). The fork pull
/// path accepts this; layer[0] must carry it or the host cannot pull.
pub const WASM_LAYER_MEDIA_TYPE: &str = "application/wasm";

/// Where to push / fetch: `registry` is `host:port`, plus the repository path
/// and a tag or digest reference. Plain HTTP when `insecure`.
#[derive(Debug, Clone)]
pub struct RegistryRef {
    /// `host:port` (e.g. `registry.wamn-system.svc.cluster.local:5000`).
    pub registry: String,
    /// The repository path (e.g. `wamn/sample-node`).
    pub repository: String,
    /// The tag or `sha256:…` digest reference (e.g. `dev`).
    pub reference: String,
    /// Plain HTTP (the in-cluster registry). TLS is a deferral.
    pub insecure: bool,
}

impl RegistryRef {
    /// The `host:port` split for the TCP connect + Host header.
    fn host_port(&self) -> anyhow::Result<(String, u16)> {
        let (host, port) = self
            .registry
            .rsplit_once(':')
            .context("registry must be host:port")?;
        Ok((host.to_string(), port.parse().context("registry port")?))
    }

    /// The full image reference string (for reporting).
    pub fn image(&self) -> String {
        format!("{}/{}:{}", self.registry, self.repository, self.reference)
    }
}

/// `sha256:<hex>` of `bytes` — the OCI content digest.
pub fn sha256_digest(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(7 + 64);
    out.push_str("sha256:");
    for b in Sha256::digest(bytes) {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// An OCI content descriptor (config or layer).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Descriptor {
    #[serde(rename = "mediaType")]
    pub media_type: String,
    pub digest: String,
    pub size: i64,
}

/// The OCI image manifest we PUT (schemaVersion 2 + config + one wasm layer +
/// annotations). Deserializable so `buildproof` can read it back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageManifest {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    #[serde(rename = "mediaType")]
    pub media_type: String,
    pub config: Descriptor,
    pub layers: Vec<Descriptor>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub annotations: BTreeMap<String, String>,
}

impl ImageManifest {
    /// The wasm layer descriptor (`layers[0]`) — the one the host pulls.
    pub fn wasm_layer(&self) -> Option<&Descriptor> {
        self.layers.first()
    }
}

/// The minimal `application/vnd.wasm.config.v0+json` config blob for a component
/// layer. The fork pull path does NOT parse this (it takes `layers.first()`), so
/// the LAYER is load-bearing for pullability, not this config; `created` is fixed
/// so the artifact is byte-reproducible.
fn wasm_config_blob(layer_digest: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "created": "1970-01-01T00:00:00Z",
        "author": null,
        "architecture": "wasm",
        "os": "wasip2",
        "layerDigests": [layer_digest],
        "component": null,
    }))
    .expect("config serializes")
}

/// Build the manifest for a wasm component + annotations (pure): the wasm as the
/// single `application/wasm` layer[0], a wasm config blob, and the annotations.
/// Returns the manifest + the exact config bytes it references.
pub fn build_manifest(
    wasm: &[u8],
    annotations: BTreeMap<String, String>,
) -> (ImageManifest, Vec<u8>) {
    let layer_digest = sha256_digest(wasm);
    let config = wasm_config_blob(&layer_digest);
    let manifest = ImageManifest {
        schema_version: 2,
        media_type: OCI_MANIFEST_MEDIA_TYPE.to_string(),
        config: Descriptor {
            media_type: WASM_CONFIG_MEDIA_TYPE.to_string(),
            digest: sha256_digest(&config),
            size: config.len() as i64,
        },
        layers: vec![Descriptor {
            media_type: WASM_LAYER_MEDIA_TYPE.to_string(),
            digest: layer_digest,
            size: wasm.len() as i64,
        }],
        annotations,
    };
    (manifest, config)
}

/// The outcome of a push: the digests the registry now holds.
#[derive(Debug, Clone)]
pub struct Pushed {
    /// The manifest digest (`sha256:…`).
    pub manifest_digest: String,
    /// The wasm layer digest.
    pub layer_digest: String,
    /// The config blob digest.
    pub config_digest: String,
    /// The pushed image reference.
    pub image: String,
}

/// A parsed HTTP response (status + headers + body).
struct Resp {
    status: StatusCode,
    location: Option<String>,
    body: Bytes,
}

/// Send one plain-HTTP/1.1 request to `host:port` (a fresh connection) and read
/// the full response. `path` is origin-form (`/v2/…`).
async fn send(
    host: &str,
    port: u16,
    method: &str,
    path: &str,
    content_type: Option<&str>,
    body: Vec<u8>,
) -> anyhow::Result<Resp> {
    let stream = TcpStream::connect((host, port))
        .await
        .with_context(|| format!("connect {host}:{port}"))?;
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .context("http1 handshake")?;
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("Host", format!("{host}:{port}"))
        .header("Content-Length", body.len().to_string());
    if let Some(ct) = content_type {
        // A GET has no body: the media type is what we ACCEPT, not what we
        // send. registry:2 refuses to serve an OCI manifest without the
        // explicit Accept (MANIFEST_UNKNOWN) — the live buildproof run
        // surfaced this; the in-process stub now pins the header.
        if method == "GET" {
            builder = builder.header("Accept", ct);
        } else {
            builder = builder.header("Content-Type", ct);
        }
    }
    let req = builder
        .body(Full::new(Bytes::from(body)))
        .context("build request")?;

    let resp = sender
        .send_request(req)
        .await
        .with_context(|| format!("{method} {path}"))?;
    let status = resp.status();
    let location = resp
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let body = resp
        .into_body()
        .collect()
        .await
        .context("read response body")?
        .to_bytes();
    Ok(Resp {
        status,
        location,
        body,
    })
}

/// The origin-form path of a registry `Location` header: registry:2 returns
/// either an absolute URL (`http://host/v2/…`) or a bare path — take just the
/// path (+ query), since we always talk to the same host.
fn location_path(location: &str) -> String {
    if let Some(rest) = location.strip_prefix("http://") {
        match rest.find('/') {
            Some(i) => rest[i..].to_string(),
            None => "/".to_string(),
        }
    } else if let Some(rest) = location.strip_prefix("https://") {
        match rest.find('/') {
            Some(i) => rest[i..].to_string(),
            None => "/".to_string(),
        }
    } else {
        location.to_string()
    }
}

/// Upload one blob via the two-step registry-v2 flow (POST an upload session,
/// then PUT the bytes with `?digest=`).
async fn push_blob(
    host: &str,
    port: u16,
    repository: &str,
    digest: &str,
    data: Vec<u8>,
) -> anyhow::Result<()> {
    // POST /v2/<repo>/blobs/uploads/ → 202 + Location (the upload URL).
    let start = send(
        host,
        port,
        "POST",
        &format!("/v2/{repository}/blobs/uploads/"),
        None,
        Vec::new(),
    )
    .await?;
    if start.status != StatusCode::ACCEPTED {
        bail!(
            "blob upload start for {digest}: expected 202, got {} ({})",
            start.status,
            String::from_utf8_lossy(&start.body)
        );
    }
    let location = start
        .location
        .context("blob upload start returned no Location header")?;
    let path = location_path(&location);
    let sep = if path.contains('?') { '&' } else { '?' };
    let put_path = format!("{path}{sep}digest={digest}");

    // PUT <location>?digest=<digest> with the blob bytes → 201.
    let done = send(
        host,
        port,
        "PUT",
        &put_path,
        Some("application/octet-stream"),
        data,
    )
    .await?;
    if done.status != StatusCode::CREATED {
        bail!(
            "blob upload finish for {digest}: expected 201, got {} ({})",
            done.status,
            String::from_utf8_lossy(&done.body)
        );
    }
    Ok(())
}

/// Push a wasm component + annotations to `target`. Uploads the config + the
/// wasm layer, then PUTs the manifest under `target.reference`.
pub async fn push(
    target: &RegistryRef,
    wasm: &[u8],
    annotations: BTreeMap<String, String>,
) -> anyhow::Result<Pushed> {
    if !target.insecure {
        bail!("registry push over TLS is not supported (v0 plain-HTTP only)");
    }
    let (host, port) = target.host_port()?;
    let repo = &target.repository;

    let (manifest, config) = build_manifest(wasm, annotations);
    let layer_digest = manifest.layers[0].digest.clone();
    let config_digest = manifest.config.digest.clone();

    // Blobs first (config + wasm layer), then the manifest that references them.
    push_blob(&host, port, repo, &config_digest, config).await?;
    push_blob(&host, port, repo, &layer_digest, wasm.to_vec()).await?;

    let manifest_bytes = serde_json::to_vec(&manifest).context("serialize manifest")?;
    let manifest_digest = sha256_digest(&manifest_bytes);
    let put = send(
        &host,
        port,
        "PUT",
        &format!("/v2/{repo}/manifests/{}", target.reference),
        Some(OCI_MANIFEST_MEDIA_TYPE),
        manifest_bytes,
    )
    .await?;
    if put.status != StatusCode::CREATED {
        bail!(
            "manifest PUT: expected 201, got {} ({})",
            put.status,
            String::from_utf8_lossy(&put.body)
        );
    }

    Ok(Pushed {
        manifest_digest,
        layer_digest,
        config_digest,
        image: target.image(),
    })
}

/// Fetch a manifest by tag or digest (the `buildproof` fetch, plain HTTP): GET
/// with the OCI manifest `Accept`. Returns the raw manifest bytes.
pub async fn fetch_manifest(target: &RegistryRef) -> anyhow::Result<Vec<u8>> {
    let (host, port) = target.host_port()?;
    let resp = send(
        &host,
        port,
        "GET",
        &format!("/v2/{}/manifests/{}", target.repository, target.reference),
        Some(OCI_MANIFEST_MEDIA_TYPE),
        Vec::new(),
    )
    .await?;
    if resp.status != StatusCode::OK {
        bail!(
            "manifest fetch {}: expected 200, got {} ({})",
            target.image(),
            resp.status,
            String::from_utf8_lossy(&resp.body)
        );
    }
    Ok(resp.body.to_vec())
}

/// Fetch a blob by digest (plain HTTP): GET `/v2/<repo>/blobs/<digest>`.
pub async fn fetch_blob(target: &RegistryRef, digest: &str) -> anyhow::Result<Vec<u8>> {
    let (host, port) = target.host_port()?;
    let resp = send(
        &host,
        port,
        "GET",
        &format!("/v2/{}/blobs/{digest}", target.repository),
        None,
        Vec::new(),
    )
    .await?;
    if resp.status != StatusCode::OK {
        bail!(
            "blob fetch {digest}: expected 200, got {} ({})",
            resp.status,
            String::from_utf8_lossy(&resp.body)
        );
    }
    Ok(resp.body.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_digest_is_prefixed_lowercase_hex() {
        assert_eq!(
            sha256_digest(b""),
            "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn build_manifest_puts_wasm_as_layer0_with_the_pullable_media_types() {
        let wasm = b"\x00asm\x0d\x00\x01\x00fake-component-bytes";
        let mut ann = BTreeMap::new();
        ann.insert("wamn.node.manifest".to_string(), "{}".to_string());
        let (m, config) = build_manifest(wasm, ann.clone());

        assert_eq!(m.schema_version, 2);
        assert_eq!(m.media_type, OCI_MANIFEST_MEDIA_TYPE);
        // layer[0] is the wasm, media type application/wasm — what the fork pulls.
        assert_eq!(m.layers.len(), 1);
        assert_eq!(m.wasm_layer().unwrap().media_type, WASM_LAYER_MEDIA_TYPE);
        assert_eq!(m.wasm_layer().unwrap().digest, sha256_digest(wasm));
        assert_eq!(m.wasm_layer().unwrap().size, wasm.len() as i64);
        // config media type + digest reference the exact config bytes.
        assert_eq!(m.config.media_type, WASM_CONFIG_MEDIA_TYPE);
        assert_eq!(m.config.digest, sha256_digest(&config));
        assert_eq!(m.annotations, ann);
    }

    #[test]
    fn location_path_normalizes_absolute_and_relative() {
        assert_eq!(
            location_path("http://reg:5000/v2/x/blobs/uploads/abc?_state=q"),
            "/v2/x/blobs/uploads/abc?_state=q"
        );
        assert_eq!(
            location_path("/v2/x/blobs/uploads/abc"),
            "/v2/x/blobs/uploads/abc"
        );
    }

    #[test]
    fn manifest_round_trips_through_json() {
        let (m, _) = build_manifest(b"\x00asm...", BTreeMap::new());
        let bytes = serde_json::to_vec(&m).unwrap();
        let back: ImageManifest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.layers[0].media_type, WASM_LAYER_MEDIA_TYPE);
        // No annotations -> the field is omitted (matches the live single-layer shape).
        assert!(!String::from_utf8_lossy(&bytes).contains("annotations"));
    }
}
