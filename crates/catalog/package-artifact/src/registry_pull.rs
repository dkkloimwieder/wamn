//! Bounded retries for registry reads, within the caller's original deadline.

use std::error::Error;
use std::io;
use std::time::Duration;

use oci_client::Reference;
use oci_client::errors::{OciDistributionError, OciErrorCode};
use oci_client::manifest::OciDescriptor;
use tokio::time::{Instant, sleep, timeout_at};

/// Pull a verified blob, discarding partial bytes before each retry.
pub async fn blob(
    client: &oci_client::Client,
    reference: &Reference,
    descriptor: &OciDescriptor,
    deadline: Instant,
) -> Result<Vec<u8>, OciDistributionError> {
    retry(
        reference,
        deadline,
        || async {
            let mut bytes = Vec::new();
            client.pull_blob(reference, descriptor, &mut bytes).await?;
            Ok(bytes)
        },
        |error| error,
    )
    .await
}

/// Four attempts with 1 s, 2 s, and 4 s delays within the caller's deadline.
///
/// The caller shares one deadline across the manifest and blobs. Each blob
/// attempt must use a fresh output buffer. `source` exposes typed errors from
/// either `oci-client` or its `anyhow` callers without parsing error messages.
pub async fn retry<T, E: From<io::Error>, F: std::future::Future<Output = Result<T, E>>>(
    reference: &Reference,
    deadline: Instant,
    mut operation: impl FnMut() -> F,
    source: fn(&E) -> &(dyn Error + 'static),
) -> Result<T, E> {
    for attempt in 1..=4 {
        let result = timeout_at(deadline, operation()).await.map_err(|_| {
            E::from(io::Error::new(
                io::ErrorKind::TimedOut,
                "registry pull deadline elapsed",
            ))
        })?;
        let error = match result {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        let Some((kind, status)) = retry_reason(source(&error)) else {
            return Err(error);
        };
        let delay = Duration::from_secs(1 << (attempt - 1));
        if attempt == 4 || Instant::now() + delay >= deadline {
            return Err(error);
        }
        tracing::warn!(
            attempt = attempt + 1,
            error_kind = kind,
            status,
            reference = %reference,
            "retry registry pull"
        );
        sleep(delay).await;
    }
    unreachable!("the last attempt returns its result")
}

fn retry_reason(mut error: &(dyn Error + 'static)) -> Option<(&'static str, Option<u16>)> {
    loop {
        if let Some(error) = error.downcast_ref::<OciDistributionError>() {
            return match error {
                OciDistributionError::ServerError { code, .. }
                    if *code == 429 || (500..600).contains(code) =>
                {
                    Some(("http", Some(*code)))
                }
                // oci-client discards the HTTP status of an OCI 429 envelope.
                OciDistributionError::RegistryError { envelope, .. }
                    if !envelope.errors.is_empty()
                        && envelope
                            .errors
                            .iter()
                            .all(|error| error.code == OciErrorCode::Toomanyrequests) =>
                {
                    Some(("http", Some(429)))
                }
                OciDistributionError::RequestError(error) => request_retry_reason(error),
                OciDistributionError::IoError(error) if error.kind() == io::ErrorKind::TimedOut => {
                    Some(("timeout", None))
                }
                _ => None,
            };
        }
        if let Some(error) = error.downcast_ref::<reqwest::Error>() {
            return request_retry_reason(error);
        }
        error = error.source()?;
    }
}

fn request_retry_reason(error: &reqwest::Error) -> Option<(&'static str, Option<u16>)> {
    if error.is_timeout() {
        Some(("timeout", None))
    } else {
        error
            .status()
            .filter(|status| status.is_server_error() || status.as_u16() == 429)
            .map(|status| ("http", Some(status.as_u16())))
    }
}

#[cfg(any(test, feature = "test-util"))]
pub mod test_server;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Repository, sha256_digest};
    use oci_client::client::{ClientConfig, ClientProtocol};
    use oci_client::secrets::RegistryAuth;
    use test_server::Server;

    fn repository(server: &Server) -> Repository {
        Repository {
            reference: server.reference.clone(),
            auth: RegistryAuth::Anonymous,
            client: oci_client::Client::try_from(ClientConfig {
                protocol: ClientProtocol::Http,
                ..ClientConfig::default()
            })
            .unwrap(),
        }
    }

    #[tokio::test]
    async fn manifest_retries_503_then_succeeds() {
        let body = br#"{"schemaVersion":2}"#.to_vec();
        let server = Server::start(vec![(503, Vec::new()), (200, body.clone())]).await;
        let start = Instant::now();
        assert_eq!(
            repository(&server).manifest().await.unwrap(),
            Some((body.clone(), sha256_digest(&body)))
        );
        assert_eq!(server.requests(), 2);
        assert!(start.elapsed() >= Duration::from_secs(1));
    }

    #[tokio::test]
    async fn manifest_404_is_absent_without_retry() {
        let server = Server::start(vec![(404, Vec::new())]).await;
        assert!(repository(&server).manifest().await.unwrap().is_none());
        assert_eq!(server.requests(), 1);
    }

    #[tokio::test]
    async fn manifest_repeated_504_stops_after_four_attempts() {
        let server = Server::start(vec![(504, Vec::new())]).await;
        let start = Instant::now();
        let error = repository(&server).manifest().await.unwrap_err();
        assert!(matches!(
            error.downcast_ref::<OciDistributionError>(),
            Some(OciDistributionError::ServerError { code: 504, .. })
        ));
        assert_eq!(server.requests(), 4);
        assert!(start.elapsed() >= Duration::from_secs(7));
    }

    #[tokio::test]
    async fn manifest_retries_oci_rate_limit_envelope() {
        let server = Server::start(vec![
            (429, br#"{"errors":[{"code":"TOOMANYREQUESTS"}]}"#.to_vec()),
            (200, b"{}".to_vec()),
        ])
        .await;
        assert!(repository(&server).manifest().await.unwrap().is_some());
        assert_eq!(server.requests(), 2);
    }

    #[tokio::test]
    async fn read_timeout_discards_partial_blob_before_retry() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut blobs = 0;
            while blobs < 2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    socket.read_exact(&mut byte).await.unwrap();
                    request.push(byte[0]);
                }
                if request.starts_with(b"GET /v2/ HTTP/") {
                    socket
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .await
                        .unwrap();
                    continue;
                }
                blobs += 1;
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\n")
                    .await
                    .unwrap();
                if blobs == 1 {
                    socket.write_all(b"comp").await.unwrap();
                    sleep(Duration::from_millis(100)).await;
                } else {
                    socket.write_all(b"component").await.unwrap();
                }
            }
            blobs
        });
        let client = oci_client::Client::try_from(ClientConfig {
            protocol: ClientProtocol::Http,
            read_timeout: Some(Duration::from_millis(30)),
            ..ClientConfig::default()
        })
        .unwrap();
        let reference = format!("{address}/components:fixture").parse().unwrap();
        let descriptor = OciDescriptor {
            digest: sha256_digest(b"component"),
            ..OciDescriptor::default()
        };
        let result = blob(
            &client,
            &reference,
            &descriptor,
            Instant::now() + Duration::from_secs(3),
        )
        .await;
        assert_eq!(result.unwrap(), b"component");
        assert_eq!(server.await.unwrap(), 2);
    }

    #[tokio::test]
    async fn blob_retries_status_errors_but_never_digest_mismatches() {
        for (status, attempts) in [(503, 2), (429, 2), (401, 1), (403, 1), (404, 1), (504, 4)] {
            let body = b"component".to_vec();
            let mut responses = vec![(status, Vec::new())];
            if attempts == 2 {
                responses.push((200, body.clone()));
            }
            let server = Server::start(responses).await;
            let repository = repository(&server);
            let descriptor = OciDescriptor {
                digest: sha256_digest(&body),
                ..OciDescriptor::default()
            };
            let result = blob(
                &repository.client,
                &repository.reference,
                &descriptor,
                Instant::now() + Duration::from_secs(30),
            )
            .await;
            assert_eq!(result.is_ok(), attempts == 2, "status {status}");
            assert_eq!(server.requests(), attempts, "status {status}");
        }
        let server = Server::start(vec![(200, b"wrong".to_vec())]).await;
        let repository = repository(&server);
        let descriptor = OciDescriptor {
            digest: sha256_digest(b"expected"),
            ..OciDescriptor::default()
        };
        assert!(matches!(
            blob(
                &repository.client,
                &repository.reference,
                &descriptor,
                Instant::now() + Duration::from_secs(3)
            )
            .await,
            Err(OciDistributionError::DigestError(_))
        ));
        assert_eq!(server.requests(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn fourth_attempt_succeeds_after_exponential_backoff() {
        let reference = "localhost/components:fixture".parse().unwrap();
        let start = Instant::now();
        let mut attempts = Vec::new();
        let result: Result<(), OciDistributionError> = retry(
            &reference,
            start + Duration::from_secs(30),
            || {
                attempts.push(start.elapsed());
                let result = if attempts.len() == 4 {
                    Ok(())
                } else {
                    Err(io::Error::from(io::ErrorKind::TimedOut).into())
                };
                async { result }
            },
            |error| error,
        )
        .await;
        result.unwrap();
        assert_eq!(attempts, [0, 1, 3, 7].map(Duration::from_secs));
    }

    #[tokio::test(start_paused = true)]
    async fn deadline_includes_attempts_and_backoff() {
        let reference = "localhost/components:fixture".parse().unwrap();
        let mut attempts = 0;
        let start = Instant::now();
        let result: Result<(), OciDistributionError> = retry(
            &reference,
            start + Duration::from_millis(1050),
            || {
                attempts += 1;
                async {
                    sleep(Duration::from_millis(40)).await;
                    Err(io::Error::from(io::ErrorKind::TimedOut).into())
                }
            },
            |error| error,
        )
        .await;
        assert!(result.is_err());
        assert_eq!(attempts, 2);
        assert_eq!(start.elapsed(), Duration::from_millis(1050));
    }
}
