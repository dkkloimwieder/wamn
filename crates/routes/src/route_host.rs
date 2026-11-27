use wamn_registry::platform_domain::PlatformDomain;

/// Accept an org control host or an environment app host in the platform domain.
pub fn valid_route_host(host: &str, org: &str, control: bool, base: &PlatformDomain) -> bool {
    let Some((label, domain)) = host.split_once('.') else {
        return false;
    };
    if domain != base.as_str() || org.is_empty() || label.len() > 63 || host.len() > 253 {
        return false;
    }
    if control {
        return label == org;
    }
    label
        .strip_suffix(org)
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
        assert!(valid_route_host(&control, "acme", true, &base));
        assert!(valid_route_host(&app, "acme", false, &base));
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
            assert!(
                !valid_route_host(&host, "acme", false, &base)
                    && !valid_route_host(&host, "acme", true, &base),
                "{host}"
            );
        }
        assert!(!valid_route_host(&control, "", true, &base));
        assert!(!valid_route_host(&control, "acme", false, &base));
        assert!(!valid_route_host(&app, "acme", true, &base));
    }
}
