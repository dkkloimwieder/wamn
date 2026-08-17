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

use std::time::Duration;

use anyhow::Context as _;
use oci_client::client::{ClientConfig, ClientProtocol, Config, ImageLayer};
use oci_client::secrets::RegistryAuth;
use oci_client::{Client, Reference};
use sha2::{Digest as _, Sha256};
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
