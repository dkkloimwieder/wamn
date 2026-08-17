//! The real `oci-client` transport, against a real registry (wamn-0h0g.15.12).
//!
//! `#[ignore]`d and env-gated because it needs a registry. Point it at a
//! **disposable** one — never the in-cluster registry, which is frozen:
//!
//! ```text
//! docker run --rm -d -p 5099:5000 --name wamn-plan-registry registry:2
//! WAMN_PLAN_REGISTRY=localhost:5099 \
//!   cargo test -p wamn-runtime --test oci_plan_source_live -- --ignored --nocapture
//! docker rm -f wamn-plan-registry
//! ```
//!
//! # Why the artifacts are pushed by the test
//!
//! There is no publisher yet: `wamn-0h0g.15.97` owns the push side and is not
//! written. So this proves the transport is *self-consistent* — bytes pushed
//! under the layout this reader expects come back byte-exact — and cannot prove
//! agreement with a real producer. That agreement is owed as a publish-side guard
//! against `.15.97`, and until it exists a disagreement would only surface here as
//! a `Mismatched` at runtime.
//!
//! What this does prove, and unit tests cannot: that a real registry accepts this
//! artifact layout at all (custom config and layer media types included), that
//! `pull_image_manifest` + `pull_blob` return the exact bytes, and that the two
//! failure dispositions arise from the wire rather than from a stub.
//!
//! # The corrupted-transfer leg needs no registry, and therefore always runs
//!
//! A conformant registry cannot be made to serve a corrupted transfer: it digests
//! every blob on upload and refuses one that disagrees with the name it is being
//! stored under. So the only way to prove that a body which does not hash to its
//! descriptor is refused BEFORE the bytes are usable is to be the corrupt server.
//! `a_corrupted_transfer_is_an_integrity_refusal` is that — a loopback socket and
//! three fixed HTTP/1.1 responses, no docker and no environment variable, so
//! unlike the leg above it runs in the ordinary suite (wamn-0h0g.15.67).

use std::time::Duration;

use anyhow::Context as _;
use oci_client::client::{ClientConfig, ClientProtocol, Config, ImageLayer};
use oci_client::manifest::{OCI_IMAGE_MEDIA_TYPE, OciDescriptor, OciImageManifest};
use oci_client::secrets::RegistryAuth;
use oci_client::{Client, Reference};
use sha2::{Digest as _, Sha256};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use wamn_runtime::plan_artifact::{
    EXECUTION_PLAN_CONFIG_BYTES, EXECUTION_PLAN_CONFIG_MEDIA_TYPE, EXECUTION_PLAN_LAYER_MEDIA_TYPE,
    OciPlanSource, PlanFetchErrorKind, PlanSource,
};

const REPOSITORY: &str = "wamn/execution-plans";
const PLAN_BYTES: &[u8] = br#"{"header":{"format-version":"0.1"},"nodes":[]}"#;
const UNPUBLISHED: &str = "sha256:9999999999999999999999999999999999999999999999999999999999999999";

fn digest_of(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

/// Push one plan artifact under `tag`, carrying `layer_bytes` as its plan layer.
///
/// `tag` is passed separately from the bytes so a test can publish an artifact
/// under a name its layer does not match — the producer fault this reader must
/// refuse rather than serve.
async fn publish(registry: &str, tag: &str, layer_bytes: &[u8]) -> anyhow::Result<()> {
    let client = Client::new(ClientConfig {
        protocol: ClientProtocol::HttpsExcept(vec![registry.to_string()]),
        ..ClientConfig::default()
    });
    let reference = Reference::with_tag(
        registry.to_string(),
        REPOSITORY.to_string(),
        tag.to_string(),
    );
    client
        .push(
            &reference,
            &[ImageLayer::new(
                layer_bytes.to_vec(),
                EXECUTION_PLAN_LAYER_MEDIA_TYPE.to_string(),
                None,
            )],
            Config::new(
                EXECUTION_PLAN_CONFIG_BYTES.to_vec(),
                EXECUTION_PLAN_CONFIG_MEDIA_TYPE.to_string(),
                None,
            ),
            &RegistryAuth::Anonymous,
            None,
        )
        .await
        .with_context(|| format!("push plan artifact {}", reference.whole()))?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a disposable `docker run --rm -d -p 5099:5000 registry:2` in WAMN_PLAN_REGISTRY"]
async fn oci_plan_source_live() -> anyhow::Result<()> {
    let registry = std::env::var("WAMN_PLAN_REGISTRY")
        .context("set WAMN_PLAN_REGISTRY to a DISPOSABLE registry, never the cluster's")?;
    let base = format!("{registry}/{REPOSITORY}");
    let source = OciPlanSource::new(&base, true, Duration::from_secs(10))?;

    // 1. A well-published plan returns byte-exact.
    let plan_hash = digest_of(PLAN_BYTES);
    let hex = plan_hash
        .strip_prefix("sha256:")
        .expect("digest is prefixed");
    publish(&registry, hex, PLAN_BYTES).await?;

    let pulled = source.fetch(&plan_hash).await?;
    assert_eq!(
        pulled, PLAN_BYTES,
        "a digest-named pull must return the exact bytes pushed"
    );

    // 2. The digest survives the round trip, which is the whole contract: the
    //    caller re-hashes these bytes and would refuse them otherwise.
    assert_eq!(digest_of(&pulled), plan_hash);

    // 3. A plan the release names but nobody pushed is `Unavailable` — releasable
    //    and requeueable, because a later attempt may find it published.
    let error = source
        .fetch(UNPUBLISHED)
        .await
        .expect_err("an unpublished plan must refuse");
    assert_eq!(error.kind(), PlanFetchErrorKind::Unavailable, "{error}");

    // 4. An artifact published under a tag its layer does not match is
    //    `Mismatched` — a producer fault, not a transport fault, and not
    //    retryable. Proven on the wire: the tag is a real plan digest, the layer
    //    under it is other content entirely.
    let forged_tag = digest_of(b"a plan that is not this one");
    let forged_hex = forged_tag
        .strip_prefix("sha256:")
        .expect("digest is prefixed");
    publish(&registry, forged_hex, b"unrelated bytes").await?;

    let error = source
        .fetch(&forged_tag)
        .await
        .expect_err("a misnamed artifact must refuse");
    assert_eq!(error.kind(), PlanFetchErrorKind::Mismatched, "{error}");

    Ok(())
}

/// A registry that publishes one plan artifact and then serves the wrong bytes
/// for it, torn down with the test that built it (wamn-0h0g.15.67).
struct CorruptRegistry {
    authority: String,
    server: JoinHandle<()>,
}

impl CorruptRegistry {
    /// Serve an artifact that NAMES `plan_hash` and CARRIES `corrupt`.
    async fn serving(plan_hash: &str, corrupt: Vec<u8>) -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("bind the corrupt registry")?;
        let authority = listener
            .local_addr()
            .context("read the corrupt registry's port")?
            .to_string();
        // Everything about the descriptor is what a well-behaved publisher would
        // have written — this reader's layer media type, the plan's digest, and a
        // size that agrees with the body it is served with. The digest is
        // therefore the ONLY disagreement, which is what makes this a corrupted
        // transfer rather than a misnamed artifact; the misnamed case is
        // `oci_plan_source_live`'s fourth step, and a different check catches it.
        let manifest = OciImageManifest {
            media_type: Some(OCI_IMAGE_MEDIA_TYPE.to_string()),
            config: OciDescriptor {
                media_type: EXECUTION_PLAN_CONFIG_MEDIA_TYPE.to_string(),
                digest: digest_of(EXECUTION_PLAN_CONFIG_BYTES),
                size: i64::try_from(EXECUTION_PLAN_CONFIG_BYTES.len())?,
                ..OciDescriptor::default()
            },
            layers: vec![OciDescriptor {
                media_type: EXECUTION_PLAN_LAYER_MEDIA_TYPE.to_string(),
                digest: plan_hash.to_string(),
                size: i64::try_from(corrupt.len())?,
                ..OciDescriptor::default()
            }],
            ..OciImageManifest::default()
        };
        let manifest = serde_json::to_vec(&manifest).context("serialize the corrupt manifest")?;
        Ok(Self {
            authority,
            server: tokio::spawn(serve_corrupt(listener, manifest, corrupt)),
        })
    }

    fn artifact_base(&self) -> String {
        format!("{}/{REPOSITORY}", self.authority)
    }
}

impl Drop for CorruptRegistry {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// Answer the GETs an anonymous digest-named pull makes, one connection each.
///
/// `GET /v2/` carries no `WWW-Authenticate`, so `oci-client` proceeds
/// unauthenticated and no token endpoint is needed. Hand-written HTTP/1.1 rather
/// than a server crate: three fixed responses do not warrant one, and
/// `wamn-runtime` depends on `hyper` without its server feature.
async fn serve_corrupt(listener: TcpListener, manifest: Vec<u8>, blob: Vec<u8>) {
    const EMPTY: &[u8] = b"";

    while let Ok((mut stream, _)) = listener.accept().await {
        let Some(target) = request_target(&mut stream).await else {
            continue;
        };
        let (status, content_type, body) = if target == "/v2/" {
            ("200 OK", "text/plain", EMPTY)
        } else if target.contains("/manifests/") {
            ("200 OK", OCI_IMAGE_MEDIA_TYPE, manifest.as_slice())
        } else if target.contains("/blobs/") {
            ("200 OK", "application/octet-stream", blob.as_slice())
        } else {
            ("404 Not Found", "text/plain", EMPTY)
        };
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        if stream.write_all(head.as_bytes()).await.is_ok() {
            let _ = stream.write_all(body).await;
        }
        let _ = stream.shutdown().await;
    }
}

/// The request target of one HTTP/1.1 head, or `None` if the peer hung up first.
///
/// A byte at a time and no body read: every request that arrives here is a GET
/// whose head is a few hundred bytes, so the simplest correct reader is the right
/// one.
async fn request_target(stream: &mut TcpStream) -> Option<String> {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        // Bound to its own statement so the future holding `&mut byte` is dropped
        // before `byte` is read back.
        let read = stream.read(&mut byte).await;
        if read.ok()? != 1 {
            return None;
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8(head).ok()?;
    head.split_whitespace().nth(1).map(str::to_string)
}

/// A body that does not hash to the descriptor it arrived under is refused, and
/// refused as an INTEGRITY fault rather than a retryable transport one.
///
/// This is the property `docs/deployment-simplification-spec.md:90` calls "plan
/// bytes fetch by digest, verify at transfer", and it is the one live disposition
/// nothing else can reach: a stub source proves what the supply path does with
/// bytes it is handed, and a real registry cannot be persuaded to hand over bad
/// ones. The kind matters as much as the refusal — wamn-0h0g.15.12's disposition
/// table sends an unavailable fetch back to the queue and an integrity failure to
/// a refusal, and re-fetching a corrupt blob only ever yields the same blob.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_corrupted_transfer_is_an_integrity_refusal() -> anyhow::Result<()> {
    let plan_hash = digest_of(PLAN_BYTES);
    let corrupt = b"not the plan these bytes claim".to_vec();
    let registry = CorruptRegistry::serving(&plan_hash, corrupt).await?;
    let base = registry.artifact_base();
    let source = OciPlanSource::new(&base, true, Duration::from_secs(10))?;

    let error = source
        .fetch(&plan_hash)
        .await
        .expect_err("a corrupted transfer must refuse instead of returning the bytes");

    assert_eq!(
        error.kind(),
        PlanFetchErrorKind::Mismatched,
        "a body that disagrees with its descriptor is an integrity fault, not a \
         retryable transport fault: {error}"
    );
    Ok(())
}
