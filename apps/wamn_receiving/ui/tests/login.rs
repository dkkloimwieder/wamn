//! Exercise the same optional login boundary that the operator binary calls.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;
use wamn_client::{ClientError, HttpRequest, HttpResponse, Transport};
use wamn_receiving_tui::login::{credentials, session_target};

const ISSUER: &str = "https://identity.example.invalid";
const AUDIENCE: &str = "urn:wamn:project-env:acme:receiving:dev:k3m9x2p7";
const PAT: &str = "receiving-startup-fixture-pat";
const SESSION: &str = "receiving-startup-fixture-session";

#[cfg(unix)]
#[test]
fn the_binary_refuses_a_non_unicode_pat_without_rendering_its_value() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    let sentinel = "receiving-non-unicode-fixture-pat";
    let mut value = sentinel.as_bytes().to_vec();
    value.push(0xff);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_wamn-receiving"))
        .env_clear()
        .env("WAMN_BASE_URL", "http://127.0.0.1:1")
        .env("WAMN_TOKEN", OsString::from_vec(value))
        .output()
        .expect("run the compiled Receiving startup");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("WAMN_TOKEN must carry the operator's access token"));
    assert!(!stderr.contains(sentinel));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(sentinel));
}

#[derive(Debug, Default)]
struct SuccessfulExchange {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl Transport for SuccessfulExchange {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, ClientError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.method, "POST");
        assert_eq!(request.url, format!("{ISSUER}/session"));
        assert!(request.headers.iter().any(|(name, value)| {
            name.eq_ignore_ascii_case("authorization") && value == &format!("Bearer {PAT}")
        }));
        let body: serde_json::Value = serde_json::from_slice(&request.body).expect("exchange JSON");
        assert_eq!(body, json!({ "aud": AUDIENCE }));
        let expires_at = SystemTime::now()
            .checked_add(Duration::from_secs(600))
            .expect("fixture expiry")
            .duration_since(UNIX_EPOCH)
            .expect("positive fixture clock")
            .as_secs();
        Ok(HttpResponse {
            status: 200,
            body: json!({
                "token_type": "Bearer",
                "access_token": SESSION,
                "expires_at": expires_at,
            })
            .to_string(),
        })
    }
}

#[derive(Debug, Default)]
struct RefusedExchange {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl Transport for RefusedExchange {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, ClientError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(request.method, "POST");
        assert_eq!(request.url, format!("{ISSUER}/session"));
        Ok(HttpResponse {
            status: 401,
            body: "{\"error\":\"unauthenticated\"}".to_owned(),
        })
    }
}

#[test]
fn optional_session_configuration_requires_the_complete_valid_pair() {
    assert!(session_target(None, None).expect("PAT-only mode").is_none());
    assert!(
        session_target(Some(ISSUER), Some(AUDIENCE))
            .expect("explicit session target")
            .is_some()
    );
    for (issuer, audience) in [
        (Some(ISSUER), None),
        (None, Some(AUDIENCE)),
        (Some(""), Some(AUDIENCE)),
        (Some(ISSUER), Some("")),
        (Some(""), Some("")),
    ] {
        assert!(session_target(issuer, audience).is_err());
    }
}

#[tokio::test]
async fn pat_only_startup_returns_the_original_credential_without_an_exchange() {
    let exchange = Arc::new(RefusedExchange::default());
    let target = session_target(None, None).expect("PAT-only configuration");
    let provider = credentials(PAT.to_owned(), target, exchange.clone())
        .await
        .expect("PAT-only startup");
    assert_eq!(provider.bearer().await.expect("PAT credential"), PAT);
    assert_eq!(provider.fresh_bearer().await.expect("fresh PAT credential"), PAT);
    assert_eq!(exchange.calls.load(Ordering::SeqCst), 0);
    assert!(!format!("{provider:?}").contains(PAT));
}

#[tokio::test]
async fn session_startup_finishes_login_and_keeps_the_pat_for_fresh_calls() {
    let exchange = Arc::new(SuccessfulExchange::default());
    let target = session_target(Some(ISSUER), Some(AUDIENCE)).expect("session configuration");
    let provider = credentials(PAT.to_owned(), target, exchange.clone())
        .await
        .expect("session startup");
    assert_eq!(exchange.calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.bearer().await.expect("session credential"), SESSION);
    assert_eq!(provider.fresh_bearer().await.expect("fresh PAT credential"), PAT);
    assert_eq!(exchange.calls.load(Ordering::SeqCst), 1);
    let diagnostic = format!("{provider:?}");
    assert!(!diagnostic.contains(PAT) && !diagnostic.contains(SESSION));
}

#[tokio::test]
async fn failed_session_login_refuses_startup_without_a_pat_fallback() {
    let exchange = Arc::new(RefusedExchange::default());
    let target = session_target(Some(ISSUER), Some(AUDIENCE)).expect("session configuration");
    let result = credentials(PAT.to_owned(), target, exchange.clone()).await;
    let error = result.expect_err("refuse startup before the terminal is entered");
    assert_eq!(exchange.calls.load(Ordering::SeqCst), 1);
    assert!(!format!("{error:?} {error}").contains(PAT));
}

#[tokio::test]
async fn empty_pat_refuses_both_modes_before_an_exchange() {
    for target in [
        session_target(None, None).expect("PAT-only configuration"),
        session_target(Some(ISSUER), Some(AUDIENCE)).expect("session configuration"),
    ] {
        let exchange = Arc::new(RefusedExchange::default());
        assert!(
            credentials(String::new(), target, exchange.clone())
                .await
                .is_err()
        );
        assert_eq!(exchange.calls.load(Ordering::SeqCst), 0);
    }
}
