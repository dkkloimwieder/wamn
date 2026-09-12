//! Native mechanism tests use the real node ABI and a test-only observation import.
//! Authenticated nested cases use the explicitly armed local PostgreSQL test below.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::process::Command;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::{Instant, timeout};
use wamn_catalog::{
    AdmittedComponent, AdmittedComponentOperation, ArtifactHash, ComponentOperationDependency,
    ComponentPackageScope, EffectiveReleaseId, PackageCoordinate, SERVING_MANIFEST_FORMAT_VERSION,
    ServingComponent, ServingComponentOperation, ServingManifest, ServingRelease,
};
use wamn_runtime::component_admission::component_digest;
use wamn_runtime::plugins::connection_http::transport::HttpTransport;
use wamn_runtime::plugins::connection_http::{
    ConnectionExecutionClosure, ConnectionHttp, ConnectionInvocation, ConnectionOrigin,
};
use wamn_runtime::plugins::wamn_blobstore::plugin::WamnBlobstore;
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_logging::{WamnLogging, WamnLoggingConfig};
use wamn_runtime::plugins::wamn_postgres::{
    ReleaseIdentity, SessionClaims, StaticCredentialProvider, WamnPostgres,
};
use wamn_runtime::release_manifest::LoadedRelease;
use wash_runtime::engine::Engine;
use wash_runtime::engine::ctx::{SharedCtx, extract_active_ctx};
use wash_runtime::engine::dispatch::DispatchTarget;
use wash_runtime::engine::workload::{ResolvedWorkload, WorkloadItem};
use wash_runtime::observability::{MeterKind, Meters};
use wash_runtime::plugin::{HostPlugin, PluginBindings, WitInterfaces};
use wash_runtime::types::LocalResources;
use wash_runtime::wit::{WitInterface, WitWorld};

use super::super::native_call::{NativeInvocation, invoke_native};
use super::super::native_workload::{
    NativeApplication, NativeComponent, NativeWorkload, NativeWorkloadSpec, load_native_application,
};
use super::super::{NodeAcquisition, OperationRefusal, OperationRefusalKind, node_types};
use super::{NATIVE_POLICY_ID, NativePolicy, NativePolicyResources, new_native_policy};

#[path = "tests/authenticated.rs"]
mod authenticated;
#[path = "tests/trace.rs"]
mod trace;

const ROOT: &str = "root:entry/run@1.0.0";
const CHILD: &str = "child:entry/run@1.0.0";
const OBSERVE: &str = "test:authority/observe@1.0.0";
const CHILD_MARKER: &str = "WAMN_NATIVE_POLICY_CHILD";
const BUDGET: Duration = Duration::from_millis(200);
const CLEANUP: Duration = Duration::from_secs(2);

// The structural types and indirect forwarding follow the existing real node
// fixture in route_authentication_live/fresh_only.rs. No scalar substitute is used.
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

#[derive(Clone, Copy)]
enum Case {
    Success,
    NestedRefusal,
    StartDeadline,
    RunDeadline,
    Cancellation,
}

fn component_bytes(operation: &str, case: Case) -> Vec<u8> {
    let nested = matches!(case, Case::NestedRefusal);
    let import = if nested {
        format!(
            r#"(import "{CHILD}" (instance $child
          (export "json" (type (eq $json)))
          (export "node-context" (type (eq $context)))
          (export "node-error" (type (eq $error)))
          (export "emission" (type (eq $emission)))
          (export "run" (func (param "ctx" $context) (param "input" $json)
            (result (result $emission (error $error)))))))"#
        )
    } else {
        String::new()
    };
    let lower = if nested {
        r#"(core func $nested (canon lower (func $child "run")
          (memory $memory "memory") (realloc (func $memory "realloc"))))"#
    } else {
        ""
    };
    let core_import = if nested {
        r#"(import "host" "nested" (func $nested (param i32 i32)))"#
    } else {
        ""
    };
    let core_binding = if nested {
        r#"(export "nested" (func $nested))"#
    } else {
        ""
    };
    let start = if matches!(case, Case::StartDeadline) {
        "(loop br 0)"
    } else {
        ""
    };
    let body = match case {
        Case::NestedRefusal => "local.get $input i32.const 256 call $nested",
        Case::RunDeadline | Case::Cancellation => "(loop br 0)",
        Case::Success | Case::StartDeadline => {
            r"
          i32.const 264 local.get $input i32.load offset=96 i32.store
          i32.const 268 local.get $input i32.load offset=100 i32.store"
        }
    };
    wat::parse_str(format!(
        r#"(component
      {NODE_TYPES}
      (import "{OBSERVE}" (instance $observe
        (export "record" (func (param "phase" u32)))))
      {import}
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
      {lower}
      (core module $main
        (import "memory" "memory" (memory 16))
        (import "host" "observe" (func $observe (param i32)))
        {core_import}
        (func $start i32.const 0 call $observe {start})
        (start $start)
        (func (export "run") (param $input i32) (result i32)
          i32.const 1 call $observe
          {body}
          i32.const 256))
      (core instance $main (instantiate $main (with "memory" (instance $memory))
        (with "host" (instance (export "observe" (func $observe)) {core_binding}))))
      (func $run (param "ctx" $context) (param "input" $json)
        (result (result $emission (error $error)))
        (canon lift (core func $main "run") (memory $memory "memory")
          (realloc (func $memory "realloc"))))
      (instance $handler
        (export "json" (type $json)) (export "node-context" (type $context))
        (export "node-error" (type $error)) (export "emission" (type $emission))
        (export "run" (func $run)))
      (export "{operation}" (instance $handler)))"#
    ))
    .expect("encode the complete node ABI fixture")
}

#[derive(Debug, Clone)]
struct Observation {
    phase: u32,
    scope: String,
    native_identity: bool,
    claims: Option<SessionClaims>,
    invocation: Option<ConnectionInvocation>,
    caller: Option<wamn_runtime::plugins::flow_http_routing::AuthenticatedCaller>,
    deadline: Option<Instant>,
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
                let trace = wamn_runtime::plugins::invocation_trace::invocation_trace(&active);
                trace.in_scope(|| {
                    let scope = active.ctx.component_id.to_string();
                    let native_identity = policy
                        .bindings
                        .read()
                        .expect("bindings lock")
                        .contains_key(&scope);
                    let claims = policy.resources.postgres.session_claims(&scope);
                    let invocation = policy.resources.blobstore.invocation(&scope);
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
                    let authority = policy
                        .invocations
                        .lock()
                        .expect("invocation lock")
                        .get(&scope)
                        .cloned();
                    let caller = authority.as_ref().and_then(|entry| entry.caller.clone());
                    let deadline = authority.as_ref().map(|entry| entry.deadline);
                    events.lock().expect("observations lock").push(Observation {
                        phase,
                        scope,
                        native_identity,
                        claims,
                        invocation,
                        caller,
                        deadline,
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
    application: Arc<NativeApplication>,
    root: AdmittedComponent,
    events: Arc<Mutex<Vec<Observation>>>,
    entered: Arc<Notify>,
}

impl Fixture {
    async fn new(case: Case) -> Self {
        let child = matches!(case, Case::NestedRefusal).then_some((Case::Success, false));
        Self::build(case, child, false).await
    }

    async fn build(case: Case, child: Option<(Case, bool)>, registered_root: bool) -> Self {
        Self::build_with_pause(case, child, registered_root, None).await
    }

    async fn build_with_pause(
        case: Case,
        child: Option<(Case, bool)>,
        registered_root: bool,
        resolution_pause: Option<Arc<ResolutionPause>>,
    ) -> Self {
        let root_bytes = component_bytes(ROOT, case);
        let mut native = Vec::new();
        let dependencies = if let Some((child_case, fresh_only)) = child {
            let bytes = component_bytes(CHILD, child_case);
            let mut child = fact(CHILD, &bytes, true, Vec::new());
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
            };
            native.push(NativeComponent { fact: child, bytes });
            vec![dependency]
        } else {
            Vec::new()
        };
        let mut root = fact(ROOT, &root_bytes, registered_root, dependencies);
        if matches!(case, Case::NestedRefusal) {
            root.imports.push(CHILD.into());
        }
        native.push(NativeComponent {
            fact: root.clone(),
            bytes: root_bytes,
        });
        let facts: Vec<_> = native
            .iter()
            .map(|component| component.fact.clone())
            .collect();
        let manifest = ServingManifest {
            format_version: SERVING_MANIFEST_FORMAT_VERSION,
            release: ServingRelease {
                tenant_id: "tenant-a".into(),
                effective_release_id: EffectiveReleaseId::new(1).expect("nonzero release"),
                environment: "test".into(),
                packages: facts
                    .iter()
                    .map(|fact| {
                        PackageCoordinate::new(&fact.scope.package_id, "1.0.0")
                            .expect("package coordinate")
                    })
                    .collect(),
            },
            components: facts
                .iter()
                .map(|fact| ServingComponent {
                    package_id: fact.scope.package_id.clone(),
                    component: fact.component.clone(),
                    interface_version: fact.interface_version.clone(),
                    digest: ArtifactHash::parse(&fact.component_digest).expect("component digest"),
                    operations: fact
                        .operations
                        .iter()
                        .map(|(name, operation)| {
                            (
                                name.clone(),
                                ServingComponentOperation {
                                    registered_operation: operation.registered_operation.clone(),
                                    fresh_only: operation.fresh_only,
                                    committed_result_schema: None,
                                    dependencies: operation.dependencies.clone(),
                                    statements: operation.statements.clone(),
                                },
                            )
                        })
                        .collect(),
                })
                .collect(),
            wirings: BTreeSet::new(),
            attachments: BTreeMap::new(),
            registrations: BTreeMap::new(),
        };
        let release = Arc::new(
            LoadedRelease::load_canonical_bytes(
                &manifest.canonical_bytes(),
                "native policy fixture",
            )
            .expect("admit the exact release manifest"),
        );
        let postgres = Arc::new(WamnPostgres::with_provider(Arc::new(
            StaticCredentialProvider::new(HashMap::new(), None),
        )));
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
                    WamnLogging::new(WamnLoggingConfig::default()).expect("logging plugin"),
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
        let engine = Arc::new(wamn_runtime::build_engine(&[]).expect("production native engine"));
        let application = load_native_application(
            Arc::clone(&engine),
            NativeWorkloadSpec {
                id: "native-policy-test".into(),
                namespace: "test".into(),
                name: "native-policy-test".into(),
                components: native,
                local_resources: LocalResources::default(),
                host_interfaces: vec![
                    WitInterface::from(ROOT),
                    WitInterface::from(CHILD),
                    WitInterface::from(OBSERVE),
                    WitInterface::from("wamn:node/types@0.1.0"),
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
            .resolved
            .dispatch_target(id, NATIVE_POLICY_ID)
            .await
            .expect("native root dispatch target")
    }

    fn request(&self, deadline: Instant) -> NativeInvocation {
        NativeInvocation {
            operation: ROOT.into(),
            input: r#"[{"value":37}]"#.into(),
            deadline,
            caller: None,
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
            acquisition: NodeAcquisition {
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
                        wiring_package_id: "workflow".into(),
                        package_id: "root".into(),
                        component_digest: self.root.component_digest.clone(),
                        component: "node".into(),
                        interface_version: self.root.interface_version.clone(),
                        operation: ROOT.into(),
                    },
                    package_id: "root".into(),
                    wiring_id: "trusted-wiring".into(),
                    wiring_version: 1,
                    node_id: "trusted-node".into(),
                    occurrence: 0,
                    component_digest: self.root.component_digest.clone(),
                    component: "node".into(),
                    operation: ROOT.into(),
                    closure: ConnectionExecutionClosure::Released,
                    effects: None,
                },
                causation: None,
            },
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
    let fixture = Fixture::new(case).await;
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
        let deadline = Instant::now()
            + if matches!(case, Case::Success | Case::NestedRefusal) {
                CLEANUP
            } else {
                BUDGET
            };
        let result = invoke_native(&target, fixture.request(deadline)).await;
        match case {
            Case::Success => {
                let emission = result
                    .expect("native dispatch succeeds")
                    .expect("typed node emission");
                assert_eq!(emission.payload, r#"[{"value":37}]"#);
                assert_eq!(emission.port, None);
            }
            Case::NestedRefusal => {
                let error =
                    result.expect_err("registered child cannot use callerless parent authority");
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
            Case::Cancellation => unreachable!("cancellation has its own caller task"),
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
        } else {
            assert!(
                !event.native_identity,
                "execution carries a distinct invocation scope"
            );
            let claims = event.claims.as_ref().expect("execution has host claims");
            assert_eq!(claims.tenant, "tenant-a");
            let invocation = event
                .invocation
                .as_ref()
                .expect("execution has host authority");
            assert_eq!(invocation.wiring_id, "trusted-wiring");
            assert_eq!(invocation.node_id, "trusted-node");
            assert_eq!(
                invocation.operation, ROOT,
                "refused child never gains an execution scope"
            );
        }
    }
    assert_eq!(
        events.iter().filter(|event| event.phase == 1).count(),
        usize::from(!matches!(case, Case::StartDeadline))
    );
    fixture
        .workload
        .resolved
        .unbind_all_plugins()
        .await
        .expect("unbind fixture workload");
}

fn isolated(name: &str, case: Case) {
    run_isolated_test(name, run_case(case));
}

fn run_isolated_test(name: &str, test: impl std::future::Future<Output = ()>) {
    let full_name = format!("router_driver::native_policy::tests::{name}");
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
fn native_nested_permission_refusal_revokes_invocation_authority() {
    isolated(
        "native_nested_permission_refusal_revokes_invocation_authority",
        Case::NestedRefusal,
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
    assert!(
        policy.application.get().is_none(),
        "an abandoned load publishes no owner"
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
