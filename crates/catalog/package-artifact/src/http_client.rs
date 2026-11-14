//! Platform HTTP transport. Requests never use proxies, redirects or automatic retries.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::Context as _;
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, Limited};
use hyper::{Request, Response};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use hyper_util::rt::{TokioExecutor, TokioTimer};
use rustls::pki_types::{CertificateDer, pem::PemObject as _};

/// A shared Hyper client, with trust fixed at construction.
#[derive(Debug)]
pub struct HttpClient(Arc<Client<HttpsConnector<HttpConnector>, Full<Bytes>>>);

impl Clone for HttpClient {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

/// The platform verifier shared by trusted endpoints and pinned connection calls.
pub fn platform_tls() -> anyhow::Result<rustls::ClientConfig> {
    let provider = rustls::crypto::CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()));
    let verifier = rustls_platform_verifier::Verifier::new(Arc::clone(&provider))?;
    Ok(rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth())
}

impl HttpClient {
    /// Add registry trust roots to the platform roots through the shared transport.
    pub fn with_extra_roots(bundles: &[Vec<u8>]) -> anyhow::Result<Self> {
        if bundles.is_empty() {
            return Self::shared();
        }
        let certificates = bundles
            .iter()
            .flat_map(|pem| CertificateDer::pem_slice_iter(pem))
            .collect::<Result<Vec<_>, _>>()
            .context("read registry trust roots")?;
        let provider = rustls::crypto::CryptoProvider::get_default()
            .cloned()
            .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()));
        let verifier = rustls_platform_verifier::Verifier::new_with_extra_roots(
            certificates,
            Arc::clone(&provider),
        )?;
        let tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth();
        Ok(Self::from_tls(tls))
    }

    /// Reuse the process client for platform endpoints and metadata requests.
    pub fn shared() -> anyhow::Result<Self> {
        static CLIENT: OnceLock<HttpClient> = OnceLock::new();
        if let Some(client) = CLIENT.get() {
            return Ok(client.clone());
        }
        let client = Self::from_tls(platform_tls()?);
        Ok(CLIENT.get_or_init(|| client).clone())
    }

    /// Trust only this nonempty PEM bundle, without ambient certificate roots.
    pub fn with_roots(pem: &[u8]) -> anyhow::Result<Self> {
        let mut roots = rustls::RootCertStore::empty();
        for certificate in CertificateDer::pem_slice_iter(pem) {
            roots.add(certificate.context("read HTTPS trust root")?)?;
        }
        anyhow::ensure!(!roots.is_empty(), "HTTPS trust bundle is empty");
        let provider = rustls::crypto::CryptoProvider::get_default()
            .cloned()
            .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()));
        let tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Self::from_tls(tls))
    }

    fn from_tls(tls: rustls::ClientConfig) -> Self {
        let connector = HttpsConnectorBuilder::new()
            .with_tls_config(tls)
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .build();
        Self(Arc::new(
            Client::builder(TokioExecutor::new())
                .timer(TokioTimer::new())
                .pool_timer(TokioTimer::new())
                .pool_max_idle_per_host(2)
                .pool_idle_timeout(Duration::from_secs(30))
                .retry_canceled_requests(false)
                .build(connector),
        ))
    }

    /// Send once and collect a bounded body within the complete request deadline.
    pub async fn send(
        &self,
        request: Request<Bytes>,
        timeout: Duration,
        body_limit: usize,
    ) -> anyhow::Result<Response<Bytes>> {
        tokio::time::timeout(timeout, async {
            let response = self.0.request(request.map(Full::new)).await?;
            let (parts, body) = response.into_parts();
            let body = Limited::new(body, body_limit)
                .collect()
                .await
                .map_err(|error| anyhow::anyhow!("HTTP response body: {error}"))?
                .to_bytes();
            Ok(Response::from_parts(parts, body))
        })
        .await
        .context("HTTP request deadline exceeded")?
    }
}
