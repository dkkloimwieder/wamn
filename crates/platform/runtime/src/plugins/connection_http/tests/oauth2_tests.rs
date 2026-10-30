//! OAuth2 credentials exercise the host send path over a local listener.

use std::collections::VecDeque;
use std::convert::Infallible;

use http_body_util::{BodyExt as _, Full};
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use super::*;

const CLIENT_ID: &str = "private client:id+";
const CLIENT_SECRET: &str = "private/secret&+";

struct Observed {
    path: String,
    method: String,
    headers: hyper::HeaderMap,
    body: Vec<u8>,
}

struct Fixture {
    base: String,
    observed: Arc<Mutex<Vec<Observed>>>,
    task: JoinHandle<()>,
    credentials: crate::connection_credentials::test_util::Fixture,
    applications: Vec<wamn_test_postgres::Database>,
}

impl Fixture {
    async fn start(tokens: Vec<(u16, String)>, api: Vec<u16>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/bound/", listener.local_addr().unwrap());
        let observed = Arc::new(Mutex::new(Vec::new()));
        let recorded = observed.clone();
        let tokens = Arc::new(Mutex::new(VecDeque::from(tokens)));
        let api = Arc::new(Mutex::new(VecDeque::from(api)));
        let task = tokio::spawn(async move {
            loop {
                let (tcp, _) = listener.accept().await.unwrap();
                let observed = recorded.clone();
                let tokens = tokens.clone();
                let api = api.clone();
                hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        TokioIo::new(tcp),
                        service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                            let observed = observed.clone();
                            let tokens = tokens.clone();
                            let api = api.clone();
                            async move {
                                let (parts, body) = request.into_parts();
                                let body = body.collect().await.unwrap().to_bytes().to_vec();
                                let token = matches!(parts.uri.path(), "/bound/token" | "/bound/");
                                observed.lock().unwrap().push(Observed {
                                    path: parts.uri.to_string(),
                                    method: parts.method.to_string(),
                                    headers: parts.headers,
                                    body,
                                });
                                let (status, body) = if token {
                                    tokens
                                        .lock()
                                        .unwrap()
                                        .pop_front()
                                        .unwrap_or((500, "unexpected token fetch".into()))
                                } else {
                                    let status = api.lock().unwrap().pop_front().unwrap_or(500);
                                    (status, format!("api-{status}"))
                                };
                                Ok::<_, Infallible>(
                                    hyper::Response::builder()
                                        .status(status)
                                        .header("connection", "close")
                                        .body(Full::new(bytes::Bytes::from(body)))
                                        .unwrap(),
                                )
                            }
                        }),
                    )
                    .await
                    .unwrap();
            }
        });
        Self {
            credentials: crate::connection_credentials::test_util::Fixture::new().await,
            base,
            observed,
            task,
            applications: Vec::new(),
        }
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "token_url": format!("{}token?audience=api", self.base),
            "client_id": CLIENT_ID,
            "scopes": ["orders:read", "orders:write+"]
        })
    }

    async fn put_http(&mut self, name: &str, parameters: &serde_json::Value, projects: &[&str]) {
        let secret = serde_json::json!({"client_secret": CLIENT_SECRET}).to_string();
        for project in projects {
            let tx = self.credentials.admin.transaction().await.unwrap();
            tx.batch_execute("SET LOCAL ROLE wamn_system")
                .await
                .unwrap();
            tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning,
            ))
            .await
            .unwrap();
            wamn_test_infrastructure::credentials::define(
                &tx,
                "acme",
                &serde_json::json!({
                    "project": project, "env": "dev", "name": name,
                    "integration_type": "http", "auth_type": "oauth2_client_credentials",
                    "parameters": {"primary-authority": self.base, "tls-verification": "disabled", "auth": parameters},
                    "credential": secret,
                }),
                self.credentials.store.as_ref(),
            ).await.unwrap();
            tx.commit().await.unwrap();
        }
    }

    async fn plugin(&mut self, parameters: &serde_json::Value) -> ConnectionHttp {
        use crate::plugins::wamn_postgres::{
            ClassCredentials, ProjectConfig, StaticCredentialProvider,
        };
        use wamn_engine::release_manifest::ReleaseScope;
        use wamn_run_state::AuthorityClass;

        for name in ["handle", "other-handle"] {
            self.put_http(name, parameters, &["project-a", "project-b"])
                .await;
        }
        let mut manifest = manifest();
        let mut invocation = invocation();
        invocation.entry = InvocationEntry::Route;
        let mut component = manifest.components.pop_first().unwrap();
        let operation = component.operations.get_mut(&invocation.operation).unwrap();
        operation.registered_operation = None;
        operation.permissions.clear();
        component.descriptor.operations.insert(
            invocation.operation.clone(),
            serde_json::from_value(serde_json::json!({
                "input-ports": [], "output-ports": [], "parameters": []
            }))
            .unwrap(),
        );
        manifest.components.insert(component);
        manifest.routes.insert(ServingRoute {
            package_id: invocation.origin.package_id.clone(),
            component: invocation.origin.component.clone(),
            operation: invocation.origin.operation.clone(),
            type_: OperationType::Command,
            reads: BTreeSet::new(),
            revision: None,
            idempotency: None,
            canonicalization: None,
        });
        let bytes = manifest.canonical_bytes();
        let release = Arc::new(
            LoadedRelease::load_canonical_bytes(
                &bytes,
                "oauth2-test",
                ReleaseScope::new("acme--project-a--dev", "dev"),
            )
            .unwrap(),
        );
        let manifest_digest = release.release().manifest_digest.as_str();
        let mut projects = HashMap::new();
        for project in ["project-a", "project-b"] {
            let database = wamn_project_state::test_database::tenant_platform();
            let (admin, driver) = tokio_postgres::connect(database.url(), tokio_postgres::NoTls)
                .await
                .unwrap();
            tokio::spawn(driver);
            admin.batch_execute("BEGIN; SELECT set_config('app.user_id','770df186-ac15-579e-b46b-c297cae2011b',true),set_config('app.operation','admin:seed-oauth2',true)").await.unwrap();
            let env_key = format!("acme--{project}--dev");
            admin.execute("INSERT INTO catalog.releases(env_key,manifest_digest,canonical_bytes) VALUES($1,$2,$3)", &[&env_key,&manifest_digest,&bytes]).await.unwrap();
            let rows = self.credentials.admin.query("SELECT c.id,i.id,i.name FROM registry.connection c JOIN registry.integration i ON i.id=c.integration_id WHERE c.project=$1 AND c.env='dev'", &[&project]).await.unwrap();
            for row in rows {
                let connection: uuid::Uuid = row.get(0);
                let integration: uuid::Uuid = row.get(1);
                let name: String = row.get(2);
                admin.execute("INSERT INTO platform.integration(id,integration_type,name,status) VALUES($1,'http',$2,'active')", &[&integration,&name]).await.unwrap();
                admin.execute("INSERT INTO platform.connection(id,integration_id,integration_type,status) VALUES($1,$2,'http','active')", &[&connection,&integration]).await.unwrap();
                admin.execute("INSERT INTO catalog.connection_requirement(env_key,component_digest,package_id,name,interfaces,mode) VALUES($1,$2,'package_a',$3,ARRAY['wamn:connection/http@0.1.0'],'write')", &[&env_key,&invocation.component_digest,&name]).await.unwrap();
                admin.execute("INSERT INTO catalog.release_connection(env_key,manifest_digest,connection_id,version) VALUES($1,$2,$3,1)", &[&env_key,&manifest_digest,&connection]).await.unwrap();
                admin.execute("INSERT INTO catalog.connection_binding(env_key,manifest_digest,component_digest,package_id,name,connection_id) VALUES($1,$2,$3,'package_a',$4,$5)", &[&env_key,&manifest_digest,&invocation.component_digest,&name,&connection]).await.unwrap();
            }
            admin.batch_execute("COMMIT").await.unwrap();
            admin.batch_execute("CREATE SCHEMA wamn_run").await.unwrap();
            let role = format!("oauth2_{}_{}", database.name(), project.replace('-', "_"));
            admin
                .batch_execute(
                    &wamn_control_provision::sql::prepare_workload_generation_sql(
                        wamn_control_provision::WorkloadRoleFamily::HttpAdmitter,
                        database.name(),
                        &role,
                        "oauth2-test-password",
                        "2099-01-01T00:00:00Z",
                    ),
                )
                .await
                .unwrap();
            let mut authority = url::Url::parse(database.url()).unwrap();
            authority.set_username(&role).unwrap();
            authority
                .set_password(Some("oauth2-test-password"))
                .unwrap();
            projects.insert(
                project.to_owned(),
                ProjectConfig {
                    credentials: ClassCredentials::default()
                        .with_class(AuthorityClass::CallableHttp, authority),
                    guest_pool_max_size: 1,
                    platform_pool_max_size: 1,
                    wait_timeout_ms: 5_000,
                    statement_timeout_ms: 10_000,
                    row_limit: 10_000,
                },
            );
            self.applications.push(database);
        }
        let postgres = Arc::new(WamnPostgres::with_provider(Arc::new(
            StaticCredentialProvider::new(projects, None),
        )));
        let facts = Arc::new(ConnectionFacts::new(
            Arc::new(ConnectionMemory::default()),
            ConnectionChanges::default(),
            Arc::new(self.credentials.reader("project-a").await),
            Arc::clone(&postgres),
            "acme--project-a--dev",
            "project-a",
            "dev",
        ));
        postgres
            .set_env_key("instance", "acme--project-a--dev")
            .unwrap();
        let plugin = ConnectionHttp::new(
            postgres,
            Arc::new(HttpTransport::new().unwrap()),
            facts,
            "acme--project-a--dev",
            "project-a",
            vec![self.base.strip_suffix("bound/").unwrap().parse().unwrap()].into(),
            Some(release),
        );
        plugin.bind_invocation("instance", invocation).unwrap();
        plugin
    }

    async fn use_project(&self, plugin: &mut ConnectionHttp, project: &str) {
        use wamn_engine::release_manifest::ReleaseScope;
        let env_key = format!("acme--{project}--dev");
        plugin.release = Some(Arc::new(
            LoadedRelease::load_canonical_bytes(
                &plugin
                    .release
                    .as_ref()
                    .unwrap()
                    .manifest()
                    .canonical_bytes(),
                "oauth2-test",
                ReleaseScope::new(&env_key, "dev"),
            )
            .unwrap(),
        ));
        plugin.facts = Arc::new(ConnectionFacts::new(
            Arc::new(ConnectionMemory::default()),
            ConnectionChanges::default(),
            Arc::new(self.credentials.reader(project).await),
            Arc::clone(&plugin.postgres),
            &env_key,
            project,
            "dev",
        ));
        plugin.postgres.set_env_key("instance", &env_key).unwrap();
        plugin.env_key = env_key.into();
        plugin.project = project.into();
    }

    async fn send(
        &self,
        plugin: &ConnectionHttp,
        handle: &str,
    ) -> Result<Response, ConnectionError> {
        let request = Request {
            requirement: handle.into(),
            path_and_query: "/api".into(),
            body: Some(b"guest-body".to_vec()),
            ..effect_request(vec![Header {
                name: "authorization".into(),
                value: b"guest-value".to_vec(),
            }])
        };
        let before = format!("{request:?}");
        let result = plugin.send("instance", &request).await;
        assert_eq!(
            format!("{request:?}"),
            before,
            "the guest request stays unchanged"
        );
        for value in [before, format!("{result:?}")] {
            assert!(!value.contains(CLIENT_ID));
            assert!(!value.contains(CLIENT_SECRET));
            assert!(!value.contains("fixture-token"));
        }
        result
    }

    fn counts(&self) -> (usize, usize) {
        let observed = self.observed.lock().unwrap();
        let tokens = observed
            .iter()
            .filter(|request| request.path.starts_with("/bound/token"))
            .count();
        (tokens, observed.len() - tokens)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn token(number: u8, expires_in: Option<u64>) -> (u16, String) {
    let mut value = serde_json::json!({"access_token": format!("fixture-token-{number}"), "token_type": "Bearer"});
    if let Some(seconds) = expires_in {
        value["expires_in"] = seconds.into();
    }
    (200, value.to_string())
}

#[tokio::test]
async fn fresh_and_cached_bearer_use_basic_form_authentication_without_guest_secrets() {
    use base64::Engine as _;

    let _lock = wamn_test_postgres::lock();
    let mut fixture = Fixture::start(vec![token(1, Some(3600))], vec![200, 200]).await;
    let plugin = fixture.plugin(&fixture.parameters()).await;
    for _ in 0..2 {
        assert_eq!(fixture.send(&plugin, "handle").await.unwrap().status, 200);
    }
    assert_eq!(fixture.counts(), (1, 2));
    let observed = fixture.observed.lock().unwrap();
    let fetch = &observed[0];
    assert_eq!(fetch.method, "POST");
    assert_eq!(fetch.path, "/bound/token?audience=api");
    assert_eq!(
        fetch.headers["content-type"],
        "application/x-www-form-urlencoded"
    );
    let basic = fetch.headers["authorization"]
        .to_str()
        .unwrap()
        .strip_prefix("Basic ")
        .unwrap();
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(basic)
            .unwrap(),
        b"private+client%3Aid%2B:private%2Fsecret%26%2B"
    );
    assert_eq!(
        fetch.body,
        b"grant_type=client_credentials&scope=orders%3Aread+orders%3Awrite%2B"
    );
    for request in &observed[1..] {
        assert_eq!(request.headers["authorization"], "Bearer fixture-token-1");
        assert_eq!(request.body, b"guest-body");
        let wire = format!("{:?}{:?}", request.headers, request.body);
        assert!(!wire.contains(CLIENT_ID));
        assert!(!wire.contains(CLIENT_SECRET));
    }
}

#[tokio::test]
async fn token_endpoint_can_equal_the_bound_base_without_adding_a_query() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = Fixture::start(vec![token(1, Some(3600))], vec![200]).await;
    let mut parameters = fixture.parameters();
    parameters["token_url"] = fixture.base.clone().into();
    let plugin = fixture.plugin(&parameters).await;
    assert_eq!(fixture.send(&plugin, "handle").await.unwrap().status, 200);
    let observed = fixture.observed.lock().unwrap();
    assert_eq!(observed.len(), 2);
    assert_eq!(observed[0].path, "/bound/");
    assert_eq!(
        observed[1].headers["authorization"],
        "Bearer fixture-token-1"
    );
}

#[tokio::test]
async fn short_or_absent_expiry_never_caches_across_requests_and_empty_scopes_omit_scope() {
    let _lock = wamn_test_postgres::lock();
    for expiry in [None, Some(0), Some(59), Some(60)] {
        let mut fixture =
            Fixture::start(vec![token(1, expiry), token(2, expiry)], vec![200, 200]).await;
        let mut parameters = fixture.parameters();
        parameters["scopes"] = serde_json::json!([]);
        let plugin = fixture.plugin(&parameters).await;
        for _ in 0..2 {
            fixture.send(&plugin, "handle").await.unwrap();
        }
        assert_eq!(fixture.counts(), (2, 2));
        assert_eq!(
            fixture.observed.lock().unwrap()[0].body,
            b"grant_type=client_credentials"
        );
    }
}

#[tokio::test]
async fn clock_seam_expires_cached_token_at_the_sixty_second_margin() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = Fixture::start(
        vec![token(1, Some(3600)), token(2, Some(3600))],
        vec![200, 200],
    )
    .await;
    let mut plugin = fixture.plugin(&fixture.parameters()).await;
    fixture.send(&plugin, "handle").await.unwrap();
    plugin.oauth2.now = || Instant::now() + Duration::from_mins(59);
    fixture.send(&plugin, "handle").await.unwrap();
    assert_eq!(fixture.counts(), (2, 2));
    assert_eq!(
        fixture.observed.lock().unwrap()[3].headers["authorization"],
        "Bearer fixture-token-2"
    );
}

#[tokio::test]
async fn cache_separates_project_connection_and_active_generation() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = Fixture::start(
        (1..=4).map(|n| token(n, Some(3600))).collect(),
        vec![200; 4],
    )
    .await;
    let mut plugin = fixture.plugin(&fixture.parameters()).await;
    fixture.send(&plugin, "handle").await.unwrap();
    let connection = plugin.facts.env_connections().await.unwrap().connections["handle"];
    let first = plugin.facts.connection(&connection).await.unwrap().unwrap();
    fixture
        .put_http("handle", &fixture.parameters(), &["project-a"])
        .await;
    fixture.send(&plugin, "handle").await.unwrap();
    let second = plugin.facts.connection(&connection).await.unwrap().unwrap();
    assert!(second.active_generation > first.active_generation);
    fixture.send(&plugin, "other-handle").await.unwrap();
    fixture.use_project(&mut plugin, "project-b").await;
    fixture.send(&plugin, "handle").await.unwrap();
    let entries = plugin.oauth2.entries.lock().unwrap();
    assert!(entries.contains_key(&(connection, first.active_generation)));
    assert!(entries.contains_key(&(connection, second.active_generation)));
    assert_eq!(entries.len(), 4);
    assert_eq!(fixture.counts(), (4, 4));
}

#[tokio::test]
async fn refused_token_never_dispatches_api_or_exposes_error_body() {
    for status in [400, 401, 302, 500] {
        let _lock = wamn_test_postgres::lock();
        let mut fixture = Fixture::start(
            vec![(status, format!("{CLIENT_ID} {CLIENT_SECRET}"))],
            vec![],
        )
        .await;
        let plugin = fixture.plugin(&fixture.parameters()).await;
        assert!(matches!(
            fixture.send(&plugin, "handle").await,
            Err(ConnectionError::CredentialUnavailable)
        ));
        assert_eq!(fixture.counts(), (1, 0));
    }
}

#[tokio::test]
async fn api_401_fetches_once_more_and_returns_the_second_response() {
    for second in [200, 401] {
        let _lock = wamn_test_postgres::lock();
        let mut fixture = Fixture::start(
            vec![token(1, Some(3600)), token(2, Some(3600))],
            vec![401, second],
        )
        .await;
        let plugin = fixture.plugin(&fixture.parameters()).await;
        assert_eq!(
            fixture.send(&plugin, "handle").await.unwrap().status,
            second
        );
        assert_eq!(fixture.counts(), (2, 2));
        let observed = fixture.observed.lock().unwrap();
        assert_eq!(
            observed[1].headers["authorization"],
            "Bearer fixture-token-1"
        );
        assert_eq!(
            observed[3].headers["authorization"],
            "Bearer fixture-token-2"
        );
        assert_eq!(observed[1].body, observed[3].body);
        if second == 401 {
            assert!(plugin.oauth2.entries.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn api_403_with_an_unchanged_fact_does_not_retry_oauth2() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = Fixture::start(vec![token(1, Some(3600))], vec![403]).await;
    let plugin = fixture.plugin(&fixture.parameters()).await;
    let response = fixture.send(&plugin, "handle").await.unwrap();
    assert_eq!(response.status, 403);
    assert_eq!(response.body, b"api-403");
    assert_eq!(fixture.counts(), (1, 1));
    assert!(plugin.oauth2.entries.lock().unwrap().is_empty());
}

#[tokio::test]
async fn failed_refresh_returns_the_first_api_401_without_another_api_request() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = Fixture::start(
        vec![
            token(1, Some(3600)),
            (400, format!("{CLIENT_ID} {CLIENT_SECRET}")),
        ],
        vec![401],
    )
    .await;
    let plugin = fixture.plugin(&fixture.parameters()).await;
    let response = fixture.send(&plugin, "handle").await.unwrap();
    assert_eq!(response.status, 401);
    assert_eq!(response.body, b"api-401");
    assert_eq!(fixture.counts(), (2, 1));
    assert!(plugin.oauth2.entries.lock().unwrap().is_empty());
}

#[tokio::test]
async fn token_url_outside_binding_or_ambiguous_path_refuses_before_dispatch() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = Fixture::start(vec![], vec![]).await;
    let origin = fixture.base.strip_suffix("bound/").unwrap();
    for endpoint in [
        "http://other.invalid/bound/token".into(),
        format!("{origin}outside/token"),
        format!("{origin}bound-other/token"),
        format!("{}../token", fixture.base),
        format!("{}%2e%2e/token", fixture.base),
        format!("{}token#fragment", fixture.base),
        "/bound/token".into(),
    ] {
        let mut parameters = fixture.parameters();
        parameters["token_url"] = endpoint.into();
        let plugin = fixture.plugin(&parameters).await;
        assert!(matches!(
            fixture.send(&plugin, "handle").await,
            Err(ConnectionError::CredentialUnavailable)
        ));
        assert_eq!(fixture.counts(), (0, 0));
    }
}

#[test]
fn malformed_credentials_and_parameters_refuse() {
    let valid = serde_json::json!({"client_id":CLIENT_ID,"token_url":"https://example.com/token","scopes":[]});
    for material in [
        None,
        Some("not json"),
        Some("null"),
        Some("{}"),
        Some(r#"{"client_secret":""}"#),
        Some(r#"{"client_secret":1}"#),
        Some(r#"{"client_secret":"private","extra":true}"#),
    ] {
        assert!(oauth2_credential(&valid, material).is_err());
    }
    let secret = serde_json::json!({"client_secret":CLIENT_SECRET}).to_string();
    let mut invalid = vec![serde_json::json!(null)];
    for field in ["token_url", "client_id", "scopes"] {
        let mut parameters = valid.clone();
        parameters.as_object_mut().unwrap().remove(field);
        invalid.push(parameters);
        let mut parameters = valid.clone();
        parameters[field] = true.into();
        invalid.push(parameters);
    }
    invalid.extend([
        serde_json::json!({"client_id":"","token_url":"https://example.com/token","scopes":[]}),
        serde_json::json!({"client_id":CLIENT_ID,"token_url":"https://example.com/token","scopes":[1]}),
        serde_json::json!({"client_id":CLIENT_ID,"token_url":"https://example.com/token","scopes":[],"extra":true}),
    ]);
    for parameters in invalid {
        assert!(oauth2_credential(&parameters, Some(&secret)).is_err());
    }
}

#[tokio::test]
async fn malformed_token_response_refuses_without_leaking_its_body() {
    for body in [
        format!("{CLIENT_ID} {CLIENT_SECRET}"),
        r#"{"access_token":"ok","token_type":"MAC"}"#.into(),
        r#"{"access_token":"","token_type":"Bearer"}"#.into(),
        r#"{"access_token":"bad\r\nheader","token_type":"Bearer"}"#.into(),
        r#"{"access_token":"ok"}"#.into(),
        r#"{"access_token":"ok","token_type":"Bearer","expires_in":-1}"#.into(),
    ] {
        let _lock = wamn_test_postgres::lock();
        let mut fixture = Fixture::start(vec![(200, body)], vec![]).await;
        let plugin = fixture.plugin(&fixture.parameters()).await;
        assert!(matches!(
            fixture.send(&plugin, "handle").await,
            Err(ConnectionError::CredentialUnavailable)
        ));
        assert_eq!(fixture.counts(), (1, 0));
    }
}
