use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Mutex;

use wamn_runtime::connection_authority::{
    AuthorityError, AuthorityErrorType, DnsResolver, HttpScheme, TlsIdentity, TlsPolicy,
    parse_http_connection_authority, resolve_http_request,
};
use wamn_runtime::{CanonicalHttpTarget, normalize_portable_http_target};
use wash_runtime::host::allowed_hosts::AllowedHost;

#[derive(Debug, Default)]
struct FixedDns {
    answers: HashMap<String, Vec<SocketAddr>>,
    calls: Mutex<Vec<(String, u16)>>,
}

impl FixedDns {
    fn with(host: &str, addresses: Vec<SocketAddr>) -> Self {
        Self {
            answers: HashMap::from([(host.to_owned(), addresses)]),
            calls: Mutex::default(),
        }
    }
}

impl DnsResolver for FixedDns {
    fn resolve(
        &self,
        host: &str,
        port: u16,
    ) -> impl Future<Output = Result<Vec<SocketAddr>, AuthorityError>> + Send {
        self.calls
            .lock()
            .expect("DNS calls lock")
            .push((host.to_owned(), port));
        std::future::ready(Ok(self.answers.get(host).cloned().unwrap_or_default()))
    }
}

#[derive(Debug)]
struct ExactNetwork(Vec<SocketAddr>);

impl ExactNetwork {
    fn allows(&self, address: SocketAddr) -> bool {
        self.0.contains(&address)
    }
}

#[derive(Debug)]
struct SequenceDns {
    answers: Mutex<VecDeque<Vec<SocketAddr>>>,
}

impl DnsResolver for SequenceDns {
    fn resolve(
        &self,
        _host: &str,
        _port: u16,
    ) -> impl Future<Output = Result<Vec<SocketAddr>, AuthorityError>> + Send {
        let answer = self
            .answers
            .lock()
            .expect("DNS answers lock")
            .pop_front()
            .unwrap_or_default();
        std::future::ready(Ok(answer))
    }
}

fn socket(octet: u8, port: u16) -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, octet)), port)
}

fn allowed(value: &str) -> AllowedHost {
    value.parse().expect("allowed-host fixture parses")
}

fn target(value: &str) -> CanonicalHttpTarget {
    normalize_portable_http_target(value).expect("portable target spelling")
}

#[tokio::test]
async fn canonical_request_pins_dns_and_preserves_host_and_tls_identity() {
    let connection =
        parse_http_connection_authority("HTTPS://ERP.Example:443/api", TlsPolicy::VerifyAuthority)
            .expect("connection definition");
    let selected = socket(1, 443);
    let denied = socket(2, 443);
    let dns = FixedDns::with("erp.example", vec![denied, selected]);
    let network = ExactNetwork(vec![selected]);

    let decision = resolve_http_request(
        &connection,
        &target("/orders?id=7"),
        &[allowed("https://erp.example")],
        &|address| network.allows(address),
        &dns,
    )
    .await
    .expect("request resolves");

    assert_eq!(
        decision.logical_url.as_ref(),
        "https://erp.example/api/orders?id=7"
    );
    assert_eq!(decision.logical_authority.scheme(), HttpScheme::Https);
    assert_eq!(decision.logical_authority.host(), "erp.example");
    assert_eq!(decision.logical_authority.port(), 443);
    assert_eq!(decision.host_header.as_ref(), "erp.example");
    assert_eq!(
        decision.transport,
        wamn_runtime::connection_authority::PinnedEndpoint {
            address: selected,
            tls_identity: Some(TlsIdentity::Dns("erp.example".into())),
        }
    );
    assert_eq!(
        dns.calls.lock().expect("DNS calls lock").as_slice(),
        &[("erp.example".to_owned(), 443)]
    );
}

#[tokio::test]
async fn exact_http_service_authority_with_non_default_port_reaches_the_resolver() {
    let connection =
        parse_http_connection_authority("http://serve-echo:8091/", TlsPolicy::Disabled)
            .expect("connection definition");
    let selected = socket(1, 8091);
    let dns = FixedDns::with("serve-echo", vec![selected]);

    let decision = resolve_http_request(
        &connection,
        &target("/hook"),
        &[allowed("serve-echo:8091")],
        &|address| ExactNetwork(vec![selected]).allows(address),
        &dns,
    )
    .await
    .expect("service authority resolves");

    assert_eq!(decision.logical_url.as_ref(), "http://serve-echo:8091/hook");
    assert_eq!(decision.host_header.as_ref(), "serve-echo:8091");
    assert_eq!(
        decision.transport,
        wamn_runtime::connection_authority::PinnedEndpoint {
            address: selected,
            tls_identity: None,
        }
    );
}

#[tokio::test]
async fn request_authority_and_base_path_injection_fail_before_dns() {
    let connection =
        parse_http_connection_authority("https://erp.example/api/", TlsPolicy::VerifyAuthority)
            .expect("connection definition");
    let dns = FixedDns::default();
    let network = ExactNetwork(Vec::new());
    let policy = [allowed("https://erp.example")];

    for spelling in [
        "https://evil.example/steal",
        "http://169.254.169.254/latest/meta-data",
        "//evil.example/steal",
    ] {
        normalize_portable_http_target(spelling).expect_err(spelling);
    }
    for spelling in ["/../outside", "/%2e%2e/outside", "/orders%2f..%2foutside"] {
        let normalized = target(spelling);
        let error = resolve_http_request(
            &connection,
            &normalized,
            &policy,
            &|address| network.allows(address),
            &dns,
        )
        .await
        .expect_err(spelling);
        assert!(
            matches!(
                error.error_type(),
                AuthorityErrorType::InvalidRequestTarget | AuthorityErrorType::BasePathEscape
            ),
            "unexpected failure for {spelling}: {error:?}"
        );
    }
    assert!(dns.calls.lock().expect("DNS calls lock").is_empty());
}

#[tokio::test]
async fn dns_rebinding_cannot_change_an_already_pinned_transport_address() {
    let connection =
        parse_http_connection_authority("https://erp.example/api/", TlsPolicy::VerifyAuthority)
            .expect("connection definition");
    let admitted = socket(1, 443);
    let rebound = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)), 443);
    let dns = SequenceDns {
        answers: Mutex::new(VecDeque::from([vec![admitted], vec![rebound]])),
    };
    let network = ExactNetwork(vec![admitted]);
    let policy = [allowed("https://erp.example")];

    let request = target("/orders");
    let first = resolve_http_request(
        &connection,
        &request,
        &policy,
        &|address| network.allows(address),
        &dns,
    )
    .await
    .expect("first DNS answer is admitted");
    let first_endpoint = &first.transport;
    assert_eq!(first_endpoint.address, admitted);

    let error = resolve_http_request(
        &connection,
        &request,
        &policy,
        &|address| network.allows(address),
        &dns,
    )
    .await
    .expect_err("rebound address is outside the network ceiling");
    assert_eq!(error.error_type(), AuthorityErrorType::NetworkDenied);
    assert_eq!(
        first_endpoint.address, admitted,
        "first decision remains pinned"
    );
}

#[tokio::test]
async fn both_outer_policy_ceiling_denials_are_final() {
    let connection =
        parse_http_connection_authority("https://erp.example/api/", TlsPolicy::VerifyAuthority)
            .expect("connection definition");
    let address = socket(1, 443);
    let dns = FixedDns::with("erp.example", vec![address]);

    let host_error = resolve_http_request(
        &connection,
        &target("/orders"),
        &[allowed("https://other.example")],
        &|address| ExactNetwork(vec![address]).allows(address),
        &dns,
    )
    .await
    .expect_err("host ceiling denies");
    assert_eq!(
        host_error.error_type(),
        AuthorityErrorType::PlatformHostDenied
    );

    let network_error = resolve_http_request(
        &connection,
        &target("/orders"),
        &[allowed("https://erp.example")],
        &ExactNetwork(Vec::new()),
        &dns,
    )
    .await
    .expect_err("network ceiling denies");
    assert_eq!(
        network_error.error_type(),
        AuthorityErrorType::NetworkDenied
    );
}

#[test]
fn definition_rejects_userinfo_fragments_incoherent_tls() {
    let cases = [
        ("https://user@erp.example/api", TlsPolicy::VerifyAuthority),
        ("https://erp.example/api#frag", TlsPolicy::VerifyAuthority),
        (
            "https://erp.example/api/../private",
            TlsPolicy::VerifyAuthority,
        ),
        (
            "https://erp.example/api/%2e%2e/private",
            TlsPolicy::VerifyAuthority,
        ),
        ("https://%65rp.example/api", TlsPolicy::VerifyAuthority),
        ("https://erp.example/api", TlsPolicy::Disabled),
        ("http://erp.example/api", TlsPolicy::VerifyAuthority),
    ];
    for (base, tls) in cases {
        assert!(
            parse_http_connection_authority(base, tls).is_err(),
            "accepted invalid definition {base:?}"
        );
    }
}
