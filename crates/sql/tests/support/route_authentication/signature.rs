use http_route::{
    AdapterLimits, Backend, BodyReadError, BodyReader, Header, HttpResponse, OperationRequest,
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
    credentials: wamn_connections::connection_credentials::test_util::Fixture,
    database: url::Url,
    _server: wamn_test_postgres::OwnedPostgres,
    memory: Arc<wamn_connections::connection_memory::ConnectionMemory>,
}

impl Fixture {
    async fn new() -> Self {
        let mut project_server = wamn_test_postgres::start(&[]).unwrap();
        let control = project_server.create_database("control").await.unwrap();
        let credentials = wamn_connections::connection_credentials::test_util::Fixture::new(
            url::Url::parse(control.url()).unwrap(),
        )
        .await;
        let coordinate = project_server.create_database("project").await.unwrap();
        let database = url::Url::parse(coordinate.url()).unwrap();
        wamn_platform_identity::test_database::project_platform(database.as_str()).await;
        Self {
            credentials,
            database,
            _server: project_server,
            memory: Arc::new(wamn_connections::connection_memory::ConnectionMemory::default()),
        }
    }

    async fn authenticator(
        &mut self,
        secret: &str,
        project: &wamn_registry::ProjectId,
    ) -> Arc<PlatformRouteAuthenticator> {
        self.credentials
            .put("webhook", secret, &["host-project"])
            .await;
        sqlx::raw_sql("SET ROLE wamn_system")
            .execute(&mut self.credentials.admin)
            .await
            .map(drop)
            .unwrap();
        let principal: wamn_identity_protocol::PrincipalId = sqlx::query("SELECT c.service_principal_id FROM registry.connection c JOIN registry.integration i ON i.id=c.integration_id WHERE i.name='webhook' AND c.project='host-project'").fetch_one(&mut self.credentials.admin).await.unwrap().get(0);
        sqlx::raw_sql("RESET ROLE")
            .execute(&mut self.credentials.admin)
            .await
            .map(drop)
            .unwrap();
        let mut client = wamn_postgres::connect(self.database.as_str())
            .await
            .unwrap();

        sqlx::raw_sql("BEGIN")
            .execute(&mut client)
            .await
            .expect("begin fixture transaction");
        sqlx::query("INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES($1,'','fixture','admin:connection-principal')").bind(&principal).execute(&mut client).await.map(|result| result.rows_affected()).unwrap();
        sqlx::query("INSERT INTO platform.\"user\"(id,type,email) VALUES ($1,'service','webhook@example.test') ON CONFLICT DO NOTHING").bind(&principal).execute(&mut client).await.map(|result| result.rows_affected()).unwrap();
        sqlx::raw_sql("COMMIT")
            .execute(&mut client)
            .await
            .expect("commit fixture transaction");
        let base = wamn_sql::WamnPostgresConfig {
            credentials: None,
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 2000,
            statement_timeout_ms: 5000,
        };
        let projects = wamn_sql::StaticCredentialProvider::projects_from_json(
            &json!({(project.as_str()):{"credentials":{"callable-http":self.database.as_str()}}})
                .to_string(),
            &base,
        )
        .unwrap();
        let postgres = Arc::new(wamn_sql::WamnPostgres::with_provider(Arc::new(
            wamn_sql::StaticCredentialProvider::new(projects, None),
        )));
        let facts = Arc::new(wamn_connections::connection_facts::ConnectionFacts::new(
            Arc::clone(&self.memory),
            Arc::new(self.credentials.reader(project).await),
            &postgres,
            &wamn_registry::project_env_key(
                &"acme".parse().unwrap(),
                project,
                &"dev".parse().unwrap(),
            ),
            project,
            &"dev".parse().unwrap(),
        ));
        let identity_reader = sqlx::postgres::PgPoolOptions::new()
            .after_connect(|connection, _| {
                Box::pin(async move {
                    sqlx::query("SET ROLE wamn_identity_reader")
                        .execute(connection)
                        .await?;
                    Ok(())
                })
            })
            .connect_lazy_with(
                wamn_postgres::connect_options(self.credentials.database.as_str())
                    .await
                    .unwrap(),
            );
        Arc::new(PlatformRouteAuthenticator::new(
            postgres,
            [facts],
            Some((identity_reader, std::collections::HashMap::new())),
        ))
    }

    async fn plugin(
        &mut self,
        secret: &str,
        project: &wamn_registry::ProjectId,
    ) -> FlowHttpRouting {
        routing_policy(
            json!({"modes":["signature"],"header":"X-Signature","credential":"org:acme:webhook"}),
        )
        .with_authenticator(self.authenticator(secret, project).await)
    }

    async fn valid_plugin(&mut self) -> FlowHttpRouting {
        self.plugin(
            r#"{"hmac":{"algorithm":"sha256","secret":"test-secret"}}"#,
            &"host-project".parse().unwrap(),
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
    type AuthenticatedCaller = AuthenticatedCaller;

    fn routes(&mut self, _: &str, _: &str) -> Result<Vec<RouteDefinition>, ProviderError> {
        Ok(vec![RouteDefinition {
            attachment_id: "orders".into(),
            path: "/orders".into(),
            method: "POST".into(),
            body_limit: 1024,
            cache_control: None,
            sealed_operation_id: "orders:order/create@1.0.0".into(),
            idempotency_rule: Some("request_id".into()),
            etag_inputs: None,
            operation_deadline_ms: 30_000,
            read_limits: None,
            auth_policy: json!({"modes":["signature"]}),
        }])
    }

    async fn authenticate(
        &mut self,
        route: &RouteDefinition,
        headers: &[Header],
        body: &[u8],
    ) -> Result<Option<AuthenticatedCaller>, http_route::AuthRejection> {
        let headers = headers
            .iter()
            .map(|h| (h.name.as_str(), h.value.as_str()))
            .collect::<Vec<_>>();
        self.routing
            .authenticate_request_for_test(&route.attachment_id.parse().unwrap(), &headers, body)
            .await
            .map_err(|(status, code)| http_route::AuthRejection { status, code })
    }

    fn validate_input(&mut self, _: &RouteDefinition, _: &str) -> Result<(), SchemaInvalid> {
        Ok(())
    }

    async fn invoke(
        &mut self,
        _route: &RouteDefinition,
        request: OperationRequest<AuthenticatedCaller>,
    ) -> HttpResponse {
        if let Some(caller) = &request.caller {
            assert_eq!(caller.credential_type(), CredentialType::Connection);
            assert!(caller.active_role().is_none());
        }
        self.delivered.push(request.payload);
        HttpResponse {
            status: 200,
            content_type: "application/json",
            body: b"null".to_vec(),
            read: None,
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
#[ignore = "requires: postgres"]
async fn exact_body_signature_delivers_the_same_json_as_none() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = Fixture::new().await;
    let (unsigned, expected) = request(routing_policy(json!({"modes":["none"]})), vec![]).await;
    let (verified, delivered) = request(fixture.valid_plugin().await, headers(signed(BODY))).await;
    assert_eq!(unsigned.status, 200);
    assert_eq!(verified.status, 200);
    assert_eq!(delivered, expected);
    assert_eq!(delivered.len(), 1);
    {
        let observed = &(serde_json::from_str::<Value>(&delivered[0]).unwrap());
        assert_eq!(observed["a"], serde_json::json!(2));
        assert_eq!(observed["z"], serde_json::json!(1));
    };
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
#[ignore = "requires: postgres"]
async fn missing_bad_and_malformed_signatures_refuse_before_delivery() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = Fixture::new().await;
    for header in [
        vec![],
        headers(signed(b"wrong body")),
        headers(signed(BODY).to_uppercase()),
        [headers(signed(BODY)), headers(signed(BODY))].concat(),
    ] {
        let (response, delivered) = request(fixture.valid_plugin().await, header).await;
        assert_eq!(response.status, 401);
        assert!(response.body.is_empty());
        assert!(delivered.is_empty());
    }
}

#[tokio::test]
#[ignore = "requires: postgres"]
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
            fixture
                .plugin(secret, &"host-project".parse().unwrap())
                .await,
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
                &"other-project".parse().unwrap(),
            )
            .await,
        routing_policy(
            json!({"modes":["signature"],"header":"x-signature","credential":"org:acme:unknown"}),
        )
        .with_authenticator(
            fixture
                .authenticator(
                    r#"{"hmac":{"algorithm":"sha256","secret":"test-secret"}}"#,
                    &"host-project".parse().unwrap(),
                )
                .await,
        ),
    ] {
        let (response, delivered) = request(routing, headers(signed(BODY))).await;
        assert_eq!(response.status, 401);
        assert!(response.body.is_empty());
        assert!(delivered.is_empty());
    }
}

#[tokio::test]
#[ignore = "requires: postgres"]
async fn disabling_a_connection_refuses_signature_and_pat_principals() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = Fixture::new().await;
    let authenticator = fixture
        .authenticator(
            r#"{"hmac":{"algorithm":"sha256","secret":"test-secret"}}"#,
            &"host-project".parse().unwrap(),
        )
        .await;
    let signature = routing_policy(
        json!({"modes":["signature"],"header":"X-Signature","credential":"org:acme:webhook"}),
    )
    .with_authenticator(Arc::<PlatformRouteAuthenticator>::clone(&authenticator));
    let pat = routing_policy(json!({"modes":["pat"]})).with_authenticator(authenticator);
    let principal: wamn_identity_protocol::PrincipalId = sqlx::query("SELECT c.service_principal_id FROM registry.connection c JOIN registry.integration i ON i.id=c.integration_id WHERE i.name='webhook' AND c.project='host-project'").fetch_one(&mut fixture.credentials.admin).await.unwrap().get(0);
    sqlx::raw_sql("BEGIN; SET LOCAL ROLE wamn_system")
        .execute(&mut fixture.credentials.admin)
        .await
        .map(drop)
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
            .to_string(),
    ))
    .execute(&mut fixture.credentials.admin)
    .await
    .map(drop)
    .unwrap();
    sqlx::query("INSERT INTO identity.project_roles(principal_id,org,project,role) VALUES ($1,'acme','host-project','admin')").bind(&principal).execute(&mut fixture.credentials.admin).await.map(|result| result.rows_affected()).unwrap();
    let token = wamn_platform_identity::issue_pat(
        &mut fixture.credentials.admin,
        &principal,
        "connection fixture",
        std::time::Duration::from_secs(3600),
    )
    .await
    .unwrap();
    sqlx::raw_sql("COMMIT")
        .execute(&mut fixture.credentials.admin)
        .await
        .map(drop)
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
                .authenticate_request_for_test(&"orders".parse().unwrap(), headers, BODY)
                .await
                .unwrap()
                .unwrap()
                .principal_id(),
            &principal
        );
    }
    for disable in [
        "UPDATE registry.integration SET status='disabled' WHERE project='host-project' AND name='webhook'",
        "UPDATE registry.connection SET status='disabled' WHERE project='host-project'",
    ] {
        sqlx::raw_sql("SET ROLE wamn_system")
            .execute(&mut fixture.credentials.admin)
            .await
            .map(drop)
            .unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe((disable).to_string()))
            .execute(&mut fixture.credentials.admin)
            .await
            .map(drop)
            .unwrap();
        sqlx::raw_sql("RESET ROLE")
            .execute(&mut fixture.credentials.admin)
            .await
            .map(drop)
            .unwrap();
        for (routing, headers) in [
            (&signature, signature_headers.as_slice()),
            (&pat, pat_headers.as_slice()),
        ] {
            assert_eq!(
                routing
                    .authenticate_request_for_test(&"orders".parse().unwrap(), headers, BODY)
                    .await
                    .unwrap_err()
                    .0,
                401
            );
        }
        sqlx::raw_sql("SET ROLE wamn_system; UPDATE registry.integration SET status='active' WHERE project='host-project'; UPDATE registry.connection SET status='active' WHERE project='host-project'; RESET ROLE").execute(&mut fixture.credentials.admin).await.map(drop).unwrap();
    }
}
