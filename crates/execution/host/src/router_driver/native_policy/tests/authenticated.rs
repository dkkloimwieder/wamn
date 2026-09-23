//! Real signed sessions and scoped PostgreSQL permissions drive native nested dispatch.

use std::collections::BTreeSet;
use std::process::Command;
use std::sync::{Arc, mpsc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;
use tokio::time::{Instant, timeout};
use tokio_postgres::{Client, NoTls};
use tracing::{Instrument as _, instrument::WithSubscriber as _};
use wamn_control_provision::{
    CredentialGeneration, WorkloadRoleFamily, WorkloadRoleScope, sql, workload_generation_role,
};
use wamn_runtime::plugins::flow_http_routing::{
    AuthenticatedCaller, CredentialKind, FlowHttpRouting, RouteInFlightLimit,
    SessionRouteAuthentication,
};
use wamn_runtime::plugins::wamn_postgres::{
    AuthorityClass, StaticCredentialProvider, WamnPostgres, WamnPostgresConfig,
};
use wamn_runtime::release_manifest::LoadedRelease;
use wamn_runtime::session_verifier::SessionVerifier;

use super::trace::TraceCapture;
use super::{BUDGET, CHILD, CHILD_MARKER, CLEANUP, Case, Fixture, ROOT};
use super::{OperationRefusal, OperationRefusalKind, invoke_native};

#[path = "../../../../../../platform/runtime/tests/support/session_fixture.rs"]
#[expect(
    dead_code,
    reason = "The shared issuer fixture also serves cache-expiry tests."
)]
mod session_fixture;
use session_fixture::{ORG, Server, claims, header, signed};

const PROJECT: &str = "test";
const TENANT: &str = "tenant-a";
const ENVIRONMENT: &str = "test";
const AUDIENCE: &str = "urn:wamn:project-env:org-a:test:test:native-b";
const ATTACHMENT: &str = "native-test-http";
const PASSWORD: &str = "native-b-disposable-test-only";
/// The test principal that the fixture writes as.
const FIXTURE_PRINCIPAL: &str = "00000000-0000-4000-8000-0000000000f1";
const TEST_NAME: &str = "native_authenticated_nested_authority_and_lifecycle";

async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
    tokio::spawn(async move { connection.await.expect("owned fixture database connection") });
    Ok(client)
}

async fn tenant_postgres(admin_url: &str) -> anyhow::Result<Arc<WamnPostgres>> {
    let admin = connect(admin_url).await?;
    let version: i32 = admin
        .query_one("SHOW server_version_num", &[])
        .await?
        .get::<_, String>(0)
        .parse()?;
    anyhow::ensure!(
        version >= 180_000,
        "native authenticated test requires PostgreSQL 18"
    );
    let occupied: bool = admin.query_one(
        "SELECT EXISTS (SELECT FROM pg_namespace WHERE nspname IN ('app_system', 'catalog', 'identity', 'wamn_run')) \
         OR EXISTS (SELECT FROM pg_roles WHERE rolname IN ('wamn_app', 'wamn_scenario_author', 'wamn_http_admitter'))",
        &[],
    ).await?.get(0);
    anyhow::ensure!(
        !occupied,
        "refuse populated PostgreSQL: arm only this test's fresh disposable server"
    );
    let database: String = admin
        .query_one("SELECT current_database()::text", &[])
        .await?
        .get(0);
    let generation = workload_generation_role(
        WorkloadRoleFamily::HttpAdmitter,
        WorkloadRoleScope::ProjectEnvironment {
            org: ORG,
            project: PROJECT,
            environment: ENVIRONMENT,
            database: &database,
        },
        CredentialGeneration::A,
    )?;
    admin
        .batch_execute(
            "CREATE ROLE wamn_app NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS; \
         CREATE ROLE wamn_scenario_author NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOBYPASSRLS; \
         CREATE SCHEMA wamn_run AUTHORIZATION postgres;",
        )
        .await?;
    admin
        .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
        .await?;
    admin
        .batch_execute(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../deploy/sql/app-schema.sql"
        )))
        .await?;
    admin
        .batch_execute(&sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::HttpAdmitter,
            &database,
            &generation,
            PASSWORD,
            "2099-01-01T00:00:00Z",
        ))
        .await?;
    // The fixture writes as its test principal, whose row stamps itself.
    admin
        .execute(
            "SELECT set_config('app.user_id', $1, false), \
                    set_config('app.operation', 'admin:seed-native-policy-fixture', false)",
            &[&FIXTURE_PRINCIPAL],
        )
        .await?;
    admin
        .execute(
            "INSERT INTO app_system.users (tenant_id, id, type, email) \
             VALUES ($1, $2::text::uuid, 'person', 'fixture@example.invalid')",
            &[&TENANT, &FIXTURE_PRINCIPAL],
        )
        .await?;
    for role in ["native-parent", "native-child"] {
        admin
            .execute(
                "INSERT INTO app_system.roles (tenant_id, name) VALUES ($1, $2)",
                &[&TENANT, &role],
            )
            .await?;
        admin.execute(
            "INSERT INTO app_system.permissions (tenant_id, role_name, permission) VALUES ($1, $2, $3)",
            &[&TENANT, &role, &ROOT],
        ).await?;
    }
    admin.execute(
        "INSERT INTO app_system.permissions (tenant_id, role_name, permission) VALUES ($1, 'native-child', $2)",
        &[&TENANT, &CHILD],
    ).await?;
    let mut scoped_url = url::Url::parse(admin_url)?;
    scoped_url
        .set_username(&generation)
        .map_err(|()| anyhow::anyhow!("set fixture login"))?;
    scoped_url
        .set_password(Some(PASSWORD))
        .map_err(|()| anyhow::anyhow!("set fixture password"))?;
    let scoped = connect(scoped_url.as_str()).await?;
    let identity = scoped
        .query_one(
            "SELECT session_user::text, rolsuper, rolbypassrls, \
         pg_has_role(current_user, 'wamn_http_admitter', 'USAGE'), \
         has_table_privilege(current_user, 'app_system.permissions', 'UPDATE') \
         FROM pg_roles WHERE rolname = current_user",
            &[],
        )
        .await?;
    assert_eq!(identity.get::<_, String>(0), generation);
    assert!(!identity.get::<_, bool>(1) && !identity.get::<_, bool>(2));
    assert!(identity.get::<_, bool>(3));
    assert!(
        !identity.get::<_, bool>(4),
        "permission reader cannot change grants"
    );
    drop(scoped);
    let base = WamnPostgresConfig {
        credentials: None,
        guest_pool_max_size: 1,
        platform_pool_max_size: 1,
        wait_timeout_ms: 2000,
        statement_timeout_ms: 5000,
        row_limit: 100,
    };
    let configuration = json!({PROJECT: {"credentials": {
        (AuthorityClass::CallableHttp.as_str()): scoped_url.as_str()
    }}});
    let projects = StaticCredentialProvider::projects_from_json(&configuration.to_string(), &base)?;
    let postgres = Arc::new(WamnPostgres::with_provider(Arc::new(
        StaticCredentialProvider::new(projects, None),
    )));
    Ok(postgres)
}

pub(super) async fn platform_postgres(admin_url: &str) -> anyhow::Result<Arc<WamnPostgres>> {
    tenant_postgres(admin_url).await?;
    let admin = connect(admin_url).await?;
    let principals =
        wamn_control_provision::platform_principals_sql(TENANT, "platform.example.invalid")?;
    admin
        .batch_execute(&format!("BEGIN; {principals} COMMIT;"))
        .await?;
    let (postgres, _) = warm_postgres(admin_url, &[]).await?;
    Ok(postgres)
}

async fn authentication_fixture(admin_url: &str) -> anyhow::Result<(Server, FlowHttpRouting)> {
    let postgres = tenant_postgres(admin_url).await?;
    let server = Server::start().await;
    let (keys, _) = server.cache();
    let verifier = SessionVerifier::new(keys, ORG, AUDIENCE)?;
    let admin = connect(admin_url).await?;
    let first = claims()["sub"].as_str().unwrap().to_owned();
    let second = "00000000-0000-4000-8000-0000000000b2";
    session_fixture::install_authority(&admin, PROJECT, ENVIRONMENT, AUDIENCE, &[&first, second])
        .await?;
    admin
        .execute(
            "SELECT set_config('app.user_id',$1,false), set_config('app.operation','admin:seed-session-authority',false)",
            &[&FIXTURE_PRINCIPAL],
        )
        .await?;
    for (principal, email) in [
        (&*first, "alice@example.test"),
        (second, "bob@example.test"),
    ] {
        admin.execute("INSERT INTO app_system.users (tenant_id,id,type,email) VALUES ($1,$2::text::uuid,'person',$3)", &[&TENANT,&principal,&email]).await?;
        for role in ["native-parent", "native-child"] {
            admin.execute("INSERT INTO app_system.user_roles (tenant_id,user_id,role_name) VALUES ($1,$2::text::uuid,$3)", &[&TENANT,&principal,&role]).await?;
        }
    }
    let authentication = Arc::new(SessionRouteAuthentication::new(
        verifier,
        Arc::new(admin),
        postgres,
        PROJECT,
    ));
    let definition = json!({"id": ATTACHMENT, "kind": "http", "route": {
        "host": "native.example.test", "path": "/native", "method": "POST"
    }});
    let manifest = json!({
        "format-version": super::SERVING_MANIFEST_FORMAT_VERSION,
        "release": {"tenant-id": TENANT, "effective-release-id": 1, "environment": ENVIRONMENT,
            "packages": [{"package-id": "root", "package-version": "1.0.0"}]},
        "components": [{"package-id": "root", "component": "node", "interface-version": "0.1.0",
            "digest": format!("sha256:{}", "a".repeat(64)), "operations": {
                ROOT: {"registered-operation": ROOT}
            }}],
        "routes": [],
        "wirings": [{"package-id": "root", "wiring-id": "trusted-wiring", "wiring-version": 1,
            "graph-hash": format!("sha256:{}", "b".repeat(64))}],
        "attachments": {ATTACHMENT: {"kind": "http", "package-id": "root", "wiring-id": "trusted-wiring",
            "wiring-version": 1, "definition-hash": wamn_execution_contract::canonical_json_sha256(&definition),
            "definition": definition, "auth-policy": {"modes": ["session"]}, "registered-operation": ROOT}},
        "registrations": {}
    });
    let release = Arc::new(LoadedRelease::load_canonical_bytes(
        &wamn_execution_contract::canonical_json_bytes(&manifest),
        "native session route test",
    )?);
    let route = FlowHttpRouting::new(Some(release), RouteInFlightLimit::default())
        .with_session_authentication(authentication);
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
    assert_eq!(caller.credential_kind(), CredentialKind::Session);
    assert!(caller.permits(ROOT));
    assert_eq!(caller.permits(CHILD), child_grant);
    caller
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scenario {
    PermissionDenied,
    FreshOnly,
    Success,
    InitializationDeadline,
    Deadline,
    Cancellation,
}

const SCENARIOS: [Scenario; 6] = [
    Scenario::PermissionDenied,
    Scenario::FreshOnly,
    Scenario::Success,
    Scenario::InitializationDeadline,
    Scenario::Deadline,
    Scenario::Cancellation,
];

fn child_has_started(fixture: &Fixture) -> bool {
    fixture
        .events
        .lock()
        .expect("observations lock")
        .iter()
        .any(|event| {
            event.phase == 1
                && event
                    .invocation
                    .as_ref()
                    .is_some_and(|invocation| invocation.operation == CHILD)
        })
}

async fn assert_case(
    scenario: Scenario,
    caller: &AuthenticatedCaller,
    postgres: Arc<WamnPostgres>,
) {
    let child_case = match scenario {
        Scenario::InitializationDeadline => Case::StartDeadline,
        Scenario::Deadline => Case::RunDeadline,
        Scenario::Cancellation => Case::Cancellation,
        _ => Case::Success,
    };
    let fixture = Fixture::build_with_reuse(
        Case::NestedRefusal,
        Some((child_case, scenario == Scenario::FreshOnly)),
        true,
        None,
        false,
        Some(postgres),
    )
    .await;
    let target = fixture.target().await;
    let trace = (scenario == Scenario::Success).then(|| TraceCapture::new(&fixture, caller));
    let deadline = Instant::now()
        + if matches!(
            scenario,
            Scenario::InitializationDeadline | Scenario::Deadline
        ) {
            BUDGET
        } else {
            Duration::from_secs(30)
        };
    let mut request = fixture.request(deadline);
    request.caller = Some(caller.clone());
    if scenario == Scenario::Cancellation {
        let task = tokio::spawn(async move { invoke_native(&target, request).await });
        timeout(CLEANUP, async {
            while !child_has_started(&fixture) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the permitted child starts before cancellation");
        task.abort();
        assert!(
            timeout(CLEANUP, task)
                .await
                .expect("caller cancellation completes")
                .expect_err("caller was cancelled")
                .is_cancelled()
        );
    } else {
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
        match scenario {
            Scenario::Success | Scenario::FreshOnly => {
                let emission = result
                    .expect("permitted nested dispatch")
                    .expect("typed emission");
                assert_eq!(emission.payload, r#"[{"value":37}]"#);
                assert_eq!(emission.port, None);
            }
            Scenario::PermissionDenied => {
                let error = result.expect_err("the registered child must be refused");
                let refusal = error
                    .downcast_ref::<OperationRefusal>()
                    .unwrap_or_else(|| panic!("native dispatch retains typed refusal: {error:#}"));
                assert_eq!(refusal.operation(), CHILD);
                assert_eq!(refusal.kind(), OperationRefusalKind::PermissionDenied);
            }
            Scenario::InitializationDeadline | Scenario::Deadline => {
                let error =
                    result.expect_err("the looping child cannot extend its parent's deadline");
                assert!(Instant::now() >= deadline && Instant::now() < deadline + CLEANUP);
                assert!(format!("{error:#}").contains("deadline"), "{error:#}");
            }
            Scenario::Cancellation => unreachable!("handled by the caller task"),
        }
    }
    fixture.assert_clean().await;
    let child_ran = matches!(
        scenario,
        Scenario::Success | Scenario::FreshOnly | Scenario::Deadline | Scenario::Cancellation
    );
    assert_eq!(child_has_started(&fixture), child_ran);
    let observations = fixture.events.lock().expect("observations lock").clone();
    let child_initialized = child_ran || scenario == Scenario::InitializationDeadline;
    assert_eq!(
        observations.iter().any(|event| {
            event.phase == 0
                && fixture
                    .workload
                    .facts_by_component_id
                    .get(&event.scope)
                    .is_some_and(|fact| fact.operations.contains_key(CHILD))
        }),
        child_initialized,
        "the permitted child reaches initialization even when its start function hangs"
    );
    let native_ids: BTreeSet<_> = fixture
        .workload
        .facts_by_component_id
        .keys()
        .cloned()
        .collect();
    let mut scopes = BTreeSet::new();
    let mut operations = BTreeSet::new();
    assert_eq!(
        observations.iter().filter(|event| event.phase == 0).count(),
        1 + usize::from(child_initialized)
    );
    assert_eq!(
        observations.iter().filter(|event| event.phase == 1).count(),
        1 + usize::from(child_ran)
    );
    for event in observations {
        if event.phase == 0 {
            assert!(event.native_identity && native_ids.contains(&event.scope));
            assert!(event.claims.is_none() && event.invocation.is_none() && event.caller.is_none());
            assert!(event.deadline.is_none());
            continue;
        }
        assert!(!event.native_identity && !native_ids.contains(&event.scope));
        assert!(
            scopes.insert(event.scope.clone()),
            "parent and child have distinct request scopes"
        );
        let inherited = event
            .caller
            .expect("authenticated authority reaches guest execution");
        assert_eq!(inherited.principal_id(), caller.principal_id());
        assert_eq!(inherited.credential_kind(), CredentialKind::Session);
        assert_eq!(inherited.permits(CHILD), caller.permits(CHILD));
        assert_eq!(
            event.deadline,
            Some(deadline),
            "nested work keeps the enclosing deadline"
        );
        let claims = event.claims.expect("host claims");
        assert_eq!(claims.tenant, TENANT);
        assert_eq!(
            claims.user_id.as_deref(),
            Some(caller.principal_id()),
            "the parent and its nested call bind the caller principal"
        );
        assert_eq!(claims.project.as_deref(), Some(PROJECT));
        assert_eq!(
            claims.release,
            fixture.request(deadline).acquisition.claims.release
        );
        let invocation = event.invocation.expect("host invocation");
        assert_eq!(
            claims.operation.as_deref(),
            Some(invocation.operation.as_str()),
            "the parent and its nested call each bind their own operation"
        );
        assert_eq!(
            invocation.origin,
            fixture.request(deadline).acquisition.invocation.origin,
            "nested execution preserves its distinct wiring owner and original root component"
        );
        let position = invocation.entry.wiring().expect("a wiring entry");
        assert_eq!(position.wiring_id, "trusted-wiring");
        assert_eq!(position.wiring_version, 1);
        assert_eq!(position.node_id, "trusted-node");
        let fact = fixture
            .workload
            .facts_by_component_id
            .values()
            .find(|fact| fact.operations.contains_key(&invocation.operation))
            .expect("admitted operation owner");
        assert_eq!(invocation.package_id, fact.scope.package_id);
        assert_eq!(invocation.component, fact.component);
        assert_eq!(invocation.component_digest, fact.component_digest);
        assert!(
            fixture
                .policy
                .resources
                .postgres
                .activate_statement_operation(&event.scope, &invocation.operation)
                .is_err(),
            "statement authority was revoked"
        );
        operations.insert(invocation.operation);
    }
    assert_eq!(
        operations,
        if child_ran {
            BTreeSet::from([ROOT.to_owned(), CHILD.to_owned()])
        } else {
            BTreeSet::from([ROOT.to_owned()])
        }
    );
    fixture
        .workload
        .resolved
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
    println!("authenticated-native-case={scenario:?} result=pass");
}

async fn assert_authenticated(admin_url: &str) -> anyhow::Result<()> {
    let (mut server, route) = authentication_fixture(admin_url).await?;
    let parent_only = authenticated(&route, false).await;
    let permitted = authenticated(&route, true).await;
    let (postgres, _) = warm_postgres(admin_url, &[]).await?;
    for scenario in SCENARIOS {
        assert_case(
            scenario,
            if scenario == Scenario::PermissionDenied {
                &parent_only
            } else {
                &permitted
            },
            Arc::clone(&postgres),
        )
        .await;
    }
    server.stop().await;
    Ok(())
}

#[test]
fn native_authenticated_nested_authority_and_lifecycle() {
    let full_name = format!("router_driver::native_policy::tests::authenticated::{TEST_NAME}");
    if std::env::var(CHILD_MARKER).as_deref() != Ok(TEST_NAME) {
        let output = Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", &full_name, "--nocapture"])
            .env(CHILD_MARKER, TEST_NAME)
            .output()
            .expect("start isolated authenticated native test");
        assert!(
            output.status.success(),
            "{full_name}: {}\n{}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("1 passed"),
            "subprocess executed the named test"
        );
        assert_eq!(
            stdout.matches("authenticated-native-case=").count(),
            SCENARIOS.len()
        );
        for scenario in SCENARIOS {
            assert!(
                stdout.contains(&format!(
                    "authenticated-native-case={scenario:?} result=pass"
                )),
                "subprocess executed {scenario:?}"
            );
        }
        assert_eq!(
            stdout
                .matches("authenticated-native-trace result=pass")
                .count(),
            1
        );
        for result_line in stdout
            .lines()
            .filter(|line| line.starts_with("authenticated-native-"))
        {
            println!("{result_line}");
        }
        return;
    }
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::database();
    let (done, finished) = mpsc::channel();
    let watchdog = std::thread::spawn(move || {
        if finished.recv_timeout(Duration::from_secs(60)) == Err(mpsc::RecvTimeoutError::Timeout) {
            eprintln!("authenticated native test exceeded its process watchdog");
            std::process::exit(124);
        }
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .event_interval(1)
        .enable_all()
        .build()
        .expect("isolated native runtime");
    runtime
        .block_on(assert_authenticated(database.url()))
        .expect("real authenticated native test");
    drop(runtime);
    done.send(()).expect("finish watchdog");
    watchdog.join().expect("join watchdog");
}

#[test]
fn native_authenticated_transaction_participant() {
    super::run_isolated_test(
        "authenticated::native_authenticated_transaction_participant",
        async {
            let _lock = wamn_test_postgres::lock();
            let database = wamn_test_postgres::database();
            let (mut server, route) = authentication_fixture(database.url())
                .await
                .expect("authenticated transaction fixture");
            let caller = authenticated(&route, true).await;
            let parent_only = authenticated(&route, false).await;
            let (postgres, _) = warm_postgres_with_pool(database.url(), &[], 2)
                .await
                .expect("participant SQL credentials");
            let admin = connect(database.url()).await.expect("admin connection");
            admin
                .batch_execute(
                    "CREATE TABLE public.native_policy_participant(value int NOT NULL); \
                     INSERT INTO public.native_policy_participant VALUES (1); \
                     GRANT SELECT, UPDATE ON public.native_policy_participant TO wamn_app",
                )
                .await
                .expect("participant table");

            let fixture = Fixture::build_with_reuse(
                Case::TransactionOwner,
                Some((Case::TransactionParticipant, false)),
                true,
                None,
                false,
                Some(Arc::clone(&postgres)),
            )
            .await;
            let mut request = fixture.request(Instant::now() + Duration::from_secs(10));
            request.caller = Some(caller.clone());
            let result = invoke_native(&fixture.target().await, request)
                .await
                .expect("authorized participant dispatch")
                .expect("participant SQL succeeds");
            assert_eq!(result.payload, "[]");
            let value: i32 = admin
                .query_one("SELECT value FROM public.native_policy_participant", &[])
                .await
                .expect("read rollback result")
                .get(0);
            assert_eq!(
                value, 1,
                "participant SQL used the owner's rolled-back transaction"
            );
            fixture.assert_clean().await;

            let mut request = fixture.request(Instant::now() + Duration::from_secs(10));
            request.caller = Some(parent_only);
            let error = invoke_native(&fixture.target().await, request)
                .await
                .expect_err("caller without participant permission is refused");
            let refusal = error
                .downcast_ref::<OperationRefusal>()
                .expect("participant refusal remains typed");
            assert_eq!(refusal.kind(), OperationRefusalKind::PermissionDenied);
            assert_eq!(refusal.operation(), CHILD);
            fixture.assert_clean().await;

            let denied = Fixture::build_with_reuse(
                Case::TransactionParticipant,
                None,
                true,
                None,
                false,
                Some(postgres),
            )
            .await;
            let mut request = denied.request(Instant::now() + Duration::from_secs(10));
            request.caller = Some(caller);
            let result = invoke_native(&denied.target().await, request)
                .await
                .expect("nonparticipant probe dispatches")
                .expect("probe reports the resource result");
            assert_eq!(
                result.payload, "view",
                "participant-view refused the nonparticipating invocation"
            );
            denied.assert_clean().await;
            server.stop().await;
        },
    );
}

#[test]
fn native_warm_alternating_callers_and_fresh_nested_component() {
    super::run_isolated_test(
        "authenticated::native_warm_alternating_callers_and_fresh_nested_component",
        async {
            let _lock = wamn_test_postgres::lock();
            let database = wamn_test_postgres::database();
            let (mut server, route) = authentication_fixture(database.url())
                .await
                .expect("scoped authentication fixture");
            let alice = authenticated(&route, true).await;
            let bob =
                authenticated_as(&route, true, Some("00000000-0000-4000-8000-0000000000b2")).await;
            assert_ne!(alice.principal_id(), bob.principal_id());
            let (postgres, credentials) = warm_postgres(database.url(), &[])
                .await
                .expect("guest-generation fixture");
            let fixture = Fixture::build_with_reuse(
                Case::NestedRefusal,
                Some((Case::Success, false)),
                true,
                None,
                true,
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
                request.caller = Some(caller.clone());
                let result = invoke_native(&target, request)
                    .await
                    .expect("authorized native dispatch")
                    .expect("emission");
                assert_eq!(
                    result.payload, input,
                    "no caller-dependent result crosses calls"
                );
                assert!(
                    fixture
                        .policy
                        .invocations
                        .lock()
                        .expect("authority")
                        .is_empty()
                );
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
                            .session_claims(&event.scope)
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
            for (id, fact) in &fixture.workload.facts_by_component_id {
                assert_eq!(
                    observations
                        .iter()
                        .filter(|event| event.phase == 0 && event.scope == *id)
                        .count(),
                    if fact == &fixture.root { 1 } else { 3 },
                    "warm root reuses while its independent child stays fresh"
                );
            }
            let callers: Vec<_> = observations
                .iter()
                .filter(|event| event.phase == 1)
                .map(|event| event.caller.as_ref().expect("bound caller").principal_id())
                .collect();
            assert_eq!(
                callers,
                vec![
                    alice.principal_id(),
                    alice.principal_id(),
                    bob.principal_id(),
                    bob.principal_id(),
                    alice.principal_id(),
                    alice.principal_id()
                ]
            );
            let denied = authenticated_as(&route, false, Some(bob.principal_id())).await;
            let mut request = fixture.request(Instant::now() + CLEANUP);
            request.caller = Some(denied);
            let error = invoke_native(&target, request)
                .await
                .expect_err("new caller cannot inherit the prior nested grant");
            assert_eq!(
                error
                    .downcast_ref::<OperationRefusal>()
                    .expect("typed permission refusal")
                    .kind(),
                OperationRefusalKind::PermissionDenied
            );
            super::warm::close(&fixture).await;

            let fresh_only = Fixture::build_with_reuse(
                Case::NestedRefusal,
                Some((Case::Success, true)),
                true,
                None,
                true,
                Some(Arc::clone(&postgres)),
            )
            .await;
            let mut request = fresh_only.request(Instant::now() + CLEANUP);
            request.caller = Some(alice.clone());
            invoke_native(&fresh_only.target().await, request)
                .await
                .expect("session admits the legacy fresh-only child")
                .expect("emission");
            let pat = pat_caller(
                database.url(),
                Arc::clone(&postgres),
                &fresh_only.policy.resources.release,
            )
            .await
            .expect("real PAT caller");
            assert_eq!(pat.credential_kind(), CredentialKind::Pat);
            for caller in [&pat, &pat] {
                let mut request = fresh_only.request(Instant::now() + CLEANUP);
                request.caller = Some(caller.clone());
                invoke_native(&fresh_only.target().await, request)
                    .await
                    .expect("PAT admits fresh-only child")
                    .expect("emission");
            }
            let root_id = fresh_only
                .workload
                .facts_by_component_id
                .iter()
                .find(|(_, fact)| *fact == &fresh_only.root)
                .expect("root native identity")
                .0;
            assert_eq!(
                fresh_only
                    .events
                    .lock()
                    .expect("events")
                    .iter()
                    .filter(|event| event.phase == 0 && &event.scope == root_id)
                    .count(),
                1,
                "session and PAT calls reuse the same root instance"
            );
            let mut request = fresh_only.request(Instant::now() + CLEANUP);
            request.caller = Some(alice);
            invoke_native(&fresh_only.target().await, request)
                .await
                .expect("session remains authorized after PAT calls")
                .expect("emission");
            super::warm::close(&fresh_only).await;
            let retired =
                Fixture::build_with_reuse(Case::Success, None, true, None, true, Some(postgres))
                    .await;
            let mut request = retired.request(Instant::now() + CLEANUP);
            request.caller = Some(bob.clone());
            invoke_native(&retired.target().await, request)
                .await
                .expect("credential initially available")
                .expect("emission");
            credentials
                .enabled
                .store(false, std::sync::atomic::Ordering::SeqCst);
            let mut request = retired.request(Instant::now() + CLEANUP);
            request.caller = Some(bob.clone());
            assert!(
                invoke_native(&retired.target().await, request)
                    .await
                    .is_err(),
                "an unavailable credential refuses without substitution"
            );
            credentials
                .enabled
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let mut request = retired.request(Instant::now() + CLEANUP);
            request.caller = Some(bob);
            invoke_native(&retired.target().await, request)
                .await
                .expect("restored credential")
                .expect("emission");
            assert_eq!(
                super::warm::starts(&retired),
                2,
                "credential refusal retires the previously reused instance"
            );
            super::warm::close(&retired).await;
            server.stop().await;
        },
    );
}

struct WarmCredentials {
    provider: StaticCredentialProvider,
    enabled: std::sync::atomic::AtomicBool,
}
impl wamn_runtime::plugins::wamn_postgres::CredentialProvider for WarmCredentials {
    fn resolve(
        &self,
        project: &str,
        class: AuthorityClass,
        tenant: Option<&str>,
    ) -> anyhow::Result<Option<wamn_runtime::plugins::wamn_postgres::ResolvedCredential>> {
        if !self.enabled.load(std::sync::atomic::Ordering::SeqCst) {
            return Ok(None);
        }
        self.provider.resolve(project, class, tenant)
    }
}

async fn warm_postgres(
    admin_url: &str,
    principals: &[&str],
) -> anyhow::Result<(Arc<WamnPostgres>, Arc<WarmCredentials>)> {
    warm_postgres_with_pool(admin_url, principals, 1).await
}

async fn warm_postgres_with_pool(
    admin_url: &str,
    principals: &[&str],
    guest_pool_max_size: usize,
) -> anyhow::Result<(Arc<WamnPostgres>, Arc<WarmCredentials>)> {
    let admin = connect(admin_url).await?;
    let database: String = admin
        .query_one("SELECT current_database()::text", &[])
        .await?
        .get(0);
    let role = workload_generation_role(
        WorkloadRoleFamily::App,
        WorkloadRoleScope::Tenant {
            tenant: TENANT,
            database: &database,
        },
        CredentialGeneration::A,
    )?;
    admin
        .batch_execute(&sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::App,
            &database,
            &role,
            PASSWORD,
            "2099-01-01T00:00:00Z",
        ))
        .await?;
    admin.execute("SELECT set_config('app.user_id', $1, false), set_config('app.operation', 'admin:warm-test', false)", &[&FIXTURE_PRINCIPAL]).await?;
    for principal in principals {
        admin.execute("INSERT INTO app_system.users (tenant_id, id, type, email) VALUES ($1, $2::text::uuid, 'person', $2 || '@example.invalid')", &[&TENANT, principal]).await?;
    }
    let mut url = url::Url::parse(admin_url)?;
    url.set_username(&role)
        .map_err(|()| anyhow::anyhow!("guest username"))?;
    url.set_password(Some(PASSWORD))
        .map_err(|()| anyhow::anyhow!("guest password"))?;
    let http_role = workload_generation_role(
        WorkloadRoleFamily::HttpAdmitter,
        WorkloadRoleScope::ProjectEnvironment {
            org: ORG,
            project: PROJECT,
            environment: ENVIRONMENT,
            database: &database,
        },
        CredentialGeneration::A,
    )?;
    let mut http_url = url.clone();
    http_url
        .set_username(&http_role)
        .map_err(|()| anyhow::anyhow!("HTTP reader username"))?;
    let configuration = json!({ PROJECT: { "credentials": {
        (AuthorityClass::GuestSql.as_str()): url.as_str(),
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
            row_limit: 100,
        },
    )?;
    let credentials = Arc::new(WarmCredentials {
        provider: StaticCredentialProvider::new(projects, None),
        enabled: std::sync::atomic::AtomicBool::new(true),
    });
    Ok((
        Arc::new(WamnPostgres::with_provider(credentials.clone())),
        credentials,
    ))
}

#[test]
fn native_warm_retirement_aborts_retained_postgres_transaction() {
    super::run_isolated_test(
        "authenticated::native_warm_retirement_aborts_retained_postgres_transaction",
        async {
            use wamn_runtime::plugins::wamn_postgres::{
                PgTransaction, SessionClaims, retained_transaction_for_test,
            };
            use wash_runtime::engine::ctx::SharedCtx;
            use wash_runtime::engine::dispatch::{GuestCall, GuestCallFuture};
            use wash_runtime::wasmtime::component::{Accessor, Instance};

            struct LeaveTransaction(PgTransaction);
            impl GuestCall for LeaveTransaction {
                fn describe(&self) -> &'static str {
                    "retain an actual PostgreSQL transaction"
                }
                fn call(
                    self: Box<Self>,
                    accessor: &Accessor<SharedCtx>,
                    _instance: Instance,
                ) -> GuestCallFuture<'_> {
                    Box::pin(async move {
                        accessor.with(|mut access| access.get().table.push(self.0))?;
                        Ok(None)
                    })
                }
            }

            let _lock = wamn_test_postgres::lock();
            let database = wamn_test_postgres::database();
            let (mut server, route) = authentication_fixture(database.url())
                .await
                .expect("database and signed caller");
            let caller = authenticated(&route, true).await;
            let (postgres, _) = warm_postgres(database.url(), &[])
                .await
                .expect("tenant guest generation");
            let admin = connect(database.url()).await.expect("admin");
            admin.batch_execute("CREATE TABLE public.warm_retirement (value text); GRANT INSERT ON public.warm_retirement TO wamn_app").await.expect("owned rollback marker");
            postgres
                .bind_session_claims(
                    "retained-transaction",
                    &SessionClaims {
                        tenant: TENANT.into(),
                        project: Some(PROJECT.into()),
                        user_id: Some(caller.principal_id().into()),
                        operation: Some(ROOT.into()),
                        ..Default::default()
                    },
                )
                .await
                .expect("transaction owner authority");
            let (transaction, pid) = retained_transaction_for_test(
                &postgres,
                "retained-transaction",
                PROJECT,
                "INSERT INTO public.warm_retirement VALUES ('must roll back')",
            )
            .await
            .expect("actual guest transaction");
            postgres.revoke_session_claims("retained-transaction");
            let fixture = super::warm::fixture(Case::Success).await;
            let target = fixture.target().await;
            target
                .dispatch(LeaveTransaction(transaction))
                .await
                .expect("native warm resource table");
            let active: bool = admin.query_one("SELECT EXISTS (SELECT FROM pg_stat_activity WHERE pid=$1 AND state='idle in transaction')", &[&pid]).await.expect("transaction state").get(0);
            assert!(active, "the retained transaction is open before retirement");
            invoke_native(&target, fixture.request(Instant::now() + CLEANUP))
                .await
                .expect("completed result")
                .expect("emission");
            timeout(CLEANUP, async {
                loop {
                    let alive: bool = admin
                        .query_one(
                            "SELECT EXISTS (SELECT FROM pg_stat_activity WHERE pid=$1)",
                            &[&pid],
                        )
                        .await
                        .expect("backend state")
                        .get(0);
                    if !alive {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("retirement destroys the session rather than repooling its transaction");
            let rows: i64 = admin
                .query_one("SELECT count(*) FROM public.warm_retirement", &[])
                .await
                .expect("rollback result")
                .get(0);
            assert_eq!(rows, 0);
            invoke_native(&target, fixture.request(Instant::now() + CLEANUP))
                .await
                .expect("later invocation")
                .expect("emission");
            assert_eq!(
                super::warm::starts(&fixture),
                2,
                "no transaction handle survives into the next instance"
            );
            super::warm::close(&fixture).await;
            server.stop().await;
        },
    );
}

async fn pat_caller(
    admin_url: &str,
    postgres: Arc<WamnPostgres>,
    release: &LoadedRelease,
) -> anyhow::Result<AuthenticatedCaller> {
    use wamn_platform_identity::{create_human, grant_project_env_membership, issue_pat};
    use wamn_runtime::plugins::flow_http_routing::RouteAuthentication;

    let system_database = wamn_control_provision::test_database::system();
    let system = connect(system_database.url()).await?;
    system
        .execute(
            "SELECT set_config('app.user_id', $1, false)",
            &[&wamn_control_provision::PlatformComponent::Provisioning
                .principal_id()
                .to_string()],
        )
        .await?;
    system.execute("INSERT INTO registry.orgs (id, placement_kind, pool_cluster) VALUES ($1, 'pooled', 'warm-test')", &[&ORG]).await?;
    system
        .execute(
            "INSERT INTO registry.projects (org, id) VALUES ($1, $2)",
            &[&ORG, &PROJECT],
        )
        .await?;
    let principal = create_human(
        &system,
        "warm-pat@example.invalid",
        "warm-pat@example.invalid",
        "Warm test caller",
    )
    .await?;
    system.execute("INSERT INTO registry.env_policies (org, name, recovery_domain, promotion_rank, instances, storage, cpu, memory, image) VALUES ($1, $2, '\"own\"'::jsonb, 1, 1, '1Gi', '1', '1Gi', 'postgres:18')", &[&ORG, &ENVIRONMENT]).await?;
    system.execute("INSERT INTO registry.project_envs (org, project, env, secret_name, instance_suffix) VALUES ($1, $2, $3, 'warm-test', 'a1b2c3d4')", &[&ORG, &PROJECT, &ENVIRONMENT]).await?;
    grant_project_env_membership(&system, principal.id(), ORG, PROJECT, ENVIRONMENT).await?;
    let token = issue_pat(
        &system,
        principal.id(),
        "warm test",
        Duration::from_secs(600),
    )
    .await?;
    let admin = connect(admin_url).await?;
    admin.execute("SELECT set_config('app.user_id', $1, false), set_config('app.operation', 'admin:warm-pat-test', false)", &[&FIXTURE_PRINCIPAL]).await?;
    admin.execute("INSERT INTO app_system.users (tenant_id,id,type,email) VALUES ($1,$2::text::uuid,'person','warm-pat@example.invalid')", &[&TENANT, &principal.id().as_str()]).await?;
    admin.execute("INSERT INTO app_system.user_roles (tenant_id,user_id,role_name) VALUES ($1,$2::text::uuid,'native-child')", &[&TENANT, &principal.id().as_str()]).await?;
    let reader_role = workload_generation_role(
        WorkloadRoleFamily::IdentityReader,
        WorkloadRoleScope::Control {
            org: ORG,
            project: PROJECT,
            environment: ENVIRONMENT,
            database: system_database.name(),
        },
        CredentialGeneration::A,
    )?;
    system
        .batch_execute(&sql::prepare_workload_generation_sql(
            WorkloadRoleFamily::IdentityReader,
            system_database.name(),
            &reader_role,
            PASSWORD,
            "2099-01-01T00:00:00Z",
        ))
        .await?;
    let mut reader_url = url::Url::parse(system_database.url())?;
    reader_url
        .set_username(&reader_role)
        .map_err(|()| anyhow::anyhow!("identity reader username"))?;
    reader_url
        .set_password(Some(PASSWORD))
        .map_err(|()| anyhow::anyhow!("identity reader password"))?;
    let reader = connect(reader_url.as_str()).await?;
    let definition = json!({"id": ATTACHMENT, "kind": "http", "route": {"host": "native.example.test", "path": "/native", "method": "POST"}});
    let mut manifest = serde_json::to_value(release.manifest())?;
    manifest["wirings"] = json!([{"package-id":"root","wiring-id":"trusted-wiring","wiring-version":1,"graph-hash":format!("sha256:{}", "b".repeat(64))}]);
    manifest["attachments"] = json!({ATTACHMENT: {"kind":"http","package-id":"root","wiring-id":"trusted-wiring","wiring-version":1,"definition-hash":wamn_execution_contract::canonical_json_sha256(&definition),"definition":definition,"auth-policy":{"modes":["pat"]},"registered-operation":ROOT}});
    let release = Arc::new(LoadedRelease::load_canonical_bytes(
        &wamn_execution_contract::canonical_json_bytes(&manifest),
        "warm PAT test",
    )?);
    let routing = FlowHttpRouting::new(Some(release), RouteInFlightLimit::default())
        .with_authentication(Arc::new(
            RouteAuthentication::new(
                Arc::new(reader),
                postgres,
                ORG,
                PROJECT,
                "unused-human-subject",
            )
            .await?,
        ));
    routing
        .authenticate_authorization_for_test(ATTACHMENT, Some(&format!("Bearer {}", token.token())))
        .await
        .map_err(|(status, code)| anyhow::anyhow!("PAT authentication refused: {status} {code}"))?
        .ok_or_else(|| anyhow::anyhow!("PAT caller absent"))
}
