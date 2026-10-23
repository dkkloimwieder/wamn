use std::collections::HashMap;

use http_route::{
    AdapterLimits, Backend, BodyReadError, BodyReader, DeliveryReport, DeliveryRequest, Header,
    ProviderError, RequestHead, RouteDefinition, SchemaInvalid, handle_request,
};

use super::*;

const BODY: &[u8] = b"{ \"z\": 1, \"a\": 2 }\n";

fn signed(body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(b"test-secret").unwrap();
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

fn plugin(secret: &str, project: &str) -> FlowHttpRouting {
    let credentials = WamnCredentials::from_projects(HashMap::from([(
        "host-project".into(),
        HashMap::from([("webhook".into(), secret.into())]),
    )]));
    routing_policy(json!({
        "modes": ["signature"], "header": "X-Signature", "credential": "webhook"
    }))
    .with_authenticator(Arc::new(
        PlatformRouteAuthenticator::default()
            .with_signature_authentication(Arc::new(credentials), project),
    ))
}

fn valid_plugin() -> FlowHttpRouting {
    plugin(
        r#"{"hmac":{"algorithm":"sha256","secret":"test-secret"}}"#,
        "host-project",
    )
}

struct Adapter {
    routing: FlowHttpRouting,
    delivered: Vec<String>,
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "the adapter fixture implements the async guest backend"
)]
impl Backend for Adapter {
    type RoutePermit = ();
    type AuthenticatedCaller = AuthenticatedCaller;

    fn routes(&mut self, _: &str, _: &str) -> Result<Vec<RouteDefinition>, ProviderError> {
        Ok(vec![RouteDefinition {
            attachment_id: "orders".into(),
            host: "api.example.test".into(),
            path: "/orders".into(),
            method: "POST".into(),
            mappings: vec![],
            body_limit: 1024,
            mapped_limit: 1024,
            cache_control: None,
        }])
    }

    async fn authenticate(
        &mut self,
        attachment_id: &str,
        headers: &[Header],
        body: &[u8],
    ) -> Result<Option<AuthenticatedCaller>, http_route::AuthRejection> {
        let headers = headers
            .iter()
            .map(|h| (h.name.as_str(), h.value.as_str()))
            .collect::<Vec<_>>();
        self.routing
            .authenticate_request_for_test(attachment_id, &headers, body)
            .await
            .map_err(|(status, code)| http_route::AuthRejection { status, code })
    }

    fn validate_input(&mut self, _: &str, _: &str) -> Result<(), SchemaInvalid> {
        Ok(())
    }

    fn try_acquire_route(&mut self, _: &str) -> Result<Option<()>, ProviderError> {
        Ok(Some(()))
    }

    fn new_delivery_id(&mut self) -> String {
        "delivery".into()
    }

    async fn deliver(&mut self, request: DeliveryRequest<AuthenticatedCaller>) -> DeliveryReport {
        assert!(request.caller.is_none());
        self.delivered.push(request.payload);
        DeliveryReport {
            outcome: Ok(http_route::DeliveryOutcome::Respond("null".into())),
            deadline_adjustments: vec![],
            etag: None,
        }
    }
}

struct Body(Option<Vec<u8>>);

impl BodyReader for Body {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "the body fixture implements the async guest reader"
    )]
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, BodyReadError> {
        Ok(self.0.take())
    }
}

async fn request(
    routing: FlowHttpRouting,
    headers: Vec<Header>,
) -> (http_route::HttpResponse, Vec<String>) {
    let mut backend = Adapter {
        routing,
        delivered: vec![],
    };
    let response = handle_request(
        &mut backend,
        &mut Body(Some(BODY.to_vec())),
        &RequestHead {
            method: "POST".into(),
            authority: "api.example.test".into(),
            target: "/orders".into(),
            headers,
        },
        AdapterLimits::default(),
    )
    .await;
    (response, backend.delivered)
}

fn headers(signature: String) -> Vec<Header> {
    vec![Header {
        name: "x-signature".into(),
        value: signature,
    }]
}

#[tokio::test]
async fn exact_body_signature_delivers_the_same_json_as_none() {
    let (unsigned, expected) = request(routing_policy(json!({"modes":["none"]})), vec![]).await;
    let (verified, delivered) = request(valid_plugin(), headers(signed(BODY))).await;
    assert_eq!(unsigned.status, 200);
    assert_eq!(verified.status, 200);
    assert_eq!(delivered, expected);
    assert_eq!(delivered.len(), 1);
    assert_eq!(
        serde_json::from_str::<Value>(&delivered[0]).unwrap(),
        json!({"a":2,"z":1})
    );
    assert_ne!(delivered[0].as_bytes(), BODY);

    // A signature over re-encoded JSON must not authenticate the raw request.
    let (refused, delivered) =
        request(valid_plugin(), headers(signed(expected[0].as_bytes()))).await;
    assert_eq!(refused.status, 401);
    assert!(refused.body.is_empty());
    assert!(delivered.is_empty());
}

#[tokio::test]
async fn missing_bad_and_malformed_signatures_refuse_before_delivery() {
    for header in [
        vec![],
        headers(signed(b"wrong body")),
        headers(signed(BODY).to_uppercase()),
        headers(format!("sha256={}", signed(BODY)[7..].to_uppercase())),
        headers("sha256=00".into()),
        headers("sha256=gg".repeat(32)),
        headers(format!("{} ", signed(BODY))),
        [headers(signed(BODY)), headers(signed(BODY))].concat(),
    ] {
        let (response, delivered) = request(valid_plugin(), header).await;
        assert_eq!(response.status, 401);
        assert!(response.body.is_empty());
        assert!(delivered.is_empty());
    }
}

#[tokio::test]
async fn missing_credentials_wrong_project_and_bad_secret_shapes_refuse() {
    for secret in [
        r#"{"headers":{"authorization":"test-secret"}}"#,
        r#"{"hmac":{"algorithm":"sha512","secret":"test-secret"}}"#,
        r#"{"hmac":{"algorithm":"sha256"}}"#,
        r#"{"hmac":{"algorithm":"sha256","secret":1}}"#,
        r#"{"hmac":{"algorithm":"sha256","secret":"test-secret","extra":1}}"#,
        r#"{"hmac":{"algorithm":"sha256","secret":"test-secret"},"extra":1}"#,
        "not json",
    ] {
        let (response, delivered) =
            request(plugin(secret, "host-project"), headers(signed(BODY))).await;
        assert_eq!(response.status, 401);
        assert!(response.body.is_empty());
        assert!(delivered.is_empty());
    }
    for routing in [
        routing_policy(
            json!({"modes":["signature"],"header":"x-signature","credential":"webhook"}),
        ),
        plugin(
            r#"{"hmac":{"algorithm":"sha256","secret":"test-secret"}}"#,
            "other-project",
        ),
        routing_policy(
            json!({"modes":["signature"],"header":"x-signature","credential":"unknown"}),
        )
        .with_authenticator(Arc::new(
            PlatformRouteAuthenticator::default().with_signature_authentication(
                Arc::new(WamnCredentials::from_projects(HashMap::from([(
                    "host-project".into(),
                    HashMap::new(),
                )]))),
                "host-project",
            ),
        )),
    ] {
        let (response, delivered) = request(routing, headers(signed(BODY))).await;
        assert_eq!(response.status, 401);
        assert!(response.body.is_empty());
        assert!(delivered.is_empty());
    }
}
