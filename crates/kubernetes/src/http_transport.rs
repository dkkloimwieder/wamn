//! Shared Hyper connector and client construction for native callers.

use hyper::body::Body;
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::{
    Builder, Client,
    connect::{Connect, HttpConnector},
};
use hyper_util::rt::TokioExecutor;

fn builder() -> Builder {
    Client::builder(TokioExecutor::new())
}

/// The URL schemes a TLS connector accepts.
#[derive(Clone, Copy, Debug)]
pub enum Schemes {
    /// Accept HTTPS endpoints only.
    HttpsOnly,
    /// Accept both HTTP and HTTPS endpoints.
    HttpAndHttps,
}

/// The HTTP protocols a TLS connector negotiates.
#[derive(Clone, Copy, Debug)]
pub enum Protocols {
    /// Negotiate HTTP/1 only.
    Http1,
    /// Negotiate HTTP/1 and HTTP/2.
    Http1AndHttp2,
}

/// Wrap a TCP connector with the caller's TLS trust and protocol policy.
///
/// # Panics
/// Panics if `tls` already contains ALPN protocols.
pub fn tls_connector<R>(
    tls: rustls::ClientConfig,
    mut http: HttpConnector<R>,
    schemes: Schemes,
    protocols: Protocols,
) -> HttpsConnector<HttpConnector<R>> {
    http.enforce_http(false);
    let builder = HttpsConnectorBuilder::new().with_tls_config(tls);
    let builder = match schemes {
        Schemes::HttpsOnly => builder.https_only(),
        Schemes::HttpAndHttps => builder.https_or_http(),
    }
    .enable_http1();
    match protocols {
        Protocols::Http1 => builder.wrap_connector(http),
        Protocols::Http1AndHttp2 => builder.enable_http2().wrap_connector(http),
    }
}

/// Construct a pooled client without changing its request or response body type.
pub fn client<C, B>(connector: C, configure: impl FnOnce(&mut Builder)) -> Client<C, B>
where
    C: Connect + Clone,
    B: Body + Send + 'static,
    B::Data: Send,
{
    let mut builder = builder();
    configure(&mut builder);
    builder.build(connector)
}

/// Construct a plaintext client with Hyper's TCP keepalive and pool defaults.
pub fn http_client<B>() -> Client<HttpConnector, B>
where
    B: Body + Send + 'static,
    B::Data: Send,
{
    builder().build_http()
}
