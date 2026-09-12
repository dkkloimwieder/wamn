//! Real signed sessions and scoped PostgreSQL permissions drive native nested dispatch.

use std::collections::BTreeSet;
use std::process::Command;
use std::sync::{Arc, mpsc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
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
use wamn_runtime::release_manifest::ReleaseManifestWeld;
use wamn_runtime::session_verifier::SessionVerifier;

use super::trace::TraceProof;
use super::{BUDGET, CHILD, CHILD_MARKER, CLEANUP, Case, Fixture, ROOT};
use super::{OperationRefusal, OperationRefusalKind, invoke_native};

#[path = "../../../../../../platform/runtime/tests/support/session_fixture.rs"]
#[expect(
    dead_code,
    reason = "The shared issuer fixture also serves cache-expiry proofs."
)]
mod session_fixture;
use session_fixture::{ORG, Server, claims, header, signed};

const URL_ENV: &str = "WAMN_NATIVE_B_AUTH_PG_URL";
const PROJECT: &str = "proof";
const TENANT: &str = "tenant-a";
const ENVIRONMENT: &str = "proof";
const AUDIENCE: &str = "urn:wamn:project-env:org-a:proof:proof:native-b";
const ATTACHMENT: &str = "native-proof-http";
const PASSWORD: &str = "native-b-disposable-proof-only";
const TEST_NAME: &str = "native_authenticated_nested_authority_and_lifecycle";

async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
    tokio::spawn(async move { connection.await.expect("owned fixture database connection") });
    Ok(client)
}

async fn authentication_fixture(admin_url: &str) -> anyhow::Result<(Server, FlowHttpRouting)> {
    let admin = connect(admin_url).await?;
    let version: i32 = admin
        .query_one("SHOW server_version_num", &[])
        .await?
        .get::<_, String>(0)
        .parse()?;
    anyhow::ensure!(
        version >= 180_000,
        "native authenticated proof requires PostgreSQL 18"
    );
    let occupied: bool = admin.query_one(
        "SELECT EXISTS (SELECT FROM pg_namespace WHERE nspname IN ('app_system', 'catalog', 'identity', 'wamn_run')) \
         OR EXISTS (SELECT FROM pg_roles WHERE rolname IN ('wamn_app', 'wamn_scenario_author', 'wamn_http_admitter'))",
        &[],
    ).await?.get(0);
    anyhow::ensure!(
        !occupied,
        "refuse populated PostgreSQL: arm only this proof's fresh disposable server"
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
    let server = Server::start().await;
    let (keys, _) = server.cache();
    let verifier = SessionVerifier::new(keys, ORG, AUDIENCE)?;
    let authentication = Arc::new(SessionRouteAuthentication::new(verifier, postgres, PROJECT));
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
        "wirings": [{"package-id": "root", "wiring-id": "trusted-wiring", "wiring-version": 1,
            "graph-hash": format!("sha256:{}", "b".repeat(64))}],
        "attachments": {ATTACHMENT: {"kind": "http", "package-id": "root", "wiring-id": "trusted-wiring",
            "wiring-version": 1, "definition-hash": wamn_execution_contract::canonical_json_sha256(&definition),
            "definition": definition, "auth-policy": {"modes": ["session"]}, "registered-operation": ROOT}},
        "registrations": {}
    });
    let release = Arc::new(ReleaseManifestWeld::load_canonical_bytes(
        &wamn_execution_contract::canonical_json_bytes(&manifest),
        "native session route proof",
    )?);
    let route = FlowHttpRouting::new(Some(release), RouteInFlightLimit::default())
        .with_session_authentication(authentication);
    Ok((server, route))
}

async fn authenticated(route: &FlowHttpRouting, child_grant: bool) -> AuthenticatedCaller {
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("wall clock")
            .as_secs(),
    )
    .expect("Unix seconds fit i64");
    let mut body = claims();
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

async fn prove_case(scenario: Scenario, caller: &AuthenticatedCaller) {
    let child_case = match scenario {
        Scenario::InitializationDeadline => Case::StartDeadline,
        Scenario::Deadline => Case::RunDeadline,
        Scenario::Cancellation => Case::Cancellation,
        _ => Case::Success,
    };
    let fixture = Fixture::build(
        Case::NestedRefusal,
        Some((child_case, scenario == Scenario::FreshOnly)),
        true,
    )
    .await;
    let target = fixture.target().await;
    let trace = (scenario == Scenario::Success).then(|| TraceProof::new(&fixture, caller));
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
            Scenario::Success => {
                let emission = result
                    .expect("permitted nested dispatch")
                    .expect("typed emission");
                assert_eq!(emission.payload, r#"[{"value":37}]"#);
                assert_eq!(emission.port, None);
            }
            Scenario::PermissionDenied | Scenario::FreshOnly => {
                let error = result.expect_err("the registered child must be refused");
                let refusal = error
                    .downcast_ref::<OperationRefusal>()
                    .unwrap_or_else(|| panic!("native dispatch retains typed refusal: {error:#}"));
                assert_eq!(refusal.operation(), CHILD);
                assert_eq!(
                    refusal.kind(),
                    if scenario == Scenario::FreshOnly {
                        OperationRefusalKind::FreshCredentialRequired
                    } else {
                        OperationRefusalKind::PermissionDenied
                    }
                );
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
        Scenario::Success | Scenario::Deadline | Scenario::Cancellation
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
        assert_eq!(claims.project.as_deref(), Some(PROJECT));
        assert_eq!(
            claims.release,
            fixture.request(deadline).acquisition.claims.release
        );
        let invocation = event.invocation.expect("host invocation");
        assert_eq!(
            invocation.origin,
            fixture.request(deadline).acquisition.invocation.origin,
            "nested execution preserves its distinct wiring owner and original root component"
        );
        assert_eq!(invocation.wiring_id, "trusted-wiring");
        assert_eq!(invocation.wiring_version, 1);
        assert_eq!(invocation.node_id, "trusted-node");
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

async fn prove() -> anyhow::Result<()> {
    let admin_url = std::env::var(URL_ENV).with_context(|| {
        format!("set {URL_ENV} to this proof's fresh disposable PostgreSQL 18 server")
    })?;
    let (mut server, route) = authentication_fixture(&admin_url).await?;
    let parent_only = authenticated(&route, false).await;
    let permitted = authenticated(&route, true).await;
    for scenario in SCENARIOS {
        prove_case(
            scenario,
            if scenario == Scenario::PermissionDenied {
                &parent_only
            } else {
                &permitted
            },
        )
        .await;
    }
    server.stop().await;
    Ok(())
}

#[test]
#[ignore = "requires WAMN_NATIVE_B_AUTH_PG_URL naming this proof's fresh disposable PostgreSQL 18 server"]
fn native_authenticated_nested_authority_and_lifecycle() {
    let full_name = format!("router_driver::native_policy::tests::authenticated::{TEST_NAME}");
    if std::env::var(CHILD_MARKER).as_deref() != Ok(TEST_NAME) {
        let output = Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", &full_name, "--include-ignored", "--nocapture"])
            .env(CHILD_MARKER, TEST_NAME)
            .output()
            .expect("start isolated authenticated native proof");
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
            "subprocess executed the named proof"
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
        for receipt in stdout
            .lines()
            .filter(|line| line.starts_with("authenticated-native-"))
        {
            println!("{receipt}");
        }
        return;
    }
    let (done, finished) = mpsc::channel();
    let watchdog = std::thread::spawn(move || {
        if finished.recv_timeout(Duration::from_secs(60)) == Err(mpsc::RecvTimeoutError::Timeout) {
            eprintln!("authenticated native proof exceeded its process watchdog");
            std::process::exit(124);
        }
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .event_interval(1)
        .enable_all()
        .build()
        .expect("isolated native runtime");
    runtime
        .block_on(prove())
        .expect("real authenticated native proof");
    drop(runtime);
    done.send(()).expect("finish watchdog");
    watchdog.join().expect("join watchdog");
}
