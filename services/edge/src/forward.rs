//! The forward: each stored sample goes to a platform route over HTTPS, with
//! the device PAT (spec 4.8).
//!
//! Every attempt reads its samples from the store, so a retry never comes from
//! memory, and a restart continues where the store says. The forward sends
//! one sample per request, as the item `{"request_id": sample_key, "value":
//! body}`, and writes the sample key into the configured `key_field` too, so
//! the platform's idempotency makes a repeated forward harmless. The outcome
//! of each attempt is written to the sample's row:
//!
//! - An item with a value: the sample is forwarded.
//! - An item error or another 4xx status: the sample is refused with the
//!   platform's reason, and the forward never sends it again. An operator
//!   resolves it (owner ruling, `wamn-e5in.9`).
//! - No answer, a timeout, a 5xx status, or a 408 or 429 status: the sample
//!   stays pending, and the forward waits a backoff before the next attempt.
//!   Those answers concern the moment, not the sample.
//! - A 401 or 403 status: a credential failure. The sample stays pending and
//!   the forward waits a backoff, as above. The forward counts each credential
//!   failure and logs the first. After `CREDENTIAL_BOUND` failures in a row it
//!   stops sending until the edge restarts, because an expired or revoked PAT
//!   needs an operator, and the edge reads the PAT only at start (owner
//!   ruling, `wamn-e5in.9`).

use std::os::unix::fs::PermissionsExt as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Context as _;
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::StatusCode;
use hyper::header::{AUTHORIZATION, CONTENT_TYPE};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject as _;
use serde_json::{Map, Value, json};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::config::ForwardConfig;
use crate::samples::{Sample, SampleStore};

/// The longest wait for one request and its answer.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// The first wait after an attempt that did not reach the platform.
const FIRST_BACKOFF: Duration = Duration::from_secs(5);
/// The longest wait between attempts.
const LAST_BACKOFF: Duration = Duration::from_mins(15);
/// The samples that one pass reads from the store.
const PASS_SIZE: u32 = 16;
/// The most bytes of a platform answer that a refusal keeps.
const REASON_BYTES: usize = 1024;
/// The credential failures in a row after which the forward stops sending.
/// With the backoff, the fifth failure comes about 75 seconds after the first.
const CREDENTIAL_BOUND: u32 = 5;

/// A running forward.
#[derive(Debug)]
pub struct Forward {
    task: JoinHandle<()>,
    credential_failures: Arc<AtomicU64>,
}

impl Forward {
    /// The answers with status 401 or 403 since start. A count above zero
    /// shows a PAT that the platform does not accept.
    pub fn credential_failures(&self) -> u64 {
        self.credential_failures.load(Ordering::Relaxed)
    }

    /// Wait for the forward to end.
    pub async fn join(self) {
        if let Err(error) = self.task.await {
            tracing::warn!(%error, "the forward failed");
        }
    }
}

/// What the platform did with one sample.
#[derive(Debug, PartialEq, Eq)]
enum Answer {
    Accepted,
    Refused(String),
    Unreachable(String),
    Credential(String),
}

/// How a pass over the pending samples ended.
#[derive(Debug, PartialEq, Eq)]
enum Pass {
    /// No sample is pending.
    Done,
    /// The platform or the store could not be reached.
    Unreachable,
    /// The platform did not accept the PAT.
    Credential,
}

/// The client and the request parts of every forward.
struct Forwarder {
    client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
    url: hyper::Uri,
    authorization: String,
    key_field: Option<String>,
}

/// Check `config`, read the PAT, and forward the samples of `samples` until
/// `stopped` turns true.
pub fn start(
    config: &ForwardConfig,
    samples: SampleStore,
    stopped: watch::Receiver<bool>,
) -> anyhow::Result<Forward> {
    let url: hyper::Uri = config.url.parse().context("parse the forward url")?;
    anyhow::ensure!(
        url.scheme_str() == Some("https"),
        "the forward url {} is not https",
        config.url
    );
    let forwarder = Forwarder {
        client: Client::builder(TokioExecutor::new()).build(connector(config)?),
        url,
        authorization: format!("Bearer {}", read_token(&config.token_file)?),
        key_field: config.key_field.clone(),
    };
    let credential_failures = Arc::new(AtomicU64::new(0));
    let task = tokio::spawn(run(
        forwarder,
        samples,
        Arc::clone(&credential_failures),
        stopped,
    ));
    Ok(Forward {
        task,
        credential_failures,
    })
}

/// Read the PAT, and refuse a file that anyone but its owner can read.
#[expect(
    clippy::verbose_bit_mask,
    reason = "a mode reads as octal permission bits"
)]
fn read_token(path: &std::path::Path) -> anyhow::Result<String> {
    let mode = std::fs::metadata(path)
        .with_context(|| format!("read the token file {}", path.display()))?
        .permissions()
        .mode();
    anyhow::ensure!(
        mode & 0o077 == 0,
        "the token file {} has mode {:o}; only its owner may read it (mode 0600)",
        path.display(),
        mode & 0o777
    );
    let token = std::fs::read_to_string(path)
        .with_context(|| format!("read the token file {}", path.display()))?;
    let token = token.trim();
    anyhow::ensure!(
        !token.is_empty(),
        "the token file {} is empty",
        path.display()
    );
    Ok(token.to_owned())
}

/// An HTTPS-only connector that trusts the system roots and `ca_file`.
fn connector(config: &ForwardConfig) -> anyhow::Result<HttpsConnector<HttpConnector>> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    if let Some(ca_file) = &config.ca_file {
        for certificate in CertificateDer::pem_file_iter(ca_file)
            .with_context(|| format!("read the ca file {}", ca_file.display()))?
        {
            roots
                .add(certificate.context("parse the ca file")?)
                .context("trust the ca file")?;
        }
    }
    anyhow::ensure!(!roots.is_empty(), "the forward trusts no certificate");
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .context("select the TLS versions")?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls)
        .https_only()
        .enable_http1()
        .build())
}

async fn run(
    forwarder: Forwarder,
    samples: SampleStore,
    credential_failures: Arc<AtomicU64>,
    mut stopped: watch::Receiver<bool>,
) {
    let mut backoff = FIRST_BACKOFF;
    let mut in_a_row = 0;
    loop {
        let pass = tokio::select! {
            pass = forward_pending(&forwarder, &samples, &credential_failures) => pass,
            _ = stopped.wait_for(|stopped| *stopped) => return,
        };
        match pass {
            Pass::Done => in_a_row = 0,
            Pass::Unreachable => {}
            Pass::Credential => {
                in_a_row += 1;
                if in_a_row >= CREDENTIAL_BOUND {
                    tracing::error!(
                        failures = in_a_row,
                        "the forward stopped: the platform refused the PAT; \
                         renew the token file and restart the edge"
                    );
                    let _ = stopped.wait_for(|stopped| *stopped).await;
                    return;
                }
            }
        }
        let reached = pass == Pass::Done;
        let wait = async {
            if reached {
                samples.wait_stored().await;
            } else {
                tokio::time::sleep(backoff).await;
            }
        };
        tokio::select! {
            () = wait => {}
            _ = stopped.wait_for(|stopped| *stopped) => return,
        }
        backoff = if reached {
            FIRST_BACKOFF
        } else {
            (backoff * 2).min(LAST_BACKOFF)
        };
    }
}

/// Send every pending sample in order. A sample that does not reach the
/// platform, or a store error, ends the pass.
async fn forward_pending(
    forwarder: &Forwarder,
    samples: &SampleStore,
    credential_failures: &AtomicU64,
) -> Pass {
    loop {
        let pending = match samples.pending(PASS_SIZE).await {
            Ok(pending) => pending,
            Err(error) => {
                tracing::warn!(%error, "the forward cannot read the samples");
                return Pass::Unreachable;
            }
        };
        if pending.is_empty() {
            return Pass::Done;
        }
        for sample in &pending {
            let key = sample.sample_key.as_str();
            let recorded = match forwarder.send(sample).await {
                Answer::Accepted => samples.forwarded(key).await,
                Answer::Refused(reason) => {
                    let recorded = samples.refused(key, &reason).await;
                    if recorded.is_ok() {
                        tracing::warn!(
                            sample_key = key,
                            %reason,
                            "the platform refused a sample; it is stored as refused"
                        );
                    }
                    recorded
                }
                Answer::Unreachable(error) => {
                    tracing::warn!(sample_key = key, %error, "the forward did not reach the platform");
                    if let Err(error) = samples.attempt_failed(key, &error).await {
                        tracing::warn!(%error, "the forward cannot record an attempt");
                    }
                    return Pass::Unreachable;
                }
                Answer::Credential(error) => {
                    if credential_failures.fetch_add(1, Ordering::Relaxed) == 0 {
                        tracing::warn!(sample_key = key, %error, "the platform refused the forward's PAT");
                    } else {
                        tracing::debug!(sample_key = key, %error, "the platform refused the forward's PAT");
                    }
                    if let Err(error) = samples.attempt_failed(key, &error).await {
                        tracing::warn!(%error, "the forward cannot record an attempt");
                    }
                    return Pass::Credential;
                }
            };
            if let Err(error) = recorded {
                tracing::warn!(sample_key = key, %error, "the forward cannot record an answer");
                return Pass::Unreachable;
            }
        }
    }
}

impl Forwarder {
    /// Send one sample and read what the platform did with it.
    async fn send(&self, sample: &Sample) -> Answer {
        let key = sample.sample_key.as_str();
        let mut item = json!({"request_id": key, "value": sample.body});
        if let Some(field) = &self.key_field
            && let Err(reason) = write_field(&mut item, field, key)
        {
            return Answer::Refused(reason);
        }
        let body = Value::Array(vec![item]).to_string();
        let request = hyper::Request::post(self.url.clone())
            .header(AUTHORIZATION, &self.authorization)
            .header(CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from(body)));
        let request = match request {
            Ok(request) => request,
            Err(error) => return Answer::Unreachable(format!("build the request: {error}")),
        };
        let exchange = async {
            let response = self.client.request(request).await?;
            let status = response.status();
            let body = response.into_body().collect().await?.to_bytes();
            anyhow::Ok((status, body))
        };
        match tokio::time::timeout(REQUEST_TIMEOUT, exchange).await {
            Ok(Ok((status, body))) => answer(status, &body, key),
            Ok(Err(error)) => Answer::Unreachable(format!("{error:#}")),
            Err(_) => Answer::Unreachable(format!(
                "no answer in {} seconds",
                REQUEST_TIMEOUT.as_secs()
            )),
        }
    }
}

/// Write `key` into the item at the dot path `field`, and create the objects
/// on the path that are missing.
fn write_field(item: &mut Value, field: &str, key: &str) -> Result<(), String> {
    let mut names = field.split('.').peekable();
    let mut target = item;
    while let Some(name) = names.next() {
        let object = target
            .as_object_mut()
            .ok_or_else(|| format!("key_field {field} crosses a value that is not an object"))?;
        if names.peek().is_none() {
            object.insert(name.to_owned(), Value::from(key));
            break;
        }
        target = object
            .entry(name)
            .or_insert_with(|| Value::Object(Map::new()));
    }
    Ok(())
}

/// What the platform did with the sample `key`, read from its answer.
fn answer(status: StatusCode, body: &[u8], key: &str) -> Answer {
    let text = || {
        let text = String::from_utf8_lossy(body);
        let end = text.floor_char_boundary(REASON_BYTES);
        format!("status {}: {}", status.as_u16(), &text[..end])
    };
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        return Answer::Credential(text());
    }
    if status.is_server_error()
        || matches!(
            status,
            StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS
        )
    {
        return Answer::Unreachable(text());
    }
    if !status.is_success() {
        return Answer::Refused(text());
    }
    let items: Option<Vec<Value>> = serde_json::from_slice(body).ok();
    let item = items.as_deref().and_then(|items| {
        items
            .iter()
            .find(|item| item.get("request_id").and_then(Value::as_str) == Some(key))
    });
    match item {
        Some(item) if item.get("error").is_some() => Answer::Refused(item["error"].to_string()),
        Some(item) if item.get("value").is_some() => Answer::Accepted,
        _ => Answer::Unreachable(format!("the platform answered no item: {}", text())),
    }
}

#[cfg(test)]
mod tests {
    use hyper::StatusCode;
    use serde_json::json;

    use super::{Answer, answer, write_field};

    #[test]
    fn the_sample_key_is_written_into_the_key_field() {
        let mut item = json!({"request_id": "k-1", "value": {"frame": "12.5 kg"}});
        write_field(&mut item, "value.idempotency_key", "k-1").expect("a path of objects");
        assert_eq!(item["value"]["idempotency_key"], "k-1");
        write_field(&mut item, "idempotency_key", "k-1").expect("a top-level field");
        assert_eq!(item["idempotency_key"], "k-1");

        let mut scalar = json!({"request_id": "k-1", "value": 12.5});
        assert!(write_field(&mut scalar, "value.idempotency_key", "k-1").is_err());
    }

    #[test]
    fn a_refusal_is_an_outcome_and_only_the_moment_is_retried() {
        let item = |body: serde_json::Value| body.to_string().into_bytes();
        let accepted = item(json!([{"request_id": "k", "value": {"id": 1}}]));
        let refused = item(json!([{"request_id": "k", "error": {"code": "invalid_input"}}]));
        assert_eq!(answer(StatusCode::OK, &accepted, "k"), Answer::Accepted);
        assert!(matches!(
            answer(StatusCode::OK, &refused, "k"),
            Answer::Refused(reason) if reason.contains("invalid_input")
        ));
        assert!(matches!(
            answer(StatusCode::BAD_REQUEST, b"no", "k"),
            Answer::Refused(_)
        ));
        for status in [
            StatusCode::SERVICE_UNAVAILABLE,
            StatusCode::REQUEST_TIMEOUT,
            StatusCode::TOO_MANY_REQUESTS,
        ] {
            assert!(matches!(answer(status, b"", "k"), Answer::Unreachable(_)));
        }
        for status in [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN] {
            assert!(matches!(answer(status, b"", "k"), Answer::Credential(_)));
        }
        assert!(matches!(
            answer(StatusCode::OK, b"not json", "k"),
            Answer::Unreachable(_)
        ));
    }
}
