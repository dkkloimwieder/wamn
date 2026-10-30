//! A release with a composed entry runs each wiring node on the entry.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use wamn_catalog::{
    AdmittedComponent, PackageCoordinate, SERVING_MANIFEST_FORMAT_VERSION, ServingComponent,
    ServingComposedEntry, ServingEntryMember, ServingManifest, ServingRelease,
};
use wamn_engine::artifact_source::{LocalComponentSource, local_component_path};
use wamn_engine::component_admission::component_digest;
use wamn_engine::operation::{OperationCall, OperationClosure, invoke_operation, node_types};
use wamn_engine::release_manifest::{LoadedRelease, ReleaseScope};
use wamn_platform_identity::connection_changes::ConnectionChanges;
use wamn_project_state::PlatformComponent;
use wamn_runtime::connection_credentials::ConnectionCredentials;
use wamn_runtime::connection_facts::ConnectionFacts;
use wamn_runtime::connection_memory::ConnectionMemory;
use wamn_runtime::plugins::connection_http::transport::HttpTransport;
use wamn_runtime::plugins::connection_http::{
    ConnectionExecutionClosure, InvocationEntry, WiringPosition,
};
use wamn_runtime::plugins::wamn_logging::{WamnLogging, WamnLoggingConfig};
use wamn_runtime::plugins::wamn_postgres::{StaticCredentialProvider, WamnPostgres};
use wamn_test_infrastructure::scratch::ScratchRoot;

use super::super::super::native_workload::native_entry_name;
use super::super::super::{NodeAcquisition, OperationHost, OperationScope, component_invocation};
use super::super::NativeFacts;
use super::{Case, NODE_TYPES, ROOT, component_bytes, fact};

/// A component whose operation export calls `wamn:operation` as an entry's
/// route layer would. It answers the result and refusal discriminants of
/// `commit`, then of `begin-operation`, as four ASCII digits.
fn operation_caller() -> Vec<u8> {
    wat::parse_str(format!(
        r#"(component
      {NODE_TYPES}
      (import "wamn:postgres/types@0.4.0" (instance $types
        (type $pg-error' (variant (case "serialization-failure") (case "connection-unavailable")
          (case "statement-timeout") (case "row-limit-exceeded" u64) (case "unique-violation" string)
          (case "foreign-key-violation" string) (case "check-violation" string)
          (case "exclusion-violation" string) (case "permission-denied")
          (case "query-error" (tuple string string))))
        (export "pg-error" (type $pg-error (eq $pg-error')))))
      (alias export $types "pg-error" (type $pg-error))
      (import "wamn:flow-http-routing/routing@0.1.0" (instance $routing
        (export "authenticated-caller" (type $caller (sub resource)))))
      (alias export $routing "authenticated-caller" (type $caller))
      (import "wamn:router-delivery/delivery@0.4.0" (instance))
      (import "wamn:operation/operation@0.1.0" (instance $operation
        (type $id' string)
        (export "sealed-operation-id" (type $id (eq $id')))
        (type $refusal' (variant (case "uncertain") (case "resolved" string) (case "conflict")
          (case "unknown-operation") (case "grant-denied") (case "invalid-caller")
          (case "not-begun") (case "already-begun") (case "postgres" $pg-error)
          (case "reads-claim-nothing")))
        (export "refusal" (type $refusal (eq $refusal')))
        (type $begun' (variant (case "new" string) (case "finished" string)))
        (export "begun" (type $begun (eq $begun')))
        (export "begin-operation" (func async (param "caller" (option (borrow $caller)))
          (param "operation" $id) (param "idempotency-key" string) (param "request-hash" (list u8))
          (result (result $begun (error $refusal)))))
        (export "commit" (func async (param "result" string) (result (result (error $refusal)))))))
      (core module $memory
        (memory (export "memory") 16)
        (data (i32.const 16) "{ROOT}")
        (global $next (mut i32) (i32.const 1024))
        (func (export "realloc") (param i32 i32) (param $align i32) (param $size i32) (result i32)
          (local $ptr i32) global.get $next local.get $align i32.const 1 i32.sub i32.add
          i32.const 0 local.get $align i32.sub i32.and local.tee $ptr
          local.get $size i32.add global.set $next local.get $ptr))
      (core instance $memory (instantiate $memory))
      (core func $commit (canon lower (func $operation "commit")
        (memory $memory "memory") (realloc (func $memory "realloc"))))
      (core func $begin (canon lower (func $operation "begin-operation")
        (memory $memory "memory") (realloc (func $memory "realloc"))))
      (core func $return (canon task.return (result (result $emission (error $error)))
        (memory $memory "memory")))
      (core module $main
        (import "memory" "memory" (memory 16))
        (import "host" "commit" (func $commit (param i32 i32 i32)))
        (import "host" "begin" (func $begin (param i32 i32 i32 i32 i32 i32 i32 i32 i32)))
        (import "host" "return" (func $return
          (param i32 i32 i32 i32 i32 i32 i32 i32 i64)))
        (func (export "callback") (param i32 i32 i32) (result i32) unreachable)
        (func $digit (param $at i32) (param $from i32)
          local.get $at local.get $from i32.load8_u i32.const 48 i32.add i32.store8)
        (func (export "run") (param $input i32) (result i32)
          i32.const 16 i32.const 0 i32.const 128 call $commit
          i32.const 0 i32.const 0 i32.const 16 i32.const {root_len} i32.const 0 i32.const 0
          i32.const 0 i32.const 0 i32.const 256 call $begin
          i32.const 700 i32.const 128 call $digit
          i32.const 701 i32.const 136 call $digit
          i32.const 702 i32.const 256 call $digit
          i32.const 703 i32.const 264 call $digit
          i32.const 0 i32.const 700 i32.const 4 i32.const 0 i32.const 0 i32.const 0
          i32.const 0 i32.const 0 i64.const 0 call $return i32.const 0))
      (core instance $main (instantiate $main (with "memory" (instance $memory))
        (with "host" (instance (export "commit" (func $commit)) (export "begin" (func $begin))
          (export "return" (func $return))))))
      (func $run async (param "ctx" $context) (param "input" $json)
        (result (result $emission (error $error)))
        (canon lift (core func $main "run") (memory $memory "memory")
          (realloc (func $memory "realloc")) async (callback (func $main "callback"))))
      (instance $handler (export "json" (type $json)) (export "node-context" (type $context))
        (export "node-error" (type $error)) (export "emission" (type $emission))
        (export "run" (func $run)))
      (export "{ROOT}" (instance $handler)))"#,
        root_len = ROOT.len(),
    ))
    .expect("encode the operation caller")
}

/// One release of the root member, on a host whose artifact directory holds
/// only what that host may load: the entry, or without one the member. The
/// operation caller is the entry, or without one the member's own bytes.
struct Release {
    host: Arc<OperationHost>,
    components: Arc<[AdmittedComponent]>,
    member: AdmittedComponent,
    entry_digest: String,
    _scratch: ScratchRoot,
}

fn release(with_entry: bool, postgres: Option<Arc<WamnPostgres>>) -> Release {
    let scratch = ScratchRoot::create().expect("scratch artifact directory");
    let entry = operation_caller();
    let entry_digest = component_digest(&entry);
    let bare = if with_entry {
        component_bytes(ROOT, Case::Success)
    } else {
        entry.clone()
    };
    let member = fact(ROOT, &bare, false);
    let composed_entry = if with_entry {
        let path = local_component_path(scratch.path(), &entry_digest).expect("entry path");
        std::fs::write(&path, &entry).expect("write the entry");
        Some(ServingComposedEntry {
            reference: format!("file:{}", path.display()),
            route_digest: entry_digest.clone(),
            application: ServingEntryMember {
                package_id: member.scope.package_id.clone(),
                component: member.component.clone(),
                digest: member.component_digest.clone(),
            },
            members: BTreeSet::from([ServingEntryMember {
                package_id: member.scope.package_id.clone(),
                component: member.component.clone(),
                digest: member.component_digest.clone(),
            }]),
        })
    } else {
        let path =
            local_component_path(scratch.path(), &member.component_digest).expect("member path");
        std::fs::write(path, &bare).expect("write the member");
        None
    };
    let manifest = ServingManifest {
        composed_entry,
        format_version: SERVING_MANIFEST_FORMAT_VERSION,
        release: ServingRelease {
            packages: BTreeSet::from([PackageCoordinate::new(&member.scope.package_id, "1.0.0")
                .expect("package coordinate")]),
        },
        components: BTreeSet::from([
            ServingComponent::project(&member, Vec::new()).expect("publish projects the member")
        ]),
        routes: BTreeSet::new(),
        attachments: BTreeMap::new(),

        host_routes: BTreeSet::new(),
    };
    let release = Arc::new(
        LoadedRelease::load_canonical_bytes(
            &manifest.canonical_bytes(),
            "composed entry fixture",
            ReleaseScope::new("tenant-a", "test").with_applications(
                serde_json::from_value(
                    serde_json::json!({"receiving": {"packages": ["root", "child"]}}),
                )
                .unwrap(),
            ),
        )
        .expect("admit the exact release manifest"),
    );
    let postgres = postgres.unwrap_or_else(|| {
        Arc::new(WamnPostgres::with_provider(Arc::new(
            StaticCredentialProvider::new(HashMap::new(), None),
        )))
    });
    let host = OperationHost::new(
        Arc::new(wamn_engine::build_engine(&[]).expect("production native engine")),
        Arc::clone(&postgres),
        Arc::new(HttpTransport::new().expect("HTTP transport")),
        Arc::new(ConnectionFacts::new(
            Arc::new(ConnectionMemory::default()),
            ConnectionChanges::default(),
            Arc::new(ConnectionCredentials::empty()),
            postgres,
            release.scope().env_key.as_str(),
            "test",
            release.scope().environment.as_str(),
        )),
        Arc::new(WamnLogging::new(&WamnLoggingConfig::default()).expect("logging plugin")),
        Arc::from([]),
        release,
        Arc::new(LocalComponentSource::new(scratch.path().to_owned())),
        OperationScope {
            project: "test".into(),
            schema: None,
            owner_prefix: "entry-test".into(),
            warm_reuse: wamn_engine::warm_reuse::WarmReuse::default(),
        },
    )
    .expect("operation host");
    Release {
        host: Arc::new(host),
        components: Arc::from([member.clone()]),
        member,
        entry_digest,
        _scratch: scratch,
    }
}

/// The member's own bytes are not on disk, so loading succeeds only when the
/// host neither pulls nor compiles them.
#[tokio::test]
async fn with_a_composed_entry_the_wiring_target_is_the_entry_and_the_member_is_not_compiled() {
    let release = release(true, None);
    let application = release
        .host
        .released_application(&release.components)
        .await
        .expect("the entry alone loads");
    let workload = &application.workload;
    assert!(
        workload.facts_by_component_id.is_empty(),
        "the member's own component is not loaded"
    );
    let (id, members) = workload.entry.as_ref().expect("the entry is loaded");
    assert_eq!(members, std::slice::from_ref(&release.member));
    assert_eq!(workload.component_ids().collect::<Vec<_>>(), [id.as_str()]);
    assert_eq!(workload.component_id(&release.member), Some(id.as_str()));
    application
        .dispatch_target(id)
        .expect("the member's calls dispatch to the entry");
    assert!(
        application
            .policy
            .components
            .contains_key(&native_entry_name(&release.entry_digest)),
        "the entry compiles under its own digest"
    );
    release
        .host
        .prepare_released(&release.components)
        .await
        .expect("the entry instantiates with its route layer imports linked");
}

#[tokio::test]
async fn without_a_composed_entry_the_wiring_target_is_the_bare_member() {
    let release = release(false, None);
    let application = release
        .host
        .released_application(&release.components)
        .await
        .expect("the member loads");
    let workload = &application.workload;
    assert!(workload.entry.is_none());
    let id = workload
        .component_id(&release.member)
        .expect("the member has its own component");
    assert_eq!(
        workload.facts_by_component_id.get(id),
        Some(&release.member)
    );
    application
        .dispatch_target(id)
        .expect("the member's calls dispatch to its own component");
}

/// A wiring call enters an operation export, and no native call binds an
/// operation entry, so the entry's `wamn:operation` refuses: `commit` with
/// `not-begun` (6) and `begin-operation` with `unknown-operation` (3).
#[tokio::test]
async fn a_wiring_call_on_the_entry_refuses_every_operation_call() {
    let database = wamn_test_postgres::database();
    let postgres = super::authenticated::platform_postgres(database.url())
        .await
        .expect("scoped database with provisioned platform principals");
    let release = release(true, Some(postgres));
    let host = &release.host;
    let input = serde_json::json!([]);
    let call = OperationCall {
        closure: OperationClosure::Released(&release.components),
        component: &release.member,
        operation: ROOT,
        context: node_types::NodeContext {
            wiring_id: "wiring".into(),
            wiring_version: 1,
            node_id: "node".into(),
            delivery_id: "delivery".into(),
            input_port: None,
            occurrence: 0,
            traceparent: None,
            tracestate: None,
            deadline_ms: Some(30_000),
            config: "{}".into(),
        },
        input: &input,
        deadline_ms: 30_000,
        facts: NativeFacts::entry(
            NodeAcquisition {
                claims: host.claims("tenant-a", Some(host.release_identity())),
                invocation: component_invocation(
                    &release.member,
                    ROOT,
                    InvocationEntry::Wiring(WiringPosition {
                        package_id: "workflow".into(),
                        wiring_hash: format!("sha256:{}", "a".repeat(64)),
                        wiring_id: "wiring".into(),
                        wiring_version: 1,
                        node_id: "node".into(),
                        occurrence: 0,
                    }),
                    ConnectionExecutionClosure::Released,
                    None,
                ),
                causation: None,
                platform: Some(PlatformComponent::Executor),
            },
            None,
        ),
    };
    let emission = invoke_operation(&**host, call, None)
        .await
        .expect("the wiring call runs on the entry")
        .expect("the node emits");
    assert_eq!(emission.payload, "1613");
}
