use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use wamn_runtime::registry_credentials::{
    RegistryCredentialsErrorType, read_registry_credentials, read_registry_push_credentials,
};

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
    assert_eq!(missing.error_type(), RegistryCredentialsErrorType::Rejected);
    assert_eq!(missing.refusal(), "registry-credentials-not-found");

    let partial_fixture =
        ScratchFile::write(br#"{"auths":{"registry.example:5000":{"username":"user"}}}"#);
    let partial = read_registry_credentials(partial_fixture.path(), REGISTRY)
        .expect_err("half credential must refuse");
    assert_eq!(partial.refusal(), "registry-credentials-incomplete");
}

#[test]
fn only_an_explicit_empty_entry_states_an_anonymous_push() {
    let empty = ScratchFile::write(br#"{"auths":{"registry.example:5000":{}}}"#);
    assert!(
        read_registry_push_credentials(empty.path(), REGISTRY)
            .expect("an empty entry is an anonymous push")
            .is_none()
    );
    let pull = read_registry_credentials(empty.path(), REGISTRY)
        .expect_err("a pull never reads an empty entry as anonymous");
    assert_eq!(pull.refusal(), "registry-credentials-incomplete");

    let missing = ScratchFile::write(br#"{"auths":{}}"#);
    let refused = read_registry_push_credentials(missing.path(), REGISTRY)
        .expect_err("a missing entry is not an anonymous push");
    assert_eq!(refused.refusal(), "registry-credentials-not-found");

    let other_field = ScratchFile::write(br#"{"auths":{"registry.example:5000":{"auth":"x"}}}"#);
    let refused = read_registry_push_credentials(other_field.path(), REGISTRY)
        .expect_err("an entry with a field is not empty");
    assert_eq!(refused.refusal(), "registry-credentials-incomplete");

    let complete = ScratchFile::write(
        br#"{"auths":{"registry.example:5000":{"username":"push-user","password":"push-secret"}}}"#,
    );
    let credentials = read_registry_push_credentials(complete.path(), REGISTRY)
        .expect("a complete entry parses")
        .expect("a complete entry is a credential");
    assert_eq!(credentials.username(), "push-user");
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
