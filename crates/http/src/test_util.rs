//! HTTP boundary fixtures shared with invocation tests.
use crate::bindings::wamn::connection::http::{Header, Request, Response};
use crate::transport::HttpTransport;
use crate::{ConnectionHttp, ConnectionTypeDescriptor};
use http_body_util::{BodyExt as _, Full};
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use sqlx::{Connection as _, Row as _};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::convert::Infallible;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use wamn_authority::connection::{
    ConnectionError, ConnectionExecutionClosure, ConnectionInvocation, ConnectionOrigin,
    InvocationEntry,
};
use wamn_authority::identity::ConnectionEffectSnapshot;
use wamn_broker::Publisher;
use wamn_catalog::{
    AdmittedComponentOperation, OperationType, PackageCoordinate, SERVING_MANIFEST_FORMAT_VERSION,
    ServingComponent, ServingManifest, ServingRelease, ServingRoute,
};
use wamn_connections::connection_facts::ConnectionFacts;
use wamn_connections::connection_memory::ConnectionMemory;
use wamn_engine::release_manifest::LoadedRelease;
use wamn_sql::WamnPostgres;
pub fn interface_address() -> std::net::IpAddr {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").expect("address selection socket");
    socket
        .connect("192.0.2.1:9")
        .expect("select a private interface");
    socket.local_addr().expect("interface address").ip()
}

pub fn digest(byte: char) -> wamn_digest::Digest {
    format!("sha256:{}", byte.to_string().repeat(64))
        .parse()
        .unwrap()
}

pub fn invocation() -> ConnectionInvocation {
    ConnectionInvocation {
        origin: ConnectionOrigin {
            package_id: "package_a".to_string(),
            component_digest: digest('a'),
            component: "notifier".to_string(),
            interface_version: "0.1".to_string(),
            operation: "orders:notify/dispatch@1.0.0".to_string(),
        },
        entry: wamn_authority::connection::InvocationEntry::Wiring(
            wamn_authority::connection::WiringPosition {
                package_id: "package_a".to_string(),
                wiring_hash: fixture_wiring_hash(),
                wiring_id: "orders".to_string(),
                wiring_version: 3,
                node_id: "notify".to_string(),
                occurrence: 2,
                configuration: Arc::default(),
            },
        ),
        package_id: "package_a".to_string(),
        component_digest: digest('a'),
        component: "notifier".to_string(),
        operation: "orders:notify/dispatch@1.0.0".to_string(),
        closure: ConnectionExecutionClosure::Released,
        effects: None,
    }
}

pub fn fixture_wiring_hash() -> wamn_digest::Digest {
    wamn_catalog::fixture_graph("orders", 3).wiring_hash()
}

pub fn snapshot() -> ConnectionEffectSnapshot {
    ConnectionEffectSnapshot {
        connection_name: None,
        read_only: false,
        wiring_hash: Some(fixture_wiring_hash()),
        component: Some("notifier".to_string()),
        interface_version: Some("0.1".to_string()),
        operation: Some("orders:notify/dispatch@1.0.0".to_string()),
        registered_operation: Some("package-a:orders/notify@1.0.0".parse().unwrap()),
        requirement_json: Some(serde_json::json!({
            "component-digest": digest('a'),
            "store-alias": "erp",
            "requirement": {
                "requirement-type": "http",
                "contract": ConnectionTypeDescriptor::http_v1().contract,
            },
        })),
        requirement_hash: Some(digest('d')),
        node_permitted: true,
        binding_active: true,
        binding_valid: true,
        instance_id: Some("manager".to_string()),
        validation_hash: Some(digest('e')),
        requirement_type: Some("http".to_string()),
        contract: Some(ConnectionTypeDescriptor::http_v1().contract),
        instance_enabled: true,
        active_generation: Some(7),
        pinned_generation: None,
        instance_revision: Some(2),
        generation: Some(7),
        definition: Some(serde_json::json!({})),
        definition_hash: Some(digest('c')),
        credential_handle: Some("manager-v7".to_string()),
    }
}

pub fn manifest() -> ServingManifest {
    let invocation = invocation();
    let snapshot = snapshot();
    ServingManifest {
        composed_entry: None,
        format_version: SERVING_MANIFEST_FORMAT_VERSION,
        release: ServingRelease {
            packages: BTreeSet::from([PackageCoordinate::new(
                "package_a",
                "1.0.0".parse().unwrap(),
            )
            .unwrap()]),
        },
        components: BTreeSet::from([ServingComponent {
            runtime_schema: None,
            descriptor: {
                let mut descriptor = wamn_catalog::ComponentDescriptor::named(
                    snapshot.component.clone().expect("component"),
                    snapshot
                        .interface_version
                        .clone()
                        .expect("interface version"),
                    invocation.component_digest,
                );
                descriptor.operations = BTreeMap::from([(
                    snapshot.operation.expect("operation"),
                    AdmittedComponentOperation {
                        host_transaction: false,
                        input_ports: Vec::new(),
                        output_ports: Vec::new(),
                        parameters: Vec::new(),
                        committed_result_schema: None,

                        registered_operation: snapshot.registered_operation,
                    },
                )]);
                descriptor
            },
            package_id: invocation.package_id,
        }]),
        routes: BTreeSet::new(),
        attachments: BTreeMap::new(),
        host_routes: BTreeSet::new(),
    }
}

pub fn effect_request(headers: Vec<Header>) -> Request {
    Request {
        requirement: "manager".to_string(),
        method: "POST".to_string(),
        path_and_query: "/notify".to_string(),
        headers,
        body: None,
        idempotency_key: None,
    }
}

pub(crate) const CLIENT_ID: &str = "private client:id+";
pub(crate) const CLIENT_SECRET: &str = "private/secret&+";

#[derive(Debug)]
pub struct Observed {
    pub path: String,
    pub method: String,
    pub headers: hyper::HeaderMap,
    pub body: Vec<u8>,
}

pub(crate) type AssertionVerifier = Arc<dyn Fn(&Observed, &str) + Send + Sync>;

type ResponsePause = (
    tokio::sync::oneshot::Sender<()>,
    tokio::sync::oneshot::Receiver<()>,
);

pub struct TestHttp {
    pub(crate) http: ConnectionHttp,
    pub facts: Arc<ConnectionFacts>,
    pub release: Arc<LoadedRelease>,
    pub(crate) oauth2: Arc<wamn_connections::oauth_token::Cache>,
    pub(crate) scope: String,
    pub database: url::Url,
}
impl std::ops::DerefMut for TestHttp {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.http
    }
}
impl std::ops::Deref for TestHttp {
    type Target = ConnectionHttp;
    fn deref(&self) -> &Self::Target {
        &self.http
    }
}

pub struct Fixture {
    pub base: String,
    pub(crate) observed: Arc<Mutex<Vec<Observed>>>,
    pub(crate) task: JoinHandle<()>,
    pub(crate) credentials: wamn_connections::connection_credentials::test_util::Fixture,
    pub(crate) server: &'static tokio::sync::Mutex<wamn_test_postgres::OwnedPostgres>,
    pub(crate) pause: Arc<Mutex<Option<ResponsePause>>>,
    pub(crate) key: tempfile::NamedTempFile,
    pub(crate) token_pause: Arc<Mutex<Option<ResponsePause>>>,
    pub pkce: Arc<Mutex<Option<String>>>,
}

impl Fixture {
    pub async fn start(tokens: Vec<(u16, String)>, api: Vec<u16>) -> Self {
        Self::start_with_verifier(tokens, api, None).await
    }

    pub async fn start_with_verifier(
        tokens: Vec<(u16, String)>,
        api: Vec<u16>,
        assertion_verifier: Option<AssertionVerifier>,
    ) -> Self {
        // Keep one server across cases, with separate databases for each fixture.
        // Its watcher stops and removes it when this test process exits.
        static SERVER: OnceLock<tokio::sync::Mutex<wamn_test_postgres::OwnedPostgres>> =
            OnceLock::new();
        let server = SERVER.get_or_init(|| {
            // These disposable databases need transaction visibility, without disk flush waits.
            tokio::sync::Mutex::new(wamn_test_postgres::start(&[("fsync", "off")]).unwrap())
        });
        let control = server
            .lock()
            .await
            .create_database(&format!("control_{}", uuid::Uuid::new_v4().simple()))
            .await
            .unwrap();
        let control = url::Url::parse(control.url()).unwrap();
        let listener = TcpListener::bind((interface_address(), 0)).await.unwrap();
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
                    .unwrap_or_else(|error| {
                        assert!(error.is_incomplete_message(), "HTTP server failed: {error}");
                    });
            }
        });
        let key = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(key.path(), [7_u8; 32]).unwrap();
        Self {
            key,
            pkce,
            token_pause,
            credentials: wamn_connections::connection_credentials::test_util::Fixture::new(control)
                .await,
            base,
            observed,
            task,
            server,
            pause,
        }
    }

    pub fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "token_url": format!("{}token?audience=api", self.base),
            "client_id": CLIENT_ID,
            "scopes": ["orders:read", "orders:write+"]
        })
    }

    pub async fn put_http(
        &mut self,
        name: &str,
        parameters: &serde_json::Value,
        projects: &[&str],
    ) {
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

    pub async fn put_auth(
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
                wamn_control_provision::bind_platform_principal_sql(
                    wamn_control_provision::PlatformComponent::Provisioning,
                ),
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
                    "parameters": {"primary-authority": self.base,
                        "tls-verification": if self.base.starts_with("https:") { "verify-authority" } else { "disabled" },
                        "auth": parameters},
                    "credential": secret,
                }),
                self.credentials.store.as_ref(),
            ).await.unwrap();
            tx.commit().await.unwrap();
        }
    }

    pub async fn plugin(&mut self, parameters: &serde_json::Value) -> TestHttp {
        self.plugin_with_auth(
            parameters,
            "oauth2_client_credentials",
            &serde_json::json!({"client_secret": CLIENT_SECRET}).to_string(),
        )
        .await
    }

    pub async fn plugin_with_auth(
        &mut self,
        parameters: &serde_json::Value,
        auth_type: &str,
        secret: &str,
    ) -> TestHttp {
        self.plugin_with_component(parameters, auth_type, secret, None)
            .await
    }

    pub async fn plugin_with_component(
        &mut self,
        parameters: &serde_json::Value,
        auth_type: &str,
        secret: &str,
        component_bytes: Option<&[u8]>,
    ) -> TestHttp {
        use wamn_engine::release_manifest::ReleaseTarget;
        use wamn_identity_protocol::AuthorityClass;
        use wamn_sql::{ClassCredentials, ProjectConfig, StaticCredentialProvider};

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
        if let Some(bytes) = component_bytes {
            let digest = wamn_digest::bytes_sha256(bytes);
            component.descriptor.component_digest = digest;
            invocation.component_digest = digest;
            invocation.origin.component_digest = digest;
            component.descriptor.imports = vec!["wamn:connection/http@0.1.0".into()];
            component.descriptor.effects = vec![wamn_catalog::AdmittedComponentEffect {
                package: "wamn:connection".into(),
                provenance: wamn_catalog::ComponentEffectProvenance::Imported,
                interfaces: component.descriptor.imports.clone(),
            }];
            component.descriptor.imports_fingerprint = wamn_digest::canonical_json_sha256(
                &serde_json::to_value(&component.descriptor.imports).unwrap(),
            );
        }
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
        let manifest_digest = release.release().manifest_digest;
        let mut projects = HashMap::new();
        let mut primary_database = None;
        for project in ["project-a", "project-b"] {
            let database = url::Url::parse(
                self.server
                    .lock()
                    .await
                    .create_database(&format!("project_{}", uuid::Uuid::new_v4().simple()))
                    .await
                    .unwrap()
                    .url(),
            )
            .unwrap();
            if project == "project-a" {
                primary_database = Some(database.clone());
            }
            wamn_platform_identity::test_database::project_platform(database.as_str()).await;
            let mut admin = wamn_postgres::connect(database.as_str()).await.unwrap();

            sqlx::raw_sql("BEGIN; INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES('770df186-ac15-579e-b46b-c297cae2011b'::text::uuid,'','fixture','admin:seed-oauth2')").execute(&mut admin).await.map(drop).unwrap();
            let project_env_key = wamn_registry::project_env_key("acme", project, "dev");
            sqlx::query("INSERT INTO catalog.releases(project_env_key,manifest_digest,canonical_bytes) VALUES($1,$2,$3)").bind(&project_env_key).bind(manifest_digest).bind(&bytes).execute(&mut admin).await.map(|result| result.rows_affected()).unwrap();
            let rows = sqlx::query("SELECT c.id,i.id,i.name FROM registry.connection c JOIN registry.integration i ON i.id=c.integration_id WHERE c.project=$1 AND c.env='dev'").bind(project).fetch_all(&mut wamn_postgres::connect(self.credentials.database.as_str()).await.unwrap()).await.unwrap();
            for row in rows {
                let connection: uuid::Uuid = row.get(0);
                let integration: uuid::Uuid = row.get(1);
                let name: String = row.get(2);
                sqlx::query("INSERT INTO platform.integration(id,integration_type,name,status) VALUES($1,'http',$2,'active')").bind(integration).bind(&name).execute(&mut admin).await.map(|result| result.rows_affected()).unwrap();
                sqlx::query("INSERT INTO platform.connection(id,integration_id,integration_type,status) VALUES($1,$2,'http','active')").bind(connection).bind(integration).execute(&mut admin).await.map(|result| result.rows_affected()).unwrap();
                sqlx::query("INSERT INTO catalog.connection_requirement(project_env_key,component_digest,package_id,name,interface,mode) VALUES($1,$2,'package_a',$3,'wamn:connection/http@0.1.0','write')").bind(&project_env_key).bind(&invocation.component_digest).bind(&name).execute(&mut admin).await.map(|result| result.rows_affected()).unwrap();
                sqlx::query("INSERT INTO catalog.release_connection(project_env_key,manifest_digest,connection_id,version) VALUES($1,$2,$3,1)").bind(&project_env_key).bind(manifest_digest).bind(connection).execute(&mut admin).await.map(|result| result.rows_affected()).unwrap();
                sqlx::query("INSERT INTO catalog.connection_binding(project_env_key,manifest_digest,component_digest,package_id,name,connection_id) VALUES($1,$2,$3,'package_a',$4,$5)").bind(&project_env_key).bind(manifest_digest).bind(&invocation.component_digest).bind(&name).bind(connection).execute(&mut admin).await.map(|result| result.rows_affected()).unwrap();
            }
            sqlx::raw_sql("COMMIT")
                .execute(&mut admin)
                .await
                .map(drop)
                .unwrap();
            for sql in [
                include_str!("../../../deploy/sql/run-state.sql"),
                include_str!("../../../deploy/sql/run-queue.sql"),
            ] {
                sqlx::raw_sql(sql).execute(&mut admin).await.unwrap();
            }
            let role = format!(
                "oauth2_{}_{}",
                database.path().trim_start_matches('/'),
                project.replace('-', "_")
            );
            sqlx::raw_sql(sqlx::AssertSqlSafe(
                wamn_control_provision::test_database::login_sql(
                    wamn_control_provision::WorkloadRoleFamily::HttpAdmitter,
                    database.path().trim_start_matches('/'),
                    &role,
                    "oauth2-test-password",
                ),
            ))
            .execute(&mut admin)
            .await
            .map(drop)
            .unwrap();
            let mut authority = url::Url::parse(database.as_str()).unwrap();
            authority.set_username(&role).unwrap();
            authority
                .set_password(Some("oauth2-test-password"))
                .unwrap();
            let database_name = database.path().trim_start_matches('/').to_owned();
            let guest_role = format!(
                "wamn_app_{}",
                wamn_identity_protocol::app_identity_hash(&project_env_key, &database_name)
            );
            sqlx::raw_sql(sqlx::AssertSqlSafe(
                wamn_control_provision::test_database::login_sql(
                    wamn_control_provision::WorkloadRoleFamily::App,
                    &database_name,
                    &guest_role,
                    "oauth2-test-password",
                ),
            ))
            .execute(&mut admin)
            .await
            .unwrap();
            let mut guest = authority.clone();
            guest.set_username(&guest_role).unwrap();
            let executor_role = format!("oauth2_executor_{}", uuid::Uuid::new_v4().simple());
            sqlx::raw_sql(sqlx::AssertSqlSafe(
                wamn_control_provision::test_database::login_sql(
                    wamn_control_provision::WorkloadRoleFamily::ExecutorPlatform,
                    &database_name,
                    &executor_role,
                    "oauth2-test-password",
                ),
            ))
            .execute(&mut admin)
            .await
            .unwrap();
            let mut executor = authority.clone();
            executor.set_username(&executor_role).unwrap();
            projects.insert(
                project.to_owned(),
                ProjectConfig {
                    credentials: ClassCredentials::default()
                        .with_class(AuthorityClass::GuestSql, guest)
                        .with_class(AuthorityClass::ExecutorPlatform, executor)
                        .with_class(AuthorityClass::CallableHttp, authority),
                    guest_pool_max_size: 1,
                    platform_pool_max_size: 1,
                    wait_timeout_ms: 5_000,
                    statement_timeout_ms: 10_000,
                },
            );
        }
        let postgres = Arc::new(WamnPostgres::with_provider(Arc::new(
            StaticCredentialProvider::new(projects, None),
        )));
        let facts = Arc::new(
            ConnectionFacts::new(
                Arc::new(ConnectionMemory::default()),
                Publisher::default(),
                Arc::new(self.credentials.reader("project-a").await),
                &postgres,
                "acme--dev--project-a",
                "project-a",
                "dev",
            )
            .with_oauth_encryption(Some(Arc::new(
                wamn_connections::oauth_encryption::Envelope::from_file(self.key.path()).unwrap(),
            ))),
        );
        let scope = uuid::Uuid::new_v4().to_string();
        wamn_sql::invocation::bind_test_context(
            &postgres,
            &scope,
            Arc::clone(&facts),
            Arc::clone(&release),
        );
        postgres
            .set_user_id(
                &scope,
                &wamn_control_provision::PlatformComponent::Executor
                    .principal_id()
                    .to_string(),
            )
            .unwrap();
        postgres.bind_invocation(&scope, invocation).unwrap();
        TestHttp {
            http: ConnectionHttp::new(
                postgres,
                Arc::new(HttpTransport::new().unwrap()),
                vec![self.base.strip_suffix("bound/").unwrap().parse().unwrap()].into(),
            ),
            database: primary_database.unwrap(),
            oauth2: facts.oauth_tokens(),
            facts,
            release,
            scope,
        }
    }

    pub async fn other_host(&self, first: &TestHttp) -> TestHttp {
        let facts = Arc::new(
            ConnectionFacts::new(
                Arc::new(ConnectionMemory::default()),
                Publisher::default(),
                Arc::new(self.credentials.reader("project-a").await),
                &first.postgres,
                "acme--dev--project-a",
                "project-a",
                "dev",
            )
            .with_oauth_encryption(Some(Arc::new(
                wamn_connections::oauth_encryption::Envelope::from_file(self.key.path()).unwrap(),
            ))),
        );
        let scope = uuid::Uuid::new_v4().to_string();
        wamn_sql::invocation::bind_test_context(
            &first.postgres,
            &scope,
            Arc::clone(&facts),
            Arc::clone(&first.release),
        );
        first
            .postgres
            .set_user_id(
                &scope,
                &wamn_control_provision::PlatformComponent::Executor
                    .principal_id()
                    .to_string(),
            )
            .unwrap();
        first
            .postgres
            .bind_invocation(&scope, first.postgres.invocation(&first.scope).unwrap())
            .unwrap();
        TestHttp {
            http: ConnectionHttp::new(
                Arc::clone(&first.postgres),
                Arc::new(HttpTransport::new().unwrap()),
                Arc::clone(&first.allowed_hosts),
            ),
            oauth2: facts.oauth_tokens(),
            facts,
            release: Arc::clone(&first.release),
            database: first.database.clone(),
            scope,
        }
    }

    pub fn pause_token(&self) -> ResponsePause {
        let (arrived, arrival) = tokio::sync::oneshot::channel();
        let (resume, resumed) = tokio::sync::oneshot::channel();
        *self.token_pause.lock().unwrap() = Some((arrived, resumed));
        (resume, arrival)
    }

    pub async fn due(&self, plugin: &TestHttp) {
        sqlx::query("UPDATE identity.oauth_token SET expires_at='-infinity'")
            .execute(
                &mut wamn_postgres::connect(self.credentials.database.as_str())
                    .await
                    .unwrap(),
            )
            .await
            .map(|result| result.rows_affected())
            .unwrap();
        plugin.oauth2.clear();
    }

    pub async fn stored(&self, connection: uuid::Uuid) -> wamn_connections::oauth_token::Token {
        let row = sqlx::query("SELECT ciphertext,nonce,wrapped_key,key_id FROM identity.oauth_token WHERE connection_id=$1").bind(connection).fetch_one(&mut wamn_postgres::connect(self.credentials.database.as_str()).await.unwrap()).await.unwrap();
        let envelope =
            wamn_connections::oauth_encryption::Envelope::from_file(self.key.path()).unwrap();
        let plaintext = envelope
            .decrypt(&wamn_connections::oauth_encryption::Encrypted {
                ciphertext: row.get(0),
                nonce: row.get(1),
                wrapped_key: row.get(2),
                key_id: row.get(3),
            })
            .await
            .unwrap();
        serde_json::from_slice(plaintext.expose()).unwrap()
    }

    pub async fn use_project(&self, plugin: &mut TestHttp, project: &str) {
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
                &plugin.postgres,
                &project_env_key,
                project,
                "dev",
            )
            .with_oauth_encryption(Some(Arc::new(
                wamn_connections::oauth_encryption::Envelope::from_file(self.key.path()).unwrap(),
            ))),
        );
        plugin.oauth2 = plugin.facts.oauth_tokens();
        wamn_sql::invocation::bind_test_context(
            &plugin.postgres,
            &plugin.scope,
            Arc::clone(&plugin.facts),
            Arc::clone(&plugin.release),
        );
        plugin
            .postgres
            .set_user_id(
                &plugin.scope,
                &wamn_control_provision::PlatformComponent::Executor
                    .principal_id()
                    .to_string(),
            )
            .unwrap();
    }

    pub async fn send(&self, plugin: &TestHttp, handle: &str) -> Result<Response, ConnectionError> {
        // Each standalone fixture send represents a fresh invocation.
        let invocation = plugin.postgres.invocation(&plugin.scope).unwrap();
        plugin.postgres.scopes.revoke(&plugin.scope);
        wamn_sql::invocation::bind_test_context(
            &plugin.postgres,
            &plugin.scope,
            Arc::clone(&plugin.facts),
            Arc::clone(&plugin.release),
        );
        plugin
            .postgres
            .set_user_id(
                &plugin.scope,
                &wamn_control_provision::PlatformComponent::Executor
                    .principal_id()
                    .to_string(),
            )
            .unwrap();
        plugin
            .postgres
            .bind_invocation(&plugin.scope, invocation)
            .unwrap();
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
        let result = plugin.send(&plugin.scope, &request).await;
        for value in [before, format!("{result:?}")] {
            assert!(!value.contains(CLIENT_ID));
            assert!(!value.contains(CLIENT_SECRET));
            assert!(!value.contains("fixture-token"));
        }
        result
    }

    pub fn counts(&self) -> (usize, usize) {
        let observed = self.observed.lock().unwrap();
        let tokens = observed
            .iter()
            .filter(|request| request.path.starts_with("/bound/token"))
            .count();
        (tokens, observed.len() - tokens)
    }

    pub async fn basic_plugin(&mut self) -> TestHttp {
        self.plugin_with_auth(
            &serde_json::json!({}),
            "http_basic",
            r#"{"username":"fixture","password":"old"}"#,
        )
        .await
    }

    pub async fn write_basic(&mut self) {
        let mut tx = self.credentials.admin.begin().await.unwrap();
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            wamn_control_provision::bind_platform_principal_sql(
                wamn_control_provision::PlatformComponent::Provisioning,
            ),
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

    pub fn pause_next_response(&self) -> ResponsePause {
        let (arrived, arrival) = tokio::sync::oneshot::channel();
        let (resume, resumed) = tokio::sync::oneshot::channel();
        *self.pause.lock().unwrap() = Some((arrived, resumed));
        (resume, arrival)
    }

    pub fn authorizations(&self) -> Vec<String> {
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

#[path = "tests/oauth2_tests/invocation.rs"]
pub mod invocation;

impl std::fmt::Debug for Fixture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fixture")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}
impl std::fmt::Debug for TestHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestHttp")
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}
