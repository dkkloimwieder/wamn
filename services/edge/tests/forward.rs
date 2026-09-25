//! The forward survives a kill between the store and the forward, and a kill
//! during the forward (spec 4.10).
//!
//! The test runs the `wamn-edge` binary as a child with a configuration file,
//! a pseudo-terminal as its device, and an HTTPS platform in the test that
//! checks the device PAT and applies each key once. It needs the built guest,
//! as the route tests do: `WAMN_FLOW_HTTP_COMPONENT` names `http-route` built
//! for `wasm32-wasip2` (docs/operations/running-tests.md).

mod support;

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio_rustls::TlsAcceptor;
use wamn_edge::refusals;

use support::{ATTACHMENT, AUDIENCE, HOST, ISSUER, ORG, PRINCIPAL, bundle, ingress, key};

const TOKEN: &str = "wamn_pat_edge_forward_test";
const REFUSED_FRAME: &str = "refuse me";
/// The longest wait for one step of the child.
const STEP: Duration = Duration::from_secs(60);

/// A pseudo-terminal pair: the controller that the test writes, and the path
/// of the device that the edge reads.
fn pseudo_terminal() -> (File, PathBuf) {
    let controller = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)
        .expect("open a pseudo-terminal");
    grantpt(&controller).expect("grant the device");
    unlockpt(&controller).expect("unlock the device");
    let device = ptsname(&controller, Vec::new())
        .expect("name the device")
        .into_string()
        .expect("the device name is UTF-8");
    (File::from(controller), PathBuf::from(device))
}

/// What the platform received and applied.
#[derive(Default)]
struct Received {
    /// Every item, in order of arrival.
    items: Vec<Value>,
    /// The answer of each applied key.
    applied: BTreeMap<String, Value>,
    /// Hold the answer of the next new key, and never send it.
    hold_next: bool,
}

/// An HTTPS platform route that applies each idempotency key once.
struct Platform {
    received: Arc<Mutex<Received>>,
    /// Fired when the platform holds an answer.
    held: Arc<Notify>,
}

impl Platform {
    fn new() -> Self {
        Self {
            received: Arc::default(),
            held: Arc::default(),
        }
    }

    /// Serve on `listener` with `acceptor` until the test ends.
    fn serve(&self, listener: TcpListener, acceptor: TlsAcceptor) {
        let (received, held) = (Arc::clone(&self.received), Arc::clone(&self.held));
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.expect("accept");
                let (acceptor, received, held) =
                    (acceptor.clone(), Arc::clone(&received), Arc::clone(&held));
                tokio::spawn(async move {
                    let Ok(mut stream) = acceptor.accept(stream).await else {
                        return;
                    };
                    let (status, answer) = match read_request(&mut stream).await {
                        Some((authorization, body)) => {
                            let Some(answer) = handle(&received, &authorization, &body) else {
                                held.notify_one();
                                return std::future::pending().await;
                            };
                            answer
                        }
                        None => (400, "[]".to_owned()),
                    };
                    let response = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: \
                         {}\r\nConnection: close\r\n\r\n{answer}",
                        answer.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
    }

    fn received(&self) -> std::sync::MutexGuard<'_, Received> {
        self.received.lock().expect("platform state")
    }
}

/// Read one request, and return its authorization header and its body.
async fn read_request(
    stream: &mut (impl tokio::io::AsyncRead + Unpin),
) -> Option<(String, Vec<u8>)> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    let head_end = loop {
        let read = stream.read(&mut buffer).await.ok()?;
        if read == 0 {
            return None;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let head = String::from_utf8(bytes[..head_end].to_vec()).ok()?;
    let header = |name: &str| {
        head.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name)
                .then(|| value.trim().to_owned())
        })
    };
    let length: usize = header("content-length")?.parse().ok()?;
    while bytes.len() < head_end + length {
        let read = stream.read(&mut buffer).await.ok()?;
        if read == 0 {
            return None;
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    Some((
        header("authorization").unwrap_or_default(),
        bytes[head_end..head_end + length].to_vec(),
    ))
}

/// The status and body that answer one request, or `None` to hold it.
fn handle(received: &Mutex<Received>, authorization: &str, body: &[u8]) -> Option<(u16, String)> {
    if authorization != format!("Bearer {TOKEN}") {
        return Some((401, "[]".to_owned()));
    }
    let items: Vec<Value> = serde_json::from_slice(body).expect("an item list");
    let item = items.into_iter().next().expect("one item");
    let request_id = item["request_id"].clone();
    let key = item["value"]["idempotency_key"]
        .as_str()
        .expect("the forward writes the key field")
        .to_owned();
    let mut received = received.lock().expect("platform state");
    received.items.push(item.clone());
    if let Some(answer) = received.applied.get(&key) {
        let answer = json!([{"request_id": request_id, "value": answer}]);
        return Some((200, answer.to_string()));
    }
    if item["value"]["frame"] == REFUSED_FRAME {
        let answer = json!([{
            "request_id": request_id,
            "error": {"code": "invalid_input", "message": "the frame is not a weight"},
        }]);
        return Some((200, answer.to_string()));
    }
    let answer = json!({"applied": key});
    received.applied.insert(key, answer.clone());
    if std::mem::take(&mut received.hold_next) {
        return None;
    }
    Some((
        200,
        json!([{"request_id": request_id, "value": answer}]).to_string(),
    ))
}

/// A certificate authority, and a TLS acceptor for 127.0.0.1 that it signs.
fn tls(directory: &Path) -> (PathBuf, TlsAcceptor) {
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("CA parameters");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let ca_key = KeyPair::generate().expect("CA key");
    let ca = ca_params.self_signed(&ca_key).expect("CA certificate");
    let issuer = Issuer::new(ca_params, ca_key);
    let server_key = KeyPair::generate().expect("server key");
    let server = CertificateParams::new(vec!["127.0.0.1".to_owned()])
        .expect("server parameters")
        .signed_by(&server_key, &issuer)
        .expect("server certificate");
    let ca_file = directory.join("platform-ca.pem");
    std::fs::write(&ca_file, ca.pem()).expect("write the CA file");
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("TLS versions")
    .with_no_client_auth()
    .with_single_cert(
        vec![server.der().clone()],
        rustls::pki_types::PrivatePkcs8KeyDer::from(server_key.serialize_der()).into(),
    )
    .expect("server TLS");
    (ca_file, TlsAcceptor::from(Arc::new(config)))
}

/// A free local port. The platform binds it later, so the forward first finds
/// no platform.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind a port")
        .local_addr()
        .expect("port")
        .port()
}

/// Write the configuration file of the child.
fn configuration(
    directory: &Path,
    digest: &str,
    device: &Path,
    port: u16,
    ca_file: &Path,
) -> PathBuf {
    let token_file = directory.join("device.pat");
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&token_file)
        .and_then(|mut file| file.write_all(TOKEN.as_bytes()))
        .expect("write the token file");
    let text = format!(
        r#"
[release]
dir = "{dir}"
digest = "{digest}"

[session]
keys = "{keys}"
issuer = "{ISSUER}"
org = "{ORG}"
audience = "{AUDIENCE}"

[store]
db = "{db}"

[http]
route_host = "{HOST}"
listen = "127.0.0.1:0"

[device]
attachment = "{ATTACHMENT}"
principal = "{PRINCIPAL}"
role = "device"

[device.serial]
path = "{device}"
baud = 9600
max_frame = 64

[forward]
url = "https://127.0.0.1:{port}/samples"
token_file = "{token}"
ca_file = "{ca}"
key_field = "value.idempotency_key"
"#,
        dir = directory.display(),
        keys = directory.join("session-keys.json").display(),
        db = directory.join("edge.db").display(),
        device = device.display(),
        token = token_file.display(),
        ca = ca_file.display(),
    );
    let path = directory.join("edge.toml");
    std::fs::write(&path, text).expect("write the configuration");
    path
}

/// The `wamn-edge` binary, running, and the lines of its log.
struct Edge {
    child: Child,
    lines: mpsc::Receiver<String>,
    seen: Vec<String>,
}

impl Edge {
    fn start(config: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_wamn-edge"))
            .arg("--config")
            .arg(config)
            .env("RUST_LOG", "wamn_edge=debug")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start wamn-edge");
        let log = child.stdout.take().expect("the child's log");
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(log).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    return;
                }
            }
        });
        let mut edge = Self {
            child,
            lines,
            seen: Vec::new(),
        };
        edge.wait_for("wamn-edge serves its release", 1);
        edge
    }

    /// Wait until the log holds `count` lines that contain `text`.
    fn wait_for(&mut self, text: &str, count: usize) {
        let deadline = Instant::now() + STEP;
        while self.seen.iter().filter(|line| line.contains(text)).count() < count {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) => self.seen.push(line),
                Err(error) => panic!(
                    "wamn-edge logged {text:?} fewer than {count} times ({error}):\n{}",
                    self.seen.join("\n")
                ),
            }
        }
    }

    /// Kill the child with SIGKILL, as a power loss would stop it.
    fn kill(mut self) {
        self.child.kill().expect("kill wamn-edge");
        self.child.wait().expect("reap wamn-edge");
    }
}

/// Wait until `done` holds for the platform.
async fn until(platform: &Platform, what: &str, done: impl Fn(&Received) -> bool) {
    let deadline = Instant::now() + STEP;
    while !done(&platform.received()) {
        assert!(Instant::now() < deadline, "the platform never saw {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Two frames are stored while the platform is down, and the edge is killed.
/// The first forward after the restart reaches the platform, which applies it
/// and never answers, and the edge is killed again. After the second restart
/// the edge sends that sample again with the same key and body, the platform
/// applies each key once, and the export ran once per frame. A frame that the
/// platform refuses is stored as refused and sent once.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires: WAMN_FLOW_HTTP_COMPONENT"]
async fn a_sample_is_forwarded_once_across_kills() {
    let (_, public) = key("key-one", 1);
    let (directory, digest) = bundle("forward", &ingress(), &public);
    let (mut controller, device) = pseudo_terminal();
    let port = free_port();
    let (ca_file, acceptor) = tls(&directory);
    let config = configuration(&directory, &digest, &device, port, &ca_file);

    // The platform is down: the samples are stored and the forward fails.
    let mut edge = Edge::start(&config);
    controller
        .write_all(b"12.5 kg\n13.0 kg\n")
        .expect("send the frames");
    edge.wait_for("the device call stored its sample", 2);
    edge.wait_for("the forward did not reach the platform", 1);
    edge.kill();

    // The platform applies the first sample and holds its answer.
    let platform = Platform::new();
    platform.received().hold_next = true;
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("bind the platform port");
    platform.serve(listener, acceptor);
    let held = Arc::clone(&platform.held);
    let edge = Edge::start(&config);
    tokio::time::timeout(STEP, held.notified())
        .await
        .expect("the platform receives the first sample");
    edge.kill();

    // The restarted edge sends the held sample again, then the second.
    let mut edge = Edge::start(&config);
    until(&platform, "both samples", |received| {
        received.items.len() >= 3
    })
    .await;
    controller
        .write_all(format!("{REFUSED_FRAME}\n").as_bytes())
        .expect("send the refused frame");
    edge.wait_for("the platform refused a sample", 1);
    edge.kill();

    let refused_key = {
        let received = platform.received();
        let keys: Vec<&str> = received
            .items
            .iter()
            .map(|item| {
                let key = item["request_id"].as_str().expect("a request id");
                assert_eq!(item["value"]["idempotency_key"], key, "one key for both");
                key
            })
            .collect();
        assert_eq!(keys.len(), 4, "{keys:?}");
        assert_eq!(keys[0], keys[1], "the held sample is sent again");
        assert_eq!(received.items[0], received.items[1], "with the same body");
        assert_eq!(received.applied.len(), 2, "each sample is applied once");
        assert_eq!(
            received.items[3]["value"]["frame"], REFUSED_FRAME,
            "the refused sample is sent once"
        );
        keys[3].to_owned()
    };

    let db = directory.join("edge.db");
    let connection = rusqlite::Connection::open(&db).expect("open the stopped edge's file");
    let count = |query: &str| -> i64 {
        connection
            .query_row(query, [], |row| row.get(0))
            .expect("count")
    };
    assert_eq!(
        count("SELECT COUNT(*) FROM intents"),
        3,
        "one export call per frame"
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM samples WHERE forwarded_at IS NOT NULL"),
        2
    );
    drop(connection);

    let command = |args: &[&str]| args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    let listed = refusals::run(&command(&["list"]), &db)
        .await
        .expect("list the refused samples");
    assert!(
        listed.starts_with(&refused_key) && listed.contains("invalid_input"),
        "{listed}"
    );
    refusals::run(
        &command(&["resolve", &refused_key, "operator-judgment"]),
        &db,
    )
    .await
    .expect("resolve the refused sample");
    assert_eq!(
        refusals::run(&command(&["list"]), &db).await.expect("list"),
        ""
    );
}
