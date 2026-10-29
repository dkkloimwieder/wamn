use http_route::{
    AdapterLimits, Backend, BodyReadError, BodyReader, DeliveryReport, DeliveryRequest, Header,
    ProviderError, RequestHead, RouteDefinition, SchemaInvalid, handle_request,
};

use super::*;

pub(super) const BODY: &[u8] = b"{ \"z\": 1, \"a\": 2 }\n";

pub(super) fn signed(body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(b"test-secret").unwrap();
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

struct Fixture {
    credentials: crate::connection_credentials::test_util::Fixture,
    database: wamn_test_postgres::Database,
}

impl Fixture {
    async fn new() -> Self {
        let credentials = crate::connection_credentials::test_util::Fixture::new().await;
        let database = wamn_project_state::test_database::tenant_platform();
        database
            .execute(
                &[&wamn_control_provision::sql::ensure_workload_acl_role_sql(
                    wamn_control_provision::WorkloadRoleFamily::HttpAdmitter,
                )],
            )
            .unwrap();
        Self {
            credentials,
            database,
        }
    }

    async fn authenticator(
        &mut self,
        secret: &str,
        project: &str,
    ) -> Arc<PlatformRouteAuthenticator> {
        self.credentials
            .put("webhook", secret, &["host-project"])
            .await;
        self.credentials
            .admin
            .batch_execute("SET ROLE wamn_system")
            .await
            .unwrap();
        let principal: String = self.credentials.admin.query_one("SELECT c.service_principal_id::text FROM registry.connection c JOIN registry.integration i ON i.id=c.integration_id WHERE i.name='webhook' AND c.project='host-project'", &[]).await.unwrap().get(0);
        self.credentials
            .admin
            .batch_execute("RESET ROLE")
            .await
            .unwrap();
        let (client, driver) = tokio_postgres::connect(self.database.url(), tokio_postgres::NoTls)
            .await
            .unwrap();
        tokio::spawn(driver);
        client.execute("SELECT set_config('app.user_id',$1,false),set_config('app.operation','admin:connection-principal',false)", &[&principal]).await.unwrap();
        client.execute("INSERT INTO platform.\"user\"(id,type,email) VALUES ($1::text::uuid,'service','webhook@example.test') ON CONFLICT DO NOTHING", &[&principal]).await.unwrap();
        let base = crate::plugins::wamn_postgres::WamnPostgresConfig {
            credentials: None,
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 2000,
            statement_timeout_ms: 5000,
            row_limit: 100,
        };
        let projects = crate::plugins::wamn_postgres::StaticCredentialProvider::projects_from_json(
            &json!({(project):{"credentials":{"callable-http":self.database.url()}}}).to_string(),
            &base,
        )
        .unwrap();
        let postgres = Arc::new(crate::plugins::wamn_postgres::WamnPostgres::with_provider(
            Arc::new(crate::plugins::wamn_postgres::StaticCredentialProvider::new(projects, None)),
        ));
        let (identity, driver) =
            tokio_postgres::connect(self.credentials.database.url(), tokio_postgres::NoTls)
                .await
                .unwrap();
        tokio::spawn(driver);
        let authentication = Arc::new(
            RouteAuthentication::new(Arc::new(identity), postgres, "acme", project, "unused")
                .await
                .unwrap(),
        );
        Arc::new(
            PlatformRouteAuthenticator::default()
                .with_authentication(authentication)
                .with_signature_authentication(
                    Arc::new(self.credentials.reader(project).await),
                    project,
                ),
        )
    }

    async fn plugin(&mut self, secret: &str, project: &str) -> FlowHttpRouting {
        routing_policy(
            json!({"modes":["signature"],"header":"X-Signature","credential":"org:acme:webhook"}),
        )
        .with_authenticator(self.authenticator(secret, project).await)
    }

    async fn valid_plugin(&mut self) -> FlowHttpRouting {
        self.plugin(
            r#"{"hmac":{"algorithm":"sha256","secret":"test-secret"}}"#,
            "host-project",
        )
        .await
    }
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
        if let Some(caller) = &request.caller {
            assert_eq!(caller.credential_type(), CredentialType::Connection);
            assert!(caller.active_role().is_none());
        }
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
    let _lock = wamn_test_postgres::lock();
    let mut fixture = Fixture::new().await;
    let (unsigned, expected) = request(routing_policy(json!({"modes":["none"]})), vec![]).await;
    let (verified, delivered) = request(fixture.valid_plugin().await, headers(signed(BODY))).await;
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
    let (refused, delivered) = request(
        fixture.valid_plugin().await,
        headers(signed(expected[0].as_bytes())),
    )
    .await;
    assert_eq!(refused.status, 401);
    assert!(refused.body.is_empty());
    assert!(delivered.is_empty());
}

#[tokio::test]
async fn missing_bad_and_malformed_signatures_refuse_before_delivery() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = Fixture::new().await;
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
        let (response, delivered) = request(fixture.valid_plugin().await, header).await;
        assert_eq!(response.status, 401);
        assert!(response.body.is_empty());
        assert!(delivered.is_empty());
    }
}

#[tokio::test]
async fn missing_credentials_wrong_project_and_bad_secret_shapes_refuse() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = Fixture::new().await;
    for secret in [
        r#"{"headers":{"authorization":"test-secret"}}"#,
        r#"{"hmac":{"algorithm":"sha512","secret":"test-secret"}}"#,
        r#"{"hmac":{"algorithm":"sha256"}}"#,
        r#"{"hmac":{"algorithm":"sha256","secret":1}}"#,
        r#"{"hmac":{"algorithm":"sha256","secret":"test-secret","extra":1}}"#,
        r#"{"hmac":{"algorithm":"sha256","secret":"test-secret"},"extra":1}"#,
        "not json",
    ] {
        let (response, delivered) = request(
            fixture.plugin(secret, "host-project").await,
            headers(signed(BODY)),
        )
        .await;
        assert_eq!(response.status, 401);
        assert!(response.body.is_empty());
        assert!(delivered.is_empty());
    }
    for routing in [
        routing_policy(
            json!({"modes":["signature"],"header":"x-signature","credential":"org:acme:webhook"}),
        ),
        fixture
            .plugin(
                r#"{"hmac":{"algorithm":"sha256","secret":"test-secret"}}"#,
                "other-project",
            )
            .await,
        routing_policy(
            json!({"modes":["signature"],"header":"x-signature","credential":"org:acme:unknown"}),
        )
        .with_authenticator(Arc::new(
            PlatformRouteAuthenticator::default().with_signature_authentication(
                Arc::new(fixture.credentials.reader("host-project").await),
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

#[tokio::test]
async fn disabling_a_connection_refuses_signature_and_pat_principals() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = Fixture::new().await;
    let authenticator = fixture
        .authenticator(
            r#"{"hmac":{"algorithm":"sha256","secret":"test-secret"}}"#,
            "host-project",
        )
        .await;
    let signature = routing_policy(
        json!({"modes":["signature"],"header":"X-Signature","credential":"org:acme:webhook"}),
    )
    .with_authenticator(authenticator.clone());
    let pat = routing_policy(json!({"modes":["pat"]})).with_authenticator(authenticator);
    let principal: String = fixture.credentials.admin.query_one("SELECT c.service_principal_id::text FROM registry.connection c JOIN registry.integration i ON i.id=c.integration_id WHERE i.name='webhook' AND c.project='host-project'", &[]).await.unwrap().get(0);
    fixture
        .credentials
        .admin
        .batch_execute("BEGIN; SET LOCAL ROLE wamn_system")
        .await
        .unwrap();
    fixture
        .credentials
        .admin
        .batch_execute(&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
        .await
        .unwrap();
    fixture.credentials.admin.execute("INSERT INTO identity.project_roles(principal_id,org,project,role) VALUES ($1::text::uuid,'acme','host-project','admin')", &[&principal]).await.unwrap();
    let token = wamn_platform_identity::issue_pat(
        &fixture.credentials.admin,
        &principal.parse().unwrap(),
        "connection fixture",
        std::time::Duration::from_secs(3600),
    )
    .await
    .unwrap();
    fixture
        .credentials
        .admin
        .batch_execute("COMMIT")
        .await
        .unwrap();
    let signature_header = signed(BODY);
    let bearer = format!("Bearer {}", token.token());
    let signature_headers = [("X-Signature", signature_header.as_str())];
    let pat_headers = [("Authorization", bearer.as_str())];
    for (routing, headers) in [
        (&signature, signature_headers.as_slice()),
        (&pat, pat_headers.as_slice()),
    ] {
        assert_eq!(
            routing
                .authenticate_request_for_test("orders", headers, BODY)
                .await
                .unwrap()
                .unwrap()
                .principal_id(),
            principal
        );
    }
    for disable in [
        "UPDATE registry.integration SET status='disabled' WHERE project='host-project' AND name='webhook'",
        "UPDATE registry.connection SET status='disabled' WHERE project='host-project'",
    ] {
        fixture
            .credentials
            .admin
            .batch_execute("SET ROLE wamn_system")
            .await
            .unwrap();
        fixture
            .credentials
            .admin
            .batch_execute(disable)
            .await
            .unwrap();
        fixture
            .credentials
            .admin
            .batch_execute("RESET ROLE")
            .await
            .unwrap();
        for (routing, headers) in [
            (&signature, signature_headers.as_slice()),
            (&pat, pat_headers.as_slice()),
        ] {
            assert_eq!(
                routing
                    .authenticate_request_for_test("orders", headers, BODY)
                    .await
                    .unwrap_err()
                    .0,
                401
            );
        }
        fixture.credentials.admin.batch_execute("SET ROLE wamn_system; UPDATE registry.integration SET status='active' WHERE project='host-project'; UPDATE registry.connection SET status='active' WHERE project='host-project'; RESET ROLE").await.unwrap();
    }
}
