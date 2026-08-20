//! Adversarial cross-tenant proof for the checkout-time identity seam
//! (wamn-0h0g.17.7).
//!
//! `execution_pool_fresh_instance.rs` proves what a second checkout sees of the
//! previous checkout's GUEST state. That argument rests on
//! `INVOCATIONS_PER_INSTANCE`, and it says nothing about the IDENTITY a warm
//! instance carries — reuse being off does not stop a store built for tenant A
//! from being handed to tenant B on its first and only checkout.
//!
//! This file proves the other half, on the production types: two tenants, ONE
//! digest pool, INTERLEAVED checkouts. A sequential pair would pass against an
//! implementation that bound identity once at construction and never rebound it,
//! so both leases are held live at the same instant and each is required to
//! resolve to its own claims while the other is still checked out.
//!
//! Every instance here is built by `ExecutionHost::instantiate` — the one
//! production store constructor — so the claims asserted are the real
//! `wamn:postgres` and `wasi:logging` registries the RLS session reads, not a
//! stand-in.

use std::sync::Arc;
use std::time::Duration;

use wamn_execution_host::{
    ExecutionAcquisition, ExecutionHost, ExecutionIdentity, ExecutionInstancePool,
    ExecutionPoolKey, ExecutionPoolLimits, INVOCATIONS_PER_INSTANCE, InvocationDisposition,
    RetirementReason, TrustedExecutionRuntimeRevision, production_capabilities,
};
use wamn_runtime::engine::build_engine;
use wamn_runtime::plugins::runner_egress::RunnerEgressPolicy;
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_logging::WamnLogging;
use wamn_runtime::plugins::wamn_postgres::{SessionClaims, WamnPostgres, WamnPostgresConfig};

/// A component with the flowrunner's export signature and no imports.
///
/// The seam under test is host-side, so the guest only has to be a real
/// component the production linker can instantiate — the claims it would read
/// are resolved by the host before the guest ever runs.
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

/// Room for two prewarmed instances under one key, on the shipped reuse policy.
fn limits() -> ExecutionPoolLimits {
    ExecutionPoolLimits {
        max_instances: 2,
        max_reserved_bytes: usize::MAX,
        max_idle_per_digest: 2,
        max_invocations_per_instance: INVOCATIONS_PER_INSTANCE,
        max_idle_age: Duration::from_secs(60),
    }
}

/// An offline plugin: no `database_url`, so no pool is ever built. The claim
/// registries this proof reads are process-resident and need no server.
fn offline_postgres() -> Arc<WamnPostgres> {
    Arc::new(
        WamnPostgres::new(WamnPostgresConfig {
            database_url: None,
            guest_pool_max_size: 1,
            platform_pool_max_size: 1,
            wait_timeout_ms: 100,
            statement_timeout_ms: 100,
            row_limit: 10,
        })
        .expect("offline wamn:postgres plugin"),
    )
}

/// Prewarm one instance through the production constructor.
///
/// `scope` is the instance's own claim scope — the id its host-side claims are
/// registered under. It is deliberately NOT a tenant: the whole point of the
/// seam is that a warm instance is fungible compute whose identity arrives at
/// checkout. The identity passed here is the placeholder a prewarm has, and
/// every element of it is expected to be replaced before the instance serves
/// anyone.
async fn prewarm(
    engine: &wash_runtime::engine::Engine,
    bytes: &[u8],
    postgres: Arc<WamnPostgres>,
    logging: Arc<WamnLogging>,
    scope: &str,
) -> ExecutionHost {
    ExecutionHost::instantiate(
        engine,
        bytes,
        postgres,
        Arc::new(WamnCredentials::empty()),
        logging,
        ExecutionIdentity {
            owner: scope,
            tenant: "prewarm-placeholder",
            schema: None,
            project: "prewarm-project",
            org: None,
            environment: None,
            database: None,
        },
        production_capabilities(Default::default(), Arc::new(RunnerEgressPolicy::default())),
        None,
        40,
    )
    .await
    .expect("prewarm a flowrunner instance")
}

/// The trace-parent element of the identity tuple is proved by the execution
/// host's own unit tests, where a subscriber can observe which span a guest call
/// runs inside; here the claims are the subject, so the acquisitions are
/// untraced.
fn acquisition(tenant: &str, schema: &str, runner: &str) -> ExecutionAcquisition {
    ExecutionAcquisition::untraced(SessionClaims {
        tenant: tenant.to_string(),
        project: Some(format!("{tenant}-project")),
        schema: Some(schema.to_string()),
        runner: Some(runner.to_string()),
        role: Some("inspector".to_string()),
        user_id: Some("11111111-1111-4111-8111-111111111111".to_string()),
        release: None,
    })
}

/// The pool key is the digest of the exact component bytes, so two instances
/// built from one component share ONE pool — which is the whole of the
/// cross-wiring amortization the rekey bought, and the whole of the exposure.
fn digest_key(bytes: &[u8]) -> ExecutionPoolKey {
    ExecutionPoolKey::new(
        TrustedExecutionRuntimeRevision::from_flowrunner_bytes(bytes).flowrunner_component_digest(),
    )
}

/// Two tenants, one digest pool, interleaved checkouts, no cross-bleed.
///
/// The order is A-checkout, B-checkout, A-acts, B-acts, A-returns, B-returns.
/// Both assertions about A's claims are made while B's lease is live and vice
/// versa, so an implementation that resolved identity from anything other than
/// the acquisition — the store's construction-time identity, or a single
/// process-resident registry entry the second acquisition overwrote — fails
/// here. Deleting the `bind_identity` call from `ExecutionInstancePool::checkout`
/// leaves both leases resolving the prewarm placeholder and fails the first
/// assertion.
#[tokio::test]
async fn interleaved_two_tenant_checkouts_on_one_digest_pool_never_cross_attribute() {
    let engine = build_engine(&[]).expect("the production pooling engine");
    let bytes = wat::parse_str(GUEST).expect("encode the identity gate component");
    let postgres = offline_postgres();
    let logging = Arc::new(WamnLogging::from_env().expect("offline wasi:logging plugin"));

    let pool = ExecutionInstancePool::new(limits()).expect("valid pool limits");
    let key = digest_key(&bytes);
    let mut scopes = Vec::new();
    for scope in ["warm-instance-0", "warm-instance-1"] {
        let host = prewarm(&engine, &bytes, postgres.clone(), logging.clone(), scope).await;
        scopes.push(host.claim_scope().to_string());
        pool.insert(key.clone(), host)
            .expect("prewarm one instance under the shared digest key");
    }

    // A acquires first.
    let first = pool
        .checkout(&key, &acquisition("tenant-a", "schema_a", "runner-a"))
        .expect("tenant A binds")
        .expect("a warm instance is available");
    // B acquires from the SAME key while A still holds its lease.
    let second = pool
        .checkout(&key, &acquisition("tenant-b", "schema_b", "runner-b"))
        .expect("tenant B binds")
        .expect("the same digest key still has a warm instance");

    let a_scope = first.instance().claim_scope().to_string();
    let b_scope = second.instance().claim_scope().to_string();
    assert_ne!(
        a_scope, b_scope,
        "two concurrently checked-out instances must not share one claim scope, \
         or their registries collide instead of isolating"
    );

    // A acts: every claim its RLS session would read is its own.
    let a_claims = postgres
        .session_claims(&a_scope)
        .expect("tenant A's instance resolves a bound identity");
    assert_eq!(a_claims.tenant, "tenant-a");
    assert_eq!(a_claims.project.as_deref(), Some("tenant-a-project"));
    assert_eq!(a_claims.schema.as_deref(), Some("schema_a"));
    assert_eq!(a_claims.runner.as_deref(), Some("runner-a"));
    assert_eq!(
        logging.claim_snapshot(&a_scope),
        Some(("tenant-a".to_string(), "tenant-a-project".to_string())),
        "the wasi:logging claim enriches A's records with A's identity"
    );

    // B acts, with A still checked out.
    let b_claims = postgres
        .session_claims(&b_scope)
        .expect("tenant B's instance resolves a bound identity");
    assert_eq!(
        b_claims.tenant, "tenant-b",
        "the second acquisition on one digest pool must not inherit the first's tenant"
    );
    assert_eq!(b_claims.project.as_deref(), Some("tenant-b-project"));
    assert_eq!(b_claims.schema.as_deref(), Some("schema_b"));
    assert_eq!(b_claims.runner.as_deref(), Some("runner-b"));
    assert_eq!(
        logging.claim_snapshot(&b_scope),
        Some(("tenant-b".to_string(), "tenant-b-project".to_string()))
    );

    // A is STILL A after B bound: the two acquisitions are simultaneously live
    // and neither has overwritten the other.
    assert_eq!(
        postgres
            .session_claims(&a_scope)
            .expect("A is still bound")
            .tenant,
        "tenant-a",
        "B's checkout must not have rewritten A's tenant while A holds its lease"
    );

    // A returns, then B returns.
    first
        .finish(InvocationDisposition::Reusable)
        .expect("A is destroyed at the end of its checkout, so no reset is attempted");
    assert_eq!(
        postgres.session_claims(&a_scope),
        None,
        "a destroyed instance resolves no tenant at all, so nothing can be read under it"
    );
    assert_eq!(
        postgres
            .session_claims(&b_scope)
            .expect("B is still bound")
            .tenant,
        "tenant-b",
        "ending A's checkout must not revoke B's still-live identity"
    );

    second
        .finish(InvocationDisposition::Reusable)
        .expect("B is destroyed at the end of its checkout too");
    assert_eq!(postgres.session_claims(&b_scope), None);

    let snapshot = pool.snapshot();
    assert_eq!(snapshot.live_instances, 0);
    assert_eq!(
        snapshot.retirements.get(&RetirementReason::MaxInvocations),
        Some(&2),
        "reuse is off: neither instance survives the checkout it served"
    );
    assert_eq!(scopes.len(), 2);
}

/// A prewarmed instance that was never acquired resolves NO tenant.
///
/// This is the fail-closed half: the seam is not only "the right identity is
/// bound at checkout" but "no identity is resolvable without one". An instance
/// that skipped a rebind cannot fall back on whatever it was constructed with,
/// because `require_tenant` has nothing to return.
#[tokio::test]
async fn an_idle_instance_carries_no_resolvable_identity_once_a_checkout_has_ended() {
    let engine = build_engine(&[]).expect("the production pooling engine");
    let bytes = wat::parse_str(GUEST).expect("encode the identity gate component");
    let postgres = offline_postgres();
    let logging = Arc::new(WamnLogging::from_env().expect("offline wasi:logging plugin"));

    let mut reusable = limits();
    reusable.max_invocations_per_instance = 8;
    let pool = ExecutionInstancePool::new(reusable).expect("valid pool limits");
    let key = digest_key(&bytes);
    let host = prewarm(
        &engine,
        &bytes,
        postgres.clone(),
        logging.clone(),
        "warm-instance-idle",
    )
    .await;
    let scope = host.claim_scope().to_string();
    pool.insert(key.clone(), host)
        .expect("prewarm one instance");

    let lease = pool
        .checkout(&key, &acquisition("tenant-a", "schema_a", "runner-a"))
        .expect("tenant A binds")
        .expect("a warm instance is available");
    assert_eq!(
        postgres
            .session_claims(&scope)
            .expect("bound during the checkout")
            .tenant,
        "tenant-a"
    );

    // The reset failure is what actually destroys this instance; the point here
    // is that the identity is gone either way, before the disposition is even
    // decided.
    lease
        .finish(InvocationDisposition::Reusable)
        .expect_err("a component instance cannot be reset in place from the host");
    assert_eq!(
        postgres.session_claims(&scope),
        None,
        "a checkout that ended in a failed reset still revoked its identity"
    );
    assert_eq!(
        pool.snapshot()
            .retirements
            .get(&RetirementReason::CleanupFailed),
        Some(&1)
    );
}
