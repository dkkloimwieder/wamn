//! Route authentication through the Postgres authorization adapter.
#![cfg(feature = "test-util")]

use serde_json::{Value, json};
use wamn_catalog::PAT_AUTHENTICATION_MODE;
use wamn_engine::flow_http_routing::{AuthenticatedCaller, CredentialType, FlowHttpRouting};

use hmac::{Hmac, KeyInit as _, Mac as _};
use sha2::Sha256;
use sqlx::Row as _;
use std::sync::Arc;
use wamn_authentication::route_authentication::PlatformRouteAuthenticator;

const UNAUTHORIZED: (u16, &str) = (401, "unauthorized");
const UNAVAILABLE: (u16, &str) = (503, "authentication-unavailable");

use self::test_util::{routing, routing_policy};
#[path = "support/route_authentication/connection_live.rs"]
mod connection_live;
#[path = "support/route_authentication/signature.rs"]
mod signature;

#[tokio::test]
async fn pat_mode_is_recognized_and_an_absent_backend_is_one_generic_outage() {
    let plugin = routing(&json!([PAT_AUTHENTICATION_MODE]));

    let rejection = plugin
        .authenticate_headers_for_test(&"orders".parse().unwrap(), &[])
        .await
        .expect_err("a protected route without its backend refuses");

    assert_eq!((rejection.0, rejection.1.as_str()), UNAVAILABLE);
}

#[tokio::test]
async fn a_bearer_and_a_session_cookie_together_refuse() {
    let plugin = routing(&json!(["pat", "session"]));

    for authorization in ["Bearer session.token", "Bearer wamn_pat_x", "Basic x"] {
        let rejection = plugin
            .authenticate_headers_for_test(
                &"orders".parse().unwrap(),
                &[
                    ("authorization", authorization),
                    ("cookie", "__Host-wamn-session=session.token"),
                ],
            )
            .await
            .expect_err("two credentials refuse");
        assert_eq!((rejection.0, rejection.1.as_str()), UNAUTHORIZED);
    }
}

/// Owner ruling on wamn-e5in.5: an empty authenticator answers a session
/// route as unauthorized and a PAT route as authentication-unavailable.
#[tokio::test]
async fn an_empty_authenticator_refuses_a_session_route_401_and_a_pat_route_503() {
    for (modes, expected) in [
        (json!(["session"]), UNAUTHORIZED),
        (json!([PAT_AUTHENTICATION_MODE]), UNAVAILABLE),
    ] {
        let rejection = routing(&modes)
            .authenticate_headers_for_test(
                &"orders".parse().unwrap(),
                &[("authorization", "Bearer token")],
            )
            .await
            .expect_err("an empty authenticator refuses");
        assert_eq!((rejection.0, rejection.1.as_str()), expected);
    }
}

#[path = "support/route_authentication/test_util.rs"]
mod test_util;
