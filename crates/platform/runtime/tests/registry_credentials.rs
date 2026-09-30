use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use wamn_runtime::registry_credentials::{RegistryCredentialsErrorKind, read_registry_credentials};

const REGISTRY: &str = "registry.example:5000";
static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

struct ScratchFile {
    root: PathBuf,
    path: PathBuf,
}

impl ScratchFile {
    fn write(contents: &[u8]) -> Self {
        let sequence = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "wamn-registry-credentials-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&root).expect("create isolated credential fixture directory");
        let path = root.join("config.json");
        std::fs::write(&path, contents).expect("write credential fixture");
        Self { root, path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_dir(&self.root);
    }
}

#[test]
fn exact_docker_config_entry_yields_one_redacted_credential() {
    let fixture = ScratchFile::write(
        br#"{
          "auths": {
            "registry.example:5000": {
              "username": "pull-user",
              "password": "pull-secret",
              "auth": "ignored-standard-docker-field"
            }
          }
        }"#,
    );
    let credentials = read_registry_credentials(fixture.path(), REGISTRY)
        .expect("exact registry credential parses");
    assert_eq!(credentials.username(), "pull-user");
    assert_eq!(credentials.password(), "pull-secret");
    let rendered = format!("{credentials:?}");
    assert!(!rendered.contains("pull-user"));
    assert!(!rendered.contains("pull-secret"));
}

#[test]
fn missing_or_partial_registry_entry_refuses_without_fallback() {
    let missing_fixture = ScratchFile::write(
        br#"{"auths":{"https://registry.example:5000":{"username":"user","password":"secret"}}}"#,
    );
    let missing = read_registry_credentials(missing_fixture.path(), REGISTRY)
        .expect_err("scheme-bearing alias must not satisfy exact authority");
    assert_eq!(missing.kind(), RegistryCredentialsErrorKind::Rejected);
    assert_eq!(missing.refusal(), "registry-credentials-not-found");

    let partial_fixture =
        ScratchFile::write(br#"{"auths":{"registry.example:5000":{"username":"user"}}}"#);
    let partial = read_registry_credentials(partial_fixture.path(), REGISTRY)
        .expect_err("half credential must refuse");
    assert_eq!(partial.refusal(), "registry-credentials-incomplete");
}

#[test]
fn malformed_document_does_not_echo_secret_bytes() {
    let fixture =
        ScratchFile::write(br#"{"auths":{"registry.example:5000":{"password":"private-value"}}"#);
    let error = read_registry_credentials(fixture.path(), REGISTRY)
        .expect_err("malformed document must refuse");
    let rendered = format!("{error:?} {error}");
    assert_eq!(error.refusal(), "registry-credentials-malformed");
    assert!(!rendered.contains("private-value"));
}
