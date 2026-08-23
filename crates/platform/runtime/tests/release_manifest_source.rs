//! Every refusal the release-manifest OCI source makes before it touches a registry.
//!
//! The one refusal that needs a real registry — a published artifact pulling
//! back byte-exact — is the ignored leg at the bottom.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use wamn_runtime::component_admission::component_digest;
use wamn_runtime::release_manifest::ReleaseManifestWeld;
use wamn_runtime::release_manifest_artifact::{
    RELEASE_MANIFEST_CONFIG_MEDIA_TYPE, release_manifest_artifact_layout,
    verify_release_manifest_artifact_layout,
};
use wamn_runtime::release_manifest_source::{ReleaseManifestFetchErrorKind, ReleaseManifestSource};

const REGISTRY: &str = "registry.example:5000";
const ARTIFACT_BASE: &str = "registry.example:5000/wamn/releases";
static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

/// A private pull credential for exactly [`REGISTRY`], removed on drop.
struct ScratchCredential {
    root: PathBuf,
    path: PathBuf,
}

impl ScratchCredential {
    fn write() -> Self {
        let sequence = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "wamn-release-manifest-source-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("scratch credential directory");
        let path = root.join("config.json");
        std::fs::write(
            &path,
            format!(r#"{{"auths":{{"{REGISTRY}":{{"username":"puller","password":"secret"}}}}}}"#),
        )
        .expect("write scratch credential");
        Self { root, path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn absent(&self) -> PathBuf {
        self.root.join("missing.json")
    }
}

impl Drop for ScratchCredential {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_puller_verifies_the_publisher_layout_without_knowing_the_size() {
    let bytes = br#"{"format-version":2}"#;
    let (layer, _, manifest) = release_manifest_artifact_layout(bytes);
    let digest = layer.sha256_digest();

    // The publisher declares the size it just wrote; a puller has only the name.
    let blobs = verify_release_manifest_artifact_layout(&manifest, &digest, None)
        .expect("the published layout verifies for a puller");
    assert_eq!(blobs.layer.digest, digest);
    assert_eq!(
        blobs.layer.size,
        i64::try_from(bytes.len()).expect("fixture length fits")
    );
    assert_eq!(blobs.config.media_type, RELEASE_MANIFEST_CONFIG_MEDIA_TYPE);
    verify_release_manifest_artifact_layout(&manifest, &digest, Some(bytes.len()))
        .expect("the publisher's own size check still passes");
    assert_eq!(
        verify_release_manifest_artifact_layout(&manifest, &digest, Some(bytes.len() + 1))
            .expect_err("a declared size the layer contradicts refuses")
            .refusal(),
        "release-manifest-artifact-layer-mismatch"
    );

    // The digest a pod template names is the whole binding. Bytes published
    // under any other name are a different release, never this one.
    let other = format!("sha256:{}", "c".repeat(64));
    assert_eq!(
        verify_release_manifest_artifact_layout(&manifest, &other, None)
            .expect_err("a foreign layer digest refuses")
            .refusal(),
        "release-manifest-artifact-layer-mismatch"
    );
}

#[test]
fn pulled_bytes_load_through_the_weld_naming_their_carrier() {
    let canonical = br#"{"attachments":{},"components":[],"format-version":2,"registrations":{},"release":{"catalog-id":"cat","catalog-version":7,"environment":"prod","tenant-id":"t1"},"wirings":[]}"#;

    let weld = ReleaseManifestWeld::load_canonical_bytes(canonical, ARTIFACT_BASE)
        .expect("verified canonical bytes load without a mount");
    assert_eq!(weld.release().release_version, 7);
    assert_eq!(
        weld.release().manifest_digest.as_str(),
        component_digest(canonical)
    );

    let mut trailing = canonical.to_vec();
    trailing.push(b'\n');
    let error = ReleaseManifestWeld::load_canonical_bytes(&trailing, ARTIFACT_BASE)
        .expect_err("a non-canonical body refuses");
    assert!(
        error.to_string().contains(ARTIFACT_BASE),
        "the refusal must name the carrier it came from: {error}"
    );
}

#[test]
fn a_mutable_or_ambient_base_refuses_before_any_transport() {
    let credential = ScratchCredential::write();

    for base in [
        "wamn/releases",
        "registry.example:5000/wamn/releases:latest",
        "https://registry.example:5000/wamn/releases",
    ] {
        let error = ReleaseManifestSource::new(base, true, credential.path())
            .expect_err("a base that cannot name an immutable artifact refuses");
        assert_eq!(
            error.kind(),
            ReleaseManifestFetchErrorKind::InvalidReference,
            "accepted {base:?}"
        );
    }
}

#[test]
fn a_missing_pull_credential_refuses_before_any_transport() {
    let credential = ScratchCredential::write();

    let error = ReleaseManifestSource::new(ARTIFACT_BASE, true, &credential.absent())
        .expect_err("an absent pull credential refuses");

    assert_eq!(error.kind(), ReleaseManifestFetchErrorKind::Credential);
}

#[tokio::test]
async fn a_digest_that_cannot_name_an_artifact_refuses_before_any_transport() {
    let credential = ScratchCredential::write();
    let source = ReleaseManifestSource::new(ARTIFACT_BASE, true, credential.path())
        .expect("an explicit base and a complete credential configure");

    // A pod template that names anything but one exact content digest is not
    // naming a release, so no request is worth making.
    let unprefixed = "a".repeat(64);
    for digest in ["latest", "sha256:short", unprefixed.as_str()] {
        let error = source
            .pull_verified(digest)
            .await
            .expect_err("a digest that is not `sha256:<64 lowercase hex>` refuses");
        assert_eq!(
            error.kind(),
            ReleaseManifestFetchErrorKind::InvalidReference,
            "accepted {digest:?}"
        );
    }
}

#[tokio::test]
#[ignore = "requires a disposable authenticated registry holding a published release"]
async fn a_published_release_pulls_back_byte_exact() {
    let artifact_base = std::env::var("WAMN_RELEASE_MANIFEST_ARTIFACT_BASE")
        .expect("set WAMN_RELEASE_MANIFEST_ARTIFACT_BASE to the published repository");
    let registry_auth_file = std::env::var("WAMN_REGISTRY_AUTH_FILE")
        .expect("set WAMN_REGISTRY_AUTH_FILE to its Docker config credential");
    let manifest_digest = std::env::var("WAMN_RELEASE_MANIFEST_DIGEST")
        .expect("set WAMN_RELEASE_MANIFEST_DIGEST to the published manifest digest");

    let source = ReleaseManifestSource::new(&artifact_base, true, Path::new(&registry_auth_file))
        .expect("the disposable registry configures");
    let bytes = source
        .pull_verified(&manifest_digest)
        .await
        .expect("the published release pulls");

    assert_eq!(component_digest(&bytes), manifest_digest);
}
