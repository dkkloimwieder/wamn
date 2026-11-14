//! OAuth2 credentials exercise the host send path over a local listener.

use sqlx::{Connection as _, Row as _};
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

type AssertionVerifier = Arc<dyn Fn(&Observed, &str) + Send + Sync>;

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
    pkce: Arc<Mutex<Option<String>>>,
}

impl Fixture {
    pub(super) async fn start(tokens: Vec<(u16, String)>, api: Vec<u16>) -> Self {
        Self::start_with_verifier(tokens, api, None).await
    }

    async fn start_with_verifier(
        tokens: Vec<(u16, String)>,
        api: Vec<u16>,
        assertion_verifier: Option<AssertionVerifier>,
    ) -> Self {
        let listener = TcpListener::bind((super::interface_address(), 0))
            .await
            .unwrap();
        let base = format!("http://{}/bound/", listener.local_addr().unwrap());
        let observed = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&observed);
        let tokens = Arc::new(Mutex::new(VecDeque::from(tokens)));
        let api = Arc::new(Mutex::new(VecDeque::from(api)));
        let pause = Arc::new(Mutex::new(None::<ResponsePause>));
        let response_pause = Arc::clone(&pause);
        let token_pause = Arc::new(Mutex::new(None::<ResponsePause>));
        let endpoint_pause = Arc::clone(&token_pause);
        let token_base = base.clone();
        let pkce = Arc::new(Mutex::new(None::<String>));
        let expected_pkce = Arc::clone(&pkce);
        let task = tokio::spawn(async move {
            loop {
                let (tcp, _) = listener.accept().await.unwrap();
                let observed = Arc::clone(&recorded);
                let tokens = Arc::clone(&tokens);
                let api = Arc::clone(&api);
                let pause = Arc::clone(&response_pause);
                let token_pause = Arc::clone(&endpoint_pause);
                let assertion_verifier = assertion_verifier.clone();
                let token_base = token_base.clone();
                let pkce = Arc::clone(&expected_pkce);
                hyper::server::conn::http1::Builder::new()
                    .serve_connection(
                        TokioIo::new(tcp),
                        service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                            let observed = Arc::clone(&observed);
                            let tokens = Arc::clone(&tokens);
                            let api = Arc::clone(&api);
                            let pause = Arc::clone(&pause);
                            let token_pause = Arc::clone(&token_pause);
                            let assertion_verifier = assertion_verifier.clone();
                            let token_base = token_base.clone();
                            let pkce = Arc::clone(&pkce);
                            async move {
                                let (parts, body) = request.into_parts();
                                let body = body.collect().await.unwrap().to_bytes().to_vec();
                                let token = matches!(parts.uri.path(), "/bound/token" | "/bound/");
                                let form: std::collections::HashMap<_, _> =
                                    url::form_urlencoded::parse(&body).into_owned().collect();
                                if token
                                    && form
                                        .get("grant_type")
                                        .is_some_and(|grant| grant == "authorization_code")
                                {
                                    use base64::Engine as _;
                                    use sha2::{Digest as _, Sha256};
                                    assert_eq!(form["code"], "fixture-code+&");
                                    assert_eq!(form["client_id"], CLIENT_ID);
                                    assert_eq!(form["client_secret"], CLIENT_SECRET);
                                    assert_eq!(
                                        form["redirect_uri"],
                                        "http://control.example.test/oauth/callback"
                                    );
                                    let challenge =
                                        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
                                            Sha256::digest(form["code_verifier"].as_bytes()),
                                        );
                                    assert_eq!(
                                        pkce.lock().unwrap().as_deref(),
                                        Some(challenge.as_str())
                                    );
                                }
                                let request = Observed {
                                    path: parts.uri.to_string(),
                                    method: parts.method.to_string(),
                                    headers: parts.headers,
                                    body,
                                };
                                if token && let Some(assertion_verifier) = assertion_verifier {
                                    assertion_verifier(&request, &token_base);
                                }
                                observed.lock().unwrap().push(request);
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
            pkce,
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
            let mut tx = self.credentials.admin.begin().await.unwrap();
            sqlx::raw_sql("SET LOCAL ROLE wamn_system")
                .execute(&mut *tx)
                .await
                .map(drop)
                .unwrap();
            sqlx::raw_sql(sqlx::AssertSqlSafe(
                (&wamn_control_provision::bind_platform_principal_sql(
                    wamn_control_provision::PlatformComponent::Provisioning,
                ))
                    .to_string(),
            ))
            .execute(&mut *tx)
            .await
            .map(drop)
            .unwrap();
            wamn_test_infrastructure::credentials::define(
                &mut tx,
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
        use wamn_engine::release_manifest::ReleaseTarget;
        use wamn_identity_protocol::AuthorityClass;

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
        component.descriptor.operations.insert(
            invocation.operation.clone(),
            serde_json::from_value(serde_json::json!({
                "input-ports": [], "output-ports": [], "parameters": []
            }))
            .unwrap(),
        );
        component.descriptor.connections = ["handle", "other-handle"]
            .into_iter()
            .map(|name| wamn_catalog::ComponentConnection {
                store_alias: name.into(),
                requirement_type: wamn_catalog::ComponentConnectionType::Http,
                mode: wamn_catalog::ConnectionMode::Write,
            })
            .collect();
        manifest.components.insert(component);
        manifest.routes.insert(ServingRoute {
            package_id: invocation.origin.package_id.clone(),
            component: invocation.origin.component.clone(),
            operation: invocation.origin.operation.clone(),
            type_: OperationType::Command,
            reads: BTreeSet::new(),
            revision: None,
            idempotency: None,
        });
        let bytes = manifest.canonical_bytes();
        let release = Arc::new(
            LoadedRelease::load_canonical_bytes(
                &bytes,
                "oauth2-test",
                ReleaseTarget::new("acme--dev--project-a", "dev").with_applications(
                    serde_json::from_value(serde_json::json!({"app":{"packages":["package_a"]}}))
                        .unwrap(),
                ),
            )
            .unwrap(),
        );
        let manifest_digest = release.release().manifest_digest.as_str();
        let mut projects = HashMap::new();
        for project in ["project-a", "project-b"] {
            let database = wamn_platform_identity::test_database::project_platform();
            let mut admin = wamn_postgres::connect(database.url()).await.unwrap();

            sqlx::raw_sql("BEGIN; SELECT set_config('app.user_id','770df186-ac15-579e-b46b-c297cae2011b',true),set_config('app.operation','admin:seed-oauth2',true)").execute(&mut admin).await.map(drop).unwrap();
            let project_env_key = wamn_registry::project_env_key("acme", project, "dev");
            sqlx::query("INSERT INTO catalog.releases(project_env_key,manifest_digest,canonical_bytes) VALUES($1,$2,$3)").bind(&project_env_key).bind(&manifest_digest).bind(&bytes).execute(&mut admin).await.map(|result| result.rows_affected()).unwrap();
            let rows = sqlx::query("SELECT c.id,i.id,i.name FROM registry.connection c JOIN registry.integration i ON i.id=c.integration_id WHERE c.project=$1 AND c.env='dev'").bind(&project).fetch_all(&mut wamn_postgres::connect(self.credentials.database.url()).await.unwrap()).await.unwrap();
            for row in rows {
                let connection: uuid::Uuid = row.get(0);
                let integration: uuid::Uuid = row.get(1);
                let name: String = row.get(2);
                sqlx::query("INSERT INTO platform.integration(id,integration_type,name,status) VALUES($1,'http',$2,'active')").bind(&integration).bind(&name).execute(&mut admin).await.map(|result| result.rows_affected()).unwrap();
                sqlx::query("INSERT INTO platform.connection(id,integration_id,integration_type,status) VALUES($1,$2,'http','active')").bind(&connection).bind(&integration).execute(&mut admin).await.map(|result| result.rows_affected()).unwrap();
                sqlx::query("INSERT INTO catalog.connection_requirement(project_env_key,component_digest,package_id,name,interface,mode) VALUES($1,$2,'package_a',$3,'wamn:connection/http@0.1.0','write')").bind(&project_env_key).bind(&invocation.component_digest).bind(&name).execute(&mut admin).await.map(|result| result.rows_affected()).unwrap();
                sqlx::query("INSERT INTO catalog.release_connection(project_env_key,manifest_digest,connection_id,version) VALUES($1,$2,$3,1)").bind(&project_env_key).bind(&manifest_digest).bind(&connection).execute(&mut admin).await.map(|result| result.rows_affected()).unwrap();
                sqlx::query("INSERT INTO catalog.connection_binding(project_env_key,manifest_digest,component_digest,package_id,name,connection_id) VALUES($1,$2,$3,'package_a',$4,$5)").bind(&project_env_key).bind(&manifest_digest).bind(&invocation.component_digest).bind(&name).bind(&connection).execute(&mut admin).await.map(|result| result.rows_affected()).unwrap();
            }
            sqlx::raw_sql("COMMIT")
                .execute(&mut admin)
                .await
                .map(drop)
                .unwrap();
            sqlx::raw_sql("CREATE SCHEMA wamn_run")
                .execute(&mut admin)
                .await
                .map(drop)
                .unwrap();
            let role = format!("oauth2_{}_{}", database.name(), project.replace('-', "_"));
            sqlx::raw_sql(sqlx::AssertSqlSafe(
                (&wamn_control_provision::test_database::login_sql(
                    wamn_control_provision::WorkloadRoleFamily::HttpAdmitter,
                    database.name(),
                    &role,
                    "oauth2-test-password",
                ))
                    .to_string(),
            ))
            .execute(&mut admin)
            .await
            .map(drop)
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
                Publisher::default(),
                Arc::new(self.credentials.reader("project-a").await),
                Arc::clone(&postgres),
                "acme--dev--project-a",
                "project-a",
                "dev",
            )
            .with_oauth_encryption(Some(Arc::new(
                crate::oauth_encryption::Envelope::from_file(self.key.path()).unwrap(),
            ))),
        );
        postgres
            .set_project_env_key("instance", "acme--dev--project-a")
            .unwrap();
        let plugin = ConnectionHttp::new(
            postgres,
            Arc::new(HttpTransport::new().unwrap()),
            facts,
            "project-a",
            vec![self.base.strip_suffix("bound/").unwrap().parse().unwrap()].into(),
            release,
        );
        plugin
            .postgres
            .bind_invocation("instance", invocation)
            .unwrap();
        plugin
    }

    async fn other_host(&self, first: &ConnectionHttp) -> ConnectionHttp {
        let facts = Arc::new(
            ConnectionFacts::new(
                Arc::new(ConnectionMemory::default()),
                Publisher::default(),
                Arc::new(self.credentials.reader("project-a").await),
                Arc::clone(&first.postgres),
                "acme--dev--project-a",
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
            "project-a",
            Arc::clone(&first.allowed_hosts),
            Arc::clone(&first.release),
        );
        plugin
    }

    fn pause_token(&self) -> ResponsePause {
        let (arrived, arrival) = tokio::sync::oneshot::channel();
        let (resume, resumed) = tokio::sync::oneshot::channel();
        *self.token_pause.lock().unwrap() = Some((arrived, resumed));
        (resume, arrival)
    }

    async fn due(&self, plugin: &ConnectionHttp) {
        sqlx::query("UPDATE identity.oauth_token SET expires_at='-infinity'")
            .execute(
                &mut wamn_postgres::connect(self.credentials.database.url())
                    .await
                    .unwrap(),
            )
            .await
            .map(|result| result.rows_affected())
            .unwrap();
        plugin.oauth2.entries.lock().unwrap().clear();
    }

    async fn stored(&self, connection: uuid::Uuid) -> crate::oauth_token::Token {
        let row = sqlx::query("SELECT ciphertext,nonce,wrapped_key,key_id FROM identity.oauth_token WHERE connection_id=$1").bind(&connection).fetch_one(&mut wamn_postgres::connect(self.credentials.database.url()).await.unwrap()).await.unwrap();
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
        use wamn_engine::release_manifest::ReleaseTarget;
        let project_env_key = wamn_registry::project_env_key("acme", project, "dev");
        plugin.release = Arc::new(
            LoadedRelease::load_canonical_bytes(
                &plugin.release.manifest().canonical_bytes(),
                "oauth2-test",
                ReleaseTarget::new(&project_env_key, "dev").with_applications(
                    serde_json::from_value(serde_json::json!({"app":{"packages":["package_a"]}}))
                        .unwrap(),
                ),
            )
            .unwrap(),
        );
        plugin.facts = Arc::new(
            ConnectionFacts::new(
                Arc::new(ConnectionMemory::default()),
                Publisher::default(),
                Arc::new(self.credentials.reader(project).await),
                Arc::clone(&plugin.postgres),
                &project_env_key,
                project,
                "dev",
            )
            .with_oauth_encryption(Some(Arc::new(
                crate::oauth_encryption::Envelope::from_file(self.key.path()).unwrap(),
            ))),
        );
        plugin.oauth2 = plugin.facts.oauth_tokens();
        plugin
            .postgres
            .set_project_env_key("instance", &project_env_key)
            .unwrap();
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
        let mut tx = self.credentials.admin.begin().await.unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning,
            ))
                .to_string(),
        ))
        .execute(&mut *tx)
        .await
        .map(drop)
        .unwrap();
        sqlx::raw_sql("SET LOCAL ROLE wamn_control")
            .execute(&mut *tx)
            .await
            .map(drop)
            .unwrap();
        let mut drops = wamn_broker::connection_changes::ConnectionDrops::default();
        wamn_platform_identity::connections::administration::execute_with_store(
            &mut tx, "acme", "connection.write",
            &serde_json::json!({
                "project":"project-a", "env":"dev", "name":"handle",
                "parameters":{"primary-authority":self.base,"tls-verification":"disabled","auth":{}},
                "credential":r#"{"username":"fixture","password":"new"}"#
            }).to_string(),
            &mut wamn_broker::auth_changes::AuthDrops::default(),
            &mut drops, Some(self.credentials.store.as_ref()),
        ).await.unwrap();
        tx.commit().await.unwrap();
        assert!(!drops.is_empty());
        wamn_broker::Publisher::default().publish(drops).await;
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
    sqlx::query(
        "UPDATE identity.oauth_token SET expires_at=clock_timestamp()+interval '60 seconds'",
    )
    .execute(&mut fixture.credentials.admin)
    .await
    .map(|result| result.rows_affected())
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
    }
    fixture.use_project(&mut plugin, "project-b").await;
    fixture.send(&plugin, "handle").await.unwrap();
    let entries = plugin.oauth2.entries.lock().unwrap();
    assert!(!entries.contains_key(&(connection, first.active_generation)));
    assert!(!entries.contains_key(&(connection, second.active_generation)));
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
        assert!(oauth2_credential_for("oauth2_client_credentials", &valid, material).is_err());
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
        assert!(
            oauth2_credential_for("oauth2_client_credentials", &parameters, Some(&secret)).is_err()
        );
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
    let mut monitor = wamn_postgres::connect(fixture.credentials.database.url())
        .await
        .unwrap();
    let (resume, arrived) = fixture.pause_token();
    let (a, b, ()) = tokio::join!(
        fixture.send(&first, "handle"),
        fixture.send(&second, "handle"),
        async {
            arrived.await.unwrap();
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let blocked: bool = sqlx::query("SELECT EXISTS(SELECT FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'SELECT registry.lock_oauth_connection%')").fetch_one(&mut monitor).await.unwrap().get(0);
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
        let row = sqlx::query("SELECT refresh_status FROM identity.oauth_token")
            .fetch_one(&mut fixture.credentials.admin)
            .await
            .unwrap();
        assert_eq!(row.get::<String, _>(0), "failing");
        sqlx::query(
            "UPDATE identity.oauth_token SET updated_at=clock_timestamp()-interval '31 seconds'",
        )
        .execute(&mut fixture.credentials.admin)
        .await
        .map(|result| result.rows_affected())
        .unwrap();
        assert!(matches!(
            fixture.send(&second, "handle").await,
            Err(ConnectionError::CredentialUnavailable)
        ));
        assert_eq!(fixture.counts().0 - before, 2);
        sqlx::query(
            "UPDATE identity.oauth_token SET updated_at=clock_timestamp()-interval '31 seconds'",
        )
        .execute(&mut fixture.credentials.admin)
        .await
        .map(|result| result.rows_affected())
        .unwrap();
        assert_eq!(fixture.send(&first, "handle").await.unwrap().status, 200);
        assert_eq!(fixture.counts().0 - before, 3);
        assert_eq!(
            sqlx::query("SELECT refresh_status FROM identity.oauth_token")
                .fetch_one(&mut fixture.credentials.admin)
                .await
                .unwrap()
                .get::<String, _>(0),
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
    let mut rotating = wamn_postgres::connect(fixture.credentials.database.url())
        .await
        .unwrap();

    let pid: i32 = sqlx::query("SELECT pg_backend_pid()")
        .fetch_one(&mut rotating)
        .await
        .unwrap()
        .get(0);
    let store = Arc::clone(&fixture.credentials.store);
    let rotation = tokio::spawn(async move {
        let mut tx = rotating.begin().await.unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning,
            ))
                .to_string(),
        ))
        .execute(&mut *tx)
        .await
        .map(drop)
        .unwrap();
        sqlx::raw_sql("SET LOCAL ROLE wamn_control")
            .execute(&mut *tx)
            .await
            .map(drop)
            .unwrap();
        let result = wamn_platform_identity::connections::administration::execute_with_store(&mut tx,"acme","connection.set_credential",
            &serde_json::json!({"project":"project-a","env":"dev","name":"handle","credential":r#"{"client_secret":"rotated-secret"}"#}).to_string(),
            &mut wamn_broker::auth_changes::AuthDrops::default(),
            &mut wamn_broker::connection_changes::ConnectionDrops::default(),Some(store.as_ref())).await.unwrap();
        assert!(result.get("error").is_none(), "{result}");
        tx.commit().await.unwrap();
    });
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let blocked: bool = sqlx::query("SELECT cardinality(pg_blocking_pids($1))>0")
                .bind(&pid)
                .fetch_one(&mut fixture.credentials.admin)
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
        sqlx::query("SELECT count(*) FROM identity.oauth_token WHERE connection_id=$1")
            .bind(&connection)
            .fetch_one(&mut fixture.credentials.admin)
            .await
            .unwrap()
            .get::<i64, _>(0),
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
    // This refresh-row test seeds the grant directly; the PKCE flow runs below.
    sqlx::query("UPDATE registry.integration SET auth_type='oauth2_authorization_code'")
        .execute(&mut fixture.credentials.admin)
        .await
        .map(|result| result.rows_affected())
        .unwrap();
    let code_host = fixture.other_host(&plugin).await;
    assert!(matches!(
        fixture.send(&code_host, "handle").await,
        Err(ConnectionError::Unauthorized)
    ));
    assert_eq!(fixture.counts(), (0, 0));
    sqlx::query("UPDATE registry.integration SET auth_type='oauth2_client_credentials'")
        .execute(&mut fixture.credentials.admin)
        .await
        .map(|result| result.rows_affected())
        .unwrap();
    fixture.send(&plugin, "handle").await.unwrap();
    sqlx::query("UPDATE registry.integration SET auth_type='oauth2_authorization_code'")
        .execute(&mut fixture.credentials.admin)
        .await
        .map(|result| result.rows_affected())
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
    sqlx::query("UPDATE identity.oauth_token SET ciphertext=$2,nonce=$3,wrapped_key=$4,key_id=$5,expires_at='-infinity' WHERE connection_id=$1").bind(&connection).bind(&encrypted.ciphertext).bind(&encrypted.nonce).bind(&encrypted.wrapped_key).bind(&encrypted.key_id).execute(&mut fixture.credentials.admin).await.map(|result| result.rows_affected()).unwrap();
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

async fn jwt_round_trip(algorithm: &'static str, separator: Option<&'static str>) {
    use super::super::jwt::tests::{assert_assertion, material};

    let _lock = wamn_test_postgres::lock();
    let identifiers = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&identifiers);
    let assertion_verifier: AssertionVerifier = Arc::new(move |request, base| {
        assert_eq!(request.method, "POST");
        assert!(!request.headers.contains_key("authorization"));
        assert_eq!(
            request.headers["content-type"],
            "application/x-www-form-urlencoded"
        );
        let jti = assert_assertion(
            &request.body,
            algorithm,
            CLIENT_ID,
            &format!("{base}token?audience=api"),
            Some(if separator == Some(",") { "a,b" } else { "a b" }),
            None,
        );
        let mut identifiers = recorded.lock().unwrap();
        assert!(!identifiers.contains(&jti));
        identifiers.push(jti);
    });
    let mut fixture = Fixture::start_with_verifier(
        vec![token(1, Some(3600)), token(2, Some(3600))],
        vec![401, 200],
        Some(assertion_verifier),
    )
    .await;
    let mut parameters = fixture.parameters();
    parameters["scopes"] = serde_json::json!(["a", "b"]);
    parameters["key_id"] = "certificate-id".into();
    parameters["algorithm"] = algorithm.into();
    if let Some(separator) = separator {
        parameters["scope_separator"] = separator.into();
    }
    let plugin = fixture
        .plugin_with_auth(
            &parameters,
            "oauth2_client_credentials_jwt",
            &material(algorithm),
        )
        .await;
    assert_eq!(fixture.send(&plugin, "handle").await.unwrap().status, 200);
    assert_eq!(fixture.counts(), (2, 2));
    let connection = plugin.facts.env_connections().await.unwrap().connections["handle"];
    assert_eq!(
        fixture.stored(connection).await.access_token.as_deref(),
        Some("fixture-token-2")
    );
}

impl Fixture {
    async fn authorize(&mut self, oauth: &crate::oauth_authorization::Authorization) -> url::Url {
        let mut tx = self.credentials.admin.begin().await.unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning,
            ))
                .to_string(),
        ))
        .execute(&mut *tx)
        .await
        .map(drop)
        .unwrap();
        sqlx::raw_sql("SET LOCAL ROLE wamn_control")
            .execute(&mut *tx)
            .await
            .map(drop)
            .unwrap();
        let context = wamn_platform_identity::connections::administration::configure(
            &mut tx,
            "acme",
            "connection.authorize",
            &serde_json::json!({"project":"project-a","env":"dev","name":"handle"}),
        )
        .await
        .unwrap();
        let url = oauth
            .authorize(
                &mut tx,
                &context,
                wamn_control_provision::PlatformComponent::Provisioning.principal_id(),
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let url = url::Url::parse(&url).unwrap();
        *self.pkce.lock().unwrap() = url
            .query_pairs()
            .find(|(key, _)| key == "code_challenge")
            .map(|(_, value)| value.into_owned());
        url
    }

    async fn rewrite(&mut self, secret: Option<&str>) {
        let mut tx = self.credentials.admin.begin().await.unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning,
            ))
                .to_string(),
        ))
        .execute(&mut *tx)
        .await
        .map(drop)
        .unwrap();
        sqlx::raw_sql("SET LOCAL ROLE wamn_control")
            .execute(&mut *tx)
            .await
            .map(drop)
            .unwrap();
        let context = wamn_platform_identity::connections::administration::configure(
            &mut tx,
            "acme",
            "connection.get",
            &serde_json::json!({"project":"project-a","env":"dev","name":"handle"}),
        )
        .await
        .unwrap();
        let (operation, request) = match secret {
            Some(secret) => (
                "connection.set_credential",
                serde_json::json!({"project":"project-a","env":"dev","name":"handle","credential":serde_json::json!({"client_secret":secret}).to_string()}),
            ),
            None => (
                "connection.write",
                serde_json::json!({"project":"project-a","env":"dev","name":"handle","parameters":context["parameters"]}),
            ),
        };
        wamn_platform_identity::connections::administration::execute_with_store(
            &mut tx,
            "acme",
            operation,
            &request.to_string(),
            &mut wamn_broker::auth_changes::AuthDrops::default(),
            &mut wamn_broker::connection_changes::ConnectionDrops::default(),
            Some(self.credentials.store.as_ref()),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
}

#[tokio::test]
async fn authorization_code_pkce_callback_refresh_and_secret_rotation() {
    use base64::Engine as _;
    let _lock = wamn_test_postgres::lock();
    let mut first = serde_json::from_str::<serde_json::Value>(&token(1, Some(3600)).1).unwrap();
    first["refresh_token"] = "original-refresh+&".into();
    let mut fixture = Fixture::start(
        vec![
            (400, r#"{"error":"invalid_grant"}"#.into()),
            (503, "provider unavailable".into()),
            (200, first.to_string()),
            token(2, Some(3600)),
            token(3, Some(3600)),
        ],
        vec![200; 3],
    )
    .await;
    let mut parameters = fixture.parameters();
    parameters["authorize_url"] =
        format!("{}authorize?audience=existing%2Bvalue", fixture.base).into();
    let plugin = fixture
        .plugin_with_auth(
            &parameters,
            "oauth2_authorization_code",
            &serde_json::json!({"client_secret":CLIENT_SECRET}).to_string(),
        )
        .await;
    let connection = plugin.facts.env_connections().await.unwrap().connections["handle"];
    let oauth = crate::oauth_authorization::Authorization::new(
        "http://control.example.test",
        plugin.facts.oauth_encryption().unwrap(),
    )
    .unwrap()
    .with_store(Arc::<wamn_kubernetes::test_util::MemoryStore>::clone(
        &fixture.credentials.store,
    ));
    let url = fixture.authorize(&oauth).await;
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(query["audience"], "existing+value");
    assert_eq!(query["response_type"], "code");
    assert_eq!(query["client_id"], CLIENT_ID);
    assert_eq!(query["redirect_uri"], oauth.redirect_uri());
    assert_eq!(query["scope"], "orders:read orders:write+");
    assert_eq!(query["code_challenge_method"], "S256");
    let callback = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs([
            ("state", query["state"].as_str()),
            ("code", "fixture-code+&"),
        ])
        .finish();
    assert!(
        oauth
            .callback(&mut fixture.credentials.admin, "acme", &callback)
            .await
            .unwrap_err()
            .reason
            .contains("invalid_grant")
    );
    assert!(
        oauth
            .callback(&mut fixture.credentials.admin, "acme", &callback)
            .await
            .unwrap_err()
            .reason
            .contains("HTTP 503")
    );
    assert_eq!(
        sqlx::query("SELECT count(*) FROM identity.oauth_authorization WHERE state=$1")
            .bind(&query["state"])
            .fetch_one(&mut fixture.credentials.admin)
            .await
            .unwrap()
            .get::<i64, _>(0),
        1
    );
    assert_eq!(
        sqlx::query("SELECT count(*) FROM identity.oauth_token")
            .fetch_one(&mut fixture.credentials.admin)
            .await
            .unwrap()
            .get::<i64, _>(0),
        0
    );
    oauth
        .callback(&mut fixture.credentials.admin, "acme", &callback)
        .await
        .unwrap();
    assert!(
        oauth
            .callback(&mut fixture.credentials.admin, "acme", &callback)
            .await
            .unwrap_err()
            .reason
            .contains("replayed")
    );
    fixture.send(&plugin, "handle").await.unwrap();
    assert_eq!(
        fixture.stored(connection).await.refresh_token.as_deref(),
        Some("original-refresh+&")
    );
    fixture.due(&plugin).await;
    fixture.send(&plugin, "handle").await.unwrap();
    fixture.rewrite(Some("rotated-secret")).await;
    assert_eq!(
        fixture.stored(connection).await.refresh_token.as_deref(),
        Some("original-refresh+&")
    );
    plugin.facts.read_again(&connection).await.unwrap();
    fixture.send(&plugin, "handle").await.unwrap();
    let observed = fixture.observed.lock().unwrap();
    let refreshes: Vec<_> = observed
        .iter()
        .filter(|request| request.body.starts_with(b"grant_type=refresh_token"))
        .collect();
    assert_eq!(refreshes.len(), 2);
    for request in &refreshes {
        assert_eq!(
            request.body,
            b"grant_type=refresh_token&refresh_token=original-refresh%2B%26"
        );
    }
    let basic = refreshes[1].headers["authorization"]
        .to_str()
        .unwrap()
        .strip_prefix("Basic ")
        .unwrap();
    assert!(
        String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(basic)
                .unwrap()
        )
        .unwrap()
        .ends_with(":rotated-secret")
    );
    assert_eq!(
        observed
            .iter()
            .filter(|request| request.path == "/bound/api")
            .map(|request| request.headers["authorization"].to_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "Bearer fixture-token-1",
            "Bearer fixture-token-2",
            "Bearer fixture-token-3"
        ]
    );
}

#[tokio::test]
async fn jwt_ps256_locked_fetch_and_401_retry() {
    jwt_round_trip("PS256", Some(",")).await;
}

#[tokio::test]
async fn authorization_code_expired_provider_error_and_new_generation_refuse_without_writes() {
    let _lock = wamn_test_postgres::lock();
    let mut fixture = Fixture::start(vec![], vec![]).await;
    let mut parameters = fixture.parameters();
    parameters["authorize_url"] = format!("{}authorize", fixture.base).into();
    let plugin = fixture
        .plugin_with_auth(
            &parameters,
            "oauth2_authorization_code",
            &serde_json::json!({"client_secret":CLIENT_SECRET}).to_string(),
        )
        .await;
    let oauth = crate::oauth_authorization::Authorization::new(
        "http://control.example.test",
        plugin.facts.oauth_encryption().unwrap(),
    )
    .unwrap()
    .with_store(Arc::<wamn_kubernetes::test_util::MemoryStore>::clone(
        &fixture.credentials.store,
    ));
    let url = fixture.authorize(&oauth).await;
    let state = url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned();
    let callback = format!("state={state}&code=unused");
    assert!(
        oauth
            .callback(
                &mut fixture.credentials.admin,
                "acme",
                &format!("state={state}&error=access_denied")
            )
            .await
            .unwrap_err()
            .reason
            .contains("access_denied")
    );
    sqlx::query("UPDATE identity.oauth_authorization SET expires_at=clock_timestamp()-interval '1 second' WHERE state=$1").bind(&state).execute(&mut fixture.credentials.admin).await.map(|result| result.rows_affected()).unwrap();
    assert_eq!(
        oauth
            .callback(&mut fixture.credentials.admin, "acme", &callback)
            .await
            .unwrap_err()
            .reason,
        "expired state"
    );
    assert_eq!(
        sqlx::query("SELECT count(*) FROM identity.oauth_authorization WHERE state=$1")
            .bind(&state)
            .fetch_one(&mut fixture.credentials.admin)
            .await
            .unwrap()
            .get::<i64, _>(0),
        1
    );
    let url = fixture.authorize(&oauth).await;
    let state = url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .into_owned();
    // Exercise the generation guard independently of write's pending-state deletion.
    sqlx::query("UPDATE identity.oauth_authorization SET generation=generation-1 WHERE state=$1")
        .bind(&state)
        .execute(&mut fixture.credentials.admin)
        .await
        .map(|result| result.rows_affected())
        .unwrap();
    assert_eq!(
        oauth
            .callback(
                &mut fixture.credentials.admin,
                "acme",
                &format!("state={state}&code=unused")
            )
            .await
            .unwrap_err()
            .reason,
        "connection generation changed"
    );
    fixture.rewrite(None).await;
    assert!(
        oauth
            .callback(
                &mut fixture.credentials.admin,
                "acme",
                &format!("state={state}&code=unused")
            )
            .await
            .unwrap_err()
            .reason
            .contains("unknown")
    );
    assert_eq!(fixture.counts(), (0, 0));
    assert_eq!(
        sqlx::query("SELECT count(*) FROM identity.oauth_token")
            .fetch_one(&mut fixture.credentials.admin)
            .await
            .unwrap()
            .get::<i64, _>(0),
        0
    );
}

struct TlsTokenEndpoint {
    url: String,
    transport: Arc<HttpTransport>,
    observed: Arc<Mutex<Vec<Observed>>>,
    accepted: Arc<std::sync::atomic::AtomicUsize>,
    task: JoinHandle<()>,
}

impl TlsTokenEndpoint {
    async fn start() -> Self {
        let certificate = rcgen::generate_simple_self_signed(vec![
            super::interface_address().to_string(),
            "localhost".into(),
        ])
        .unwrap();
        let server = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.cert.der().clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
                certificate.signing_key.serialize_der(),
            )),
        )
        .unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(certificate.cert.der().clone()).unwrap();
        let client = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
        let transport = Arc::new(HttpTransport::with_tls_config(client));
        let listener = TcpListener::bind((super::interface_address(), 0))
            .await
            .unwrap();
        let url = format!("https://{}/token", listener.local_addr().unwrap());
        let observed = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&observed);
        let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let accept_count = Arc::clone(&accepted);
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server));
        let task = tokio::spawn(async move {
            loop {
                let (tcp, _) = listener.accept().await.unwrap();
                accept_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let stream = acceptor.accept(tcp).await.unwrap();
                let observed = Arc::clone(&recorded);
                hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                    let observed = Arc::clone(&observed);
                    async move {
                        let (parts, body) = request.into_parts();
                        let body = body.collect().await.unwrap().to_bytes().to_vec();
                        observed.lock().unwrap().push(Observed { path: parts.uri.to_string(), method: parts.method.to_string(), headers: parts.headers, body });
                        Ok::<_,Infallible>(hyper::Response::builder().header("connection","close").body(Full::new(bytes::Bytes::from_static(br#"{"access_token":"fixture-token-1","refresh_token":"separate-refresh","token_type":"Bearer","expires_in":3600}"#))).unwrap())
                    }
                })).await.unwrap();
            }
        });
        Self {
            url,
            transport,
            observed,
            accepted,
            task,
        }
    }

    fn authority(&self) -> String {
        let url = url::Url::parse(&self.url).unwrap();
        format!(
            "{}:{}",
            url.host_str().unwrap(),
            url.port_or_known_default().unwrap()
        )
    }
}

impl Drop for TlsTokenEndpoint {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn token_authority_health_allows_secret_and_jwt_and_refuses_other_origins() {
    let endpoint = TlsTokenEndpoint::start().await;
    let refused_endpoint = TlsTokenEndpoint::start().await;
    let port = url::Url::parse(&endpoint.url).unwrap().port().unwrap();
    for auth_type in ["oauth2_client_credentials", "oauth2_client_credentials_jwt"] {
        let mut auth = serde_json::json!({"client_id":"client", "scopes":["a","b"], "token_url":endpoint.url, "token_authority":endpoint.authority()});
        let material = if auth_type == "oauth2_client_credentials_jwt" {
            auth["algorithm"] = "ES256".into();
            auth["key_id"] = "certificate-id".into();
            super::super::jwt::tests::material("ES256")
        } else {
            serde_json::json!({"client_secret":"secret"}).to_string()
        };
        let mut parameters = serde_json::json!({"primary-authority":format!("https://{}:{port}/api/", super::interface_address()), "tls-verification":"verify-authority", "auth":auth});
        health(&endpoint.transport, &parameters, auth_type, Some(&material))
            .await
            .unwrap();
        let before = endpoint.observed.lock().unwrap().len();
        parameters["auth"]["token_url"] = refused_endpoint.url.clone().into();
        assert!(
            health(&endpoint.transport, &parameters, auth_type, Some(&material))
                .await
                .is_err()
        );
        assert_eq!(endpoint.observed.lock().unwrap().len(), before);
        assert!(refused_endpoint.observed.lock().unwrap().is_empty());
        assert_eq!(
            refused_endpoint
                .accepted
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
    }
}

#[tokio::test]
async fn token_authority_client_credentials_fetch_and_refresh_refuse_other_origins() {
    let _lock = wamn_test_postgres::lock();
    for allowed in [true, false] {
        let endpoint = TlsTokenEndpoint::start().await;
        let other = TlsTokenEndpoint::start().await;
        let mut fixture = Fixture::start(vec![], vec![200; 2]).await;
        let mut auth = fixture.parameters();
        auth["token_authority"] = endpoint.authority().into();
        auth["token_url"] = if allowed { &endpoint.url } else { &other.url }
            .clone()
            .into();
        let mut plugin = fixture.plugin(&auth).await;
        plugin.transport = Arc::clone(&endpoint.transport);
        let mut hosts = plugin.allowed_hosts.to_vec();
        hosts.push(AllowedHost::Authority(
            endpoint.authority().parse().unwrap(),
        ));
        plugin.allowed_hosts = hosts.into();
        if allowed {
            fixture.send(&plugin, "handle").await.unwrap();
            fixture.due(&plugin).await;
            fixture.send(&plugin, "handle").await.unwrap();
            assert_eq!(endpoint.observed.lock().unwrap().len(), 2);
        } else {
            assert!(matches!(
                fixture.send(&plugin, "handle").await,
                Err(ConnectionError::CredentialUnavailable)
            ));
            assert!(endpoint.observed.lock().unwrap().is_empty());
            assert_eq!(fixture.counts(), (0, 0));
        }
        assert!(other.observed.lock().unwrap().is_empty());
        assert_eq!(other.accepted.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn token_authority_authorization_code_exchange_and_refresh_refuse_other_origins() {
    use base64::Engine as _;
    use sha2::{Digest as _, Sha256};
    let _lock = wamn_test_postgres::lock();
    for allowed in [true, false] {
        let endpoint = TlsTokenEndpoint::start().await;
        let other = TlsTokenEndpoint::start().await;
        let mut fixture = Fixture::start(vec![], vec![200; 2]).await;
        let mut auth = fixture.parameters();
        auth["authorize_url"] = "https://browser.example/authorize".into();
        auth["token_authority"] = endpoint.authority().into();
        auth["token_url"] = if allowed { &endpoint.url } else { &other.url }
            .clone()
            .into();
        let mut plugin = fixture
            .plugin_with_auth(
                &auth,
                "oauth2_authorization_code",
                &serde_json::json!({"client_secret":CLIENT_SECRET}).to_string(),
            )
            .await;
        plugin.transport = Arc::clone(&endpoint.transport);
        let mut hosts = plugin.allowed_hosts.to_vec();
        hosts.push(AllowedHost::Authority(
            endpoint.authority().parse().unwrap(),
        ));
        plugin.allowed_hosts = hosts.into();
        let oauth = crate::oauth_authorization::Authorization::new(
            "http://control.example.test",
            plugin.facts.oauth_encryption().unwrap(),
        )
        .unwrap()
        .with_store(Arc::<wamn_kubernetes::test_util::MemoryStore>::clone(
            &fixture.credentials.store,
        ))
        .with_transport(Arc::clone(&endpoint.transport));
        let url = fixture.authorize(&oauth).await;
        assert_eq!(url.host_str(), Some("browser.example"));
        let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        let result = oauth
            .callback(
                &mut fixture.credentials.admin,
                "acme",
                &format!("state={}&code=code", query["state"]),
            )
            .await;
        if allowed {
            result.unwrap();
            fixture.send(&plugin, "handle").await.unwrap();
            fixture.due(&plugin).await;
            fixture.send(&plugin, "handle").await.unwrap();
            let requests = endpoint.observed.lock().unwrap();
            assert_eq!(requests.len(), 2);
            let form: std::collections::HashMap<_, _> =
                url::form_urlencoded::parse(&requests[0].body)
                    .into_owned()
                    .collect();
            assert_eq!(
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .encode(Sha256::digest(form["code_verifier"].as_bytes())),
                query["code_challenge"]
            );
            assert_eq!(form["grant_type"], "authorization_code");
            assert_eq!(
                requests[1].body,
                b"grant_type=refresh_token&refresh_token=separate-refresh"
            );
        } else {
            assert_eq!(
                result.unwrap_err().reason,
                "token endpoint authority refused"
            );
            assert!(endpoint.observed.lock().unwrap().is_empty());
            assert_eq!(fixture.counts(), (0, 0));
            assert_eq!(
                sqlx::query("SELECT count(*) FROM identity.oauth_authorization WHERE state=$1")
                    .bind(&query["state"])
                    .fetch_one(&mut fixture.credentials.admin)
                    .await
                    .unwrap()
                    .get::<i64, _>(0),
                1
            );
            assert_eq!(
                sqlx::query("SELECT count(*) FROM identity.oauth_token")
                    .fetch_one(&mut fixture.credentials.admin)
                    .await
                    .unwrap()
                    .get::<i64, _>(0),
                0
            );
        }
        assert!(other.observed.lock().unwrap().is_empty());
        assert_eq!(other.accepted.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}
