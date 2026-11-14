//! Address minimum for administrator-bound HTTP destinations.

use std::net::{IpAddr, Ipv6Addr, SocketAddr};

/// Private and cluster addresses remain available to explicitly bound targets.
pub(super) fn allows(address: SocketAddr) -> bool {
    match address.ip().to_canonical() {
        IpAddr::V4(ip) => !ip.is_link_local() && !ip.is_loopback() && !ip.is_unspecified(),
        IpAddr::V6(ip) => {
            !ip.is_unicast_link_local()
                && !ip.is_loopback()
                && !ip.is_unspecified()
                && ip != Ipv6Addr::new(0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x254)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use crate::normalize_portable_http_target;

    use super::*;
    use crate::connection_authority::{
        AuthorityError, AuthorityErrorType, DnsResolver, TlsPolicy,
        parse_http_connection_authority, resolve_http_request,
    };

    #[test]
    fn bound_address_minimum_preserves_private_targets() {
        for address in [
            "0.0.0.0:80",
            "[::]:80",
            "[::ffff:0.0.0.0]:80",
            "127.0.0.1:80",
            "[::1]:80",
            "[::ffff:127.0.0.1]:80",
            "169.254.0.1:80",
            "169.254.169.254:80",
            "169.254.255.255:443",
            "[fe80::1]:80",
            "[febf::1]:80",
            "[fd00:ec2::254]:80",
            "[::ffff:169.254.169.254]:80",
        ] {
            assert!(!allows(address.parse().unwrap()), "{address}");
        }
        for address in [
            "10.96.0.1:443",
            "172.16.0.1:80",
            "192.168.1.1:80",
            "8.8.8.8:443",
            "[fd00::1]:80",
            "[::ffff:10.96.0.1]:443",
            "[2001:4860:4860::8888]:443",
        ] {
            assert!(allows(address.parse().unwrap()), "{address}");
        }
    }

    struct ChangingDns(Mutex<Vec<SocketAddr>>);

    impl DnsResolver for ChangingDns {
        fn resolve(
            &self,
            _: &str,
            _: u16,
        ) -> impl Future<Output = Result<Vec<SocketAddr>, AuthorityError>> + Send {
            std::future::ready(Ok(self.0.lock().unwrap().clone()))
        }
    }

    #[tokio::test]
    async fn mixed_answers_pin_allowed_peer_and_rebinding_refuses() {
        let connection =
            parse_http_connection_authority("http://erp.example/", TlsPolicy::Disabled).unwrap();
        let hosts = ["http://erp.example".parse().unwrap()];
        let target = normalize_portable_http_target("/item").unwrap();
        let allowed: SocketAddr = "192.168.1.1:80".parse().unwrap();
        let denied: SocketAddr = "169.254.169.254:80".parse().unwrap();
        let dns = ChangingDns(Mutex::new(vec![denied, allowed]));
        let first = resolve_http_request(&connection, &target, &hosts, &allows, &dns)
            .await
            .unwrap();
        let origin = &first.transport;
        assert_eq!(origin.address, allowed);
        *dns.0.lock().unwrap() = vec![denied];
        let error = resolve_http_request(&connection, &target, &hosts, &allows, &dns)
            .await
            .unwrap_err();
        assert_eq!(error.error_type(), AuthorityErrorType::NetworkDenied);
        assert_eq!(origin.address, allowed);
    }
}
