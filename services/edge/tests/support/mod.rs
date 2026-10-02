//! The release bundle of `apps/edge_device` and the session keys that the
//! edge tests share.
//!
//! Each test binary builds the bundle once. `tools/build-components` builds
//! the `edge_device` component, and Cargo builds the http-route guest as the
//! Dockerfile does, which must match its pin. `write_package`, the dev loop's
//! bundle writer, admits the component and writes the bundle.

#![allow(
    dead_code,
    reason = "each test binary uses part of the shared fixtures"
)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use serde_json::json;
use wamn_catalog::edge_bundle::{BUNDLE_FILE_NAME, file_digest};
use wamn_catalog::{RELEASE_MANIFEST_FILE_NAME, ServingManifest};
use wamn_control::dev::edge_bundle::{PackageRelease, write_package};
use wamn_edge::config::{EdgeConfig, HttpConfig, ReleaseConfig, SessionConfig, StoreConfig};
use wamn_edge::serve::{EdgeHost, serve};
use wamn_session::keys::PublicSessionKey;
use wamn_session::token::{SessionAuthority, SessionClaims, SessionHeader};

pub const PACKAGE: &str = "edge_device";
/// A registered export is keyed by its permission identity.
pub const OPERATION: &str = "edge-device:sample/read@2.1.0";
pub const ATTACHMENT: &str = "sample-read-http";
/// The built-in role that holds every served operation.
pub const ROLE: &str = "admin";
/// The route host of the release.
pub const HOST: &str = "receiving.localhost";
pub const PATH: &str = "/sample/read";
pub const ISSUER: &str = "https://identity.example.test";
pub const ORG: &str = "org-a";
pub const AUDIENCE: &str = "tenant-a/edge";
pub const PRINCIPAL: &str = "0b5c7f3e-9a41-4d2e-8f6a-1c2d3e4f5a6b";
/// The tenant and environment of the release.
const TENANT: &str = "tenant-a";
const ENVIRONMENT: &str = "edge";

/// A key pair from a fixed seed, and its public JWK under `kid`.
pub fn key(kid: &str, seed: u8) -> (Ed25519KeyPair, PublicSessionKey) {
    let pair = Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).expect("seed");
    let public = PublicSessionKey {
        kid: kid.into(),
        kty: "OKP".into(),
        crv: "Ed25519".into(),
        alg: "Ed25519".into(),
        r#use: "sig".into(),
        x: URL_SAFE_NO_PAD.encode(pair.public_key().as_ref()),
    };
    (pair, public)
}

/// A session for `roles`, signed by `pair` under `kid`.
pub fn session(pair: &Ed25519KeyPair, kid: &str, roles: &[&str]) -> String {
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_secs(),
    )
    .expect("the clock fits in i64");
    let claims = SessionClaims {
        iss: ISSUER.into(),
        sub: PRINCIPAL.into(),
        org: ORG.into(),
        aud: AUDIENCE.into(),
        roles: roles.iter().map(|role| (*role).to_owned()).collect(),
        exp: now + 300,
        iat: now - 10,
        jti: "token-1".into(),
        authority: SessionAuthority::Login(PRINCIPAL.into()),
        csrf: None,
    };
    let header =
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&SessionHeader::new(kid)).expect("header"));
    let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).expect("claims"));
    let signed = format!("{header}.{body}");
    let signature = URL_SAFE_NO_PAD.encode(pair.sign(signed.as_bytes()).as_ref());
    format!("{signed}.{signature}")
}

/// Copy the bundle into a directory named for the test, add the key file, and
/// return the directory with the bundle digest.
pub fn bundle(test: &str, public: &PublicSessionKey) -> (PathBuf, String) {
    static SOURCE: OnceLock<PathBuf> = OnceLock::new();
    let source = SOURCE.get_or_init(build_bundle);
    let directory = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("edge-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("create the bundle directory");
    for entry in std::fs::read_dir(source).expect("read the bundle directory") {
        let entry = entry.expect("read a bundle entry");
        std::fs::copy(entry.path(), directory.join(entry.file_name())).expect("copy a bundle file");
    }
    std::fs::write(
        directory.join("session-keys.json"),
        serde_json::to_vec(&json!({"keys": [public]})).expect("keys"),
    )
    .expect("write the key file");
    let bundle = std::fs::read(directory.join(BUNDLE_FILE_NAME)).expect("read the bundle");
    (directory, file_digest(&bundle))
}

/// Build the component and the route guest, and write the bundle of
/// `apps/edge_device` into a new directory of this test binary.
fn build_bundle() -> PathBuf {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let package = repository.join("apps/edge_device");
    run(Command::new(repository.join("tools/build-components"))
        .current_dir(&repository)
        .args(["app", "apps/edge_device"]));
    let flags = run(&mut Command::new(repository.join("tools/guest-rustflags")));
    run(Command::new(env!("CARGO"))
        .current_dir(repository.join("apps"))
        .env("RUSTFLAGS", String::from_utf8(flags).expect("flags").trim())
        .args(["build", "--locked", "--offline", "--release"])
        .args(["--target", "wasm32-wasip2", "-p", "http-route"]));
    let ingress = repository.join("apps/target/wasm32-wasip2/release/http_route.wasm");
    let pin = std::fs::read_to_string(
        repository.join("apps/platform/ingress/http-route/http_route.wasm.sha256"),
    )
    .expect("read the http-route pin");
    let ingress_digest = file_digest(&std::fs::read(&ingress).expect("read the route guest"));
    assert_eq!(
        Some(&ingress_digest["sha256:".len()..]),
        pin.split_whitespace().next(),
        "the http-route guest matches its pin"
    );
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("edge-device-bundle-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    write_package(
        &package,
        "device",
        &repository.join("apps/target/virtualized/std-empty-environment/device.wasm"),
        &ingress,
        &PackageRelease {
            tenant: TENANT,
            environment: ENVIRONMENT,
            publisher: PRINCIPAL,
            route_host: HOST,
        },
        &out,
    )
    .expect("write the edge_device bundle");
    out
}

/// Run `command` and return its standard output, or fail the test with its
/// standard error.
fn run(command: &mut Command) -> Vec<u8> {
    let output = command.output().expect("start the build command");
    assert!(
        output.status.success(),
        "{command:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

/// The digest of the release in the bundle at `directory`, as intents name it.
pub fn release(directory: &Path) -> String {
    let bytes =
        std::fs::read(directory.join(RELEASE_MANIFEST_FILE_NAME)).expect("read the manifest");
    ServingManifest::from_canonical_bytes(&bytes)
        .expect("the bundle manifest is canonical")
        .1
        .to_string()
}

/// The configuration of the edge over the bundle in `directory`, with no
/// device.
pub fn config(directory: &Path, digest: String) -> EdgeConfig {
    EdgeConfig {
        release: ReleaseConfig {
            dir: directory.to_owned(),
            digest,
        },
        session: SessionConfig {
            keys: directory.join("session-keys.json"),
            issuer: ISSUER.into(),
            org: ORG.into(),
            audience: AUDIENCE.into(),
        },
        store: StoreConfig {
            db: directory.join("edge.db"),
        },
        http: HttpConfig {
            route_host: HOST.into(),
            listen: "127.0.0.1:0".parse().expect("address"),
        },
        device: None,
        forward: None,
    }
}

pub async fn start(config: EdgeConfig) -> EdgeHost {
    Box::pin(serve(config))
        .await
        .expect("the edge serves its bundle")
}
