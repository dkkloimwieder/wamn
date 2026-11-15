//! Release artifact layout, pull credentials, and transport integrity.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use wamn_digest::bytes_sha256;
use wamn_package_artifact::release_manifest_artifact::{
    release_manifest_artifact_layout, verify_release_manifest_artifact_layout,
};
use wamn_package_artifact::release_manifest_source::{
    ReleaseManifestFetchErrorType, ReleaseManifestSource,
};

const REGISTRY: &str = "registry.example:5000";
const ARTIFACT_BASE: &str = "registry.example:5000/wamn/releases";
static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

/// A private pull credential for exactly one registry authority, removed on drop.
struct ScratchCredential {
    root: PathBuf,
    path: PathBuf,
}

impl ScratchCredential {
    fn write() -> Self {
        Self::write_for(REGISTRY)
    }

    /// The loader keys on the bare authority, so a stub on an ephemeral port
    /// needs its own entry rather than [`REGISTRY`]'s.
    fn write_for(registry: &str) -> Self {
        let sequence = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "wamn-release-manifest-source-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("scratch credential directory");
        let path = root.join("config.json");
        std::fs::write(
            &path,
            format!(r#"{{"auths":{{"{registry}":{{"username":"puller","password":"secret"}}}}}}"#),
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
    let bytes = br#"{"format-version":1}"#;
    let (_, _, manifest) = release_manifest_artifact_layout(bytes);
    let digest = bytes_sha256(bytes);

    verify_release_manifest_artifact_layout(&manifest, &digest, None)
        .expect("the published layout verifies for a puller");
    verify_release_manifest_artifact_layout(&manifest, &digest, Some(bytes.len()))
        .expect("the published layout verifies with its size");
    for (expected_digest, expected_size) in [
        (digest, Some(bytes.len() + 1)),
        (bytes_sha256(b"another release"), None),
    ] {
        verify_release_manifest_artifact_layout(&manifest, &expected_digest, expected_size)
            .expect_err("a contradictory descriptor refuses");
    }
}

#[test]
fn a_missing_pull_credential_refuses_before_any_transport() {
    let credential = ScratchCredential::write();

    let error = ReleaseManifestSource::new(ARTIFACT_BASE, true, &credential.absent())
        .expect_err("an absent pull credential refuses");

    assert_eq!(
        error.error_type(),
        ReleaseManifestFetchErrorType::Credential
    );
}

#[tokio::test]
#[ignore = "requires: env:WAMN_RELEASE_MANIFEST_ARTIFACT_BASE, env:WAMN_REGISTRY_AUTH_FILE, env:WAMN_RELEASE_MANIFEST_DIGEST"]
async fn a_published_release_pulls_verified_bytes_and_refuses_an_absent_digest() {
    let live_env = |name: &str, expectation: &str| {
        let value = std::env::var(name).unwrap_or_default();
        assert!(
            !value.trim().is_empty(),
            "this test requires {name}: {expectation}"
        );
        value
    };
    let artifact_base = live_env(
        "WAMN_RELEASE_MANIFEST_ARTIFACT_BASE",
        "the explicit <registry>/<repository> the release was pushed to",
    );
    // The loader reads `auths.<bare host:port>.username` and `.password` and has
    // no `auth` field at all, so Docker's base64 form is silently ignored and a
    // scheme-prefixed key is not found. There is also no anonymous path: even an
    // unauthenticated throwaway registry needs a dummy non-empty pair here.
    let registry_auth_file = live_env(
        "WAMN_REGISTRY_AUTH_FILE",
        "a Docker config whose auths key is that registry's bare host:port, carrying \
         explicit non-empty username and password fields",
    );
    let manifest_digest = live_env(
        "WAMN_RELEASE_MANIFEST_DIGEST",
        "the published manifest digest, sha256:<64 lowercase hex>",
    )
    .parse::<wamn_digest::Digest>()
    .expect("published digest");

    let source = ReleaseManifestSource::new(&artifact_base, true, Path::new(&registry_auth_file))
        .expect("the disposable registry configures")
        .with_ca_paths(&[])
        .expect("no extra bundles leaves this source on the compiled-in roots");
    let canonical_bytes = source
        .pull_verified(&manifest_digest)
        .await
        .expect("the published release pulls");

    assert_eq!(bytes_sha256(&canonical_bytes), manifest_digest);

    // A digest this repository does not hold is a refusal, never an empty
    // release — the same posture a pod pointed at a rolled-back tag must take.
    let absent = (format!("sha256:{}", "f".repeat(64)))
        .parse::<wamn_digest::Digest>()
        .unwrap();
    assert_eq!(
        source
            .pull_verified(&absent)
            .await
            .expect_err("a digest the repository does not hold refuses")
            .error_type(),
        ReleaseManifestFetchErrorType::Unavailable
    );
}

/// An in-process HTTP responder that serves exactly the bytes a test hands it.
///
/// This is deliberately not a registry. It exists to answer a manifest whose
/// layer descriptor disagrees with the body it then returns — the one lie a
/// conforming registry structurally cannot tell, because `registry:2` validates
/// blob digests on upload and refuses a manifest naming an absent blob. The
/// refusals below exist for an untrusted transport, so nothing but an untrusted
/// transport can provoke them.
struct LyingRegistry {
    authority: String,
    listening: tokio::task::JoinHandle<()>,
}

impl Drop for LyingRegistry {
    fn drop(&mut self) {
        self.listening.abort();
    }
}

/// Bind an ephemeral port and answer every pull phase from these fixed bytes.
async fn lying_registry(manifest_json: Vec<u8>, body: Vec<u8>) -> LyingRegistry {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral loopback port binds");
    let authority = listener
        .local_addr()
        .expect("the listener reports its assigned port")
        .to_string();
    let listening = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let manifest_json = manifest_json.clone();
            let body = body.clone();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

                let mut head = Vec::new();
                let mut chunk = [0_u8; 1024];
                while !head.ends_with(b"\r\n\r\n") {
                    match stream.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(read) => head.extend_from_slice(&chunk[..read]),
                    }
                }
                let target = String::from_utf8_lossy(&head)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .split(' ')
                    .nth(1)
                    .unwrap_or_default()
                    .to_owned();
                // The `/v2/` probe answers without a `WWW-Authenticate`
                // challenge, which is how `oci-client` decides no token is
                // needed; the credential is still read, so the production
                // configuration path is the one under test.
                let (content_type, payload) = if target.contains("/manifests/") {
                    ("application/vnd.oci.image.manifest.v1+json", manifest_json)
                } else if target.contains("/blobs/") {
                    ("application/octet-stream", body)
                } else {
                    ("application/json", b"{}".to_vec())
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    payload.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.write_all(&payload).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    LyingRegistry {
        authority,
        listening,
    }
}

#[tokio::test]
async fn registry_pulls_accept_published_bytes_and_refuse_contradictions() {
    let canonical = br#"{"format-version":1}"#;
    let digest = bytes_sha256(canonical);
    let (_, _, manifest) = release_manifest_artifact_layout(canonical);
    let published = serde_json::to_vec(&manifest).expect("the layout serializes");
    let mut changed = canonical.to_vec();
    changed[1] = b'F';
    for (manifest, body, expected) in [
        (published.clone(), canonical.to_vec(), None),
        (
            published,
            changed,
            Some(ReleaseManifestFetchErrorType::Mismatched),
        ),
        (
            br#"{"schemaVersion":1}"#.to_vec(),
            Vec::new(),
            Some(ReleaseManifestFetchErrorType::Mismatched),
        ),
    ] {
        let registry = lying_registry(manifest, body).await;
        let credential = ScratchCredential::write_for(&registry.authority);
        let result = ReleaseManifestSource::new(
            &format!("{}/wamn/releases", registry.authority),
            true,
            credential.path(),
        )
        .expect("the source configures")
        .pull_verified(&digest)
        .await;
        match expected {
            None => assert_eq!(
                bytes_sha256(&result.expect("published artifact pulls")),
                digest
            ),
            Some(expected) => assert_eq!(
                result.expect_err("a contradiction refuses").error_type(),
                expected
            ),
        }
    }
}
