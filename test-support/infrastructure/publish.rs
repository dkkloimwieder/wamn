//! Registry, web artifact and package manifest inputs shared by the publish,
//! OCI and environment tests.

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::scratch::ScratchRoot;

/// The compiled manifest of the one-model `source_fixture` package.
pub const SOURCE_MANIFEST: &str = include_str!("publish/source-manifest.json");

/// The canonical `web.json` of the `web_fixture` client.
pub const WEB_MANIFEST: &[u8] = include_bytes!("publish/web.json");

/// [`SOURCE_MANIFEST`], read as the package manifest that publish admits.
pub fn source_manifest() -> wamn_component_policy::PackageManifest {
    serde_json::from_str(SOURCE_MANIFEST).expect("the fixture source manifest parses")
}

/// Write a standalone web build, `dist/` and `web.json`, under `root`.
///
/// `reversed` writes the files in the other order, with other times and
/// modes, so that packing two builds compares only their contents.
pub fn artifact_fixture(root: &Path, reversed: bool) {
    fs::create_dir_all(root.join("dist/assets")).unwrap();
    let mut files = vec![
        ("dist/index.html", b"index".as_slice()),
        ("dist/assets/app.js", b"script"),
    ];
    if reversed {
        files.reverse();
    }
    for (name, bytes) in files {
        let path = root.join(name);
        fs::write(&path, bytes).unwrap();
        fs::File::open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(
                SystemTime::UNIX_EPOCH + Duration::from_secs(if reversed { 900 } else { 100 }),
            ))
            .unwrap();
        fs::set_permissions(
            &path,
            fs::Permissions::from_mode(if reversed { 0o755 } else { 0o600 }),
        )
        .unwrap();
    }
    fs::write(root.join("web.json"), WEB_MANIFEST).unwrap();
}

/// The package registry configuration for the `web` repository of `server`,
/// with a registry auth file under `scratch`.
///
/// The caller's type is the product's registry configuration, read through
/// its own deserializer.
pub fn fixture_registry<R: serde::de::DeserializeOwned>(
    scratch: &ScratchRoot,
    server: &Server,
) -> R {
    let registry = server.reference.registry();
    let auth = scratch.path().join("registry-auth.json");
    fs::write(
        &auth,
        serde_json::to_vec(&serde_json::json!({"auths": {registry: {
            "username": "fixture", "password": "fixture"
        }}}))
        .unwrap(),
    )
    .unwrap();
    serde_json::from_value(serde_json::json!({
        "artifact_base": format!("{registry}/web"),
        "registry_auth_file": auth,
        "insecure_registry": true,
        "oci_ca_paths": [],
    }))
    .expect("the fixture registry configuration parses")
}

/// A loopback registry that answers `GET /v2/` and then each further request
/// with the next scripted response, repeating the last one.
#[derive(Debug)]
pub struct Server {
    pub reference: oci_client::Reference,
    requests: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Server {
    pub async fn start(responses: Vec<(u16, Vec<u8>)>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    if socket.read_exact(&mut byte).await.is_err() {
                        break;
                    }
                    request.push(byte[0]);
                }
                let (status, body) = if request.starts_with(b"GET /v2/ HTTP/") {
                    (200, &b""[..])
                } else {
                    let index = count.fetch_add(1, Ordering::SeqCst);
                    let (status, body) = &responses[index.min(responses.len() - 1)];
                    (*status, body.as_slice())
                };
                let header = format!(
                    "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nContent-Type: application/vnd.oci.image.manifest.v1+json\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                if socket.write_all(header.as_bytes()).await.is_ok() {
                    let _ = socket.write_all(body).await;
                }
            }
        });
        Self {
            reference: format!("{address}/components:fixture").parse().unwrap(),
            requests,
            task,
        }
    }

    /// The scripted requests answered so far, not counting `GET /v2/`.
    pub fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
