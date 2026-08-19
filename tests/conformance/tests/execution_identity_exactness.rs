//! The execution identity seam is EXACT at both ends (wamn-0h0g.17.9,
//! wamn-0h0g.17.10).
//!
//! `execution_pool_checkout_identity.rs` proves the seam is *isolating*: two
//! interleaved checkouts on one digest pool never cross-attribute. That argument
//! says nothing about the two exactness properties proved here, both of which
//! are about a SINGLE instance:
//!
//! 1. **Construction is complete** (wamn-0h0g.17.9). `ExecutionHost::instantiate`
//!    hands the SAME `project` to `ConnectionHttp` (which freezes it) and to the
//!    `wamn:postgres` claim registry (which the guest's own data path reads
//!    through `project_for`). If it registers only some of the identity tuple,
//!    the two halves of one process resolve DIFFERENT databases.
//!
//! 2. **Revocation is complete** (wamn-0h0g.17.10). Ending a checkout must leave
//!    the instance carrying no resolvable identity in ANY process-resident
//!    registry — `wamn:postgres`, `wasi:logging`, and the `wamn:runner/egress`
//!    declaration alike. A registry that is written at bind and not cleared at
//!    revoke leaves an idle pooled instance resolving the tenant it last served.
//!
//! Every instance here is built by `ExecutionHost::instantiate` — the one
//! production store constructor — so the registries asserted are the real ones
//! the RLS session, the log enrichment, and the outgoing-HTTP gate read.

use std::sync::Arc;
use std::time::Duration;

use wamn_execution_host::{
    ExecutionAcquisition, ExecutionHost, ExecutionIdentity, ExecutionInstancePool,
    ExecutionPoolKey, ExecutionPoolLimits, INVOCATIONS_PER_INSTANCE, InvocationDisposition,
    TrustedExecutionRuntimeRevision, production_capabilities,
};
use wamn_runtime::engine::build_engine;
use wamn_runtime::plugins::runner_egress::RunnerEgressPolicy;
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_logging::WamnLogging;
use wamn_runtime::plugins::wamn_postgres::{
    DEFAULT_PROJECT, SessionClaims, WamnPostgres, WamnPostgresConfig,
};

/// A component with the flowrunner's export signature and no imports.
///
/// The seam under test is host-side, so the guest only has to be a real
/// component the production linker can instantiate — every claim asserted here
/// is resolved by the host before the guest ever runs.
const GUEST: &str = r#"
    (component
        (core module $guest
            (memory (export "memory") 1)
            (global $next (mut i32) (i32.const 1024))
            (func $realloc
                (export "realloc")
                (param $old i32)
                (param $old-size i32)
                (param $align i32)
                (param $new-size i32)
                (result i32)
                (local $result i32)
                global.get $next
                local.tee $result
                local.get $new-size
                i32.add
                global.set $next
                local.get $result)
            (func (export "run")
                (param i32 i32 i32 i32)
                (result i32)
                i32.const 64
                i32.const 0
                i32.store
                i32.const 68
                i32.const 0
                i32.store
                i32.const 72
                i32.const 0
                i32.store
                i32.const 64)
            )
        (core instance $guest (instantiate $guest))
        (func (export "run")
            (param "run-id" string)
            (param "payload" string)
            (result (result u32 (error string)))
            (canon lift
                (core func $guest "run")
                (memory $guest "memory")
                (realloc (func $guest "realloc"))))
    )
"#;

/// A project name that is NOT the default, which is the whole reachability
/// condition of wamn-0h0g.17.9: a process whose `WAMN_PROJECT` equals the
/// default cannot observe the split, because the absent registration falls back
/// to exactly the value `ConnectionHttp` froze.
const OTHER_PROJECT: &str = "invoicing";

/// An offline plugin: no `database_url`, so no pool is ever built. Every
/// registry read here is process-resident and needs no server.
fn offline_postgres() -> Arc<WamnPostgres> {
    Arc::new(
        WamnPostgres::new(WamnPostgresConfig {
            database_url: None,
            pool_max_size: 1,
            wait_timeout_ms: 100,
            statement_timeout_ms: 100,
            row_limit: 10,
        })
        .expect("offline wamn:postgres plugin"),
    )
}

fn limits() -> ExecutionPoolLimits {
    ExecutionPoolLimits {
        max_instances: 2,
        max_reserved_bytes: usize::MAX,
        max_idle_per_digest: 2,
        max_invocations_per_instance: INVOCATIONS_PER_INSTANCE,
        max_idle_age: Duration::from_secs(60),
    }
}

/// Build one instance through the production constructor, under `project`.
async fn instantiate(
    engine: &wash_runtime::engine::Engine,
    bytes: &[u8],
    postgres: Arc<WamnPostgres>,
    logging: Arc<WamnLogging>,
    egress: Arc<RunnerEgressPolicy>,
    scope: &str,
    project: &str,
) -> ExecutionHost {
    ExecutionHost::instantiate(
        engine,
        bytes,
        postgres,
        Arc::new(WamnCredentials::empty()),
        logging,
        ExecutionIdentity {
            owner: scope,
            tenant: "construction-tenant",
            schema: None,
            project,
            org: None,
            environment: None,
            database: None,
        },
        production_capabilities(Default::default(), egress),
        None,
        40,
    )
    .await
    .expect("instantiate a flowrunner instance")
}

fn digest_key(bytes: &[u8]) -> ExecutionPoolKey {
    ExecutionPoolKey::new(
        TrustedExecutionRuntimeRevision::from_flowrunner_bytes(bytes).flowrunner_component_digest(),
    )
}

/// wamn-0h0g.17.9 — one process, one project, both paths.
///
/// `ExecutionHost::instantiate` freezes `identity.project` into `ConnectionHttp`
/// (which resolves the effect-authority snapshot and the credential vault under
/// it) and must register the SAME value on the `wamn:postgres` plugin, which is
/// what `project_for` — the guest's own data path — resolves. Registering the
/// tenant, schema and runner but not the project leaves `project_for` falling
/// back to `DEFAULT_PROJECT` while `ConnectionHttp` carries
/// `identity.project`: two databases inside one process.
///
/// The test is deliberately built on a NON-default project, because that is the
/// only configuration in which the defect is observable at all.
#[tokio::test]
async fn instantiate_registers_the_same_project_the_connection_effect_path_froze() {
    assert_ne!(
        OTHER_PROJECT, DEFAULT_PROJECT,
        "the split is unobservable when the process project IS the default, so a \
         test built on the default value would pass against the defect"
    );

    let engine = build_engine(&[]).expect("the production pooling engine");
    let bytes = wat::parse_str(GUEST).expect("encode the identity gate component");
    let postgres = offline_postgres();
    let logging = Arc::new(WamnLogging::from_env().expect("offline wasi:logging plugin"));

    let host = instantiate(
        &engine,
        &bytes,
        postgres.clone(),
        logging.clone(),
        Arc::new(RunnerEgressPolicy::default()),
        "instance-under-project",
        OTHER_PROJECT,
    )
    .await;
    let scope = host.claim_scope().to_string();

    let claims = postgres
        .session_claims(&scope)
        .expect("instantiate registers the construction identity");
    assert_eq!(
        claims.project.as_deref(),
        Some(OTHER_PROJECT),
        "the guest data path must resolve the SAME project ConnectionHttp froze; \
         an unregistered project silently resolves the default database instead"
    );

    // The logging claim is the third reader of the same value, and it is
    // resolved from `identity.project` directly — so an agreeing pair here is
    // what makes "one project per process" true of every reader, not just two.
    assert_eq!(
        logging.claim_snapshot(&scope),
        Some(("construction-tenant".to_string(), OTHER_PROJECT.to_string())),
        "the wasi:logging claim carries the same project as the data path"
    );
}

/// The `ConnectionHttp` half of wamn-0h0g.17.9, which no runtime assertion can
/// reach.
///
/// `ConnectionHttp` freezes its project into a private `Box<str>` and is moved
/// into the store's `Ctx.plugins` map, which is private to the fork — there is
/// no handle from outside the store to read it back. So the only way to assert
/// that the value registered on the plugin and the value frozen into
/// `ConnectionHttp` are the SAME value is to assert that `instantiate` passes
/// the same binding to both, on the one source section that constructs them.
///
/// Kills the mutant the runtime test above cannot: registering
/// `identity.project` on the plugin while handing `DEFAULT_PROJECT` (or any
/// other expression) to `ConnectionHttp::new`, which re-opens the split with the
/// registry now on the other side of it.
#[test]
fn instantiate_hands_one_project_binding_to_the_registry_and_to_connection_http() {
    let host = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("the conformance package lives at tests/conformance")
            .join("crates/execution/host/src/lib.rs"),
    )
    .expect("read the execution host source");

    let (_, body) = host
        .split_once("    pub async fn instantiate(")
        .expect("execution host declares instantiate");
    let instantiate = body
        .split_once("    pub fn claim_scope(&self)")
        .map(|(section, _)| section)
        .expect("instantiate ends before claim_scope");

    assert!(
        instantiate.contains("plugin.set_project(owner, project)?;"),
        "instantiate must register the identity's project on the wamn:postgres \
         plugin, which is what the guest data path resolves"
    );

    let connection_http = instantiate
        .split_once("ConnectionHttp::new(")
        .map(|(_, rest)| {
            rest.split_once(");")
                .map(|(args, _)| args)
                .expect("the ConnectionHttp::new call is closed")
        })
        .expect("instantiate constructs the trusted HTTP effect");
    assert!(
        connection_http
            .lines()
            .any(|line| line.trim() == "project,"),
        "ConnectionHttp must freeze the SAME `project` binding the plugin was \
         registered with, not a separately resolved value: {connection_http}"
    );
}

/// wamn-0h0g.17.10 — ending a checkout leaves NOTHING resolvable.
///
/// Three process-resident registries are keyed by the claim scope and are
/// identity-derived: the `wamn:postgres` claims, the `wasi:logging`
/// `(tenant, project)` claim, and the `wamn:runner/egress` declaration the
/// trusted runner supplies for the run it drives. `revoke_identity` clearing
/// only the first leaves an idle pooled instance still resolving a logging
/// claim and an egress allowlist for the tenant it last served —
/// overwritten-on-next-bind rather than removed.
#[tokio::test]
async fn ending_a_checkout_revokes_every_registry_the_instance_resolved_under() {
    let engine = build_engine(&[]).expect("the production pooling engine");
    let bytes = wat::parse_str(GUEST).expect("encode the identity gate component");
    let postgres = offline_postgres();
    let logging = Arc::new(WamnLogging::from_env().expect("offline wasi:logging plugin"));
    let egress = Arc::new(RunnerEgressPolicy::default());

    let pool = ExecutionInstancePool::new(limits()).expect("valid pool limits");
    let key = digest_key(&bytes);
    let host = instantiate(
        &engine,
        &bytes,
        postgres.clone(),
        logging.clone(),
        egress.clone(),
        "instance-to-revoke",
        OTHER_PROJECT,
    )
    .await;
    let scope = host.claim_scope().to_string();
    pool.insert(key.clone(), host)
        .expect("prewarm one instance");

    let lease = pool
        .checkout(
            &key,
            &ExecutionAcquisition::untraced(SessionClaims {
                tenant: "tenant-a".to_string(),
                project: Some("tenant-a-project".to_string()),
                schema: Some("schema_a".to_string()),
                runner: Some("runner-a".to_string()),
                role: None,
                user_id: None,
                release: None,
            }),
        )
        .expect("tenant A binds")
        .expect("a warm instance is available");

    // The run this checkout serves declares its resolved egress through the
    // trusted `wamn:runner/egress` channel, which writes THIS registry under
    // THIS claim scope. Written here directly because the channel is only
    // reachable from inside a guest call, and the registry is the subject.
    egress.set_declared(&scope, &["effects.example.com".to_string()]);

    assert_eq!(
        postgres
            .session_claims(&scope)
            .expect("bound during the checkout")
            .tenant,
        "tenant-a"
    );
    assert_eq!(
        logging.claim_snapshot(&scope),
        Some(("tenant-a".to_string(), "tenant-a-project".to_string()))
    );
    assert!(
        egress.declared(&scope).is_some(),
        "the run declared an egress set under this claim scope"
    );

    drop(lease);

    assert_eq!(
        postgres.session_claims(&scope),
        None,
        "the postgres claims are revoked when the checkout ends"
    );
    assert_eq!(
        logging.claim_snapshot(&scope),
        None,
        "an idle instance must not still resolve the logging claim of the tenant \
         it last served — overwritten-on-next-bind is not revocation"
    );
    assert_eq!(
        egress.declared(&scope),
        None,
        "an idle instance must not still resolve the egress allowlist of the run \
         it last served"
    );
}

/// The same exactness on the path a lease takes when its disposition is
/// declared rather than dropped, so the clear is not attached to one exit only.
#[tokio::test]
async fn a_finished_lease_revokes_every_registry_too() {
    let engine = build_engine(&[]).expect("the production pooling engine");
    let bytes = wat::parse_str(GUEST).expect("encode the identity gate component");
    let postgres = offline_postgres();
    let logging = Arc::new(WamnLogging::from_env().expect("offline wasi:logging plugin"));
    let egress = Arc::new(RunnerEgressPolicy::default());

    let pool = ExecutionInstancePool::new(limits()).expect("valid pool limits");
    let key = digest_key(&bytes);
    let host = instantiate(
        &engine,
        &bytes,
        postgres.clone(),
        logging.clone(),
        egress.clone(),
        "instance-to-finish",
        OTHER_PROJECT,
    )
    .await;
    let scope = host.claim_scope().to_string();
    pool.insert(key.clone(), host)
        .expect("prewarm one instance");

    let lease = pool
        .checkout(
            &key,
            &ExecutionAcquisition::untraced(SessionClaims {
                tenant: "tenant-b".to_string(),
                project: Some("tenant-b-project".to_string()),
                schema: None,
                runner: None,
                role: None,
                user_id: None,
                release: None,
            }),
        )
        .expect("tenant B binds")
        .expect("a warm instance is available");
    egress.set_declared(&scope, &["effects.example.com".to_string()]);

    lease
        .finish(InvocationDisposition::Reusable)
        .expect("this instance is destroyed at the end of its checkout");

    assert_eq!(postgres.session_claims(&scope), None);
    assert_eq!(logging.claim_snapshot(&scope), None);
    assert_eq!(egress.declared(&scope), None);
}
