//! Construction of an execution instance registers its project EXACTLY
//! (wamn-0h0g.17.9).
//!
//! `execution_pool_checkout_identity.rs` proves the checkout seam is
//! *isolating*: two interleaved checkouts on one digest pool never
//! cross-attribute. That argument says nothing about a SINGLE instance's
//! construction, which is what this proves.
//!
//! `ExecutionHost::instantiate` hands the SAME `project` to `ConnectionHttp`
//! (which freezes it) and to the `wamn:postgres` claim registry (which the
//! guest's own data path reads through `project_for`). If it registers only
//! some of the identity tuple, the two halves of one process resolve DIFFERENT
//! databases.
//!
//! Every instance here is built by `ExecutionHost::instantiate` — the one
//! production store constructor — so the registries asserted are the real ones
//! the RLS session and the log enrichment read.

use std::sync::Arc;

use wamn_execution_host::{ExecutionHost, ExecutionIdentity, production_capabilities};
use wamn_runtime::engine::build_engine;
use wamn_runtime::plugins::runner_egress::RunnerEgressPolicy;
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_logging::WamnLogging;
use wamn_runtime::plugins::wamn_postgres::{DEFAULT_PROJECT, WamnPostgres, WamnPostgresConfig};

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
