//! 5.5e lane gate: the hand-rolled OCI push against an in-process registry-v2
//! STUB (a hyper server), then a read-back that asserts the EXACT bytes /
//! manifest / annotations that were PUT. This is the lane-level gate for 0si.5 —
//! the E2E cannot reach the live in-cluster registry, so the stub stands in for
//! it and pins the wire shape the wash-runtime host pulls.

use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

use wamn_builder::registry::{
    self, ImageManifest, RegistryRef, WASM_CONFIG_MEDIA_TYPE, WASM_LAYER_MEDIA_TYPE,
};

/// The stub's in-memory store: blobs by digest + manifests by reference, plus a
/// monotonic upload-id counter.
#[derive(Default)]
struct Store {
    blobs: HashMap<String, Vec<u8>>,
    manifests: HashMap<String, Vec<u8>>,
    next_upload: u64,
    // digest of the LAST PUT manifest's body — for the byte-exactness assertion.
    last_manifest_ref: Option<String>,
}

type Shared = Arc<Mutex<Store>>;

async fn handle(
    req: Request<Incoming>,
    store: Shared,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let body = req
        .into_body()
        .collect()
        .await
        .map(|b| b.to_bytes())
        .unwrap_or_default();

    // POST /v2/<repo>/blobs/uploads/  -> 202 + Location
    if method == hyper::Method::POST && path.ends_with("/blobs/uploads/") {
        let repo = path
            .trim_start_matches("/v2/")
            .trim_end_matches("/blobs/uploads/")
            .to_string();
        let id = {
            let mut s = store.lock().unwrap();
            s.next_upload += 1;
            s.next_upload
        };
        let loc = format!("/v2/{repo}/blobs/uploads/{id}");
        return Ok(Response::builder()
            .status(StatusCode::ACCEPTED)
            .header("Location", loc)
            .body(Full::new(Bytes::new()))
            .unwrap());
    }

    // PUT /v2/<repo>/blobs/uploads/<id>?digest=<d>  -> 201
    if method == hyper::Method::PUT && path.contains("/blobs/uploads/") {
        let digest = query
            .split('&')
            .find_map(|kv| kv.strip_prefix("digest="))
            .expect("digest query param")
            .to_string();
        store.lock().unwrap().blobs.insert(digest, body.to_vec());
        return Ok(Response::builder()
            .status(StatusCode::CREATED)
            .body(Full::new(Bytes::new()))
            .unwrap());
    }

    // PUT /v2/<repo>/manifests/<ref>  -> 201
    if method == hyper::Method::PUT && path.contains("/manifests/") {
        let reference = path.rsplit("/manifests/").next().unwrap().to_string();
        {
            let mut s = store.lock().unwrap();
            s.manifests.insert(reference.clone(), body.to_vec());
            s.last_manifest_ref = Some(reference);
        }
        return Ok(Response::builder()
            .status(StatusCode::CREATED)
            .body(Full::new(Bytes::new()))
            .unwrap());
    }

    // GET /v2/<repo>/manifests/<ref>  -> 200 + manifest
    if method == hyper::Method::GET && path.contains("/manifests/") {
        let reference = path.rsplit("/manifests/").next().unwrap();
        let m = store.lock().unwrap().manifests.get(reference).cloned();
        return Ok(match m {
            Some(bytes) => Response::builder()
                .status(StatusCode::OK)
                .body(Full::new(Bytes::from(bytes)))
                .unwrap(),
            None => Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Full::new(Bytes::new()))
                .unwrap(),
        });
    }

    // GET /v2/<repo>/blobs/<digest>  -> 200 + blob
    if method == hyper::Method::GET && path.contains("/blobs/") {
        let digest = path.rsplit("/blobs/").next().unwrap();
        let b = store.lock().unwrap().blobs.get(digest).cloned();
        return Ok(match b {
            Some(bytes) => Response::builder()
                .status(StatusCode::OK)
                .body(Full::new(Bytes::from(bytes)))
                .unwrap(),
            None => Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Full::new(Bytes::new()))
                .unwrap(),
        });
    }

    Ok(Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Full::new(Bytes::new()))
        .unwrap())
}

/// Start the stub on an ephemeral port; returns `host:port`.
async fn start_stub(store: Shared) -> String {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            let store = store.clone();
            tokio::spawn(async move {
                let io = TokioIo::new(sock);
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, service_fn(move |req| handle(req, store.clone())))
                    .await;
            });
        }
    });
    format!("127.0.0.1:{}", addr.port())
}

#[tokio::test]
async fn push_then_read_back_pins_the_pullable_shape() {
    let store: Shared = Arc::new(Mutex::new(Store::default()));
    let registry = start_stub(store.clone()).await;

    let target = RegistryRef {
        registry,
        repository: "wamn/sample-node".to_string(),
        reference: "dev".to_string(),
        insecure: true,
    };

    // A fake but non-empty "component" (the push/fetch is media-type + digest
    // driven; it never parses the wasm).
    let wasm = b"\x00asm\x0d\x00\x01\x00the-node-bytes".to_vec();
    let mut annotations = BTreeMap::new();
    annotations.insert(
        "wamn.node.manifest".to_string(),
        r#"{"schema-version":"0.1"}"#.to_string(),
    );

    let pushed = registry::push(&target, &wasm, annotations.clone())
        .await
        .expect("push succeeds");
    assert_eq!(pushed.layer_digest, registry::sha256_digest(&wasm));

    // Read the manifest back from the stub (the buildproof fetch path) and assert
    // the EXACT wire shape the wash-runtime host pulls.
    let manifest_bytes = registry::fetch_manifest(&target)
        .await
        .expect("fetch manifest");
    let manifest: ImageManifest = serde_json::from_slice(&manifest_bytes).unwrap();
    assert_eq!(manifest.schema_version, 2);
    assert_eq!(manifest.media_type, registry::OCI_MANIFEST_MEDIA_TYPE);
    // ONE layer, application/wasm, digest == sha256(wasm), size == wasm.len().
    assert_eq!(manifest.layers.len(), 1);
    let layer = manifest.wasm_layer().unwrap();
    assert_eq!(layer.media_type, WASM_LAYER_MEDIA_TYPE);
    assert_eq!(layer.digest, registry::sha256_digest(&wasm));
    assert_eq!(layer.size, wasm.len() as i64);
    // config media type matches the live wash-pushed artifact.
    assert_eq!(manifest.config.media_type, WASM_CONFIG_MEDIA_TYPE);
    // the annotation round-trips byte-identically.
    assert_eq!(manifest.annotations, annotations);

    // The wasm LAYER blob the host would pull is byte-identical to what we built.
    let fetched_layer = registry::fetch_blob(&target, &layer.digest)
        .await
        .expect("fetch layer blob");
    assert_eq!(
        fetched_layer, wasm,
        "the pulled layer is the exact component bytes"
    );

    // The config blob is present + digest-consistent.
    let fetched_config = registry::fetch_blob(&target, &manifest.config.digest)
        .await
        .expect("fetch config blob");
    assert_eq!(
        registry::sha256_digest(&fetched_config),
        manifest.config.digest
    );
}
