//! Native mechanism tests use the real node ABI and a test-only observation import.
//! Authenticated call-graph cases use the explicitly armed local PostgreSQL test below.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::{Instant, timeout};
use tracing_subscriber::layer::SubscriberExt as _;
use wamn_catalog::{
    AdmittedComponent, AdmittedComponentOperation, ComponentOperationDependency,
    ComponentPackageScope, ComponentSqlField, ComponentSqlStatement, ComponentSqlValueType,
    EffectiveReleaseId, PackageCoordinate, SERVING_MANIFEST_FORMAT_VERSION, ServingComponent,
    ServingManifest, ServingRelease,
};
use wamn_engine::component_admission::component_digest;
use wamn_engine::release_manifest::LoadedRelease;
use wamn_engine::router_delivery::{OperationRefusal, OperationRefusalKind};
use wamn_project_state::PlatformComponent;
use wamn_runtime::plugins::connection_http::transport::HttpTransport;
use wamn_runtime::plugins::connection_http::{
    ConnectionExecutionClosure, ConnectionHttp, ConnectionInvocation, ConnectionOrigin,
};
use wamn_runtime::plugins::wamn_blobstore::plugin::WamnBlobstore;
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_logging::{WamnLogging, WamnLoggingConfig};
use wamn_runtime::plugins::wamn_postgres::{
    ReleaseIdentity, SessionClaims, StaticCredentialProvider, WAMN_POSTGRES_ID, WamnPostgres,
};
use wash_runtime::engine::Engine;
use wash_runtime::engine::ctx::{SharedCtx, extract_active_ctx};
use wash_runtime::engine::dispatch::DispatchTarget;
use wash_runtime::engine::workload::{ResolvedWorkload, WorkloadItem};
use wash_runtime::host::HostRef;
use wash_runtime::host::http::{HostHandler, NullServer};
use wash_runtime::observability::{MeterKind, Meters};
use wash_runtime::plugin::{HostPlugin, PluginBindings, WitInterfaces};
use wash_runtime::types::{Component, LocalResources, Workload};
use wash_runtime::wit::{WitInterface, WitWorld};

use super::super::NodeAcquisition;
use super::super::native_call::{NativeInvocation, invoke_native, prepare_native};
use super::super::native_workload::{
    NativeApplication, NativeComponent, NativeWorkload, NativeWorkloadSpec, load_native_application,
};
use super::{
    NATIVE_POLICY_ID, NativeFacts, NativePolicy, NativePolicyResources, new_native_policy,
};
use wamn_engine::operation::node_types;

#[path = "tests/authenticated.rs"]
mod authenticated;
#[path = "tests/trace.rs"]
mod trace;
#[path = "tests/warm.rs"]
mod warm;

const ROOT: &str = "root:entry/run@1.0.0";
/// An operation that the root's call graph reaches. Publish folds its grant
/// into the root. No host runs it: composition embeds it in the root.
const CHILD: &str = "child:entry/run@1.0.0";
/// The participant that the composed transaction owner selects.
const PARTICIPANT: &str = "root:entry/participant@1.0.0";
const OBSERVE: &str = "test:authority/observe@1.0.0";
const STATEMENTS: &str = "wamn:postgres/statements@0.1.0";
const CHILD_MARKER: &str = "WAMN_NATIVE_POLICY_CHILD";
const BUDGET: Duration = Duration::from_millis(200);
const CLEANUP: Duration = Duration::from_secs(2);

// The structural types and indirect forwarding match the native async
// participation fixtures below. No scalar substitute is used.
const NODE_TYPES: &str = r#"
      (import "wamn:node/types@0.1.0" (instance $node
        (type $json' string)
        (export "json" (type $json (eq $json')))
        (type $context' (record
          (field "wiring-id" string) (field "wiring-version" u32)
          (field "node-id" string) (field "delivery-id" string)
          (field "input-port" (option string)) (field "occurrence" u32)
          (field "traceparent" (option string)) (field "tracestate" (option string))
          (field "deadline-ms" (option u64)) (field "config" $json)))
        (export "node-context" (type $context (eq $context')))
        (type $detail' (record (field "message" string) (field "code" (option string))))
        (export "error-detail" (type $detail (eq $detail')))
        (type $rate' (record (field "detail" $detail) (field "retry-after-ms" (option u64))))
        (export "rate-limit-detail" (type $rate (eq $rate')))
        (type $error' (variant (case "retryable" $detail) (case "rate-limited" $rate)
          (case "terminal" $detail) (case "invalid-input" $detail) (case "cancelled")))
        (export "node-error" (type $error (eq $error')))
        (type $emission' (record (field "payload" $json) (field "port" (option string))))
        (export "emission" (type $emission (eq $emission')))))
      (alias export $node "json" (type $json))
      (alias export $node "node-context" (type $context))
      (alias export $node "node-error" (type $error))
      (alias export $node "emission" (type $emission))
"#;

#[derive(Debug, Clone, Copy)]
enum Case {
    Success,
    CalleeGrant,
    StartDeadline,
    RunDeadline,
    Cancellation,
    Trap,
    PostgresImport,
    TransactionOwner,
    TransactionParticipant,
}

fn component_bytes(operation: &str, case: Case) -> Vec<u8> {
    if matches!(case, Case::TransactionOwner) {
        return transaction_owner_component(operation);
    }
    if matches!(case, Case::TransactionParticipant) {
        return transaction_participant_component(operation);
    }
    let postgres = if matches!(case, Case::PostgresImport) {
        format!(r#"(import "{STATEMENTS}" (instance))"#)
    } else {
        String::new()
    };
    let start = if matches!(case, Case::StartDeadline) {
        "(loop br 0)"
    } else {
        ""
    };
    let body = match case {
        Case::RunDeadline | Case::Cancellation => "(loop br 0)",
        Case::Trap => "unreachable",
        Case::Success | Case::CalleeGrant | Case::StartDeadline | Case::PostgresImport => "",
        Case::TransactionOwner | Case::TransactionParticipant => unreachable!(),
    };
    wat::parse_str(format!(
        r#"(component
      {NODE_TYPES}
      (import "{OBSERVE}" (instance $observe
        (export "record" (func (param "phase" u32)))))
      {postgres}
      (core module $memory
        (memory (export "memory") 16)
        (global $next (mut i32) (i32.const 1024))
        (func (export "realloc") (param $old i32) (param $old-size i32)
          (param $align i32) (param $size i32) (result i32) (local $new i32)
          global.get $next local.get $align i32.const 1 i32.sub i32.add
          i32.const 0 local.get $align i32.sub i32.and local.tee $new
          local.get $size i32.add global.set $next
          global.get $next i32.const 1048576 i32.gt_u if unreachable end
          local.get $old if
            local.get $new local.get $old local.get $old-size memory.copy
          end local.get $new))
      (core instance $memory (instantiate $memory))
      (core func $observe (canon lower (func $observe "record")))
      (core func $return (canon task.return (result (result $emission (error $error)))
        (memory $memory "memory")))
      (core module $main
        (import "memory" "memory" (memory 16))
        (import "host" "observe" (func $observe (param i32)))
        (import "host" "return" (func $return
          (param i32 i32 i32 i32 i32 i32 i32 i32 i64)))
        (func (export "callback") (param i32 i32 i32) (result i32) unreachable)
        (func $start i32.const 0 call $observe {start})
        (start $start)
        (func (export "run") (param $input i32) (result i32)
          i32.const 1 call $observe
          {body}
          i32.const 0 local.get $input i32.load offset=96 local.get $input i32.load offset=100
          i32.const 0 i32.const 0 i32.const 0 i32.const 0 i32.const 0 i64.const 0
          call $return i32.const 0))
      (core instance $main (instantiate $main (with "memory" (instance $memory))
        (with "host" (instance (export "observe" (func $observe))
          (export "return" (func $return))))))
      (func $run async (param "ctx" $context) (param "input" $json)
        (result (result $emission (error $error)))
        (canon lift (core func $main "run") (memory $memory "memory")
          (realloc (func $memory "realloc")) async (callback (func $main "callback"))))
      (instance $handler
        (export "json" (type $json)) (export "node-context" (type $context))
        (export "node-error" (type $error)) (export "emission" (type $emission))
        (export "run" (func $run)))
      (export "{operation}" (instance $handler)))"#
    ))
    .expect("encode the complete node ABI fixture")
}

const POSTGRES_TYPES: &str = r#"
      (import "wamn:postgres/types@0.1.0" (instance $types
        (type $sql-value' (variant (case "null") (case "boolean" bool) (case "int32" s32)
          (case "int64" s64) (case "float64" f64) (case "text" string) (case "bytes" (list u8))
          (case "numeric" string) (case "timestamptz" string) (case "json" string) (case "uuid" string)))
        (export "sql-value" (type $sql-value (eq $sql-value')))
        (type $column' (record (field "name" string) (field "type-name" string)))
        (export "column" (type $column (eq $column')))
        (type $row-set' (record (field "columns" (list $column)) (field "rows" (list (list $sql-value)))))
        (export "row-set" (type $row-set (eq $row-set')))
        (type $pg-error' (variant (case "serialization-failure") (case "connection-unavailable")
          (case "statement-timeout") (case "row-limit-exceeded" u64) (case "unique-violation" string)
          (case "foreign-key-violation" string) (case "check-violation" string)
          (case "exclusion-violation" string) (case "permission-denied")
          (case "query-error" (tuple string string))))
        (export "pg-error" (type $pg-error (eq $pg-error')))))
      (alias export $types "sql-value" (type $sql-value))
      (alias export $types "row-set" (type $row-set))
      (alias export $types "pg-error" (type $pg-error))
      (import "wamn:postgres/statements@0.1.0" (instance $statements
        (type $contract-part' (enum "binds" "columns"))
        (export "contract-part" (type $contract-part (eq $contract-part')))
        (type $value-shape' (record (field "count" u32) (field "types" (list string))))
        (export "value-shape" (type $value-shape (eq $value-shape')))
        (type $contract-mismatch' (record (field "statement-digest" string) (field "part" $contract-part)
          (field "expected" $value-shape) (field "observed" $value-shape)))
        (export "contract-mismatch" (type $contract-mismatch (eq $contract-mismatch')))
        (type $statement-error' (variant (case "unknown-statement" string)
          (case "statement-contract-mismatch" $contract-mismatch) (case "postgres" $pg-error)))
        (export "statement-error" (type $statement-error (eq $statement-error')))
        (export "transaction" (type $transaction (sub resource)))
        (export "transaction-view" (type $transaction-view (sub resource)))
        (export "begin" (func async (result (result (own $transaction) (error $statement-error)))))
        (export "[method]transaction.select-participant" (func async
          (param "self" (borrow $transaction)) (param "participant-operation" string)
          (result (result (error $statement-error)))))
        (export "[method]transaction.rollback" (func async
          (param "self" (borrow $transaction)) (result (result (error $statement-error)))))
        (export "participant-view" (func async
          (result (result (own $transaction-view) (error $statement-error)))))
        (export "[method]transaction-view.run" (func async (param "self" (borrow $transaction-view))
          (param "statement-digest" string) (param "binds" (list $sql-value))
          (result (result $row-set (error $statement-error)))))))
"#;

fn transaction_participant_component(operation: &str) -> Vec<u8> {
    wat::parse_str(format!(r#"(component
      {NODE_TYPES}{POSTGRES_TYPES}
      (core module $memory
        (memory (export "memory") 16)
        (data (i32.const 16) "{digest}")
        (data (i32.const 320) "[]")
        (data (i32.const 400) "viewrun")
        (global $next (mut i32) (i32.const 1024))
        (func (export "realloc") (param i32 i32) (param $align i32) (param $size i32) (result i32)
          (local $ptr i32) global.get $next local.get $align i32.const 1 i32.sub i32.add
          i32.const 0 local.get $align i32.sub i32.and local.tee $ptr
          local.get $size i32.add global.set $next local.get $ptr))
      (core instance $memory (instantiate $memory))
      (core func $view (canon lower (func $statements "participant-view")
        (memory $memory "memory") (realloc (func $memory "realloc"))))
      (core func $run-view (canon lower (func $statements "[method]transaction-view.run")
        (memory $memory "memory") (realloc (func $memory "realloc"))))
      (core func $return (canon task.return (result (result $emission (error $error)))
        (memory $memory "memory")))
      (core module $main
        (import "memory" "memory" (memory 16))
        (import "host" "view" (func $view (param i32)))
        (import "host" "run-view" (func $run-view (param i32 i32 i32 i32 i32 i32)))
        (import "host" "return" (func $return
          (param i32 i32 i32 i32 i32 i32 i32 i32 i64)))
        (func (export "callback") (param i32 i32 i32) (result i32) unreachable)
        (func (export "run") (param i32) (result i32)
          i32.const 64 call $view
          i32.const 64 i32.load8_u if
            i32.const 0 i32.const 400 i32.const 4 i32.const 0 i32.const 0 i32.const 0
            i32.const 0 i32.const 0 i64.const 0 call $return i32.const 0 return end
          i32.const 72 i32.load i32.const 16 i32.const {digest_len} i32.const 0 i32.const 0 i32.const 128 call $run-view
          i32.const 128 i32.load8_u if
            i32.const 0 i32.const 404 i32.const 3 i32.const 0 i32.const 0 i32.const 0
            i32.const 0 i32.const 0 i64.const 0 call $return i32.const 0 return end
          i32.const 0 i32.const 320 i32.const 2 i32.const 0 i32.const 0 i32.const 0
          i32.const 0 i32.const 0 i64.const 0 call $return i32.const 0))
      (core instance $main (instantiate $main (with "memory" (instance $memory))
        (with "host" (instance (export "view" (func $view)) (export "run-view" (func $run-view))
          (export "return" (func $return))))))
      (func $run async (param "ctx" $context) (param "input" $json)
        (result (result $emission (error $error)))
        (canon lift (core func $main "run") (memory $memory "memory")
          (realloc (func $memory "realloc")) async (callback (func $main "callback"))))
      (instance $handler (export "json" (type $json)) (export "node-context" (type $context))
        (export "node-error" (type $error)) (export "emission" (type $emission))
        (export "run" (func $run)))
      (export "{operation}" (instance $handler)))"#,
      digest = TRANSACTION_SQL_DIGEST,
      digest_len = TRANSACTION_SQL_DIGEST.len(),
    )).expect("encode transaction participant")
}

/// A composed transaction owner: the base half begins a transaction and
/// selects the published participant, and the participant half, embedded in
/// the same component, runs its statement on the view. No host sits between.
fn transaction_owner_component(operation: &str) -> Vec<u8> {
    wat::parse_str(format!(r#"(component
      {NODE_TYPES}{POSTGRES_TYPES}
      (core module $memory
        (memory (export "memory") 16) (data (i32.const 16) "{PARTICIPANT}")
        (data (i32.const 320) "[]")
        (data (i32.const 480) "{digest}")
        (data (i32.const 600) "viewrun")
        (global $next (mut i32) (i32.const 1024))
        (func (export "realloc") (param i32 i32) (param $align i32) (param $size i32) (result i32)
          (local $ptr i32) global.get $next local.get $align i32.const 1 i32.sub i32.add
          i32.const 0 local.get $align i32.sub i32.and local.tee $ptr
          local.get $size i32.add global.set $next local.get $ptr))
      (core instance $memory (instantiate $memory))
      (core func $begin (canon lower (func $statements "begin") (memory $memory "memory") (realloc (func $memory "realloc"))))
      (core func $select (canon lower (func $statements "[method]transaction.select-participant") (memory $memory "memory") (realloc (func $memory "realloc"))))
      (core func $rollback (canon lower (func $statements "[method]transaction.rollback") (memory $memory "memory") (realloc (func $memory "realloc"))))
      (core func $view (canon lower (func $statements "participant-view") (memory $memory "memory") (realloc (func $memory "realloc"))))
      (core func $run-view (canon lower (func $statements "[method]transaction-view.run") (memory $memory "memory") (realloc (func $memory "realloc"))))
      (core func $return (canon task.return (result (result $emission (error $error)))
        (memory $memory "memory")))
      (core module $main
        (import "memory" "memory" (memory 16))
        (import "host" "begin" (func $begin (param i32))) (import "host" "select" (func $select (param i32 i32 i32 i32)))
        (import "host" "rollback" (func $rollback (param i32 i32)))
        (import "host" "view" (func $view (param i32)))
        (import "host" "run-view" (func $run-view (param i32 i32 i32 i32 i32 i32)))
        (import "host" "return" (func $return
          (param i32 i32 i32 i32 i32 i32 i32 i32 i64)))
        (func (export "callback") (param i32 i32 i32) (result i32) unreachable)
        (func (export "run") (param $input i32) (result i32) (local $transaction i32)
          i32.const 64 call $begin i32.const 64 i32.load8_u if unreachable end
          i32.const 72 i32.load local.tee $transaction i32.const 16 i32.const {participant_len} i32.const 96 call $select
          i32.const 96 i32.load8_u if unreachable end
          i32.const 128 call $view
          i32.const 128 i32.load8_u if
            i32.const 0 i32.const 600 i32.const 4 i32.const 0 i32.const 0 i32.const 0
            i32.const 0 i32.const 0 i64.const 0 call $return i32.const 0 return end
          i32.const 136 i32.load i32.const 480 i32.const {digest_len} i32.const 0 i32.const 0 i32.const 160 call $run-view
          i32.const 160 i32.load8_u if
            i32.const 0 i32.const 604 i32.const 3 i32.const 0 i32.const 0 i32.const 0
            i32.const 0 i32.const 0 i64.const 0 call $return i32.const 0 return end
          local.get $transaction i32.const 704 call $rollback
          i32.const 0 i32.const 320 i32.const 2 i32.const 0 i32.const 0 i32.const 0
          i32.const 0 i32.const 0 i64.const 0 call $return i32.const 0))
      (core instance $main (instantiate $main (with "memory" (instance $memory))
        (with "host" (instance (export "begin" (func $begin)) (export "select" (func $select))
          (export "rollback" (func $rollback)) (export "view" (func $view))
          (export "run-view" (func $run-view)) (export "return" (func $return))))))
      (func $run async (param "ctx" $context) (param "input" $json) (result (result $emission (error $error)))
        (canon lift (core func $main "run") (memory $memory "memory") (realloc (func $memory "realloc")) async (callback (func $main "callback"))))
      (instance $handler (export "json" (type $json)) (export "node-context" (type $context))
        (export "node-error" (type $error)) (export "emission" (type $emission)) (export "run" (func $run)))
      (export "{operation}" (instance $handler))
      (export "{PARTICIPANT}" (instance $handler)))"#,
      participant_len = PARTICIPANT.len(),
      digest = TRANSACTION_SQL_DIGEST,
      digest_len = TRANSACTION_SQL_DIGEST.len(),
    )).expect("encode composed transaction owner")
}

const TRANSACTION_SQL: &str = "UPDATE native_policy_participant SET value = 2 RETURNING value";
const TRANSACTION_SQL_DIGEST: &str =
    "sha256:1404226aba656e74db5db97045be6526e3451412526dc1ae2f99302f9b0d7bd3";

#[derive(Debug, Clone)]
struct Observation {
    phase: u32,
    scope: String,
    native_identity: bool,
    claims: Option<SessionClaims>,
    invocation: Option<ConnectionInvocation>,
    /// The same host-attested coordinates as seen from the postgres surface,
    /// which binds its own registry (`wamn-0h0g.7.9`).
    postgres_invocation: Option<ConnectionInvocation>,
    logging: Option<(String, String)>,
}

#[derive(Debug, Default)]
struct ResolutionPause {
    entered: Notify,
    policy: Mutex<Option<Arc<NativePolicy>>>,
}

#[derive(Debug)]
struct Observe {
    policy: Arc<NativePolicy>,
    events: Arc<Mutex<Vec<Observation>>>,
    entered: Arc<Notify>,
    resolution_pause: Option<Arc<ResolutionPause>>,
}

#[async_trait::async_trait]
impl HostPlugin for Observe {
    fn id(&self) -> &'static str {
        "native-policy-observer"
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([
                WitInterface::from(OBSERVE),
                WitInterface::from("wamn:node/types@0.1.0"),
            ]),
            exports: HashSet::new(),
        }
    }

    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        _interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        item.linker().instance("wamn:node/types@0.1.0")?;
        let policy = Arc::clone(&self.policy);
        let events = Arc::clone(&self.events);
        let entered = Arc::clone(&self.entered);
        item.linker().instance(OBSERVE)?.func_wrap(
            "record",
            move |mut store: wash_runtime::wasmtime::StoreContextMut<'_, SharedCtx>,
                  (phase,): (u32,)| {
                let active = extract_active_ctx(store.data_mut());
                let trace = wamn_engine::invocation_trace::invocation_trace(&active);
                trace.in_scope(|| {
                    let scope = active.ctx.component_id.to_string();
                    let native_identity = policy
                        .bindings
                        .read()
                        .expect("bindings lock")
                        .contains_key(&scope);
                    let claims = policy.resources.postgres.session_claims(&scope);
                    let invocation = policy.resources.blobstore.invocation(&scope);
                    let postgres_invocation = policy.resources.postgres.invocation(&scope);
                    let logging = policy.resources.logging.claim_snapshot(&scope);
                    let _effect = invocation
                        .as_ref()
                        .filter(|_| phase == 1)
                        .map(|invocation| {
                            tracing::info_span!("test.host.observe",
                                wamn.operation = %invocation.operation,
                                wamn.component_digest = %invocation.component_digest,
                            )
                            .entered()
                        });
                    events.lock().expect("observations lock").push(Observation {
                        phase,
                        scope,
                        native_identity,
                        claims,
                        invocation,
                        postgres_invocation,
                        logging,
                    });
                    if phase == 1 {
                        entered.notify_one();
                    }
                    Ok(())
                })
            },
        )?;
        Ok(())
    }

    async fn on_workload_resolved(
        &self,
        _workload: &ResolvedWorkload,
        _component_id: &str,
    ) -> anyhow::Result<()> {
        if let Some(pause) = &self.resolution_pause {
            *pause.policy.lock().expect("pause policy lock") = Some(Arc::clone(&self.policy));
            pause.entered.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(())
    }
}

fn fact(
    operation: &str,
    bytes: &[u8],
    registered: bool,
    dependencies: Vec<ComponentOperationDependency>,
) -> AdmittedComponent {
    AdmittedComponent {
        scope: ComponentPackageScope {
            tenant_id: "tenant-a".into(),
            package_id: operation
                .split_once(':')
                .expect("qualified operation")
                .0
                .into(),
            package_version: "1.0.0".into(),
        },
        component: "node".into(),
        interface_version: "0.1.0".into(),
        operations: BTreeMap::from([(
            operation.into(),
            AdmittedComponentOperation {
                registered_operation: registered.then(|| operation.into()),
                fresh_only: false,
                committed_result_schema: None,
                pre_commit: None,
                pre_commit_required: false,
                dependencies,
                input_ports: Vec::new(),
                output_ports: Vec::new(),
                parameters: Vec::new(),
                statements: BTreeMap::new(),
            },
        )]),
        component_digest: component_digest(bytes),
        imports: vec![OBSERVE.into(), "wamn:node/types@0.1.0".into()],
        imports_fingerprint: component_digest(OBSERVE.as_bytes()),
        effects: Vec::new(),
    }
}

struct Fixture {
    engine: Arc<Engine>,
    policy: Arc<NativePolicy>,
    workload: Arc<NativeWorkload>,
    application: Arc<NativeApplication<NativePolicy>>,
    root: AdmittedComponent,
    events: Arc<Mutex<Vec<Observation>>>,
    entered: Arc<Notify>,
}

impl Fixture {
    async fn new(case: Case) -> Self {
        let callee = matches!(case, Case::CalleeGrant).then_some(false);
        Self::build(case, callee, false).await
    }

    async fn build(case: Case, callee: Option<bool>, registered_root: bool) -> Self {
        Self::build_with_pause(case, callee, registered_root, None).await
    }

    async fn build_with_pause(
        case: Case,
        callee: Option<bool>,
        registered_root: bool,
        resolution_pause: Option<Arc<ResolutionPause>>,
    ) -> Self {
        Self::build_with_reuse(case, callee, registered_root, resolution_pause, None, None).await
    }

    /// `callee` declares a call to [`CHILD`] with its `fresh_only`. Publish
    /// folds that call into the root's grant. Composition embeds the callee,
    /// so the release lists only the root and the host loads only the root.
    async fn build_with_reuse(
        case: Case,
        callee: Option<bool>,
        registered_root: bool,
        resolution_pause: Option<Arc<ResolutionPause>>,
        reclaim_seconds: Option<i32>,
        postgres: Option<Arc<WamnPostgres>>,
    ) -> Self {
        let root_bytes = component_bytes(ROOT, case);
        let mut callees = Vec::new();
        let dependencies = if let Some(fresh_only) = callee {
            let mut child = fact(
                CHILD,
                &component_bytes(CHILD, Case::Success),
                true,
                Vec::new(),
            );
            child
                .operations
                .get_mut(CHILD)
                .expect("child operation")
                .fresh_only = fresh_only;
            let dependency = ComponentOperationDependency {
                package: "child".into(),
                version: "1.0.0".into(),
                digest: child.component_digest.clone(),
                operation: CHILD.into(),
                participant: None,
            };
            callees.push(child);
            vec![dependency]
        } else {
            Vec::new()
        };
        let mut root = fact(ROOT, &root_bytes, registered_root, dependencies);
        let transaction_statements = BTreeMap::from([(
            TRANSACTION_SQL_DIGEST.into(),
            ComponentSqlStatement {
                name: "update-participant".into(),
                path: "sql/update-participant.sql".into(),
                sql: TRANSACTION_SQL.into(),
                binds: Vec::new(),
                columns: vec![ComponentSqlField {
                    name: "value".into(),
                    value_type: ComponentSqlValueType::Int32,
                    nullable: false,
                }],
                transactional: true,
            },
        )]);
        if matches!(case, Case::TransactionOwner | Case::TransactionParticipant) {
            root.imports = vec![
                "wamn:node/types@0.1.0".into(),
                "wamn:postgres/types@0.1.0".into(),
                STATEMENTS.into(),
            ];
        }
        if matches!(case, Case::TransactionOwner) {
            let mut participant = root.operations[ROOT].clone();
            participant.registered_operation = Some(PARTICIPANT.into());
            participant.statements = transaction_statements.clone();
            root.operations.insert(PARTICIPANT.into(), participant);
        }
        if matches!(case, Case::TransactionParticipant) {
            root.operations
                .get_mut(ROOT)
                .expect("root operation")
                .statements = transaction_statements;
        }
        if matches!(case, Case::PostgresImport) {
            root.imports.push(STATEMENTS.into());
        }
        let mut served = ServingComponent::project(&root, &|dependency| {
            callees
                .iter()
                .find(|callee| callee.component_digest == dependency.digest)
        })
        .expect("publish folds the root's call graph");
        if matches!(case, Case::TransactionOwner) {
            // The composed owner embeds a base whose pre-commit slot the
            // participant fills. Publish folds that graph as below.
            let entry = served.operations.get_mut(ROOT).expect("root operation");
            entry.permissions.insert(PARTICIPANT.into());
            entry.participant = Some(PARTICIPANT.into());
            entry
                .statements
                .extend(root.operations[PARTICIPANT].statements.clone());
        }
        let native = vec![NativeComponent {
            fact: root.clone(),
            bytes: root_bytes,
        }];
        let facts = vec![root.clone()];
        let manifest = ServingManifest {
            format_version: SERVING_MANIFEST_FORMAT_VERSION,
            release: ServingRelease {
                tenant_id: "tenant-a".into(),
                effective_release_id: EffectiveReleaseId::new(1).expect("nonzero release"),
                environment: "test".into(),
                packages: facts
                    .iter()
                    .chain(&callees)
                    .map(|fact| {
                        PackageCoordinate::new(&fact.scope.package_id, "1.0.0")
                            .expect("package coordinate")
                    })
                    .collect(),
            },
            components: BTreeSet::from([served]),
            routes: BTreeSet::new(),
            attachments: BTreeMap::new(),
            workflow: wamn_catalog::WorkflowSection::default(),
        };
        let release = Arc::new(
            LoadedRelease::load_canonical_bytes(
                &manifest.canonical_bytes(),
                "native policy fixture",
            )
            .expect("admit the exact release manifest"),
        );
        let postgres = postgres.unwrap_or_else(|| {
            Arc::new(WamnPostgres::with_provider(Arc::new(
                StaticCredentialProvider::new(HashMap::new(), None),
            )))
        });
        let vault = Arc::new(WamnCredentials::from_projects(HashMap::new()));
        let policy = new_native_policy(
            &facts,
            NativePolicyResources {
                connection_http: Arc::new(ConnectionHttp::new(
                    Arc::clone(&postgres),
                    Arc::new(HttpTransport::new().expect("HTTP transport")),
                    Arc::clone(&vault),
                    "tenant-a",
                    "test",
                    Arc::from(Vec::new()),
                    Some(Arc::clone(&release)),
                )),
                blobstore: Arc::new(WamnBlobstore::new(
                    Arc::clone(&postgres),
                    vault,
                    "tenant-a",
                    "test",
                    Some(Arc::clone(&release)),
                )),
                postgres,
                logging: Arc::new(
                    WamnLogging::new(&WamnLoggingConfig::default()).expect("logging plugin"),
                ),
                release,
                project: "test".into(),
            },
        )
        .expect("prepare immutable component policy");
        let events = Arc::default();
        let entered = Arc::new(Notify::new());
        let observer = Arc::new(Observe {
            policy: Arc::clone(&policy),
            events: Arc::clone(&events),
            entered: Arc::clone(&entered),
            resolution_pause,
        });
        let plugins: HashMap<&'static str, Arc<dyn HostPlugin>> = HashMap::from([
            (NATIVE_POLICY_ID, Arc::clone(&policy) as Arc<dyn HostPlugin>),
            (observer.id(), observer as Arc<dyn HostPlugin>),
        ]);
        let engine = Arc::new(wamn_engine::build_engine(&[]).expect("production native engine"));
        let application = load_native_application(
            Arc::clone(&engine),
            NativeWorkloadSpec {
                warm_reuse: if let Some(reclaim_seconds) = reclaim_seconds {
                    wamn_engine::warm_reuse::WarmReuse::new(
                        &[root.component_digest.clone()],
                        1,
                        reclaim_seconds,
                    )
                    .expect("trusted root with bounded native reuse")
                } else {
                    wamn_engine::warm_reuse::WarmReuse::default()
                },
                id: "native-policy-test".into(),
                namespace: "test".into(),
                name: "native-policy-test".into(),
                components: native,
                local_resources: LocalResources::default(),
                host_interfaces: vec![
                    WitInterface::from(ROOT),
                    WitInterface::from(OBSERVE),
                    WitInterface::from("wamn:node/types@0.1.0"),
                    WitInterface::from(STATEMENTS),
                ],
            },
            Arc::clone(&policy),
            &plugins,
            &PluginBindings::new(),
            &Meters::new(MeterKind::Off),
        )
        .await
        .expect("load and resolve the real node ABI");
        let workload = Arc::clone(&application.workload);
        Self {
            engine,
            policy,
            workload,
            application,
            root,
            events,
            entered,
        }
    }

    async fn target(&self) -> DispatchTarget {
        let id = self
            .workload
            .facts_by_component_id
            .iter()
            .find_map(|(id, fact)| (fact == &self.root).then_some(id))
            .expect("root native identity");
        self.workload
            .dispatch_target(id, NATIVE_POLICY_ID)
            .await
            .expect("native root dispatch target")
    }

    fn request(&self, deadline: Instant) -> NativeInvocation<NativePolicy> {
        NativeInvocation {
            operation: ROOT.into(),
            input: r#"[{"value":37}]"#.into(),
            deadline,
            application: Arc::clone(&self.application),
            context: node_types::NodeContext {
                wiring_id: "forged-guest-wiring".into(),
                wiring_version: 999,
                node_id: "forged-guest-node".into(),
                delivery_id: "delivery".into(),
                input_port: None,
                occurrence: 0,
                traceparent: None,
                tracestate: None,
                deadline_ms: Some(30_000),
                config: "{}".into(),
            },
            facts: NativeFacts::entry(
                NodeAcquisition {
                    claims: SessionClaims {
                        tenant: "tenant-a".into(),
                        project: Some("test".into()),
                        release: Some(ReleaseIdentity {
                            effective_release_id: 1,
                            manifest_digest: self.policy.resources.release.manifest().digest(),
                        }),
                        ..SessionClaims::default()
                    },
                    invocation: ConnectionInvocation {
                        origin: ConnectionOrigin {
                            package_id: "root".into(),
                            component_digest: self.root.component_digest.clone(),
                            component: "node".into(),
                            interface_version: self.root.interface_version.clone(),
                            operation: ROOT.into(),
                        },
                        entry: wamn_runtime::plugins::connection_http::InvocationEntry::Wiring(
                            wamn_runtime::plugins::connection_http::WiringPosition {
                                package_id: "workflow".into(),
                                wiring_id: "trusted-wiring".into(),
                                wiring_version: 1,
                                node_id: "trusted-node".into(),
                                occurrence: 0,
                            },
                        ),
                        package_id: "root".into(),
                        component_digest: self.root.component_digest.clone(),
                        component: "node".into(),
                        operation: ROOT.into(),
                        closure: ConnectionExecutionClosure::Released,
                        effects: None,
                    },
                    causation: None,
                    platform: None,
                },
                None,
            ),
        }
    }

    async fn assert_clean(&self) {
        timeout(CLEANUP, async {
            loop {
                if self
                    .policy
                    .invocations
                    .lock()
                    .expect("invocation lock")
                    .is_empty()
                    && self.engine.guest_memory().in_use() == 0
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("native cancellation revokes authority and returns all guest memory");
        assert!(
            self.policy.traces.is_empty(),
            "native completion releases every invocation trace"
        );
        for event in self.events.lock().expect("observations lock").iter() {
            assert!(
                self.policy
                    .resources
                    .postgres
                    .session_claims(&event.scope)
                    .is_none()
            );
            assert!(
                self.policy
                    .resources
                    .blobstore
                    .invocation(&event.scope)
                    .is_none()
            );
            assert!(
                self.policy
                    .resources
                    .postgres
                    .invocation(&event.scope)
                    .is_none(),
                "revoke releases the postgres invocation with every other registry"
            );
            assert!(
                self.policy
                    .resources
                    .logging
                    .claim_snapshot(&event.scope)
                    .is_none()
            );
            assert!(
                self.policy
                    .resources
                    .postgres
                    .activate_statement_operation(&event.scope, ROOT)
                    .is_err()
            );
            // Rebinding succeeds only if the HTTP registry released this scope.
            self.policy
                .resources
                .connection_http
                .bind_invocation(
                    &event.scope,
                    self.request(Instant::now() + CLEANUP)
                        .facts
                        .acquisition
                        .invocation,
                )
                .expect("HTTP authority scope was revoked");
            self.policy
                .resources
                .connection_http
                .revoke_invocation(&event.scope);
        }
    }
}

async fn run_case(case: Case) {
    let database = (!matches!(case, Case::Cancellation)).then(wamn_test_postgres::database);
    let fixture = if let Some(database) = &database {
        let postgres = authenticated::platform_postgres(database.url())
            .await
            .expect("scoped database with provisioned platform principals");
        let callee = matches!(case, Case::CalleeGrant).then_some(false);
        Fixture::build_with_reuse(case, callee, false, None, None, Some(postgres)).await
    } else {
        Fixture::new(case).await
    };
    let target = fixture.target().await;
    assert!(
        fixture.events.lock().expect("observations lock").is_empty(),
        "resolution does not grant invocation authority"
    );
    if matches!(case, Case::Cancellation) {
        let request = fixture.request(Instant::now() + Duration::from_secs(30));
        let task = tokio::spawn(async move { invoke_native(&target, request).await });
        timeout(CLEANUP, fixture.entered.notified())
            .await
            .expect("actual node run starts before cancellation");
        task.abort();
        assert!(
            timeout(CLEANUP, task)
                .await
                .expect("cancel native invocation promptly")
                .expect_err("caller task is cancelled")
                .is_cancelled()
        );
    } else {
        // A deadline case says something only once the guest spins in the
        // loop under test: `start` records phase 0, `run` records phase 1.
        // Instantiation runs inside the same budget, so a loaded machine can
        // spend all of it first. That attempt has no event of the looping
        // phase; it is not a result, and it runs again with twice the budget.
        let looping = match case {
            Case::StartDeadline => Some(0),
            Case::RunDeadline => Some(1),
            _ => None,
        };
        let mut budget = BUDGET;
        let (deadline, result) = loop {
            let deadline = Instant::now()
                + if matches!(case, Case::Success | Case::CalleeGrant | Case::Trap) {
                    CLEANUP
                } else {
                    budget
                };
            let mut request = fixture.request(deadline);
            request.facts.acquisition.platform = Some(PlatformComponent::Materializer);
            let result = invoke_native(&target, request).await;
            let looped = looping.is_none_or(|phase| {
                fixture
                    .events
                    .lock()
                    .expect("observations lock")
                    .iter()
                    .any(|event| event.phase == phase)
            });
            // The last budget attempt stands, and the checks below report it.
            if looped || budget >= 64 * BUDGET {
                break (deadline, result);
            }
            let error = result.expect_err("a guest that never looped cannot finish");
            assert!(format!("{error:#}").contains("deadline"), "{error:#}");
            fixture.assert_clean().await;
            budget *= 2;
        };
        match case {
            Case::Success => {
                let emission = result
                    .expect("native dispatch succeeds")
                    .expect("typed node emission");
                assert_eq!(emission.payload, r#"[{"value":37}]"#);
                assert_eq!(emission.port, None);
            }
            Case::CalleeGrant => {
                // The folded grant is checked once, before the guest runs.
                let error =
                    result.expect_err("a callerless entry cannot use a callee's registered grant");
                let refusal = error.downcast_ref::<OperationRefusal>().unwrap_or_else(|| {
                    panic!("native boundary retains typed operation refusal: {error:#}")
                });
                assert_eq!(refusal.kind(), OperationRefusalKind::PermissionDenied);
                assert_eq!(refusal.operation(), CHILD);
            }
            Case::StartDeadline | Case::RunDeadline => {
                let error = result.expect_err("guest cannot extend the enclosing deadline");
                assert!(Instant::now() >= deadline);
                assert!(
                    Instant::now() < deadline + CLEANUP,
                    "deadline cancellation is bounded"
                );
                assert!(format!("{error:#}").contains("deadline"), "{error:#}");
            }
            Case::Trap => {
                let error = result.expect_err("a guest trap fails the invocation");
                let text = format!("{error:#}");
                assert!(text.contains("wasm trap"), "{text}");
                assert!(!text.contains("deadline"), "{text}");
            }
            Case::Cancellation => unreachable!("cancellation has its own caller task"),
            Case::PostgresImport => unreachable!("the postgres bind case has its own test"),
            Case::TransactionOwner | Case::TransactionParticipant => {
                unreachable!("transaction participation has its own authenticated test")
            }
        }
    }
    fixture.assert_clean().await;
    let events = fixture.events.lock().expect("observations lock").clone();
    assert!(events.iter().any(|event| event.phase == 0));
    for event in &events {
        if event.phase == 0 {
            assert!(
                event.native_identity,
                "initialization carries only native component identity"
            );
            assert!(event.claims.is_none());
            assert!(event.invocation.is_none());
            assert!(event.postgres_invocation.is_none());
            assert!(event.logging.is_none());
        } else {
            assert!(
                !event.native_identity,
                "execution carries a distinct invocation scope"
            );
            let claims = event.claims.as_ref().expect("execution has host claims");
            assert_eq!(claims.tenant, "tenant-a");
            if !matches!(case, Case::Cancellation) {
                assert_eq!(
                    claims.user_id,
                    Some(PlatformComponent::Materializer.principal_id().to_string()),
                    "a callerless delivery binds its platform principal"
                );
                assert_eq!(
                    claims.operation.as_deref(),
                    Some(ROOT),
                    "a callerless delivery binds the operation that it runs"
                );
            }
            assert_eq!(
                event.logging,
                Some(("tenant-a".into(), "test".into())),
                "execution has a logging claim"
            );
            let invocation = event
                .invocation
                .as_ref()
                .expect("execution has host authority");
            let position = invocation.entry.wiring().expect("a wiring entry");
            assert_eq!(position.wiring_id, "trusted-wiring");
            assert_eq!(position.node_id, "trusted-node");
            // The postgres surface reaches the SAME node and wiring
            // coordinates, from its own registry, for the whole invocation.
            assert_eq!(
                event.postgres_invocation.as_ref(),
                Some(invocation),
                "a postgres effect can name the node that raised it"
            );
            assert_eq!(invocation.operation, ROOT, "only the entry has a scope");
        }
    }
    assert_eq!(
        events.iter().filter(|event| event.phase == 1).count(),
        usize::from(!matches!(case, Case::StartDeadline | Case::CalleeGrant))
    );
    fixture
        .workload
        .unbind_all_plugins()
        .await
        .expect("unbind fixture workload");
}

fn isolated(name: &str, case: Case) {
    run_isolated_test(name, run_case(case));
}

fn run_isolated_test(name: &str, test: impl std::future::Future<Output = ()>) {
    let full_name = format!("operation::native_policy::tests::{name}");
    if std::env::var(CHILD_MARKER).as_deref() != Ok(name) {
        let output = Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", &full_name, "--nocapture"])
            .env(CHILD_MARKER, name)
            .output()
            .expect("start isolated native policy test");
        assert!(
            output.status.success(),
            "{full_name}: {}\n{}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "subprocess must execute the named test"
        );
        return;
    }
    let (done, finished) = mpsc::channel();
    let watchdog = std::thread::spawn(move || {
        if finished.recv_timeout(Duration::from_secs(60)) == Err(mpsc::RecvTimeoutError::Timeout) {
            eprintln!("native policy test exceeded its process watchdog");
            std::process::exit(124);
        }
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .event_interval(1)
        .enable_all()
        .build()
        .expect("isolated native runtime");
    runtime.block_on(test);
    drop(runtime);
    done.send(()).expect("finish watchdog");
    watchdog.join().expect("join watchdog");
}

#[test]
fn native_node_success_revokes_invocation_authority() {
    isolated(
        "native_node_success_revokes_invocation_authority",
        Case::Success,
    );
}
#[test]
fn native_callee_grant_refusal_revokes_invocation_authority() {
    isolated(
        "native_callee_grant_refusal_revokes_invocation_authority",
        Case::CalleeGrant,
    );
}
#[test]
fn native_node_initialization_has_no_authority_and_obeys_deadline() {
    isolated(
        "native_node_initialization_has_no_authority_and_obeys_deadline",
        Case::StartDeadline,
    );
}
#[test]
fn native_node_execution_obeys_enclosing_deadline() {
    isolated(
        "native_node_execution_obeys_enclosing_deadline",
        Case::RunDeadline,
    );
}
#[test]
fn native_node_cancellation_revokes_invocation_authority() {
    isolated(
        "native_node_cancellation_revokes_invocation_authority",
        Case::Cancellation,
    );
}
#[test]
fn native_node_trap_revokes_invocation_authority() {
    isolated("native_node_trap_revokes_invocation_authority", Case::Trap);
}

/// Counts `WARN` events whose message says wamn:postgres calls will be refused.
#[derive(Clone, Default)]
struct RefusedCallWarns(Arc<AtomicUsize>);

impl RefusedCallWarns {
    fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for RefusedCallWarns {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        if *event.metadata().level() != tracing::Level::WARN {
            return;
        }
        let mut message = Message(String::new());
        event.record(&mut message);
        if message.0.contains("calls will be refused") {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
}

async fn assert_postgres_bind_warns() {
    let warns = RefusedCallWarns::default();
    tracing::subscriber::set_global_default(tracing_subscriber::registry().with(warns.clone()))
        .expect("the isolated test process installs the warn capture first");

    // The driver's native path: preload, then one request, with an empty
    // workload config that a wash bind would treat as a missing tenant.
    let fixture = Fixture::new(Case::PostgresImport).await;
    let target = fixture.target().await;
    prepare_native(&target, Instant::now() + CLEANUP)
        .await
        .expect("preload the wamn:postgres node");
    assert_eq!(warns.count(), 0, "the native preload does not warn");
    let emission = invoke_native(&target, fixture.request(Instant::now() + CLEANUP))
        .await
        .expect("native dispatch succeeds")
        .expect("typed node emission");
    assert_eq!(emission.payload, r#"[{"value":37}]"#);
    assert_eq!(warns.count(), 0, "a native request does not warn");
    let postgres = &fixture.policy.resources.postgres;
    assert!(
        postgres.linker_entry_binds() > 0,
        "the native bind linked the node's wamn:postgres import"
    );
    assert_eq!(postgres.scope_registrations(), 0);
    fixture
        .workload
        .unbind_all_plugins()
        .await
        .expect("unbind fixture workload");

    // A wash bind of the same import with no tenant still warns, so the
    // capture above counts this warn when it happens.
    let wash = Arc::new(WamnPostgres::with_provider(Arc::new(
        StaticCredentialProvider::new(HashMap::new(), None),
    )));
    let plugins: HashMap<&'static str, Arc<dyn HostPlugin>> =
        HashMap::from([(WAMN_POSTGRES_ID, Arc::clone(&wash) as Arc<dyn HostPlugin>)]);
    let egress: Arc<dyn HostHandler> = Arc::new(NullServer::default());
    let resolved = wamn_engine::build_engine(&[])
        .expect("production engine")
        .initialize_workload(
            "wash-postgres-bind",
            Workload {
                namespace: "test".into(),
                name: "wash-postgres-bind".into(),
                annotations: HashMap::new(),
                service: None,
                components: vec![Component {
                    name: "wash-postgres".into(),
                    bytes: wat::parse_str(format!(
                        r#"(component (import "{STATEMENTS}" (instance)))"#
                    ))
                    .expect("encode the wash postgres guest")
                    .into(),
                    ..Component::default()
                }],
                host_interfaces: vec![WitInterface::from(STATEMENTS)],
                volumes: Vec::new(),
            },
        )
        .expect("initialize the wash workload")
        .resolve(
            Some(&plugins),
            &PluginBindings::new(),
            &HostRef::from_handler(&egress),
            &Meters::new(MeterKind::Off),
        )
        .await
        .expect("resolve the wash workload");
    assert_eq!(wash.scope_registrations(), 1);
    assert_eq!(warns.count(), 1, "a wash bind with no tenant warns once");
    resolved
        .unbind_all_plugins()
        .await
        .expect("unbind wash workload");
}

#[test]
fn native_postgres_guest_binds_without_the_tenant_warn() {
    run_isolated_test(
        "native_postgres_guest_binds_without_the_tenant_warn",
        assert_postgres_bind_warns(),
    );
}

#[tokio::test]
async fn native_application_cancelled_resolution_clears_partial_bindings() {
    let pause = Arc::new(ResolutionPause::default());
    let loading = tokio::spawn(Fixture::build_with_pause(
        Case::Success,
        None,
        false,
        Some(Arc::clone(&pause)),
    ));
    timeout(CLEANUP, pause.entered.notified())
        .await
        .expect("resolution reaches its public notification hook");
    let policy = pause
        .policy
        .lock()
        .expect("pause policy lock")
        .clone()
        .expect("binding policy result");
    assert!(!policy.bindings.read().expect("bindings lock").is_empty());
    assert!(
        policy.traces.is_empty(),
        "resolution creates no invocation trace"
    );
    assert!(
        policy
            .invocations
            .lock()
            .expect("invocation lock")
            .is_empty()
    );
    loading.abort();
    assert!(
        timeout(CLEANUP, loading)
            .await
            .expect("cancel pending resolution")
            .is_err_and(|error| error.is_cancelled())
    );
    assert!(
        policy.bindings.read().expect("bindings lock").is_empty(),
        "the retained policy cannot retain bindings from abandoned resolution"
    );
    assert!(
        policy
            .invocations
            .lock()
            .expect("invocation lock")
            .is_empty()
    );
    assert!(
        policy.traces.is_empty(),
        "abandoned resolution retains no trace"
    );
}

async fn assert_call_owner() {
    let fixture = Fixture::new(Case::Cancellation).await;
    let owner = Arc::downgrade(&fixture.application);
    let engine = Arc::clone(&fixture.engine);
    let policy = Arc::clone(&fixture.policy);
    let events = Arc::clone(&fixture.events);
    let entered = Arc::clone(&fixture.entered);
    let target = fixture.target().await;
    let request = fixture.request(Instant::now() + Duration::from_secs(30));
    let task = tokio::spawn(async move { invoke_native(&target, request).await });
    drop(fixture);
    timeout(CLEANUP, entered.notified())
        .await
        .expect("actual native handler holds the application");
    assert!(
        owner.upgrade().is_some(),
        "the call owns the application after its driver owner drops"
    );
    assert_eq!(policy.invocations.lock().expect("invocation lock").len(), 1);
    assert!(
        !policy.traces.is_empty(),
        "the active call retains its trace"
    );
    task.abort();
    assert!(
        timeout(CLEANUP, task)
            .await
            .expect("cancel caller")
            .expect_err("caller cancellation")
            .is_cancelled()
    );
    timeout(CLEANUP, async {
        while owner.upgrade().is_some() || engine.guest_memory().in_use() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("native guest abandonment releases the final application owner and memory");
    assert!(policy.bindings.read().expect("bindings lock").is_empty());
    assert!(
        policy
            .invocations
            .lock()
            .expect("invocation lock")
            .is_empty()
    );
    assert!(
        policy.traces.is_empty(),
        "cancelled work releases its trace"
    );
    for event in events.lock().expect("observations lock").iter() {
        assert!(
            policy
                .resources
                .postgres
                .session_claims(&event.scope)
                .is_none()
        );
        assert!(
            policy
                .resources
                .blobstore
                .invocation(&event.scope)
                .is_none()
        );
        assert!(
            policy
                .resources
                .logging
                .claim_snapshot(&event.scope)
                .is_none()
        );
        assert!(
            policy
                .resources
                .postgres
                .activate_statement_operation(&event.scope, ROOT)
                .is_err()
        );
    }
}

#[test]
fn native_application_call_retains_owner_until_guest_cancellation() {
    run_isolated_test(
        "native_application_call_retains_owner_until_guest_cancellation",
        assert_call_owner(),
    );
}
