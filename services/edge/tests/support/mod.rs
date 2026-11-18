//! The release bundle of `apps/edge_device` that the edge tests share.
//!
//! Each test binary builds the bundle once. It reads the `edge_device`
//! component from the package's build output, so run
//! `wamn build apps/edge_device` first. `WAMN_INGRESS_COMPONENT` supplies the
//! prebuilt route guest, which must match its pin. `write_package` admits the
//! component and writes the bundle.

#![allow(
    dead_code,
    reason = "each test binary uses part of the shared fixtures"
)]

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use wamn_catalog::edge_bundle::BUNDLE_FILE_NAME;
use wamn_dev::dev::edge_bundle::{EdgeTarget, write_package};
use wamn_digest::bytes_sha256;
use wamn_edge::config::{EdgeConfig, HttpConfig, ReleaseConfig, StoreConfig, TargetConfig};
use wamn_edge::serve::{EdgeHost, serve};

pub const PACKAGE: &str = "edge_device";
/// A registered export is keyed by its permission identity.
pub const OPERATION: &str = "edge-device:sample/read@2.1.0";
pub const ATTACHMENT: &str = "sample-read-http";
/// The built-in role that holds every served operation.
pub const ROLE: &str = "admin";
/// The route host of the release.
pub const HOST: &str = "receiving.localhost";
pub const PATH: &str = "/sample/read";
pub const ORG: &str = "org-a";
pub const PRINCIPAL: &str = "0b5c7f3e-9a41-4d2e-8f6a-1c2d3e4f5a6b";
/// The project and environment of the release.
pub const PROJECT: &str = "plant";
pub const ENVIRONMENT: &str = "edge";

/// Copy the bundle into a directory named for the test and
/// return the directory with the bundle digest. The directory lies under the
/// system temporary directory, not the target directory, so the edge's
/// `status.sock` in it stays within the Unix socket path limit (wamn-xvk3e).
pub fn bundle(test: &str) -> (PathBuf, wamn_digest::Digest) {
    static SOURCE: OnceLock<PathBuf> = OnceLock::new();
    let source = SOURCE.get_or_init(build_bundle);
    let directory = std::env::temp_dir().join(format!("edge-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).expect("create the bundle directory");
    for entry in std::fs::read_dir(source).expect("read the bundle directory") {
        let entry = entry.expect("read a bundle entry");
        std::fs::copy(entry.path(), directory.join(entry.file_name())).expect("copy a bundle file");
    }
    let bundle = std::fs::read(directory.join(BUNDLE_FILE_NAME)).expect("read the bundle");
    (directory, bytes_sha256(&bundle))
}

/// Write the bundle from the prepared application and pinned route guest.
fn build_bundle() -> PathBuf {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let package = repository.join("apps/edge_device");
    let ingress = PathBuf::from(
        std::env::var_os("WAMN_INGRESS_COMPONENT")
            .expect("set WAMN_INGRESS_COMPONENT to the pinned route guest"),
    );
    let pin = std::fs::read_to_string(repository.join("guests/ingress/ingress.wasm.sha256"))
        .expect("read the ingress pin");
    let ingress_digest = bytes_sha256(&std::fs::read(&ingress).expect("read the route guest"));
    assert_eq!(
        Some(ingress_digest.hex().as_str()),
        pin.split_whitespace().next(),
        "the ingress guest matches its pin"
    );
    let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("edge-device-bundle-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    write_package(
        &package,
        &"device".parse().unwrap(),
        &repository.join("apps/target/wamn/edge_device/components/device.wasm"),
        &ingress,
        &EdgeTarget {
            org: &ORG.parse().unwrap(),
            project: &PROJECT.parse().unwrap(),
            environment: &ENVIRONMENT.parse().unwrap(),
        },
        &out,
    )
    .expect("write the edge_device bundle");
    out
}

/// The configuration of the edge over the bundle in `directory`, with no
/// device.
pub fn config(directory: &Path, digest: wamn_digest::Digest) -> EdgeConfig {
    EdgeConfig {
        target: TargetConfig {
            org: ORG.parse().unwrap(),
            project: PROJECT.parse().unwrap(),
            environment: ENVIRONMENT.parse().unwrap(),
        },
        release: ReleaseConfig {
            dir: directory.to_owned(),
            digest,
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
