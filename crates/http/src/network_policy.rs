//! Address rules for administrator-bound HTTP destinations.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// The address rule of the target type a connection call runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AddressPolicy {
    /// The cloud cluster reaches public addresses only.
    Cloud,
    /// A local device also reaches private addresses.
    Local,
}

impl AddressPolicy {
    /// The rule for a `registry.target.type` value; `None` for any other value.
    pub(crate) fn for_target(target_type: &str) -> Option<Self> {
        match target_type {
            "cloud" => Some(Self::Cloud),
            "local" => Some(Self::Local),
            _ => None,
        }
    }

    pub(crate) fn allows(self, address: SocketAddr) -> bool {
        allows(address) && (self == Self::Local || !private(address.ip().to_canonical()))
    }
}

/// The private ranges: 10/8, 172.16/12, 192.168/16, 100.64/10 and fc00::/7.
fn private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_private() || shared(ip),
        IpAddr::V6(ip) => ip.is_unique_local(),
    }
}

/// The shared address space 100.64/10 (RFC 6598).
fn shared(ip: Ipv4Addr) -> bool {
    let [first, second, ..] = ip.octets();
    first == 100 && second & 0xc0 == 64
}

/// The minimum on every target: loopback, link-local, unspecified and cloud
/// metadata addresses are refused.
fn allows(address: SocketAddr) -> bool {
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
    use super::*;

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

    #[test]
    fn cloud_refuses_each_private_range_and_local_allows_it() {
        for address in [
            "10.0.0.1:443",
            "10.255.255.255:443",
            "172.16.0.1:443",
            "172.31.255.255:443",
            "192.168.0.1:443",
            "192.168.255.255:443",
            "100.64.0.1:443",
            "100.127.255.255:443",
            "[fc00::1]:443",
            "[fdff:ffff::1]:443",
            "[::ffff:10.96.0.1]:443",
        ] {
            let address = address.parse().unwrap();
            assert!(!AddressPolicy::Cloud.allows(address), "{address}");
            assert!(AddressPolicy::Local.allows(address), "{address}");
        }
        for address in [
            "8.8.8.8:443",
            "172.32.0.1:443",
            "100.128.0.1:443",
            "[2001:4860:4860::8888]:443",
        ] {
            assert!(
                AddressPolicy::Cloud.allows(address.parse().unwrap()),
                "{address}"
            );
        }
    }
}
