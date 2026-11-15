//! Real signed sessions and authorized PostgreSQL permissions drive native nested dispatch.

use sqlx::PgConnection as Client;
use sqlx::Row as _;
use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;
use std::sync::{Arc, mpsc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;
use tokio::time::Instant;

use tracing::{Instrument as _, instrument::WithSubscriber as _};
use wamn_authentication::route_authentication::PlatformRouteAuthenticator;
use wamn_catalog::{
    ComponentDeclaration, ComponentOperationDeclaration, ComponentPackageCoordinate,
};
use wamn_control_provision::{WorkloadRoleFamily, WorkloadRoleIdentity, workload_login};
use wamn_engine::component_admission::{ComponentAdmissionRequest, validate_component_admission};
use wamn_engine::flow_http_routing::{AuthenticatedCaller, CredentialType, FlowHttpRouting};
use wamn_engine::operation::invocation_policy::InvocationPolicy as _;
use wamn_engine::release_manifest::LoadedRelease;
use wamn_identity_protocol::verifier::SessionVerifier;
use wamn_postgres_capability::{
    AuthorityClass, StaticCredentialProvider, WamnPostgres, WamnPostgresConfig,
};

use super::invoke_native;
use super::trace::TraceCapture;
use super::{CHILD, CHILD_MARKER, CLEANUP, Case, Fixture, ROOT};

#[path = "../../../../../platform/postgres-capability/tests/support/session_fixture.rs"]
#[expect(
    dead_code,
    reason = "The shared issuer fixture also serves cache-expiry tests."
)]
mod session_fixture;
use session_fixture::{ORG, Server, claims, header, signed};

const PROJECT: &str = "test";
const PROJECT_ENV_KEY: &str = super::PROJECT_ENV_KEY;
const ENVIRONMENT: &str = "test";
const AUDIENCE: &str = "urn:wamn:project-env:org-a:test:test:native-b";
const ATTACHMENT: &str = "native-test-http";
const PASSWORD: &str = "native-b-disposable-test-only";
/// The test principal that the fixture writes as.
const FIXTURE_PRINCIPAL: &str = "00000000-0000-4000-8000-0000000000f1";
const TEST_NAME: &str = "native_authenticated_call_graph_grant";
/// Keeps a warm root through the slow fixture work between calls. The warm
/// test counts instance starts, and a reclaim during that work adds a start.
const REUSE_WINDOW_SECONDS: i32 = 600;

async fn connect(url: &str) -> anyhow::Result<Client> {
    let client = wamn_postgres::connect(url).await?;

    Ok(client)
}

async fn project_env_key_postgres(admin_url: &str) -> anyhow::Result<Arc<WamnPostgres>> {
    let mut admin = connect(admin_url).await?;
    let version: i32 = sqlx::query("SHOW server_version_num")
        .fetch_one(&mut admin)
        .await?
        .get::<String, _>(0)
        .parse()?;
    anyhow::ensure!(
        version >= 180_000,
        "native authenticated test requires PostgreSQL 18"
    );
    let occupied: bool = sqlx::query("SELECT EXISTS (SELECT FROM pg_namespace WHERE nspname IN ('platform', 'catalog', 'identity', 'wamn_run')) \
         OR EXISTS (SELECT FROM pg_roles WHERE rolname IN ('wamn_app', 'wamn_http_admitter'))").fetch_one(&mut admin).await?.get(0);
    anyhow::ensure!(
        !occupied,
        "refuse populated PostgreSQL: arm only this test's fresh disposable server"
    );
    wamn_control_provision::test_database::system_schema(admin_url).await;
    let mut database: String = sqlx::query("SELECT current_database()::text")
        .fetch_one(&mut admin)
        .await?
        .get(0);
    let generation = workload_login(
        WorkloadRoleFamily::HttpAdmitter,
        WorkloadRoleIdentity::ProjectEnvironment {
            org: ORG,
            project: PROJECT,
            environment: &ENVIRONMENT.parse().unwrap(),
            database: &mut database,
        },
    )?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("CREATE ROLE wamn_app NOLOGIN NOINHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS; \
          \
         CREATE SCHEMA wamn_run AUTHORIZATION postgres;")
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    sqlx::raw_sql(wamn_catalog::CATALOG_SCHEMA_SQL)
        .execute(&mut admin)
        .await
        .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../deploy/sql/app-schema.sql"
        )))
        .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::test_database::login_sql(
            WorkloadRoleFamily::HttpAdmitter,
            &mut database,
            &generation,
            PASSWORD,
        ))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    sqlx::raw_sql("BEGIN")
        .execute(&mut admin)
        .await
        .expect("begin fixture transaction");
    // The fixture writes as its test principal, whose row stamps itself.
    sqlx::query(
        "INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES($1::text::uuid,'','fixture','admin:seed-native-policy-fixture')",
    )
    .bind(&FIXTURE_PRINCIPAL)
    .execute(&mut admin)
    .await
    .map(|result| result.rows_affected())?;
    sqlx::query(
        "INSERT INTO platform.\"user\" (id, type, email) \
             VALUES ($1::text::uuid, 'user', 'fixture@example.invalid')",
    )
    .bind(&FIXTURE_PRINCIPAL)
    .execute(&mut admin)
    .await
    .map(|result| result.rows_affected())?;
    for role in ["native-parent", "native-child"] {
        sqlx::query("INSERT INTO platform.role (name) VALUES ($1)")
            .bind(&role)
            .execute(&mut admin)
            .await
            .map(|result| result.rows_affected())?;
        sqlx::query(
            "INSERT INTO platform.permission (role_name, permission) \
             VALUES ($1, regexp_replace($2, '@[^@]*$', ''))",
        )
        .bind(&role)
        .bind(&ROOT)
        .execute(&mut admin)
        .await
        .map(|result| result.rows_affected())?;
    }
    sqlx::query(
        "INSERT INTO platform.permission (role_name, permission) \
             VALUES ('native-child', regexp_replace($1, '@[^@]*$', ''))",
    )
    .bind(&CHILD)
    .execute(&mut admin)
    .await
    .map(|result| result.rows_affected())?;
    sqlx::raw_sql("COMMIT")
        .execute(&mut admin)
        .await
        .expect("commit fixture transaction");
    let mut authorized_url = url::Url::parse(admin_url)?;
    authorized_url
        .set_username(&generation)
        .map_err(|()| anyhow::anyhow!("set fixture login"))?;
    authorized_url
        .set_password(Some(PASSWORD))
        .map_err(|()| anyhow::anyhow!("set fixture password"))?;
    let mut authorized = connect(authorized_url.as_str()).await?;
    let identity = sqlx::query(
        "SELECT session_user::text, rolsuper, rolbypassrls, \
         pg_has_role(current_user, 'wamn_http_admitter', 'USAGE'), \
         has_table_privilege(current_user, 'platform.permission', 'UPDATE') \
         FROM pg_roles WHERE rolname = current_user",
    )
    .fetch_one(&mut authorized)
    .await?;
    assert_eq!(identity.get::<String, _>(0), generation);
    assert!(!identity.get::<bool, _>(1) && !identity.get::<bool, _>(2));
    assert!(identity.get::<bool, _>(3));
    assert!(
        !identity.get::<bool, _>(4),
        "permission reader cannot change grants"
    );
    drop(authorized);
    let base = WamnPostgresConfig {
        credentials: None,
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 2000,
        statement_timeout_ms: 5000,
    };
    let configuration = json!({PROJECT: {"credentials": {
        (AuthorityClass::CallableHttp.as_str()): authorized_url.as_str()
    }}});
    let projects = StaticCredentialProvider::projects_from_json(&configuration.to_string(), &base)?;
    let postgres = Arc::new(WamnPostgres::with_provider(Arc::new(
        StaticCredentialProvider::new(projects, None),
    )));
    Ok(postgres)
}

pub(super) async fn platform_postgres(admin_url: &str) -> anyhow::Result<Arc<WamnPostgres>> {
    project_env_key_postgres(admin_url).await?;
    let mut admin = connect(admin_url).await?;
    let principals = wamn_control_provision::platform_principals_sql("platform.example.invalid")?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("BEGIN; {principals} COMMIT;")))
        .execute(&mut admin)
        .await
        .map(drop)?;
    let (postgres, _) = warm_postgres(admin_url, &[]).await?;
    Ok(postgres)
}

async fn authentication_fixture(admin_url: &str) -> anyhow::Result<(Server, FlowHttpRouting)> {
    let postgres = project_env_key_postgres(admin_url).await?;
    let server = Server::start().await;
    let (keys, _) = server.cache();
    let verifier = SessionVerifier::new(keys, ORG, AUDIENCE)?;
    let mut admin = connect(admin_url).await?;
    let first = claims()["sub"].as_str().unwrap().to_owned();
    let second = "00000000-0000-4000-8000-0000000000b2";
    session_fixture::install_authority(
        &mut admin,
        PROJECT,
        &ENVIRONMENT.parse().unwrap(),
        AUDIENCE,
        &[&first, second],
    )
    .await?;
    sqlx::raw_sql("BEGIN")
        .execute(&mut admin)
        .await
        .expect("begin fixture transaction");
    sqlx::query("INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES($1::text::uuid,'','fixture','admin:seed-session-authority')").bind(&FIXTURE_PRINCIPAL).execute(&mut admin).await.map(|result| result.rows_affected())?;
    for (principal, email) in [
        (&*first, "alice@example.test"),
        (second, "bob@example.test"),
    ] {
        sqlx::query(
            "INSERT INTO platform.\"user\" (id,type,email) VALUES ($1::text::uuid,'user',$2)",
        )
        .bind(&principal)
        .bind(&email)
        .execute(&mut admin)
        .await
        .map(|result| result.rows_affected())?;
        for role in ["native-parent", "native-child"] {
            sqlx::query(
                "INSERT INTO platform.user_role (user_id,role_name) VALUES ($1::text::uuid,$2)",
            )
            .bind(&principal)
            .bind(&role)
            .execute(&mut admin)
            .await
            .map(|result| result.rows_affected())?;
        }
    }
    let authentication = Arc::new(PlatformRouteAuthenticator::for_test(
        postgres,
        &PROJECT_ENV_KEY.parse().unwrap(),
        Some(wamn_postgres::pool(admin_url, 4).await?),
        Some(verifier),
    ));
    let definition = json!({"id": ATTACHMENT, "type": "http", "route": {
        "host": "native.example.test", "path": "/native", "method": "POST"
    }});
    let manifest = json!({
        "format-version": super::SERVING_MANIFEST_FORMAT_VERSION,
        "release": {"packages": [{"package-id": "root", "package-version": "1.0.0"}]},
        "components": [{"package-id": "root",
            "descriptor": {"component": "node", "interface-version": "0.1.0", "component-digest": (format!("sha256:{}", "a".repeat(64))).parse::<wamn_digest::Digest>().unwrap(), "imports": [], "imports-fingerprint": wamn_digest::canonical_json_sha256(&serde_json::json!([])), "effects": [], "connections": [],
            "operations": {
                ROOT: {"registered-operation": ROOT, "input-ports": [], "output-ports": [], "parameters": []}
            }}}],
        "routes": [{"package-id":"root","component":"node","operation":ROOT,"type":"command"}],
        "attachments": {ATTACHMENT: {"type":"http","package-id":"root","component":"node","operation":ROOT,
            "definition-hash":wamn_digest::canonical_json_sha256(&definition),
            "definition":definition,"auth-policy":{"modes":["session"]},"registered-operation":ROOT}}

    });
    let release = Arc::new(LoadedRelease::load_canonical_bytes(
        &wamn_digest::canonical_json_bytes(&manifest),
        "native session route test",
        wamn_engine::release_manifest::ReleaseTarget::new(
            PROJECT_ENV_KEY.parse().unwrap(),
            ENVIRONMENT.parse().unwrap(),
        )
        .with_applications(serde_json::from_value(
            json!({"receiving": {"packages": ["root", "child"]}}),
        )?),
    )?);
    let route = FlowHttpRouting::for_test(Some(release)).with_authenticator(authentication);
    sqlx::raw_sql("COMMIT")
        .execute(&mut admin)
        .await
        .expect("commit fixture transaction");
    Ok((server, route))
}

async fn authenticated(route: &FlowHttpRouting, child_grant: bool) -> AuthenticatedCaller {
    authenticated_as(route, child_grant, None).await
}

async fn authenticated_as(
    route: &FlowHttpRouting,
    child_grant: bool,
    principal: Option<&str>,
) -> AuthenticatedCaller {
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("wall clock")
            .as_secs(),
    )
    .expect("Unix seconds fit i64");
    let mut body = claims();
    if let Some(principal) = principal {
        body["sub"] = json!(principal);
        body["authority"] = json!({"login":principal});
    }
    body["aud"] = json!(AUDIENCE);
    body["iat"] = json!(now);
    body["exp"] = json!(now + 900);
    body["roles"] = json!([if child_grant {
        "native-child"
    } else {
        "native-parent"
    }]);
    let token = signed(&header(), &body);
    let (message, signature) = token.rsplit_once('.').expect("signed token");
    let replacement = if signature.starts_with('A') { "B" } else { "A" };
    let forged = format!("Bearer {message}.{replacement}{}", &signature[1..]);
    assert!(
        route
            .authenticate_authorization_for_test(ATTACHMENT, Some(&forged))
            .await
            .is_err(),
        "a changed signature cannot mint caller authority"
    );
    let caller = route
        .authenticate_authorization_for_test(ATTACHMENT, Some(&format!("Bearer {token}")))
        .await
        .expect("production session authentication")
        .expect("verified caller");
    assert_eq!(
        caller.principal_id(),
        body["sub"].as_str().expect("signed principal")
    );
    assert_eq!(caller.credential_type(), CredentialType::Session);
    assert!(caller.permits(ROOT));
    assert_eq!(caller.permits(CHILD), child_grant);
    caller
}

/// Activation tests the entry's grant once, before the guest runs.
async fn assert_case(caller: &AuthenticatedCaller, postgres: Arc<WamnPostgres>) {
    let fixture =
        Fixture::build_with_reuse(Case::CalleeGrant, true, true, None, None, Some(postgres)).await;
    let target = fixture.target().await;
    let trace = Some(TraceCapture::new(&fixture, caller));
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut request = fixture.request(deadline);
    request.facts.caller = Some(caller.clone());
    let invocation = invoke_native(&target, request);
    let result = match &trace {
        // The dispatcher exists only while the caller polls. Native must
        // explicitly carry it and the span into its spawned guest task.
        Some(trace) => {
            invocation
                .instrument(trace.span.clone())
                .with_subscriber(trace.dispatcher.clone())
                .await
        }
        None => invocation.await,
    };
    let emission = result
        .expect("the permitted call graph dispatches")
        .expect("typed emission");
    let payload: serde_json::Value = serde_json::from_str(&emission.payload).unwrap();
    assert_eq!(payload.as_array().unwrap().len(), 1);
    assert_eq!(payload[0]["value"].as_u64(), Some(37));
    assert_eq!(emission.port, None);
    fixture.assert_clean().await;
    let observations = fixture.events.lock().expect("observations lock").clone();
    let native_ids: BTreeSet<_> = fixture
        .workload
        .facts_by_component_id
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        observations.iter().filter(|event| event.phase == 0).count(),
        1
    );
    assert_eq!(
        observations.iter().filter(|event| event.phase == 1).count(),
        1
    );
    for event in observations {
        if event.phase == 0 {
            assert!(event.native_identity && native_ids.contains(&event.scope));
            assert!(event.identity.is_none() && event.invocation.is_none());
            continue;
        }
        assert!(!event.native_identity && !native_ids.contains(&event.scope));
        let identity = event.identity.expect("host identity");
        assert_eq!(
            identity.project_env_key,
            Some(PROJECT_ENV_KEY.parse().unwrap())
        );
        assert_eq!(
            identity.user_id.as_deref(),
            Some(caller.principal_id()),
            "the entry binds the caller principal"
        );
        assert_eq!(identity.project.as_deref(), Some(PROJECT));
        assert_eq!(
            identity.release,
            fixture.request(deadline).facts.acquisition.identity.release
        );
        let invocation = event.invocation.expect("host invocation");
        assert_eq!(invocation.operation, ROOT);
        assert_eq!(identity.operation.as_deref(), Some(ROOT));
        assert_eq!(
            invocation.origin,
            fixture
                .request(deadline)
                .facts
                .acquisition
                .invocation
                .origin
        );
        let position = invocation.entry.wiring().expect("a wiring entry");
        assert_eq!(position.wiring_id, "trusted-wiring");
        assert_eq!(position.wiring_version, 1);
        assert_eq!(position.node_id, "trusted-node");
        assert_eq!(invocation.package_id, fixture.root.target.package_id);
        assert_eq!(invocation.component, fixture.root.component);
        assert_eq!(invocation.component_digest, fixture.root.component_digest);
    }
    fixture
        .workload
        .unbind_all_plugins()
        .await
        .expect("unbind the owned native workload");
    assert!(
        fixture
            .policy
            .bindings
            .read()
            .expect("bindings lock")
            .is_empty()
    );
    if let Some(trace) = trace {
        trace.assert_parentage(&fixture, caller);
    }
    println!("authenticated-native-case=Success result=pass");
}

async fn assert_authenticated(admin_url: &str) -> anyhow::Result<()> {
    let (mut server, route) = authentication_fixture(admin_url).await?;
    let permitted = authenticated(&route, true).await;
    let (postgres, _) = warm_postgres(admin_url, &[]).await?;
    assert_case(&permitted, postgres).await;
    server.stop().await;
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn native_authenticated_call_graph_grant(pool: sqlx::PgPool) {
    let full_name = format!("operation::native_policy::tests::authenticated::{TEST_NAME}");
    if std::env::var(CHILD_MARKER).as_deref() != Ok(TEST_NAME) {
        let server = wamn_test_postgres::start(&[]).expect("isolated authentication server");
        let coordinate = server.database("postgres").expect("server coordinate");
        let output = Command::new(std::env::current_exe().expect("test executable"))
            .env("DATABASE_URL", coordinate.url())
            .args(["--exact", &full_name, "--nocapture"])
            .env(CHILD_MARKER, TEST_NAME)
            .envs(
                wamn_release_control::release_chart::TELEMETRY_VARIABLES
                    .into_iter()
                    .filter(|(name, _)| name.starts_with("OTEL_TRACES_")),
            )
            .output()
            .expect("start isolated authenticated native test");
        assert!(
            output.status.success(),
            "{full_name}: {}\n{}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    let (done, finished) = mpsc::channel();
    let watchdog = std::thread::spawn(move || {
        if finished.recv_timeout(Duration::from_secs(60)) == Err(mpsc::RecvTimeoutError::Timeout) {
            eprintln!("authenticated native test exceeded its process watchdog");
            std::process::exit(124);
        }
    });
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .event_interval(1)
                    .enable_all()
                    .build()
                    .expect("isolated native runtime");
                runtime
                    .block_on(assert_authenticated(database.as_str()))
                    .expect("real authenticated native test");
            })
            .join()
            .unwrap();
    });
    done.send(()).expect("finish watchdog");
    watchdog.join().expect("join watchdog");
}

/// The admission request of the composed member fixture: the root's one
/// operation calls the child embedded in it, a base the package pins.
fn member_admission_request(child_digest: &wamn_digest::Digest) -> ComponentAdmissionRequest {
    ComponentAdmissionRequest {
        declaration: ComponentDeclaration {
            connections: Vec::new(),
            target: ComponentPackageCoordinate {
                project_env_key: PROJECT_ENV_KEY.parse().unwrap(),
                package_id: "root".into(),
                package_version: "1.0.0".into(),
            },
            component: "node".into(),
            interface_version: "0.1.0".into(),
            operations: BTreeMap::from([(
                ROOT.into(),
                ComponentOperationDeclaration {
                    registered_operation: Some(ROOT.into()),
                    committed_result_schema: None,
                    input_ports: Vec::new(),
                    output_ports: Vec::new(),
                    parameters: Vec::new(),
                },
            )]),
        },
        admitted_platform_packages: BTreeSet::from(["wamn:postgres".to_owned()]),
        base_digests: BTreeSet::from([child_digest.to_owned()]),
    }
}

#[sqlx::test(migrations = false)]
async fn native_member_statement_instances(pool: sqlx::PgPool) {
    super::run_isolated_test(
        "authenticated::native_member_statement_instances",
        Some(&pool),
        || async {
            let (composed, child) = super::member_instances_component();
            let child_digest = wamn_digest::bytes_sha256(&child);
            let engine = wamn_engine::build_engine(&[]).expect("validation engine");
            let admitted = validate_component_admission(
                &engine,
                &composed,
                member_admission_request(&child_digest),
            )
            .expect("publish admits each member's statements import by its instance type");
            assert_eq!(
                admitted
                    .component
                    .imports
                    .iter()
                    .filter(|import| import.as_str() == super::STATEMENTS)
                    .count(),
                1,
                "the two named instances are one capability: {:?}",
                admitted.component.imports
            );
            let _lock = wamn_test_postgres::lock();
            let database = wamn_test_postgres::url(&pool);
            let (mut server, route) = authentication_fixture(database.as_str())
                .await
                .expect("authenticated member fixture");
            let caller = authenticated(&route, true).await;
            let (postgres, _) = warm_postgres_with_pool(database.as_str(), &[], 2, None)
                .await
                .expect("member SQL credentials");
            let mut admin = connect(database.as_str()).await.expect("admin connection");
            sqlx::raw_sql(sqlx::AssertSqlSafe(
                ("CREATE SCHEMA native_member; GRANT USAGE ON SCHEMA native_member TO wamn_app; \
                     CREATE TABLE native_member.native_policy_members(value text NOT NULL); \
                     GRANT SELECT, INSERT ON native_member.native_policy_members TO wamn_app")
                    .to_string(),
            ))
            .execute(&mut admin)
            .await
            .map(drop)
            .expect("member table");
            let fixture = Fixture::build_with_reuse(
                Case::MemberInstances,
                false,
                true,
                None,
                None,
                Some(postgres),
            )
            .await;
            let mut rows = async || {
                sqlx::query("SELECT value FROM native_member.native_policy_members ORDER BY value")
                    .fetch_all(&mut admin)
                    .await
                    .expect("read member rows")
                    .iter()
                    .map(|row| row.get::<String, _>(0))
                    .collect::<Vec<_>>()
            };
            let component_id = fixture
                .workload
                .facts_by_component_id
                .iter()
                .find_map(|(id, fact)| (fact == &fixture.root).then_some(id.clone()))
                .expect("root native identity");

            for (input, commits) in [(r#"[{"value":37}]"#, true), ("[]", false)] {
                let mut request = fixture.request(Instant::now() + Duration::from_secs(10));
                request.input = input.into();
                request.facts.caller = Some(caller.clone());
                request.facts = fixture
                    .policy
                    .prepare(&component_id, ROOT, &request.facts)
                    .await
                    .expect("admit the invocation");
                let scope = fixture.policy.scope_id(&request.facts).unwrap();
                let transaction = fixture
                    .policy
                    .resources
                    .postgres
                    .begin_operation_transaction(scope)
                    .await
                    .unwrap();
                fixture
                    .policy
                    .resources
                    .postgres
                    .bind_operation_transaction(scope, &transaction)
                    .unwrap();
                let result = invoke_native(&fixture.target().await, request)
                    .await
                    .expect("the composed root dispatches");
                if commits {
                    result.expect("the root succeeds");
                    transaction
                        .commit()
                        .await
                        .expect("commit the request element");
                    assert_eq!(
                        rows().await,
                        ["a", "b1"],
                        "the child's statement saw the root's row in the one host transaction"
                    );
                } else {
                    result.expect_err("the root fails after both members wrote");
                    transaction
                        .rollback()
                        .await
                        .expect("roll back the request element");
                    assert_eq!(
                        rows().await,
                        ["a", "b1"],
                        "a rolled-back request element leaves no write of either member"
                    );
                }
            }
            fixture.assert_clean().await;
            server.stop().await;
        },
    );
}

/// `wamn-azko6`: a command without SQL whose manifest declares `transaction =
/// explicit_per_input` calls a member with SQL. Its released operation holds
/// no statement, and the host still begins the request element transaction from the
/// declared field; the member works in it through its own instance, and the
/// request element commits or rolls back whole.
#[sqlx::test(migrations = false)]
async fn native_sql_free_command_begins_from_its_transaction_field(pool: sqlx::PgPool) {
    super::run_isolated_test(
        "authenticated::native_sql_free_command_begins_from_its_transaction_field",
        Some(&pool),
        || async {
            let (composed, child) = super::sql_free_instances_component();
            let child_digest = wamn_digest::bytes_sha256(&child);
            let engine = wamn_engine::build_engine(&[]).expect("validation engine");
            validate_component_admission(
                &engine,
                &composed,
                member_admission_request(&child_digest),
            )
            .expect("publish admits the SQL-free root over its pinned child");
            let _lock = wamn_test_postgres::lock();
            let database = wamn_test_postgres::url(&pool);
            let (mut server, route) = authentication_fixture(database.as_str())
                .await
                .expect("authenticated member fixture");
            let caller = authenticated(&route, true).await;
            let (postgres, _) = warm_postgres_with_pool(database.as_str(), &[], 2, None)
                .await
                .expect("member SQL credentials");
            let mut admin = connect(database.as_str()).await.expect("admin connection");
            sqlx::raw_sql(sqlx::AssertSqlSafe(
                ("CREATE SCHEMA native_member; GRANT USAGE ON SCHEMA native_member TO wamn_app; \
                     CREATE TABLE native_member.native_policy_members(value text NOT NULL); \
                     GRANT SELECT, INSERT ON native_member.native_policy_members TO wamn_app")
                    .to_string(),
            ))
            .execute(&mut admin)
            .await
            .map(drop)
            .expect("member table");
            let fixture = Fixture::build_with_reuse(
                Case::SqlFreeRoot,
                false,
                true,
                None,
                None,
                Some(postgres),
            )
            .await;
            assert!(fixture.root.operations[ROOT].host_transaction);
            for commits in [true, false] {
                let mut request = fixture.request(Instant::now() + Duration::from_secs(10));
                request.facts.caller = Some(caller.clone());
                request.context.config = if commits { "{}" } else { "null" }.into();
                let input = json!([{"request_id":"element", "value":{"key":if commits { "commit" } else { "rollback" }}}]);
                let result = wamn_engine::operation::invoke_operation(
                    &fixture,
                    wamn_engine::operation::OperationCall {
                        closure: wamn_engine::operation::OperationClosure::Candidate(
                            &fixture.application,
                        ),
                        component: &fixture.root,
                        operation: ROOT,
                        context: request.context,
                        input: &input,
                        deadline_ms: 10_000,
                        facts: request.facts,
                    },
                )
                .await
                .expect("invoke through the production operation entry");
                if commits {
                    result.expect("the command commits");
                } else {
                    result.expect_err("the command refuses after its member writes");
                }
                let rows: i64 =
                    sqlx::query_scalar("SELECT count(*) FROM native_member.native_policy_members")
                        .fetch_one(&mut admin)
                        .await
                        .expect("read committed member writes");
                assert_eq!(
                    rows, 1,
                    "only the successful invocation commits its member write"
                );
            }
            fixture.assert_clean().await;
            server.stop().await;
        },
    );
}

#[sqlx::test(migrations = false)]
async fn native_warm_alternating_callers_and_credentials(pool: sqlx::PgPool) {
    super::run_isolated_test(
        "authenticated::native_warm_alternating_callers_and_credentials",
        Some(&pool),
        || async {
            let _lock = wamn_test_postgres::lock();
            let database = wamn_test_postgres::url(&pool);
            let (mut server, route) = authentication_fixture(database.as_str())
                .await
                .expect("authorized authentication fixture");
            let alice = authenticated(&route, true).await;
            let bob =
                authenticated_as(&route, true, Some("00000000-0000-4000-8000-0000000000b2")).await;
            assert_ne!(alice.principal_id(), bob.principal_id());
            let directory = wamn_test_infrastructure::scratch::ScratchRoot::create()
                .expect("credential directory");
            let credential_file = directory.path().join("guest");
            let (postgres, provider) =
                warm_postgres_with_pool(database.as_str(), &[], 1, Some(&credential_file))
                    .await
                    .expect("guest-generation fixture");
            let fixture = Fixture::build_with_reuse(
                Case::CalleeGrant,
                true,
                true,
                None,
                Some(REUSE_WINDOW_SECONDS),
                Some(Arc::clone(&postgres)),
            )
            .await;
            let target = fixture.target().await;
            for (caller, input) in [
                (&alice, "alice-only"),
                (&bob, "bob-only"),
                (&alice, "alice-again"),
            ] {
                let mut request = fixture.request(Instant::now() + CLEANUP);
                request.input = input.into();
                request.facts.caller = Some(caller.clone());
                let result = invoke_native(&target, request)
                    .await
                    .expect("authorized native dispatch")
                    .expect("emission");
                assert_eq!(
                    result.payload, input,
                    "no caller-dependent result crosses calls"
                );
                assert!(fixture.policy.resources.postgres.scopes.is_empty());
                for event in fixture
                    .events
                    .lock()
                    .expect("events")
                    .iter()
                    .filter(|event| event.phase == 1)
                {
                    assert!(
                        fixture
                            .policy
                            .resources
                            .postgres
                            .session_identity(&event.scope)
                            .is_none()
                    );
                    assert!(
                        fixture
                            .policy
                            .resources
                            .blobstore
                            .invocation(&event.scope)
                            .is_none()
                    );
                }
            }
            let observations = fixture.events.lock().expect("events").clone();
            assert_eq!(
                observations.iter().filter(|event| event.phase == 0).count(),
                1,
                "the warm root is reused"
            );
            let callers: Vec<_> = observations
                .iter()
                .filter(|event| event.phase == 1)
                .map(|event| {
                    event
                        .identity
                        .as_ref()
                        .and_then(|identity| identity.user_id.clone())
                        .expect("bound caller principal")
                })
                .collect();
            assert_eq!(
                callers,
                vec![
                    alice.principal_id().to_owned(),
                    bob.principal_id().to_owned(),
                    alice.principal_id().to_owned(),
                ]
            );
            let mut request = fixture.request(Instant::now() + CLEANUP);
            request.facts.caller = Some(alice.clone());
            invoke_native(&fixture.target().await, request)
                .await
                .expect("session admits the callee")
                .expect("emission");
            let pat = pat_caller(
                database.as_str(),
                Arc::clone(&postgres),
                &fixture.policy.resources.release,
            )
            .await
            .expect("real PAT caller");
            assert_eq!(pat.credential_type(), CredentialType::Pat);
            for caller in [&pat, &pat] {
                let mut request = fixture.request(Instant::now() + CLEANUP);
                request.facts.caller = Some(caller.clone());
                invoke_native(&fixture.target().await, request)
                    .await
                    .expect("PAT admits the callee")
                    .expect("emission");
            }
            let root_id = fixture
                .workload
                .facts_by_component_id
                .iter()
                .find(|(_, fact)| *fact == &fixture.root)
                .expect("root native identity")
                .0;
            assert_eq!(
                fixture
                    .events
                    .lock()
                    .expect("events")
                    .iter()
                    .filter(|event| event.phase == 0 && &event.scope == root_id)
                    .count(),
                1,
                "session and PAT calls reuse the same root instance"
            );
            let mut request = fixture.request(Instant::now() + CLEANUP);
            request.facts.caller = Some(alice);
            invoke_native(&fixture.target().await, request)
                .await
                .expect("session remains authorized after PAT calls")
                .expect("emission");
            super::warm::close(&fixture).await;
            let retired_postgres = Arc::new(WamnPostgres::with_provider(provider));
            let retired = Fixture::build_with_reuse(
                Case::Success,
                false,
                true,
                None,
                Some(REUSE_WINDOW_SECONDS),
                Some(Arc::clone(&retired_postgres)),
            )
            .await;
            // Readiness creates a warm instance before credential resolution.
            super::prepare_native(&retired.target().await, Instant::now() + CLEANUP)
                .await
                .expect("warm instance ready before its first invocation");
            let login = workload_login(
                WorkloadRoleFamily::App,
                WorkloadRoleIdentity::ProjectEnvKey {
                    project_env_key: &PROJECT_ENV_KEY.parse().unwrap(),
                    database: database.path().trim_start_matches('/'),
                },
            )
            .unwrap();
            let mut observer = connect(database.as_str()).await.unwrap();
            let mut replacements = Vec::new();
            for disappearing in [true, false] {
                let starts_before = super::warm::starts(&retired);
                if disappearing {
                    std::fs::rename(&credential_file, directory.path().join("unmounted")).unwrap();
                } else {
                    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                        "ALTER ROLE \"{login}\" NOLOGIN"
                    )))
                    .execute(&mut observer)
                    .await
                    .unwrap();
                    sqlx::query(
                        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE usename=$1",
                    )
                    .bind(&login)
                    .fetch_all(&mut observer)
                    .await
                    .unwrap();
                }
                retired_postgres.auth_cache().clear();
                let before_refusal = retired.events.lock().unwrap().len();
                let mut request = retired.request(Instant::now() + CLEANUP);
                request.facts.caller = Some(bob.clone());
                assert!(
                    invoke_native(&retired.target().await, request)
                        .await
                        .is_err(),
                    "unavailable credentials refuse without substitution"
                );
                assert_eq!(
                    retired.events.lock().unwrap().len(),
                    before_refusal,
                    "credential refusal never reaches a guest instance"
                );
                if disappearing {
                    std::fs::rename(directory.path().join("unmounted"), &credential_file).unwrap();
                } else {
                    sqlx::raw_sql(sqlx::AssertSqlSafe(format!("ALTER ROLE \"{login}\" LOGIN")))
                        .execute(&mut observer)
                        .await
                        .unwrap();
                }
                retired_postgres.auth_cache().clear();
                let mut request = retired.request(Instant::now() + CLEANUP);
                request.facts.caller = Some(bob.clone());
                invoke_native(&retired.target().await, request)
                    .await
                    .expect("restored credential")
                    .expect("emission");
                replacements.push((disappearing, starts_before, super::warm::starts(&retired)));
            }
            super::warm::close(&retired).await;
            assert!(
                replacements
                    .iter()
                    .all(|(_, before, after)| *after == before + 1),
                "credential refusals must replace warm instances (disappeared, before, after): {replacements:?}"
            );
            server.stop().await;
        },
    );
}

async fn warm_postgres(
    admin_url: &str,
    principals: &[&str],
) -> anyhow::Result<(Arc<WamnPostgres>, Arc<StaticCredentialProvider>)> {
    warm_postgres_with_pool(admin_url, principals, 1, None).await
}

async fn warm_postgres_with_pool(
    admin_url: &str,
    principals: &[&str],
    guest_pool_max_size: usize,
    credential_file: Option<&std::path::Path>,
) -> anyhow::Result<(Arc<WamnPostgres>, Arc<StaticCredentialProvider>)> {
    let mut admin = connect(admin_url).await?;
    let mut database: String = sqlx::query("SELECT current_database()::text")
        .fetch_one(&mut admin)
        .await?
        .get(0);
    let role = workload_login(
        WorkloadRoleFamily::App,
        WorkloadRoleIdentity::ProjectEnvKey {
            project_env_key: &PROJECT_ENV_KEY.parse().unwrap(),
            database: &mut database,
        },
    )?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::test_database::login_sql(
            WorkloadRoleFamily::App,
            &mut database,
            &role,
            PASSWORD,
        ))
            .to_string(),
    ))
    .execute(&mut admin)
    .await
    .map(drop)?;
    sqlx::raw_sql("BEGIN")
        .execute(&mut admin)
        .await
        .expect("begin fixture transaction");
    sqlx::query("INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES($1::text::uuid,'','fixture','admin:warm-test')").bind(&FIXTURE_PRINCIPAL).execute(&mut admin).await.map(|result| result.rows_affected())?;
    for principal in principals {
        sqlx::query("INSERT INTO platform.\"user\" (id, type, email) VALUES ($1::text::uuid, 'user', $1 || '@example.invalid')").bind(principal).execute(&mut admin).await.map(|result| result.rows_affected())?;
    }
    sqlx::raw_sql("COMMIT")
        .execute(&mut admin)
        .await
        .expect("commit fixture transaction");
    let mut url = url::Url::parse(admin_url)?;
    url.set_username(&role)
        .map_err(|()| anyhow::anyhow!("guest username"))?;
    url.set_password(Some(PASSWORD))
        .map_err(|()| anyhow::anyhow!("guest password"))?;
    let http_role = workload_login(
        WorkloadRoleFamily::HttpAdmitter,
        WorkloadRoleIdentity::ProjectEnvironment {
            org: ORG,
            project: PROJECT,
            environment: &ENVIRONMENT.parse().unwrap(),
            database: &mut database,
        },
    )?;
    let mut http_url = url.clone();
    http_url
        .set_username(&http_role)
        .map_err(|()| anyhow::anyhow!("HTTP reader username"))?;
    let guest_credential = if let Some(path) = credential_file {
        std::fs::write(path, url.as_str())?;
        path.to_str().expect("credential path")
    } else {
        url.as_str()
    };
    let configuration = json!({ PROJECT: { "credentials": {
        (AuthorityClass::GuestSql.as_str()): guest_credential,
        (AuthorityClass::CallableHttp.as_str()): http_url.as_str()
    } } });
    let projects = StaticCredentialProvider::projects_from_json(
        &configuration.to_string(),
        &WamnPostgresConfig {
            credentials: None,
            guest_pool_max_size,
            platform_pool_max_size: 1,
            wait_timeout_ms: 2000,
            statement_timeout_ms: 5000,
        },
    )?;
    let provider = Arc::new(StaticCredentialProvider::new(projects, None));
    let postgres = WamnPostgres::with_provider(
        Arc::clone(&provider) as Arc<dyn wamn_postgres_capability::CredentialProvider>
    );
    Ok((Arc::new(postgres), provider))
}

async fn pat_caller(
    admin_url: &str,
    postgres: Arc<WamnPostgres>,
    release: &LoadedRelease,
) -> anyhow::Result<AuthenticatedCaller> {
    use wamn_platform_identity::{assign_project_role, create_service, issue_pat};

    let mut system_server = wamn_test_postgres::start(&[])?;
    let coordinate = system_server.create_database("control").await?;
    let system_database = url::Url::parse(coordinate.url())?;
    wamn_control_provision::test_database::system(system_database.as_str()).await;
    let mut system = connect(system_database.as_str()).await?;
    sqlx::query("INSERT INTO registry.orgs (id, placement_type, pool_cluster) VALUES ($1, 'pooled', 'warm-test')").bind(&ORG).execute(&mut system).await.map(|result| result.rows_affected())?;
    sqlx::query("INSERT INTO registry.projects (org, id) VALUES ($1, $2)")
        .bind(&ORG)
        .bind(&PROJECT)
        .execute(&mut system)
        .await
        .map(|result| result.rows_affected())?;
    let principal = create_service(
        &mut system,
        "warm-test",
        "Warm test caller",
        &mut wamn_broker::auth_changes::AuthDrops::default(),
    )
    .await?;
    sqlx::query("INSERT INTO registry.env_policies (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image) VALUES ($1, $2, '\"own\"'::jsonb, 1, 1, '1Gi', '1', '1Gi', 'postgres:18')").bind(&ORG).bind(&ENVIRONMENT).execute(&mut system).await.map(|result| result.rows_affected())?;
    sqlx::query("WITH parent_environment AS (INSERT INTO registry.environments (org,env,policy_name) VALUES ($1,$3,$3) ON CONFLICT (org,env) DO NOTHING) INSERT INTO registry.environment_projects (org,project,env,secret_name,instance_suffix) VALUES ($1,$2,$3,'warm-test','a1b2c3d4')").bind(&ORG).bind(&PROJECT).bind(&ENVIRONMENT).execute(&mut system).await.map(|result| result.rows_affected())?;
    assign_project_role(
        &mut system,
        principal.id(),
        ORG,
        PROJECT,
        "admin",
        &mut wamn_broker::auth_changes::AuthDrops::default(),
    )
    .await?;
    let token = issue_pat(
        &mut system,
        principal.id(),
        "warm test",
        Duration::from_secs(600),
    )
    .await?;
    let mut admin = connect(admin_url).await?;
    let mut transaction =
        wamn_test_postgres::transaction(&mut admin, FIXTURE_PRINCIPAL, "admin:warm-pat-test")
            .await?;
    sqlx::query("INSERT INTO platform.\"user\" (id,type,email) VALUES ($1::text::uuid,'service','warm-pat@example.invalid')").bind(&principal.id().to_string().as_str()).execute(&mut *transaction).await.map(|result| result.rows_affected())?;
    sqlx::query(
        "INSERT INTO platform.user_role (user_id,role_name) VALUES ($1::text::uuid,'native-child')",
    )
    .bind(&principal.id().to_string().as_str())
    .execute(&mut *transaction)
    .await
    .map(|result| result.rows_affected())?;
    transaction.commit().await?;
    let reader_role = workload_login(
        WorkloadRoleFamily::IdentityReader,
        WorkloadRoleIdentity::Control {
            org: ORG,
            project: PROJECT,
            environment: &ENVIRONMENT.parse().unwrap(),
            database: system_database.path().trim_start_matches('/'),
        },
    )?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&wamn_control_provision::test_database::login_sql(
            WorkloadRoleFamily::IdentityReader,
            system_database.path().trim_start_matches('/'),
            &reader_role,
            PASSWORD,
        ))
            .to_string(),
    ))
    .execute(&mut system)
    .await
    .map(drop)?;
    let mut reader_url = url::Url::parse(system_database.as_str())?;
    reader_url
        .set_username(&reader_role)
        .map_err(|()| anyhow::anyhow!("identity reader username"))?;
    reader_url
        .set_password(Some(PASSWORD))
        .map_err(|()| anyhow::anyhow!("identity reader password"))?;
    let reader = wamn_postgres::pool(reader_url.as_str(), 4).await?;
    let definition = json!({"id": ATTACHMENT, "type": "http", "route": {"host": "native.example.test", "path": "/native", "method": "POST"}});
    let mut manifest = serde_json::to_value(release.manifest())?;
    manifest["routes"] =
        json!([{"package-id":"root","component":"node","operation":ROOT,"type":"command"}]);
    manifest["attachments"] = json!({ATTACHMENT: {"type":"http","package-id":"root","component":"node","operation":ROOT,"definition-hash":wamn_digest::canonical_json_sha256(&definition),"definition":definition,"auth-policy":{"modes":["pat"]},"registered-operation":ROOT}});

    let release = Arc::new(LoadedRelease::load_canonical_bytes(
        &wamn_digest::canonical_json_bytes(&manifest),
        "warm PAT test",
        wamn_engine::release_manifest::ReleaseTarget::new(
            PROJECT_ENV_KEY.parse().unwrap(),
            ENVIRONMENT.parse().unwrap(),
        )
        .with_applications(serde_json::from_value(
            json!({"receiving": {"packages": ["root", "child"]}}),
        )?),
    )?);
    let routing = FlowHttpRouting::for_test(Some(release)).with_authenticator(Arc::new(
        PlatformRouteAuthenticator::for_test(
            postgres,
            &PROJECT_ENV_KEY.parse().unwrap(),
            Some(reader),
            None,
        ),
    ));
    routing
        .authenticate_authorization_for_test(ATTACHMENT, Some(&format!("Bearer {}", token.token())))
        .await
        .map_err(|(status, code)| anyhow::anyhow!("PAT authentication refused: {status} {code}"))?
        .ok_or_else(|| anyhow::anyhow!("PAT caller absent"))
}
