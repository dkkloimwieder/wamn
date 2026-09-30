//! Every reader of a registry token asks one metadata server.
//!
//! The token function reads `GCE_METADATA_HOST`, and `docker_credential` finds
//! the helper on `PATH`. Setting either while another thread runs is unsound,
//! so this binary holds exactly one test. It sets both before it starts a
//! runtime, and every leg shares the one loopback server.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use docker_credential::{CredentialRetrievalError, DockerCredential, get_credential_from_reader};
use serde_json::json;
use wamn_catalog::{
    AdmittedComponent, ComponentDeclaration, ComponentOperationDeclaration, ComponentPackageScope,
    ComponentPortDeclaration, normalize_component_fact,
};
use wamn_engine::artifact_source::{ArtifactSource as _, ComponentArtifactFetchErrorType};
use wamn_engine::component_admission::component_digest;
use wamn_runtime::component_artifact_source::{
    ComponentArtifactSource, ComponentArtifactSourceConfig,
};
use wamn_runtime::registry_credentials::{
    METADATA_HOST_ENV, RegistryCredentialsErrorType, read_metadata_registry_credentials,
};
use wamn_runtime::release_manifest_artifact::release_manifest_artifact_layout;
use wamn_runtime::release_manifest_source::{ReleaseManifestFetchErrorType, ReleaseManifestSource};

const REGISTRY: &str = "registry.example:5000";
const TOKEN_PATH: &str = "/computeMetadata/v1/instance/service-accounts/default/token";

type Answers = Vec<(u16, &'static str)>;

/// Answers the `n`th request since the last [`Self::answer`] with its `n`th
/// answer, and every later one with the last, and records each request head.
#[derive(Clone, Default)]
struct Script(Arc<Mutex<(Answers, Vec<String>)>>);

impl Script {
    fn answer(&self, answers: Answers) {
        *self.0.lock().expect("script lock") = (answers, Vec::new());
    }

    fn heads(&self) -> Vec<String> {
        self.0.lock().expect("script lock").1.clone()
    }

    fn next(&self, head: String) -> (u16, &'static str) {
        let mut state = self.0.lock().expect("script lock");
        state.1.push(head);
        let index = (state.1.len() - 1).min(state.0.len() - 1);
        state.0[index]
    }
}

/// Read one request head, up to its blank line.
async fn read_head(stream: &mut tokio::net::TcpStream) -> String {
    use tokio::io::AsyncReadExt as _;

    let mut head = Vec::new();
    let mut chunk = [0_u8; 1024];
    while !head.ends_with(b"\r\n\r\n") {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => head.extend_from_slice(&chunk[..read]),
        }
    }
    String::from_utf8_lossy(&head).into_owned()
}

/// Serve `listener` as the metadata server, one request per connection.
async fn serve(listener: std::net::TcpListener, script: Script) {
    use tokio::io::AsyncWriteExt as _;

    let listener = tokio::net::TcpListener::from_std(listener).expect("the listener converts");
    while let Ok((mut stream, _)) = listener.accept().await {
        let head = read_head(&mut stream).await;
        let (status, body) = script.next(head);
        let response = format!(
            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
    }
}

/// A registry whose `/v2/` probe asks for HTTP Basic and whose pulls serve one
/// fixed manifest and body, recording each request head.
async fn challenging_registry(manifest: Vec<u8>, body: Vec<u8>) -> (String, Script) {
    use tokio::io::AsyncWriteExt as _;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral loopback port binds");
    let authority = listener.local_addr().expect("the port").to_string();
    let heads = Script::default();
    heads.answer(vec![(0, "")]);
    let recorded = heads.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let head = read_head(&mut stream).await;
            let target = head.split(' ').nth(1).unwrap_or_default().to_owned();
            recorded.next(head);
            let (status, content_type, payload) = if target == "/v2/" {
                (
                    "401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"stub\"",
                    "application/json",
                    b"{}".to_vec(),
                )
            } else if target.contains("/manifests/") {
                (
                    "200 OK",
                    "application/vnd.oci.image.manifest.v1+json",
                    manifest.clone(),
                )
            } else {
                ("200 OK", "application/octet-stream", body.clone())
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                payload.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.write_all(&payload).await;
            let _ = stream.shutdown().await;
        }
    });
    (authority, heads)
}

/// The `Authorization` header of each manifest request, in order.
fn manifest_authorizations(heads: &[String]) -> Vec<String> {
    heads
        .iter()
        .filter(|head| {
            head.split(' ')
                .nth(1)
                .is_some_and(|target| target.contains("/manifests/"))
        })
        .map(|head| {
            head.lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .starts_with("authorization:")
                        .then(|| line["authorization:".len()..].trim().to_owned())
                })
                .unwrap_or_default()
        })
        .collect()
}

fn basic(token: &str) -> String {
    let pair = format!("oauth2accesstoken:{token}");
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(pair)
    )
}

fn admitted(bytes: &[u8]) -> AdmittedComponent {
    normalize_component_fact(
        ComponentDeclaration {
            scope: ComponentPackageScope {
                tenant_id: "tenant-a".to_owned(),
                package_id: "orders".to_owned(),
                package_version: "1.0.0".to_owned(),
            },
            component: "transform".to_owned(),
            interface_version: "0.1.0".to_owned(),
            operations: BTreeMap::from([(
                "run".to_owned(),
                ComponentOperationDeclaration {
                    pre_commit: None,
                    pre_commit_required: false,
                    committed_result_schema: None,
                    fresh_only: false,
                    registered_operation: None,
                    dependencies: Vec::new(),
                    input_ports: vec![ComponentPortDeclaration {
                        name: "input".to_owned(),
                        schema: json!({}),
                    }],
                    output_ports: Vec::new(),
                    parameters: Vec::new(),
                },
            )]),
            connections: Vec::new(),
        },
        component_digest(bytes),
        ["wasi:logging/logging@0.1.0".to_owned()],
        Vec::new(),
    )
    .expect("fixture admits")
    .component
}

#[test]
fn every_token_reader_asks_the_one_metadata_server() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback port binds");
    listener
        .set_nonblocking(true)
        .expect("the listener goes non-blocking");
    let host = listener.local_addr().expect("the port").to_string();
    let helper_directory = std::path::Path::new(env!("CARGO_BIN_EXE_docker-credential-wamn"))
        .parent()
        .expect("the helper has a directory")
        .to_owned();
    let path = std::env::join_paths(std::iter::once(helper_directory).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .expect("PATH joins");
    // SAFETY: this binary's only test sets these before it starts any thread
    // of its own, and libtest runs no other test beside it.
    unsafe {
        std::env::set_var(METADATA_HOST_ENV, &host);
        std::env::set_var("PATH", path);
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("the runtime starts");
    runtime.block_on(async move {
        let script = Script::default();
        tokio::spawn(serve(listener, script.clone()));

        the_token_is_the_password_of_oauth2accesstoken(&script).await;
        an_unusable_answer_refuses_by_name_without_the_token(&script).await;
        each_release_pull_asks_again_and_sends_its_token(&script).await;
        a_failed_token_request_refuses_before_any_registry_request(&script).await;
        each_component_pull_asks_again(&script).await;
        the_helper_answers_with_the_same_token_function(&script).await;
    });
}

async fn the_token_is_the_password_of_oauth2accesstoken(script: &Script) {
    script.answer(vec![(
        200,
        r#"{"access_token":"token-1","expires_in":3599,"token_type":"Bearer"}"#,
    )]);
    let credentials = read_metadata_registry_credentials(REGISTRY)
        .await
        .expect("a token answer is a credential");
    assert_eq!(credentials.username(), "oauth2accesstoken");
    assert_eq!(credentials.password(), "token-1");

    let heads = script.heads();
    assert_eq!(heads.len(), 1);
    assert!(heads[0].starts_with(&format!("GET {TOKEN_PATH} HTTP/1.1\r\n")));
    assert!(
        heads[0]
            .to_ascii_lowercase()
            .contains("\r\nmetadata-flavor: google\r\n"),
        "the request names the metadata flavor: {}",
        heads[0]
    );
}

async fn an_unusable_answer_refuses_by_name_without_the_token(script: &Script) {
    for (status, body, kind, refusal) in [
        (
            500,
            "{}",
            RegistryCredentialsErrorType::Unreadable,
            "registry-token-metadata-unavailable",
        ),
        (
            200,
            r#"{"token":"private-value""#,
            RegistryCredentialsErrorType::Rejected,
            "registry-token-metadata-malformed",
        ),
        (
            200,
            r#"{"access_token":""}"#,
            RegistryCredentialsErrorType::Rejected,
            "registry-token-metadata-incomplete",
        ),
    ] {
        script.answer(vec![(status, body)]);
        let error = read_metadata_registry_credentials(REGISTRY)
            .await
            .expect_err("an unusable answer refuses");
        assert_eq!((error.kind(), error.refusal()), (kind, refusal));
        let rendered = format!("{error:?} {error}");
        assert!(rendered.contains(REGISTRY));
        assert!(!rendered.contains("private-value"));
    }
}

async fn each_release_pull_asks_again_and_sends_its_token(script: &Script) {
    let canonical = br#"{"format-version":1}"#;
    let (layer, _, manifest) = release_manifest_artifact_layout(canonical);
    let (authority, registry) = challenging_registry(
        serde_json::to_vec(&manifest).expect("the layout serializes"),
        canonical.to_vec(),
    )
    .await;
    script.answer(vec![
        (200, r#"{"access_token":"token-1"}"#),
        (200, r#"{"access_token":"token-2"}"#),
    ]);
    let source = ReleaseManifestSource::with_registry_token_metadata(
        &format!("{authority}/wamn/releases"),
        true,
    )
    .expect("an explicit base configures");

    for _ in 0..2 {
        let pulled = source
            .pull_verified(&layer.sha256_digest())
            .await
            .expect("a pull with a metadata token succeeds");
        assert_eq!(pulled, canonical);
    }
    assert_eq!(script.heads().len(), 2, "one token request per pull");
    assert_eq!(
        manifest_authorizations(&registry.heads()),
        vec![basic("token-1"), basic("token-2")]
    );
}

async fn a_failed_token_request_refuses_before_any_registry_request(script: &Script) {
    let canonical = br#"{"format-version":1}"#;
    let (layer, _, manifest) = release_manifest_artifact_layout(canonical);
    let (authority, registry) = challenging_registry(
        serde_json::to_vec(&manifest).expect("the layout serializes"),
        canonical.to_vec(),
    )
    .await;
    script.answer(vec![(503, "{}")]);
    let error = ReleaseManifestSource::with_registry_token_metadata(
        &format!("{authority}/wamn/releases"),
        true,
    )
    .expect("an explicit base configures")
    .pull_verified(&layer.sha256_digest())
    .await
    .expect_err("a pull without a token refuses");
    assert_eq!(error.kind(), ReleaseManifestFetchErrorType::Credential);
    assert_eq!(error.refusal(), "registry-token-metadata-unavailable");
    assert!(registry.heads().is_empty());
}

async fn each_component_pull_asks_again(script: &Script) {
    script.answer(vec![(200, r#"{"access_token":"token"}"#)]);
    // Port 9 refuses, so each pull stops at the registry after its token.
    let config = ComponentArtifactSourceConfig::new(
        "127.0.0.1:9/wamn/components",
        true,
        std::time::Duration::from_secs(5),
    )
    .expect("source config validates")
    .with_registry_token_metadata();
    let source = ComponentArtifactSource::new(config).expect("registry client builds");
    let component = admitted(b"component-bytes");
    for pulls in 1..=2 {
        let error = source
            .pull_verified(&component)
            .await
            .expect_err("the refusing registry refuses the pull");
        assert_eq!(error.refusal(), "component-artifact-manifest-unavailable");
        assert_eq!(script.heads().len(), pulls);
    }

    script.answer(vec![(503, "{}")]);
    let error = source
        .pull_verified(&component)
        .await
        .expect_err("a pull without a token refuses");
    assert_eq!(error.kind(), ComponentArtifactFetchErrorType::Unavailable);
    assert_eq!(error.refusal(), "registry-token-metadata-unavailable");
}

async fn the_helper_answers_with_the_same_token_function(script: &Script) {
    const CONFIG: &[u8] = br#"{"credHelpers":{"registry.example:5000":"wamn"}}"#;
    // `docker_credential` runs the helper and blocks on its answer, so it runs
    // off the runtime's workers, which keep serving the metadata server.
    let lookup = || tokio::task::spawn_blocking(|| get_credential_from_reader(CONFIG, REGISTRY));

    script.answer(vec![(200, r#"{"access_token":"token-3"}"#)]);
    let credential = lookup().await.expect("the lookup finishes");
    assert_eq!(
        credential.expect("the helper answers"),
        DockerCredential::UsernamePassword("oauth2accesstoken".to_owned(), "token-3".to_owned())
    );
    assert_eq!(script.heads().len(), 1);

    script.answer(vec![(500, "{}")]);
    match lookup().await.expect("the lookup finishes") {
        Err(CredentialRetrievalError::HelperFailure { stderr, .. }) => {
            assert!(
                stderr.contains("registry-token-metadata-unavailable"),
                "the helper names the refusal: {stderr}"
            );
        }
        other => panic!("a failed token request fails the helper: {other:?}"),
    }
}
