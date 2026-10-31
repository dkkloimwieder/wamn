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

type ResponsePause = (
    tokio::sync::oneshot::Sender<()>,
    tokio::sync::oneshot::Receiver<()>,
);

pub(super) struct Fixture {
    base: String,
    observed: Arc<Mutex<Vec<Observed>>>,
    task: JoinHandle<()>,
    credentials: crate::connection_credentials::test_util::Fixture,
    applications: Vec<wamn_test_postgres::Database>,
    pause: Arc<Mutex<Option<ResponsePause>>>,
    key: tempfile::NamedTempFile,
    token_pause: Arc<Mutex<Option<ResponsePause>>>,
}

impl Fixture {
    pub(super) async fn start(tokens: Vec<(u16, String)>, api: Vec<u16>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/bound/", listener.local_addr().unwrap());
        let observed = Arc::new(Mutex::new(Vec::new()));
        let recorded = observed.clone();
        let tokens = Arc::new(Mutex::new(VecDeque::from(tokens)));
        let api = Arc::new(Mutex::new(VecDeque::from(api)));
        let pause = Arc::new(Mutex::new(None::<ResponsePause>));
        let response_pause = Arc::clone(&pause);
        let token_pause = Arc::new(Mutex::new(None::<ResponsePause>));
        let endpoint_pause = Arc::clone(&token_pause);
        let task = tokio::spawn(async move {
            loop {
                let (tcp, _) = listener.accept().await.unwrap();
                let observed = recorded.clone();
                let tokens = tokens.clone();
                let api = api.clone();
                let pause = Arc::clone(&response_pause);
                let token_pause = Arc::clone(&endpoint_pause);
                hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        TokioIo::new(tcp),
                        service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                            let observed = observed.clone();
                            let tokens = tokens.clone();
                            let api = api.clone();
                            let pause = Arc::clone(&pause);
                            let token_pause = Arc::clone(&token_pause);
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
                                let paused = if token {
                                    token_pause.lock().unwrap().take()
                                } else {
                                    pause.lock().unwrap().take()
                                };
                                if let Some((arrived, resume)) = paused {
                                    arrived.send(()).unwrap();
                                    resume.await.unwrap();
                                }
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
        let key = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(key.path(), [7_u8; 32]).unwrap();
        Self {
            key,
            token_pause,
            credentials: crate::connection_credentials::test_util::Fixture::new().await,
            base,
            observed,
            task,
            applications: Vec::new(),
            pause,
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
        self.put_auth(
            name,
            parameters,
            projects,
            "oauth2_client_credentials",
            &secret,
        )
        .await;
    }

    async fn put_auth(
        &mut self,
        name: &str,
        parameters: &serde_json::Value,
        projects: &[&str],
        auth_type: &str,
        secret: &str,
    ) {
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
                    "integration_type": "http", "auth_type": auth_type,
                    "parameters": {"primary-authority": self.base, "tls-verification": "disabled", "auth": parameters},
                    "credential": secret,
                }),
                self.credentials.store.as_ref(),
            ).await.unwrap();
            tx.commit().await.unwrap();
        }
    }

    async fn plugin(&mut self, parameters: &serde_json::Value) -> ConnectionHttp {
        self.plugin_with_auth(
            parameters,
            "oauth2_client_credentials",
            &serde_json::json!({"client_secret": CLIENT_SECRET}).to_string(),
        )
        .await
    }

    async fn plugin_with_auth(
        &mut self,
        parameters: &serde_json::Value,
        auth_type: &str,
        secret: &str,
    ) -> ConnectionHttp {
        use crate::plugins::wamn_postgres::{
            ClassCredentials, ProjectConfig, StaticCredentialProvider,
        };
        use wamn_engine::release_manifest::ReleaseScope;
        use wamn_run_state::AuthorityClass;

        for name in ["handle", "other-handle"] {
            self.put_auth(
                name,
                parameters,
                &["project-a", "project-b"],
                auth_type,
                secret,
            )
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
                admin.execute("INSERT INTO catalog.connection_requirement(env_key,component_digest,package_id,name,interface,mode) VALUES($1,$2,'package_a',$3,'wamn:connection/http@0.1.0','write')", &[&env_key,&invocation.component_digest,&name]).await.unwrap();
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
        let facts = Arc::new(
            ConnectionFacts::new(
                Arc::new(ConnectionMemory::default()),
                ConnectionChanges::default(),
                Arc::new(self.credentials.reader("project-a").await),
                Arc::clone(&postgres),
                "acme--project-a--dev",
                "project-a",
                "dev",
            )
            .with_oauth_encryption(Some(Arc::new(
                crate::oauth_encryption::Envelope::from_file(self.key.path()).unwrap(),
            ))),
        );
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

    async fn other_host(&self, first: &ConnectionHttp) -> ConnectionHttp {
        let facts = Arc::new(
            ConnectionFacts::new(
                Arc::new(ConnectionMemory::default()),
                ConnectionChanges::default(),
                Arc::new(self.credentials.reader("project-a").await),
                Arc::clone(&first.postgres),
                "acme--project-a--dev",
                "project-a",
                "dev",
            )
            .with_oauth_encryption(Some(Arc::new(
                crate::oauth_encryption::Envelope::from_file(self.key.path()).unwrap(),
            ))),
        );
        let plugin = ConnectionHttp::new(
            Arc::clone(&first.postgres),
            Arc::new(HttpTransport::new().unwrap()),
            facts,
            "acme--project-a--dev",
            "project-a",
            Arc::clone(&first.allowed_hosts),
            first.release.clone(),
        );
        plugin
            .bind_invocation("instance", first.invocation("instance").unwrap())
            .unwrap();
        plugin
    }

    fn pause_token(&self) -> ResponsePause {
        let (arrived, arrival) = tokio::sync::oneshot::channel();
        let (resume, resumed) = tokio::sync::oneshot::channel();
        *self.token_pause.lock().unwrap() = Some((arrived, resumed));
        (resume, arrival)
    }

    async fn due(&self, plugin: &ConnectionHttp) {
        self.credentials
            .admin
            .execute(
                "UPDATE identity.oauth_token SET expires_at='-infinity'",
                &[],
            )
            .await
            .unwrap();
        plugin.oauth2.entries.lock().unwrap().clear();
    }

    async fn stored(&self, connection: uuid::Uuid) -> crate::oauth_token::Token {
        let row = self.credentials.admin.query_one("SELECT ciphertext,nonce,wrapped_key,key_id FROM identity.oauth_token WHERE connection_id=$1", &[&connection]).await.unwrap();
        let envelope = crate::oauth_encryption::Envelope::from_file(self.key.path()).unwrap();
        let plaintext = envelope
            .decrypt(&crate::oauth_encryption::Encrypted {
                ciphertext: row.get(0),
                nonce: row.get(1),
                wrapped_key: row.get(2),
                key_id: row.get(3),
            })
            .await
            .unwrap();
        serde_json::from_slice(plaintext.expose()).unwrap()
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
        plugin.facts = Arc::new(
            ConnectionFacts::new(
                Arc::new(ConnectionMemory::default()),
                ConnectionChanges::default(),
                Arc::new(self.credentials.reader(project).await),
                Arc::clone(&plugin.postgres),
                &env_key,
                project,
                "dev",
            )
            .with_oauth_encryption(Some(Arc::new(
                crate::oauth_encryption::Envelope::from_file(self.key.path()).unwrap(),
            ))),
        );
        plugin.oauth2 = plugin.facts.oauth_tokens();
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

    pub(super) fn counts(&self) -> (usize, usize) {
        let observed = self.observed.lock().unwrap();
        let tokens = observed
            .iter()
            .filter(|request| request.path.starts_with("/bound/token"))
            .count();
        (tokens, observed.len() - tokens)
    }

    pub(super) async fn basic_plugin(&mut self) -> ConnectionHttp {
        self.plugin_with_auth(
            &serde_json::json!({}),
            "http_basic",
            r#"{"username":"fixture","password":"old"}"#,
        )
        .await
    }

    pub(super) async fn write_basic(&mut self) {
        let tx = self.credentials.admin.transaction().await.unwrap();
        tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
        .await
        .unwrap();
        tx.batch_execute("SET LOCAL ROLE wamn_control")
            .await
            .unwrap();
        let mut drops = wamn_platform_identity::connection_changes::ConnectionDrops::default();
        wamn_platform_identity::connections::administration::execute_with_store(
            &tx, "acme", "connection.write",
            &serde_json::json!({
                "project":"project-a", "env":"dev", "name":"handle",
                "parameters":{"primary-authority":self.base,"tls-verification":"disabled","auth":{}},
                "credential":r#"{"username":"fixture","password":"new"}"#
            }).to_string(),
            &mut wamn_platform_identity::auth_changes::AuthDrops::default(),
            &mut drops, Some(self.credentials.store.as_ref()),
        ).await.unwrap();
        tx.commit().await.unwrap();
        assert!(!drops.is_empty());
        wamn_platform_identity::connection_changes::ConnectionChanges::default()
            .publish(drops)
            .await;
    }

    pub(super) fn pause_next_response(&self) -> ResponsePause {
        let (arrived, arrival) = tokio::sync::oneshot::channel();
        let (resume, resumed) = tokio::sync::oneshot::channel();
        *self.pause.lock().unwrap() = Some((arrived, resumed));
        (resume, arrival)
    }

    pub(super) fn authorizations(&self) -> Vec<String> {
        self.observed
            .lock()
            .unwrap()
            .iter()
            .map(|request| {
                request.headers["authorization"]
                    .to_str()
                    .unwrap()
                    .to_owned()
            })
            .collect()
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
    let plugin = fixture.plugin(&fixture.parameters()).await;
    fixture.send(&plugin, "handle").await.unwrap();
    fixture
        .credentials
        .admin
        .execute(
            "UPDATE identity.oauth_token SET expires_at=clock_timestamp()+interval '60 seconds'",
            &[],
        )
        .await
        .unwrap();
    for (_, expires) in plugin.oauth2.entries.lock().unwrap().values_mut() {
        *expires = tokio::time::Instant::now();
    }
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
    {
        let entries = plugin.oauth2.entries.lock().unwrap();
        assert!(!entries.contains_key(&(connection, first.active_generation)));
        assert!(entries.contains_key(&(connection, second.active_generation)));
        assert_eq!(entries.len(), 2);
    }
    fixture.use_project(&mut plugin, "project-b").await;
    fixture.send(&plugin, "handle").await.unwrap();
    let entries = plugin.oauth2.entries.lock().unwrap();
    assert!(!entries.contains_key(&(connection, first.active_generation)));
    assert!(!entries.contains_key(&(connection, second.active_generation)));
    assert_eq!(entries.len(), 1);
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
async fn failed_refresh_returns_credential_unavailable_without_another_api_request() {
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
    assert!(matches!(
        fixture.send(&plugin, "handle").await,
        Err(ConnectionError::CredentialUnavailable)
    ));
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

#[tokio::test]
async fn two_hosts_refresh_one_due_row_once() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = Fixture::start(
        vec![token(1, Some(3600)), token(2, Some(3600))],
        vec![200; 3],
    )
    .await;
    let first = fixture.plugin(&fixture.parameters()).await;
    fixture.send(&first, "handle").await.unwrap();
    fixture.due(&first).await;
    let second = fixture.other_host(&first).await;
    assert!(!Arc::ptr_eq(
        &first.facts.oauth_encryption().unwrap(),
        &second.facts.oauth_encryption().unwrap()
    ));
    let before = fixture.counts().0;
    let (resume, arrived) = fixture.pause_token();
    let (a, b, ()) = tokio::join!(
        fixture.send(&first, "handle"),
        fixture.send(&second, "handle"),
        async {
            arrived.await.unwrap();
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let blocked: bool = fixture.credentials.admin.query_one(
                        "SELECT EXISTS(SELECT FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'SELECT registry.lock_oauth_connection%')", &[]
                    ).await.unwrap().get(0);
                    if blocked { break; }
                    tokio::task::yield_now().await;
                }
            }).await.unwrap();
            resume.send(()).unwrap();
        }
    );
    assert_eq!(a.unwrap().status, 200);
    assert_eq!(b.unwrap().status, 200);
    assert_eq!(fixture.counts().0 - before, 1);
    let observed = fixture.observed.lock().unwrap();
    for request in observed
        .iter()
        .filter(|request| request.path == "/bound/api")
        .skip(1)
    {
        assert_eq!(request.headers["authorization"], "Bearer fixture-token-2");
    }
}

#[tokio::test]
async fn provider_failure_is_shared_for_thirty_seconds_with_or_without_a_row() {
    let _lock = wamn_test_postgres::lock();
    for existing in [false, true] {
        let mut replies = vec![
            (500, "private failure".into()),
            (500, "private failure".into()),
            token(3, Some(3600)),
        ];
        if existing {
            replies.insert(0, token(1, Some(3600)));
        }
        let mut fixture = Fixture::start(replies, vec![200; 2]).await;
        let first = fixture.plugin(&fixture.parameters()).await;
        if existing {
            fixture.send(&first, "handle").await.unwrap();
            fixture.due(&first).await;
        }
        let second = fixture.other_host(&first).await;
        let before = fixture.counts().0;
        let (a, b) = tokio::join!(
            fixture.send(&first, "handle"),
            fixture.send(&second, "handle")
        );
        assert!(matches!(a, Err(ConnectionError::CredentialUnavailable)));
        assert!(matches!(b, Err(ConnectionError::CredentialUnavailable)));
        assert_eq!(fixture.counts().0 - before, 1);
        let row = fixture
            .credentials
            .admin
            .query_one("SELECT refresh_status FROM identity.oauth_token", &[])
            .await
            .unwrap();
        assert_eq!(row.get::<_, String>(0), "failing");
        fixture.credentials.admin.execute("UPDATE identity.oauth_token SET updated_at=clock_timestamp()-interval '31 seconds'",&[]).await.unwrap();
        assert!(matches!(
            fixture.send(&second, "handle").await,
            Err(ConnectionError::CredentialUnavailable)
        ));
        assert_eq!(fixture.counts().0 - before, 2);
        fixture.credentials.admin.execute("UPDATE identity.oauth_token SET updated_at=clock_timestamp()-interval '31 seconds'",&[]).await.unwrap();
        assert_eq!(fixture.send(&first, "handle").await.unwrap().status, 200);
        assert_eq!(fixture.counts().0 - before, 3);
        assert_eq!(
            fixture
                .credentials
                .admin
                .query_one("SELECT refresh_status FROM identity.oauth_token", &[])
                .await
                .unwrap()
                .get::<_, String>(0),
            "ok"
        );
    }
}

#[tokio::test]
async fn credential_rotation_waits_for_refresh_and_removes_the_old_token() {
    use base64::Engine as _;

    let _lock = wamn_test_postgres::lock();
    let mut fixture = Fixture::start(
        vec![
            token(1, Some(3600)),
            token(2, Some(3600)),
            token(3, Some(3600)),
        ],
        vec![200; 3],
    )
    .await;
    let plugin = Arc::new(fixture.plugin(&fixture.parameters()).await);
    fixture.send(&plugin, "handle").await.unwrap();
    fixture.due(&plugin).await;
    let connection = plugin.facts.env_connections().await.unwrap().connections["handle"];
    let (resume, arrived) = fixture.pause_token();
    let refreshing = Arc::clone(&plugin);
    let refresh = tokio::spawn(async move {
        refreshing
            .send(
                "instance",
                &Request {
                    requirement: "handle".into(),
                    path_and_query: "/api".into(),
                    ..effect_request(Vec::new())
                },
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(10), arrived)
        .await
        .unwrap()
        .unwrap();
    let (mut rotating, driver) =
        tokio_postgres::connect(fixture.credentials.database.url(), tokio_postgres::NoTls)
            .await
            .unwrap();
    tokio::spawn(driver);
    let pid: i32 = rotating
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);
    let store = Arc::clone(&fixture.credentials.store);
    let rotation = tokio::spawn(async move {
        let tx = rotating.transaction().await.unwrap();
        tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
        .await
        .unwrap();
        tx.batch_execute("SET LOCAL ROLE wamn_control")
            .await
            .unwrap();
        let result = wamn_platform_identity::connections::administration::execute_with_store(&tx,"acme","connection.set_credential",
            &serde_json::json!({"project":"project-a","env":"dev","name":"handle","credential":r#"{"client_secret":"rotated-secret"}"#}).to_string(),
            &mut wamn_platform_identity::auth_changes::AuthDrops::default(),
            &mut wamn_platform_identity::connection_changes::ConnectionDrops::default(),Some(store.as_ref())).await.unwrap();
        assert!(result.get("error").is_none(), "{result}");
        tx.commit().await.unwrap();
    });
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let blocked: bool = fixture
                .credentials
                .admin
                .query_one("SELECT cardinality(pg_blocking_pids($1))>0", &[&pid])
                .await
                .unwrap()
                .get(0);
            if blocked {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    resume.send(()).unwrap();
    refresh.await.unwrap().unwrap();
    rotation.await.unwrap();
    assert_eq!(
        fixture
            .credentials
            .admin
            .query_one(
                "SELECT count(*) FROM identity.oauth_token WHERE connection_id=$1",
                &[&connection]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    plugin.facts.read_again(&connection).await.unwrap();
    fixture.send(&plugin, "handle").await.unwrap();
    let stored = fixture.stored(connection).await;
    assert_eq!(stored.access_token.as_deref(), Some("fixture-token-3"));
    let observed = fixture.observed.lock().unwrap();
    let fetches: Vec<_> = observed
        .iter()
        .filter(|request| request.path.starts_with("/bound/token"))
        .collect();
    let basic = fetches[2].headers["authorization"]
        .to_str()
        .unwrap()
        .strip_prefix("Basic ")
        .unwrap();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(basic)
        .unwrap();
    assert!(
        String::from_utf8(decoded)
            .unwrap()
            .ends_with(":rotated-secret")
    );
}

#[tokio::test]
async fn authorization_code_requires_a_row_and_keeps_or_replaces_the_refresh_token() {
    let _lock = wamn_test_postgres::lock();
    let mut renewed = serde_json::from_str::<serde_json::Value>(&token(3, Some(3600)).1).unwrap();
    renewed["refresh_token"] = "renewed-refresh".into();
    let mut fixture = Fixture::start(
        vec![
            token(1, Some(3600)),
            token(2, Some(3600)),
            (200, renewed.to_string()),
        ],
        vec![200; 3],
    )
    .await;
    let plugin = fixture.plugin(&fixture.parameters()).await;
    let connection = plugin.facts.env_connections().await.unwrap().connections["handle"];
    // Administration's authorization-code write is delivered by issue .5.
    fixture
        .credentials
        .admin
        .execute(
            "UPDATE registry.integration SET auth_type='oauth2_authorization_code'",
            &[],
        )
        .await
        .unwrap();
    let code_host = fixture.other_host(&plugin).await;
    assert!(matches!(
        fixture.send(&code_host, "handle").await,
        Err(ConnectionError::Unauthorized)
    ));
    assert_eq!(fixture.counts(), (0, 0));
    fixture
        .credentials
        .admin
        .execute(
            "UPDATE registry.integration SET auth_type='oauth2_client_credentials'",
            &[],
        )
        .await
        .unwrap();
    fixture.send(&plugin, "handle").await.unwrap();
    fixture
        .credentials
        .admin
        .execute(
            "UPDATE registry.integration SET auth_type='oauth2_authorization_code'",
            &[],
        )
        .await
        .unwrap();
    let encrypted = code_host
        .facts
        .oauth_encryption()
        .unwrap()
        .encrypt(
            &serde_json::to_vec(&crate::oauth_token::Token {
                access_token: Some("old-access".into()),
                refresh_token: Some("old-refresh".into()),
            })
            .unwrap(),
        )
        .await
        .unwrap();
    fixture.credentials.admin.execute("UPDATE identity.oauth_token SET ciphertext=$2,nonce=$3,wrapped_key=$4,key_id=$5,expires_at='-infinity' WHERE connection_id=$1",
        &[&connection,&encrypted.ciphertext,&encrypted.nonce,&encrypted.wrapped_key,&encrypted.key_id]).await.unwrap();
    fixture.send(&code_host, "handle").await.unwrap();
    assert_eq!(
        fixture.stored(connection).await.refresh_token.as_deref(),
        Some("old-refresh")
    );
    fixture.due(&code_host).await;
    fixture.send(&code_host, "handle").await.unwrap();
    assert_eq!(
        fixture.stored(connection).await.refresh_token.as_deref(),
        Some("renewed-refresh")
    );
    let observed = fixture.observed.lock().unwrap();
    let grants: Vec<_> = observed
        .iter()
        .filter(|request| request.path.starts_with("/bound/token"))
        .collect();
    for grant in &grants[1..] {
        assert_eq!(
            grant.body,
            b"grant_type=refresh_token&refresh_token=old-refresh"
        );
    }
}
