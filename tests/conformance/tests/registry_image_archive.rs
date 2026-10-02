//! `tools/registry-image-archive` and `tools/journey-image-cache` in kind,
//! against a private registry that requires a login
//! (docs/plan/upgrade-environment.md §4.4).
//!
//! The cache tool builds and relabels an image, which the test pushes to the
//! registry. The archive tool fetches it and a guest by digest with the login
//! of this machine. Each kind node imports the image archive. CRI must report
//! the tag and the pinned digest, and no node may hold the credential.

use std::fs;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

const USER: &str = "wamn-test";
/// The smallest valid component bytes: the Wasm magic and version.
const GUEST_LAYER: &[u8] = b"\0asm\x01\0\0\0";

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repository root")
        .to_path_buf()
}

fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0; bytes];
    fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut buffer))
        .expect("read random bytes");
    hex::encode(buffer)
}

fn sha256(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn private_directory(path: &Path) {
    fs::create_dir_all(path).expect("create private directory");
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).expect("set directory mode");
}

/// Runs a command with optional standard input and returns its output.
/// Arguments never carry the password, so a failure message may show them.
fn run(command: &mut Command, input: Option<&[u8]>) -> Output {
    let description = format!("{command:?}");
    let mut child = command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("start {description}: {error}"));
    if let Some(input) = input {
        child
            .stdin
            .take()
            .expect("standard input")
            .write_all(input)
            .expect("write standard input");
    }
    child
        .wait_with_output()
        .unwrap_or_else(|error| panic!("wait for {description}: {error}"))
}

fn succeed(command: &mut Command, input: Option<&[u8]>) -> String {
    let description = format!("{command:?}");
    let output = run(command, input);
    assert!(
        output.status.success(),
        "{description} exited {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8 output")
}

/// Every resource that the test creates, removed by its exact name.
struct Owned {
    work: PathBuf,
    cluster: String,
    registry: String,
    images: Vec<String>,
    cluster_created: bool,
    registry_created: bool,
}

impl Drop for Owned {
    fn drop(&mut self) {
        if self.cluster_created {
            let _ = Command::new("kind")
                .args(["delete", "cluster", "--name", &self.cluster, "--kubeconfig"])
                .arg(self.work.join("kubeconfig"))
                .status();
        }
        if self.registry_created {
            let _ = Command::new("docker")
                .args(["rm", "--force", "--volumes", &self.registry])
                .status();
        }
        for image in &self.images {
            let _ = Command::new("docker").args(["image", "rm", image]).status();
        }
        let _ = fs::remove_dir_all(&self.work);
    }
}

/// Uploads one blob to the registry with the login in `login`.
fn push_blob(login: &Path, ca: &Path, base: &str, repository: &str, bytes: &[u8]) -> String {
    let digest = sha256(bytes);
    let headers = succeed(
        Command::new("curl")
            .args(["-sSf", "--cacert"])
            .arg(ca)
            .arg("-K")
            .arg(login)
            .args(["-X", "POST", "-D", "-", "-o", "/dev/null"])
            .arg(format!("{base}/v2/{repository}/blobs/uploads/")),
        None,
    );
    let location = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("location")
                .then(|| value.trim().to_owned())
        })
        .expect("upload location");
    let location = if location.starts_with('/') {
        format!("{base}{location}")
    } else {
        location
    };
    let separator = if location.contains('?') { '&' } else { '?' };
    succeed(
        Command::new("curl")
            .args(["-sSf", "--cacert"])
            .arg(ca)
            .arg("-K")
            .arg(login)
            .args([
                "-X",
                "PUT",
                "-H",
                "Content-Type: application/octet-stream",
                "--data-binary",
                "@-",
                "-o",
                "/dev/null",
            ])
            .arg(format!("{location}{separator}digest={digest}")),
        Some(bytes),
    );
    digest
}

/// Extracts an archive and makes sure that every blob matches its name.
fn archive_index(archive: &Path, directory: &Path) -> Value {
    private_directory(directory);
    succeed(
        Command::new("tar")
            .arg("-xf")
            .arg(archive)
            .arg("-C")
            .arg(directory),
        None,
    );
    for entry in fs::read_dir(directory.join("blobs/sha256")).expect("list blobs") {
        let path = entry.expect("blob entry").path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("blob name");
        assert_eq!(
            sha256(&fs::read(&path).expect("read blob")),
            format!("sha256:{name}"),
            "a blob of {} does not match its name",
            archive.display()
        );
    }
    serde_json::from_slice(&fs::read(directory.join("index.json")).expect("read index"))
        .expect("parse index")
}

fn names(index: &Value, digest: &str) -> Vec<String> {
    index["manifests"]
        .as_array()
        .expect("index manifests")
        .iter()
        .map(|manifest| {
            assert_eq!(
                manifest["digest"], digest,
                "an index entry names another digest"
            );
            manifest["annotations"]["io.containerd.image.name"]
                .as_str()
                .expect("image name")
                .to_owned()
        })
        .collect()
}

#[test]
#[ignore = "requires: docker, kind, curl, jq, openssl, tar"]
fn kind_nodes_import_the_pinned_digest_and_hold_no_credential() {
    wamn_test_postgres::require_prerequisites(&["docker", "kind", "curl", "jq", "openssl", "tar"]);
    let root = repository_root();
    let nonce = random_hex(8);
    let cluster = format!("wamn-registry-archive-{nonce}");
    let mut owned = Owned {
        work: std::env::temp_dir().join(&cluster),
        registry: format!("{cluster}-registry"),
        cluster,
        images: Vec::new(),
        cluster_created: false,
        registry_created: false,
    };
    let work = owned.work.clone();
    private_directory(&work);
    let password = random_hex(16);

    // A registry with TLS and a login, on a loopback port of this machine.
    private_directory(&work.join("auth"));
    let htpasswd = succeed(
        Command::new("docker").args([
            "run",
            "--rm",
            "-i",
            "--entrypoint",
            "htpasswd",
            "httpd:2-alpine",
            "-Bni",
            USER,
        ]),
        Some(password.as_bytes()),
    );
    fs::write(work.join("auth/htpasswd"), htpasswd).expect("write htpasswd");
    private_directory(&work.join("certs"));
    let ca = work.join("certs/cert.pem");
    succeed(
        Command::new("openssl")
            .args(["req", "-x509", "-newkey", "ec", "-pkeyopt"])
            .args(["ec_paramgen_curve:prime256v1", "-nodes", "-days", "1"])
            .args(["-subj", "/CN=localhost", "-addext"])
            .arg("subjectAltName=DNS:localhost,IP:127.0.0.1")
            .arg("-keyout")
            .arg(work.join("certs/key.pem"))
            .arg("-out")
            .arg(&ca),
        None,
    );
    owned.registry_created = true;
    succeed(
        Command::new("docker")
            .args(["run", "--detach", "--name", &owned.registry])
            .args(["--publish", "127.0.0.1::5000"])
            .args(["--env", "REGISTRY_AUTH=htpasswd"])
            .args([
                "--env",
                "REGISTRY_AUTH_HTPASSWD_REALM=registry archive test",
            ])
            .args(["--env", "REGISTRY_AUTH_HTPASSWD_PATH=/auth/htpasswd"])
            .args(["--env", "REGISTRY_HTTP_TLS_CERTIFICATE=/certs/cert.pem"])
            .args(["--env", "REGISTRY_HTTP_TLS_KEY=/certs/key.pem"])
            .arg("--volume")
            .arg(format!("{}:/auth:ro", work.join("auth").display()))
            .arg("--volume")
            .arg(format!("{}:/certs:ro", work.join("certs").display()))
            .arg("registry:2"),
        None,
    );
    let port = succeed(
        Command::new("docker").args([
            "inspect",
            "--format",
            r#"{{(index (index .NetworkSettings.Ports "5000/tcp") 0).HostPort}}"#,
            &owned.registry,
        ]),
        None,
    )
    .trim()
    .to_owned();
    let host = format!("localhost:{port}");
    let base = format!("https://{host}");
    let status = succeed(
        Command::new("curl")
            .args([
                "-sS",
                "--retry",
                "30",
                "--retry-delay",
                "1",
                "--retry-connrefused",
            ])
            .arg("--cacert")
            .arg(&ca)
            .args(["-o", "/dev/null", "-w", "%{http_code}"])
            .arg(format!("{base}/v2/")),
        None,
    );
    assert_eq!(status, "401", "the registry must require a login");

    // The login of this machine, in a private Docker configuration.
    let docker_config = work.join("docker");
    private_directory(&docker_config);
    succeed(
        Command::new("docker")
            .env("DOCKER_CONFIG", &docker_config)
            .args(["login", &host, "--username", USER, "--password-stdin"]),
        Some(password.as_bytes()),
    );
    let login = work.join("curl-login");
    fs::write(&login, format!("user = \"{USER}:{password}\"\n")).expect("write curl login");
    fs::set_permissions(&login, fs::Permissions::from_mode(0o600)).expect("set login mode");

    // The cache tool builds the image by its content and relabels it for the run.
    let head = succeed(
        Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["rev-parse", "HEAD"]),
        None,
    )
    .trim()
    .to_owned();
    let context = work.join("context");
    private_directory(&context);
    fs::write(
        context.join("Dockerfile"),
        "FROM scratch\nCOPY marker /marker\n",
    )
    .expect("write Dockerfile");
    fs::write(context.join("marker"), &nonce).expect("write marker");
    let name = format!("registry-archive-{nonce}");
    let built = succeed(
        Command::new(root.join("tools/journey-image-cache"))
            .env("XDG_CACHE_HOME", work.join("cache"))
            .arg("ensure-context")
            .arg(&context)
            .args([&name, &head, "run", &owned.cluster, "debug"]),
        None,
    )
    .trim()
    .to_owned();
    owned.images.push(built.clone());
    let relabeled = format!("wamn-{name}:run");
    owned.images.push(relabeled.clone());
    let repository = format!("wamn-{name}");
    let tagged = format!("{host}/{repository}:run");
    succeed(
        Command::new("docker").args(["tag", &relabeled, &tagged]),
        None,
    );
    owned.images.push(tagged.clone());
    succeed(
        Command::new("docker")
            .env("DOCKER_CONFIG", &docker_config)
            .args(["push", &tagged]),
        None,
    );
    let image_digest = succeed(
        Command::new("docker").args([
            "image",
            "inspect",
            "--format",
            "{{range .RepoDigests}}{{println .}}{{end}}",
            &tagged,
        ]),
        None,
    )
    .lines()
    .find_map(|line| {
        line.strip_prefix(&format!("{host}/{repository}@"))
            .map(str::to_owned)
    })
    .expect("pushed image digest");
    let image = format!("{tagged}@{image_digest}");
    let pinned = format!("{host}/{repository}@{image_digest}");

    // A guest pushed by digest only, as a component artifact.
    let guest_repository = format!("guest-{nonce}");
    let config = b"{}";
    let config_digest = push_blob(&login, &ca, &base, &guest_repository, config);
    let layer_digest = push_blob(&login, &ca, &base, &guest_repository, GUEST_LAYER);
    let manifest = serde_json::to_vec(&json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": "application/vnd.wasm.config.v0+json",
            "digest": config_digest,
            "size": config.len(),
        },
        "layers": [{
            "mediaType": "application/wasm",
            "digest": layer_digest,
            "size": GUEST_LAYER.len(),
        }],
    }))
    .expect("encode guest manifest");
    let guest_digest = sha256(&manifest);
    succeed(
        Command::new("curl")
            .args(["-sSf", "--cacert"])
            .arg(&ca)
            .arg("-K")
            .arg(&login)
            .args(["-X", "PUT", "-o", "/dev/null", "--data-binary", "@-"])
            .args([
                "-H",
                "Content-Type: application/vnd.oci.image.manifest.v1+json",
            ])
            .arg(format!(
                "{base}/v2/{guest_repository}/manifests/{guest_digest}"
            )),
        Some(&manifest),
    );
    fs::remove_file(&login).expect("remove curl login");

    // The archive tool fetches both by digest with the login of this machine.
    let archive = |reference: &str, path: &Path| {
        succeed(
            Command::new(root.join("tools/registry-image-archive"))
                .env("DOCKER_CONFIG", &docker_config)
                .env("CURL_CA_BUNDLE", &ca)
                .arg(reference)
                .arg(path),
            None,
        );
    };
    let guest = format!("{host}/{guest_repository}@{guest_digest}");
    let guest_archive = work.join("guest.tar");
    archive(&guest, &guest_archive);
    let guest_index = archive_index(&guest_archive, &work.join("guest"));
    assert_eq!(names(&guest_index, &guest_digest), [guest]);
    for digest in [&guest_digest, &config_digest, &layer_digest] {
        assert!(
            work.join("guest/blobs/sha256")
                .join(digest.trim_start_matches("sha256:"))
                .is_file(),
            "the guest archive lacks {digest}"
        );
    }
    let image_archive = work.join("image.tar");
    archive(&image, &image_archive);
    let image_index = archive_index(&image_archive, &work.join("image"));
    let mut expected = [tagged.clone(), pinned.clone()];
    expected.sort();
    assert_eq!(names(&image_index, &image_digest), expected);

    // Two kind nodes import the image archive. They get no login.
    fs::write(
        work.join("kind.yaml"),
        "kind: Cluster\napiVersion: kind.x-k8s.io/v1alpha4\nnodes:\n- role: control-plane\n- role: worker\n",
    )
    .expect("write kind configuration");
    owned.cluster_created = true;
    succeed(
        Command::new("kind")
            .args(["create", "cluster", "--name", &owned.cluster, "--config"])
            .arg(work.join("kind.yaml"))
            .arg("--kubeconfig")
            .arg(work.join("kubeconfig")),
        None,
    );
    let nodes = succeed(
        Command::new("kind").args(["get", "nodes", "--name", &owned.cluster]),
        None,
    );
    let nodes: Vec<&str> = nodes.lines().collect();
    assert_eq!(nodes.len(), 2, "the cluster must have two nodes");
    let archive_bytes = fs::read(&image_archive).expect("read image archive");
    let credential = format!(
        "{password}\n{}\n",
        succeed(
            Command::new("base64").arg("-w0"),
            Some(format!("{USER}:{password}").as_bytes())
        )
    );
    for node in &nodes {
        succeed(
            Command::new("docker").args([
                "exec",
                "-i",
                node,
                "ctr",
                "--namespace",
                "k8s.io",
                "images",
                "import",
                "--platform",
                "linux/amd64",
                "-",
            ]),
            Some(&archive_bytes),
        );
        // CRI records an imported image from an event, after the import returns.
        let mut report = None;
        for _ in 0..10 {
            let output = run(
                Command::new("docker")
                    .args(["exec", node, "crictl", "inspecti", "-o", "json", &image]),
                None,
            );
            if output.status.success() {
                report = Some(output.stdout);
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        let report: Value = serde_json::from_slice(
            &report.unwrap_or_else(|| panic!("CRI on {node} shows no {image}")),
        )
        .expect("parse CRI report");
        let status = &report["status"];
        assert!(
            status["repoTags"]
                .as_array()
                .expect("tags")
                .contains(&json!(tagged)),
            "CRI on {node} lacks the tag: {status}"
        );
        assert_eq!(
            status["repoDigests"],
            json!([pinned]),
            "CRI on {node} must report the pinned digest and no other"
        );
        assert_eq!(
            report["info"]["imageSpec"]["config"]["Labels"]["wamn.dev/source-head"],
            json!(head),
            "the image on {node} lost the label of the cache tool"
        );

        // grep exits 1 when no file holds the password or the encoded login.
        let search = run(
            Command::new("docker").args([
                "exec",
                "-i",
                node,
                "grep",
                "-rlsF",
                "-D",
                "skip",
                "-f",
                "/dev/stdin",
                "/etc",
                "/root",
                "/var/lib/kubelet",
                "/kind",
            ]),
            Some(credential.as_bytes()),
        );
        assert_eq!(
            search.status.code(),
            Some(1),
            "the node {node} must hold no credential; files: {}",
            String::from_utf8_lossy(&search.stdout)
        );
    }
}
