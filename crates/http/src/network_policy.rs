//! Address rules for administrator-bound HTTP destinations.

use std::net::SocketAddr;

use wash_runtime::host::egress_policy::EgressAddressPolicy;

use wamn_connection::bound_http::Target;

pub(crate) fn allows(target: Target, address: SocketAddr) -> bool {
    EgressAddressPolicy {
        deny_special: true,
        allow_private: target == Target::Local,
    }
    .permits(address.ip())
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
            assert!(
                !allows(Target::Local, address.parse().unwrap()),
                "{address}"
            );
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
            assert!(allows(Target::Local, address.parse().unwrap()), "{address}");
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
            "[64:ff9b::a00:1]:443",
            "[2002:a00:1::1]:443",
        ] {
            let address = address.parse().unwrap();
            assert!(!allows(Target::Cloud, address), "{address}");
            assert!(allows(Target::Local, address), "{address}");
        }
        assert!(!allows(Target::Cloud, "168.63.129.16:80".parse().unwrap()));
        for address in [
            "8.8.8.8:443",
            "172.32.0.1:443",
            "100.128.0.1:443",
            "[2001:4860:4860::8888]:443",
        ] {
            assert!(allows(Target::Cloud, address.parse().unwrap()), "{address}");
        }
    }
}
