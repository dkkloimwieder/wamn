//! Canonical, fail-closed HTTP connection authority resolution.

use std::future::Future;
use std::net::{IpAddr, SocketAddr};

use hyper::Uri;
use url::{Host, Url};
use wamn_flow::node_contract::CanonicalHttpTarget;
use wash_runtime::host::allowed_hosts::AllowedHost;

/// Supported outbound HTTP schemes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HttpScheme {
    Http,
    Https,
}

impl HttpScheme {
    fn default_port(self) -> u16 {
        match self {
            Self::Http => 80,
            Self::Https => 443,
        }
    }
}

/// TLS posture owned by the connection definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsPolicy {
    Disabled,
    VerifyAuthority,
}

/// One canonical logical HTTP authority.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CanonicalAuthority {
    scheme: HttpScheme,
    host: Box<str>,
    port: u16,
}

impl CanonicalAuthority {
    pub fn scheme(&self) -> HttpScheme {
        self.scheme
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The canonical HTTP `Host`/`:authority` value.
    pub fn http_authority(&self) -> String {
        let host = bracket_ip_v6(&self.host);
        if self.port == self.scheme.default_port() {
            host.into_owned()
        } else {
            format!("{host}:{}", self.port)
        }
    }
}

/// Immutable authority-bearing fields of one HTTP connection generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpConnectionAuthority {
    base_url: Url,
    authority: CanonicalAuthority,
    base_path: Box<str>,
    proxy: Option<ProxyAuthority>,
}

impl HttpConnectionAuthority {
    /// Canonical base URL, including its normalized base path.
    pub fn canonical_base_url(&self) -> &str {
        self.base_url.as_str()
    }

    /// Canonical logical authority selected by this definition.
    pub fn authority(&self) -> &CanonicalAuthority {
        &self.authority
    }

    /// Canonical configured proxy authority, when present.
    pub fn proxy_authority(&self) -> Option<&CanonicalAuthority> {
        self.proxy.as_ref().map(|proxy| &proxy.authority)
    }

    /// Canonical configured proxy URL, when present.
    pub fn canonical_proxy_url(&self) -> Option<&str> {
        self.proxy.as_ref().map(|proxy| proxy.url.as_str())
    }
}

/// The TLS identity retained while transport connects to a pinned address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TlsIdentity {
    Dns(Box<str>),
    Ip(IpAddr),
}

/// A checked endpoint whose address must be used by the connector as-is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedEndpoint {
    pub address: SocketAddr,
    pub tls_identity: Option<TlsIdentity>,
}

/// The transport path selected exclusively from the connection definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportDecision {
    Direct {
        origin: PinnedEndpoint,
    },
    Proxy {
        proxy: PinnedEndpoint,
        origin: PinnedEndpoint,
    },
}

/// Complete output of one authority decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityDecision {
    pub logical_url: Box<str>,
    pub logical_authority: CanonicalAuthority,
    pub host_header: Box<str>,
    pub transport: TransportDecision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProxyAuthority {
    url: Url,
    authority: CanonicalAuthority,
}

/// Stable classification for a refused authority decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorityErrorKind {
    InvalidDefinition,
    UnsupportedScheme,
    InvalidTlsPolicy,
    InvalidRequestTarget,
    BasePathEscape,
    RedirectDenied,
    PlatformHostDenied,
    DnsResolutionFailed,
    NetworkDenied,
}

/// A fail-closed authority resolution error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityError {
    kind: AuthorityErrorKind,
    detail: Box<str>,
}

impl AuthorityError {
    fn new(kind: AuthorityErrorKind, detail: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    pub fn kind(&self) -> AuthorityErrorKind {
        self.kind
    }

    /// Construct a DNS failure from a resolver implementation.
    pub fn dns_resolution_failed(detail: impl Into<Box<str>>) -> Self {
        Self::new(AuthorityErrorKind::DnsResolutionFailed, detail)
    }
}

impl std::fmt::Display for AuthorityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.detail)
    }
}

impl std::error::Error for AuthorityError {}

/// Host-controlled DNS seam. Results are filtered and one address is pinned.
pub trait DnsResolver: Send + Sync {
    fn resolve(
        &self,
        host: &str,
        port: u16,
    ) -> impl Future<Output = Result<Vec<SocketAddr>, AuthorityError>> + Send;
}

/// The cluster network ceiling applied to resolved transport addresses.
pub trait NetworkPolicy: Send + Sync {
    fn allows(&self, address: SocketAddr) -> bool;
}

/// Production DNS resolver backed by Tokio's host-controlled resolver.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioDnsResolver;

impl DnsResolver for TokioDnsResolver {
    fn resolve(
        &self,
        host: &str,
        port: u16,
    ) -> impl Future<Output = Result<Vec<SocketAddr>, AuthorityError>> + Send {
        let host = host.to_owned();
        async move {
            tokio::net::lookup_host((host.as_str(), port))
                .await
                .map(|addresses| addresses.collect())
                .map_err(|error| {
                    AuthorityError::new(
                        AuthorityErrorKind::DnsResolutionFailed,
                        format!("DNS resolution failed for {host}:{port}: {error}"),
                    )
                })
        }
    }
}

/// Parse and validate the authority-bearing fields of one connection generation.
pub fn parse_http_connection_authority(
    base_url: &str,
    tls: TlsPolicy,
    proxy_url: Option<&str>,
) -> Result<HttpConnectionAuthority, AuthorityError> {
    let mut base_url = parse_absolute_url(base_url, "connection base URL")?;
    let authority = canonical_authority(&base_url)?;
    validate_tls(authority.scheme, tls, "connection")?;
    if base_url.query().is_some() {
        return Err(AuthorityError::new(
            AuthorityErrorKind::InvalidDefinition,
            "connection base URL cannot contain a query",
        ));
    }

    let base_path = canonical_base_path(base_url.path())?;
    base_url.set_path(&base_path);
    let proxy = proxy_url.map(parse_proxy).transpose()?;

    Ok(HttpConnectionAuthority {
        base_url,
        authority,
        base_path: base_path.into_boxed_str(),
        proxy,
    })
}

/// Resolve a guest-controlled relative path/query under a trusted connection.
pub async fn resolve_http_request<R, N>(
    connection: &HttpConnectionAuthority,
    target: &CanonicalHttpTarget,
    platform_hosts: &[AllowedHost],
    network: &N,
    dns: &R,
) -> Result<AuthorityDecision, AuthorityError>
where
    R: DnsResolver,
    N: NetworkPolicy,
{
    validate_relative_target(target.as_str())?;
    let target = connection.base_url.join(target.as_str()).map_err(|error| {
        AuthorityError::new(
            AuthorityErrorKind::InvalidRequestTarget,
            format!("invalid relative HTTP target: {error}"),
        )
    })?;
    resolve_target(connection, target, platform_hosts, network, dns).await
}

/// Re-enter the resolver for one redirect without widening authority or base path.
pub async fn resolve_http_redirect<R, N>(
    connection: &HttpConnectionAuthority,
    previous_logical_url: &str,
    location: &str,
    platform_hosts: &[AllowedHost],
    network: &N,
    dns: &R,
) -> Result<AuthorityDecision, AuthorityError>
where
    R: DnsResolver,
    N: NetworkPolicy,
{
    reject_ambiguous_target(location, AuthorityErrorKind::RedirectDenied)?;
    let previous = Url::parse(previous_logical_url).map_err(|error| {
        AuthorityError::new(
            AuthorityErrorKind::RedirectDenied,
            format!("invalid previous redirect URL: {error}"),
        )
    })?;
    let target = previous.join(location).map_err(|error| {
        AuthorityError::new(
            AuthorityErrorKind::RedirectDenied,
            format!("invalid redirect location: {error}"),
        )
    })?;
    if canonical_authority(&target)? != connection.authority {
        return Err(AuthorityError::new(
            AuthorityErrorKind::RedirectDenied,
            "redirect changes the connection authority",
        ));
    }
    resolve_target(connection, target, platform_hosts, network, dns).await
}

async fn resolve_target<R, N>(
    connection: &HttpConnectionAuthority,
    target: Url,
    platform_hosts: &[AllowedHost],
    network: &N,
    dns: &R,
) -> Result<AuthorityDecision, AuthorityError>
where
    R: DnsResolver,
    N: NetworkPolicy,
{
    validate_target_url(connection, &target)?;
    require_host_policy(&target, platform_hosts)?;

    let origin = pin_endpoint(&connection.authority, network, dns).await?;
    let transport = if let Some(proxy) = &connection.proxy {
        require_host_policy(&proxy.url, platform_hosts)?;
        let proxy = pin_endpoint(&proxy.authority, network, dns).await?;
        TransportDecision::Proxy { proxy, origin }
    } else {
        TransportDecision::Direct { origin }
    };

    Ok(AuthorityDecision {
        logical_url: target.as_str().into(),
        logical_authority: connection.authority.clone(),
        host_header: connection.authority.http_authority().into(),
        transport,
    })
}

fn parse_absolute_url(value: &str, label: &str) -> Result<Url, AuthorityError> {
    if let Some((_, remainder)) = value.split_once("://") {
        let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
        let raw_authority = &remainder[..authority_end];
        if raw_authority.contains(['%', '\\']) {
            return Err(AuthorityError::new(
                AuthorityErrorKind::InvalidDefinition,
                format!("{label} contains an ambiguous authority encoding"),
            ));
        }
        let path_and_suffix = &remainder[authority_end..];
        if path_and_suffix.starts_with('/') {
            let path_end = path_and_suffix
                .find(['?', '#'])
                .unwrap_or(path_and_suffix.len());
            let raw_path = &path_and_suffix[..path_end];
            if raw_path.contains('\\') {
                return Err(AuthorityError::new(
                    AuthorityErrorKind::InvalidDefinition,
                    format!("{label} contains an ambiguous path separator"),
                ));
            }
            validate_path(raw_path, AuthorityErrorKind::InvalidDefinition)?;
        }
    }
    let url = Url::parse(value).map_err(|error| {
        AuthorityError::new(
            AuthorityErrorKind::InvalidDefinition,
            format!("invalid {label}: {error}"),
        )
    })?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(AuthorityError::new(
            AuthorityErrorKind::InvalidDefinition,
            format!("{label} cannot contain user-info"),
        ));
    }
    if url.fragment().is_some() {
        return Err(AuthorityError::new(
            AuthorityErrorKind::InvalidDefinition,
            format!("{label} cannot contain a fragment"),
        ));
    }
    Ok(url)
}

fn canonical_authority(url: &Url) -> Result<CanonicalAuthority, AuthorityError> {
    let scheme = match url.scheme() {
        "http" => HttpScheme::Http,
        "https" => HttpScheme::Https,
        scheme => {
            return Err(AuthorityError::new(
                AuthorityErrorKind::UnsupportedScheme,
                format!("unsupported HTTP connection scheme {scheme:?}"),
            ));
        }
    };
    let host = url.host().ok_or_else(|| {
        AuthorityError::new(
            AuthorityErrorKind::InvalidDefinition,
            "HTTP connection authority has no host",
        )
    })?;
    let host = match host {
        Host::Domain(domain) => domain.to_ascii_lowercase(),
        Host::Ipv4(address) => address.to_string(),
        Host::Ipv6(address) => address.to_string(),
    };
    if !host.is_ascii() {
        return Err(AuthorityError::new(
            AuthorityErrorKind::InvalidDefinition,
            "canonical HTTP host is not ASCII",
        ));
    }
    let port = url.port_or_known_default().ok_or_else(|| {
        AuthorityError::new(
            AuthorityErrorKind::InvalidDefinition,
            "HTTP connection authority has no effective port",
        )
    })?;
    Ok(CanonicalAuthority {
        scheme,
        host: host.into_boxed_str(),
        port,
    })
}

fn validate_tls(scheme: HttpScheme, tls: TlsPolicy, label: &str) -> Result<(), AuthorityError> {
    let coherent = matches!(
        (scheme, tls),
        (HttpScheme::Http, TlsPolicy::Disabled) | (HttpScheme::Https, TlsPolicy::VerifyAuthority)
    );
    if coherent {
        Ok(())
    } else {
        Err(AuthorityError::new(
            AuthorityErrorKind::InvalidTlsPolicy,
            format!("{label} TLS policy is inconsistent with its scheme"),
        ))
    }
}

fn canonical_base_path(path: &str) -> Result<String, AuthorityError> {
    validate_path(path, AuthorityErrorKind::InvalidDefinition)?;
    let mut path = path.to_owned();
    if !path.ends_with('/') {
        path.push('/');
    }
    Ok(path)
}

fn parse_proxy(value: &str) -> Result<ProxyAuthority, AuthorityError> {
    let url = parse_absolute_url(value, "proxy URL")?;
    if url.query().is_some() || url.path() != "/" {
        return Err(AuthorityError::new(
            AuthorityErrorKind::InvalidDefinition,
            "proxy URL cannot contain a path or query",
        ));
    }
    let authority = canonical_authority(&url)?;
    let tls = match authority.scheme {
        HttpScheme::Http => TlsPolicy::Disabled,
        HttpScheme::Https => TlsPolicy::VerifyAuthority,
    };
    validate_tls(authority.scheme, tls, "proxy")?;
    Ok(ProxyAuthority { url, authority })
}

fn validate_relative_target(value: &str) -> Result<(), AuthorityError> {
    if Url::parse(value).is_ok() {
        return Err(AuthorityError::new(
            AuthorityErrorKind::InvalidRequestTarget,
            "HTTP request target must be connection-relative",
        ));
    }
    reject_ambiguous_target(value, AuthorityErrorKind::InvalidRequestTarget)
}

fn reject_ambiguous_target(value: &str, kind: AuthorityErrorKind) -> Result<(), AuthorityError> {
    if value.contains('\\') || value.contains('#') {
        return Err(AuthorityError::new(kind, "ambiguous HTTP target encoding"));
    }
    let path = value.split_once('?').map_or(value, |(path, _)| path);
    validate_path(path, kind)
}

fn validate_path(path: &str, kind: AuthorityErrorKind) -> Result<(), AuthorityError> {
    for segment in path.split('/') {
        if segment == "." || segment == ".." {
            return Err(AuthorityError::new(
                kind,
                "HTTP target contains a dot segment",
            ));
        }
    }
    let bytes = path.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len()
            || !bytes[index + 1].is_ascii_hexdigit()
            || !bytes[index + 2].is_ascii_hexdigit()
        {
            return Err(AuthorityError::new(kind, "malformed HTTP percent encoding"));
        }
        let decoded = (hex_value(bytes[index + 1]) << 4) | hex_value(bytes[index + 2]);
        if matches!(decoded, b'/' | b'\\' | b'.' | 0) {
            return Err(AuthorityError::new(
                kind,
                "ambiguous encoded HTTP path separator",
            ));
        }
        index += 3;
    }
    Ok(())
}

fn hex_value(value: u8) -> u8 {
    match value {
        b'0'..=b'9' => value - b'0',
        b'a'..=b'f' => value - b'a' + 10,
        b'A'..=b'F' => value - b'A' + 10,
        _ => unreachable!("hex_value is called only after is_ascii_hexdigit"),
    }
}

fn validate_target_url(
    connection: &HttpConnectionAuthority,
    target: &Url,
) -> Result<(), AuthorityError> {
    if !target.username().is_empty() || target.password().is_some() || target.fragment().is_some() {
        return Err(AuthorityError::new(
            AuthorityErrorKind::InvalidRequestTarget,
            "resolved HTTP target contains user-info or a fragment",
        ));
    }
    if canonical_authority(target)? != connection.authority {
        return Err(AuthorityError::new(
            AuthorityErrorKind::RedirectDenied,
            "resolved HTTP target changes the connection authority",
        ));
    }
    if !target.path().starts_with(connection.base_path.as_ref()) {
        return Err(AuthorityError::new(
            AuthorityErrorKind::BasePathEscape,
            "resolved HTTP target escapes the connection base path",
        ));
    }
    Ok(())
}

fn require_host_policy(url: &Url, platform_hosts: &[AllowedHost]) -> Result<(), AuthorityError> {
    let uri: Uri = url.as_str().parse().map_err(|error| {
        AuthorityError::new(
            AuthorityErrorKind::InvalidDefinition,
            format!("canonical HTTP URL is not a valid URI: {error}"),
        )
    })?;
    if platform_hosts.iter().any(|entry| entry.matches(&uri)) {
        Ok(())
    } else {
        Err(AuthorityError::new(
            AuthorityErrorKind::PlatformHostDenied,
            format!("platform host policy denies {uri}"),
        ))
    }
}

async fn pin_endpoint<R, N>(
    authority: &CanonicalAuthority,
    network: &N,
    dns: &R,
) -> Result<PinnedEndpoint, AuthorityError>
where
    R: DnsResolver,
    N: NetworkPolicy,
{
    let mut addresses = match authority.host.parse::<IpAddr>() {
        Ok(address) => vec![SocketAddr::new(address, authority.port)],
        Err(_) => dns.resolve(&authority.host, authority.port).await?,
    };
    addresses.sort_unstable();
    addresses.dedup();
    if addresses.is_empty() {
        return Err(AuthorityError::dns_resolution_failed(format!(
            "DNS resolution returned no addresses for {}:{}",
            authority.host, authority.port
        )));
    }
    let address = addresses
        .into_iter()
        .find(|address| address.port() == authority.port && network.allows(*address))
        .ok_or_else(|| {
            AuthorityError::new(
                AuthorityErrorKind::NetworkDenied,
                format!(
                    "cluster network policy denies every resolved address for {}:{}",
                    authority.host, authority.port
                ),
            )
        })?;
    let tls_identity = match authority.scheme {
        HttpScheme::Http => None,
        HttpScheme::Https => Some(match authority.host.parse::<IpAddr>() {
            Ok(address) => TlsIdentity::Ip(address),
            Err(_) => TlsIdentity::Dns(authority.host.clone()),
        }),
    };
    Ok(PinnedEndpoint {
        address,
        tls_identity,
    })
}

fn bracket_ip_v6(host: &str) -> std::borrow::Cow<'_, str> {
    if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{host}]").into()
    } else {
        host.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct EmptyDns;

    impl DnsResolver for EmptyDns {
        fn resolve(
            &self,
            _host: &str,
            _port: u16,
        ) -> impl Future<Output = Result<Vec<SocketAddr>, AuthorityError>> + Send {
            std::future::ready(Ok(Vec::new()))
        }
    }

    #[derive(Debug)]
    struct DenyNetwork;

    impl NetworkPolicy for DenyNetwork {
        fn allows(&self, _address: SocketAddr) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn resolved_target_outside_base_is_refused() {
        let connection = parse_http_connection_authority(
            "https://manager.example/api/",
            TlsPolicy::VerifyAuthority,
            None,
        )
        .expect("connection definition");
        let outside = Url::parse("https://manager.example/outside").expect("outside target");
        let error = resolve_target(&connection, outside, &[], &DenyNetwork, &EmptyDns)
            .await
            .expect_err("resolved target escaped its configured base");
        assert_eq!(error.kind(), AuthorityErrorKind::BasePathEscape);
    }
}
