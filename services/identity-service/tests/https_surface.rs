//! Redaction and authority validation for identity database configuration.

use wamn_identity_service::IdentityConfig;
use wamn_platform_install::identity_role::identity_issuer_login;

const ISSUER: &str = "https://identity.service.internal";
const PASSWORD: &str = "identity-surface-fixture-secret";

#[test]
fn validated_configuration_debug_does_not_disclose_database_credentials() {
    let role = identity_issuer_login(ISSUER).expect("targeted role");
    let url = format!("postgres://{role}:{PASSWORD}@127.0.0.1/wamn_system");
    let config = IdentityConfig::new(ISSUER, &url).expect("targeted configuration");
    assert!(!format!("{config:?}").contains(PASSWORD));
    assert!(!format!("{config:?}").contains(&url));
    let refused = format!("postgres://wamn_system:{PASSWORD}@unreachable.invalid/wamn_system");
    let error = IdentityConfig::new(ISSUER, &refused).expect_err("owner refused before any socket");
    assert!(!format!("{error:?} {error}").contains(PASSWORD));
    assert!(!format!("{error:?} {error}").contains(&refused));
}
