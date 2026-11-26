use wamn_registry::platform_domain::PlatformDomain;

/// Accept the namespace control host or an application host in the platform domain.
pub fn valid_route_host(host: &str, namespace: &str, base: &PlatformDomain) -> bool {
    let Some((label, domain)) = host.split_once('.') else {
        return false;
    };
    if domain != base.as_str() || namespace.is_empty() || label.len() > 63 || host.len() > 253 {
        return false;
    }
    if label == namespace {
        return true;
    }
    label
        .strip_suffix(namespace)
        .and_then(|prefix| prefix.strip_suffix("--"))
        .is_some_and(|prefix| {
            !prefix.is_empty()
                && !prefix.starts_with('-')
                && !prefix.ends_with('-')
                && prefix
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::valid_route_host;
    use wamn_registry::platform_domain::PlatformDomain;

    #[test]
    fn a_route_host_must_be_the_control_or_an_app_host_of_its_namespace() {
        let base: PlatformDomain = "example.test".parse().unwrap();
        let other: PlatformDomain = "other.test".parse().unwrap();
        let (acme, globex) = (
            "acme".parse::<wamn_registry::OrgId>().unwrap(),
            "globex".parse::<wamn_registry::OrgId>().unwrap(),
        );
        let control = format!("{acme}.{base}");
        let app = wamn_registry::name::app_host("shop", &acme, &base);
        assert!(valid_route_host(&control, "acme", &base));
        assert!(valid_route_host(&app, "acme", &base));
        for host in [
            format!("{globex}.{base}"),
            wamn_registry::name::app_host("shop", &globex, &base),
            format!("{acme}.{other}"),
            wamn_registry::name::app_host("shop", &acme, &other),
            wamn_registry::name::app_host("", &acme, &base),
            wamn_registry::name::app_host("-shop", &acme, &base),
            wamn_registry::name::app_host("Shop", &acme, &base),
            wamn_registry::name::app_host(&"a".repeat(58), &acme, &base),
            format!("shop-{acme}.{base}"),
            format!("shop.{acme}.{base}"),
            acme.to_string(),
        ] {
            assert!(!valid_route_host(&host, "acme", &base), "{host}");
        }
        assert!(!valid_route_host(&control, "", &base));
    }
}
