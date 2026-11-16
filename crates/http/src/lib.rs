//! Environment-bound HTTP for ordinary `wamn:node` components.
//!
//! The host binds the exact entry (a route, or a wiring node position and
//! occurrence) and the component digest before invoking a pooled component. The
//! guest names only a store alias; the database must resolve that alias at the
//! component grain and the mounted manifest must contain the exact route or
//! wiring version/hash and the component tuple. No run, plan, frame or effect
//! record participates.

use std::collections::HashMap;
#[cfg(test)]
use std::sync::Mutex;

use anyhow::Context as _;

use wash_runtime::host::allowed_hosts::AllowedHost;

use wamn_authority::connection_authority::{
    AuthorityDecision, AuthorityError, HttpConnectionAuthority, TokioDnsResolver,
    bound_http_target, bound_oauth_token, resolve_http_request,
};

mod authentication;
mod jwt;
mod network_policy;
pub mod transport;

use transport::HttpTransport;
use crate::oauth::{Request as Oauth2Request, Response as Oauth2Token};

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        path: [
            "../../wit/wamn-connection",
            "../../wit/wamn-flow-http-routing",
            "../../wit/wamn-postgres-0.4",
            "../../wit/wamn-node",
            "../../wit/wamn-operation-0.1",
            "../../wit/wasmcloud-blobstore",
            "../authority/wit",
        ],
        world: "wamn:host/connection-http-plugin@0.1.0",
        imports: { default: async | trappable | tracing },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wamn::connection::http::Request;
#[cfg(test)]
use bindings::wamn::connection::http::Header;
use wamn_authority::connection::ConnectionError;

fn oauth2_request(
    auth_type: &str,
    parameters: &serde_json::Value,
    material: Option<&str>,
    refresh_token: Option<&str>,
) -> Result<Oauth2Request, ConnectionError> {
    if auth_type == "oauth2_client_credentials_jwt" {
        jwt::request(parameters, material, chrono::Utc::now().timestamp())
    } else {
        Oauth2Request::credentials(auth_type, parameters, material, refresh_token)
    }
}

/// Apply the same origin, path, address, and pinning rules to every OAuth grant.
pub(crate) async fn token_endpoint(
    connection: &HttpConnectionAuthority,
    token_authority: Option<&str>,
    token_url: &str,
    platform_hosts: Option<&[AllowedHost]>,
) -> Result<AuthorityDecision, AuthorityError> {
    let (authority, target, endpoint) = bound_oauth_token(connection, token_authority, token_url)?;
    let url = url::Url::parse(authority.canonical_base_url()).expect("canonical token URL");
    let exact = [AllowedHost::Authority(
        format!(
            "{}:{}",
            url.host_str().expect("canonical token host"),
            authority.authority().port()
        )
        .parse()
        .expect("canonical token authority"),
    )];
    let mut decision = resolve_http_request(
        &authority,
        &target,
        platform_hosts.unwrap_or(&exact),
        &network_policy::allows,
        &TokioDnsResolver,
    )
    .await?;
    decision.logical_url = endpoint;
    Ok(decision)
}

fn reserved_header(name: &hyper::header::HeaderName) -> bool {
    matches!(
        name.as_str(),
        "host"
            | "content-length"
            | "transfer-encoding"
            | "connection"
            | "keep-alive"
            | "te"
            | "trailer"
            | "upgrade"
            | "proxy-connection"
            | "proxy-authorization"
            | "idempotency-key"
            | "x-http-method-override"
            | "x-http-method"
    )
}

fn outbound_headers(
    request: &Request,
    credentials: HashMap<String, String>,
) -> Result<hyper::header::HeaderMap, ConnectionError> {
    let mut headers = hyper::header::HeaderMap::new();
    for header in &request.headers {
        let name = hyper::header::HeaderName::from_bytes(header.name.as_bytes())
            .map_err(|_| ConnectionError::AuthorityDenied)?;
        if matches!(name.as_str(), "x-http-method-override" | "x-http-method") {
            continue;
        }
        if reserved_header(&name) {
            return Err(ConnectionError::AuthorityDenied);
        }
        let value = hyper::header::HeaderValue::from_bytes(&header.value)
            .map_err(|_| ConnectionError::AuthorityDenied)?;
        headers.append(name, value);
    }
    for (name, value) in credentials {
        let name = hyper::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| ConnectionError::CredentialUnavailable)?;
        let value = hyper::header::HeaderValue::from_str(&value)
            .map_err(|_| ConnectionError::CredentialUnavailable)?;
        headers.insert(name, value);
    }
    if let Some(key) = &request.idempotency_key {
        let value =
            hyper::header::HeaderValue::from_str(key).map_err(|_| ConnectionError::Incompatible)?;
        headers.insert("idempotency-key", value);
    }
    inject_trace_context(&mut headers);
    Ok(headers)
}

/// The W3C fields of the effect span, written onto one outbound request.
///
/// A field the guest already sent stays: `guests/http-request`
/// forwards its node context's traceparent with `push_header_unless_present`,
/// and the host adds context rather than rewriting the guest's.
struct TraceContextHeaders<'a> {
    headers: &'a mut hyper::header::HeaderMap,
}

impl opentelemetry::propagation::Injector for TraceContextHeaders<'_> {
    fn set(&mut self, key: &str, value: String) {
        // The W3C propagator writes `tracestate` even when the context carries
        // none, and an empty field is not a field.
        if value.is_empty() {
            return;
        }
        let Ok(name) = hyper::header::HeaderName::from_bytes(key.as_bytes()) else {
            return;
        };
        if self.headers.contains_key(&name) {
            return;
        }
        let Ok(value) = hyper::header::HeaderValue::from_str(&value) else {
            return;
        };
        self.headers.insert(name, value);
    }
}

/// [12.6] Put the running `wamn.connection_http` span on the wire.
///
/// Without this the only traceparent leaving the process was whatever the guest
/// happened to forward, so a downstream service parented to the host's caller
/// and never saw the effect at all.
///
/// The span is read through `tracing`, never `opentelemetry::global::tracer`:
/// the runtime's `initialize_observability` installs a `tracing-opentelemetry`
/// layer and a propagator but NO global tracer provider, so the global tracer
/// silently answers with a no-op span.
///
/// Public because the `trace-test` condition drives THIS function rather than a
/// copy of it (`wamn-k9ea`): the condition's whole assertion is that what crosses the
/// process boundary is what production injects, so a reimplementation there
/// does not test this one.
pub fn inject_trace_context(headers: &mut hyper::header::HeaderMap) {
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    let context = tracing::Span::current().context();
    opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&context, &mut TraceContextHeaders { headers });
    });
}

async fn fetch_health_token(
    transport: &HttpTransport,
    decision: AuthorityDecision,
    request: &Oauth2Request,
) -> Result<Oauth2Token, ConnectionError> {
    request
        .send(&crate::oauth::Transport {
            http: transport.clone(),
            decision,
        })
        .await
}

/// Test candidate HTTP parameters without writing connection facts.
pub async fn health(
    transport: &HttpTransport,
    parameters: &serde_json::Value,
    auth_type: &str,
    credential: Option<&str>,
) -> Result<(), String> {
    async {
        let authority = wamn_authority::connection_authority::parse_http_definition(parameters)?;
        let url = url::Url::parse(authority.canonical_base_url())?;
        anyhow::ensure!(url.scheme() == "https", "HTTP health requires TLS");
        let host = url.host_str().context("HTTP authority has no host")?;
        let port = url
            .port_or_known_default()
            .context("HTTP authority has no port")?;
        let allowed = [AllowedHost::Authority(format!("{host}:{port}").parse()?)];
        let (target, endpoint) = bound_http_target(&authority, authority.canonical_base_url())?;
        let mut decision = resolve_http_request(
            &authority,
            &target,
            &allowed,
            &network_policy::allows,
            &TokioDnsResolver,
        )
        .await?;
        decision.logical_url = endpoint;
        let request = Request {
            requirement: "health".into(),
            method: "HEAD".into(),
            path_and_query: "/".into(),
            headers: Vec::new(),
            body: None,
            idempotency_key: None,
        };
        let auth = parameters
            .get("auth")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        let headers = if matches!(
            auth_type,
            "oauth2_client_credentials" | "oauth2_client_credentials_jwt"
        ) {
            let token_request = oauth2_request(auth_type, &auth, credential, None)
                .map_err(|_| anyhow::anyhow!("HTTP authentication unavailable"))?;
            let token_decision = token_endpoint(
                &authority,
                token_request.token_authority.as_deref(),
                &token_request.token_url,
                None,
            )
            .await?;
            let token = fetch_health_token(transport, token_decision, &token_request)
                .await
                .map_err(|_| anyhow::anyhow!("HTTP authentication unavailable"))?;
            HashMap::from([(
                "authorization".into(),
                format!("Bearer {}", token.access_token),
            )])
        } else {
            authentication::headers(auth_type, &auth, credential, &request, &mut decision)
                .map_err(|_| anyhow::anyhow!("HTTP authentication unavailable"))?
        };
        let headers = outbound_headers(&request, headers)
            .map_err(|_| anyhow::anyhow!("HTTP authentication unavailable"))?;
        let mut outbound = hyper::Request::builder()
            .method("HEAD")
            .uri(decision.logical_url.as_ref())
            .body(&[] as &[u8])?;
        *outbound.headers_mut() = headers;
        let response = transport
            .send_once(&decision, outbound)
            .await
            .map_err(|_| anyhow::anyhow!("HTTP health request failed"))?;
        health_status(response.status().as_u16())
    }
    .await
    .map_err(|error: anyhow::Error| error.to_string())
}

fn health_status(status: u16) -> anyhow::Result<()> {
    anyhow::ensure!(!matches!(status, 401 | 403 | 500..=599), "{status}");
    Ok(())
}

#[cfg(test)]
mod tests {

    use super::*;
    use std::sync::Arc;
    use crate::bindings::wamn::connection::http::Header;

    use crate::test_util::{
        effect_request, interface_address,
    };

    mod oauth2_tests;

    /// The hash of the fixture wiring's graph, the one the manifest carries.

    #[test]
    fn guest_headers_cannot_supply_transport_or_proxy_authority() {
        for name in [
            "idempotency-key",
            "host",
            "content-length",
            "transfer-encoding",
            "proxy-authorization",
            "connection",
        ] {
            for name in [name.to_owned(), name.to_ascii_uppercase()] {
                let request = Request {
                    idempotency_key: Some("host".into()),
                    ..effect_request(vec![Header {
                        name,
                        value: b"guest".to_vec(),
                    }])
                };
                assert!(matches!(
                    outbound_headers(&request, HashMap::new()),
                    Err(ConnectionError::AuthorityDenied)
                ));
            }
        }
        let request = Request {
            idempotency_key: Some("host".into()),
            ..effect_request(vec![
                Header {
                    name: "x-http-method".into(),
                    value: b"DELETE".to_vec(),
                },
                Header {
                    name: "X-HTTP-Method-Override".into(),
                    value: b"PUT".to_vec(),
                },
            ])
        };
        let headers = outbound_headers(&request, HashMap::new()).unwrap();
        assert!(!headers.contains_key("x-http-method"));
        assert!(!headers.contains_key("x-http-method-override"));
        assert_eq!(request.method, "POST");
        assert_eq!(
            headers["idempotency-key"],
            request.idempotency_key.as_ref().unwrap()
        );
    }

    #[test]
    fn the_effect_span_is_injected_onto_the_outbound_request() {
        use opentelemetry::propagation::TextMapPropagator as _;
        use opentelemetry::trace::{TraceContextExt as _, TracerProvider as _};
        use tracing_opentelemetry::OpenTelemetrySpanExt as _;
        use tracing_subscriber::layer::SubscriberExt as _;

        let propagator = opentelemetry_sdk::propagation::TraceContextPropagator::new();
        opentelemetry::global::set_text_map_propagator(propagator.clone());
        let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder().build();
        let subscriber = tracing_subscriber::registry().with(
            tracing_opentelemetry::layer().with_tracer(provider.tracer("connection-http-test")),
        );
        let _guard = tracing::subscriber::set_default(subscriber);
        let span = tracing::info_span!("wamn.connection_http");
        let _entered = span.enter();
        let guest = HashMap::from([(
            "traceparent".to_owned(),
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".to_owned(),
        )]);
        let guest_context = propagator.extract(&guest);
        for (fields, expected) in [(HashMap::new(), span.context()), (guest, guest_context)] {
            let request = effect_request(
                fields
                    .into_iter()
                    .map(|(name, value)| Header {
                        name,
                        value: value.into_bytes(),
                    })
                    .collect(),
            );
            let headers = outbound_headers(&request, HashMap::new()).unwrap();
            assert!(!headers.contains_key("tracestate"));
            let fields: HashMap<String, String> = headers
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_str().unwrap().to_owned()))
                .collect();
            let actual = propagator.extract(&fields);
            let actual_span = actual.span();
            let actual = actual_span.span_context();
            let expected_span = expected.span();
            let expected = expected_span.span_context();
            assert!(actual.is_valid());
            assert_eq!(actual.trace_id(), expected.trace_id());
            assert_eq!(actual.span_id(), expected.span_id());
            assert_eq!(actual.trace_flags(), expected.trace_flags());
            assert_eq!(actual.trace_state(), expected.trace_state());
        }
    }

}

#[cfg(test)]
mod connection_mode_tests {
    #[test]
    fn health_accepts_only_the_ruled_statuses() {
        for status in [200, 204, 301, 400, 404, 429] {
            super::health_status(status).unwrap();
        }
        for status in [401, 403, 500, 503, 599] {
            super::health_status(status).expect_err("unhealthy status");
        }
    }

}

mod oauth;
pub mod oauth_authorization;

/// Shared HTTP fixtures for host invocation tests.
#[cfg(any(test, feature = "test-util"))]
pub mod test_util;
