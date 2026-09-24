//! Loader admission tests use scalar guests because they do not dispatch a node.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use wamn_catalog::{
    AdmittedComponent, AdmittedComponentOperation, ComponentOperationDependency,
    ComponentPackageScope,
};
use wash_runtime::observability::{MeterKind, Meters};
use wash_runtime::plugin::PluginBindings;
use wash_runtime::types::LocalResources;
use wash_runtime::wit::WitInterface;

use super::{NativeComponent, NativeWorkload, NativeWorkloadSpec, load_native_workload};

const HANDLER: &str = "wamn:node/handler@0.1.0";
const OPERATION: &str = "palette:entry/run@1.0.0";
const PARENT: &str = "caller:entry/run@1.0.0";

fn component(name: &str, export: &str, import: Option<&str>, value: u32) -> NativeComponent {
    let import_wat = import.map_or_else(String::new, |name| {
        format!(r#"(import "{name}" (instance (export "run" (func (result u32)))))"#)
    });
    let bytes = wat::parse_str(format!(
        r#"(component
          {import_wat}
          (core module $module
            (func (export "run") (result i32) i32.const {value}))
          (core instance $instance (instantiate $module))
          (func $run (result u32) (canon lift (core func $instance "run")))
          (instance $exports (export "run" (func $run)))
          (export "{export}" (instance $exports)))"#
    ))
    .expect("valid scalar loader fixture");
    let imports: Vec<String> = import.into_iter().map(str::to_owned).collect();
    let imports_fingerprint = wamn_execution_contract::canonical_json_sha256(
        &serde_json::to_value(&imports).expect("import list serializes"),
    );
    NativeComponent {
        fact: AdmittedComponent {
            scope: ComponentPackageScope {
                tenant_id: "tenant-a".into(),
                package_id: export.split(':').next().expect("package namespace").into(),
                package_version: "1.0.0".into(),
            },
            component: name.into(),
            interface_version: "1.0.0".into(),
            operations: BTreeMap::from([(
                export.into(),
                AdmittedComponentOperation {
                    pre_commit: None,
                    pre_commit_required: false,
                    registered_operation: None,
                    fresh_only: false,
                    committed_result_schema: None,
                    dependencies: Vec::new(),
                    input_ports: Vec::new(),
                    output_ports: Vec::new(),
                    parameters: Vec::new(),
                    statements: BTreeMap::new(),
                },
            )]),
            component_digest: crate::component_admission::component_digest(&bytes),
            imports,
            imports_fingerprint,
            effects: Vec::new(),
        },
        bytes,
    }
}

async fn load(components: Vec<NativeComponent>) -> anyhow::Result<NativeWorkload> {
    load_with_reuse(
        components,
        crate::warm_reuse::WarmReuse::default(),
        vec![WitInterface::from(OPERATION)],
    )
    .await
}

async fn load_with_reuse(
    components: Vec<NativeComponent>,
    warm_reuse: crate::warm_reuse::WarmReuse,
    host_interfaces: Vec<WitInterface>,
) -> anyhow::Result<NativeWorkload> {
    load_native_workload(
        Arc::new(crate::build_engine(&[]).expect("production engine")),
        NativeWorkloadSpec {
            warm_reuse,
            id: "native-import-admission-test".into(),
            namespace: "test".into(),
            name: "native-import-admission-test".into(),
            components,
            local_resources: LocalResources::default(),
            // Host policy implements nested calls. Declaring an interface here
            // must not bypass the admitted provider uniqueness check.
            host_interfaces,
        },
        &HashMap::default(),
        &PluginBindings::new(),
        &Meters::new(MeterKind::Off),
    )
    .await
}

#[tokio::test]
async fn native_loader_allows_repeated_export_only_handlers() {
    let first = component("transform", HANDLER, None, 37);
    let repeat = NativeComponent {
        fact: first.fact.clone(),
        bytes: first.bytes.clone(),
    };
    let second = component("http-request", HANDLER, None, 37);
    assert_eq!(first.fact.component_digest, second.fact.component_digest);
    let workload = load(vec![first, repeat, second])
        .await
        .expect("shared handler exports retain separate admitted identities");
    let names: std::collections::BTreeSet<_> = workload
        .facts_by_component_id
        .values()
        .map(|fact| fact.component.as_str())
        .collect();
    assert_eq!(
        names,
        std::collections::BTreeSet::from(["http-request", "transform"])
    );
    workload.unbind_all_plugins().await.expect("unbind");
}

#[tokio::test]
async fn native_loader_refuses_ambiguous_imported_operation() {
    for second_value in [37, 91] {
        let first = component("selected", OPERATION, None, 37);
        let second = component("other", OPERATION, None, second_value);
        let mut caller = component("caller", PARENT, Some(OPERATION), 0);
        caller
            .fact
            .operations
            .get_mut(PARENT)
            .expect("caller operation")
            .dependencies
            .push(ComponentOperationDependency {
                participant: None,
                package: first.fact.scope.package_id.clone(),
                version: first.fact.scope.package_version.clone(),
                digest: first.fact.component_digest.clone(),
                operation: OPERATION.into(),
            });
        let first_digest = first.fact.component_digest.clone();
        let second_digest = second.fact.component_digest.clone();
        let error = load(vec![first, second, caller])
            .await
            .expect_err("exact dependency selection cannot hide ambiguous imported providers");
        let detail = format!("{error:#}");
        assert!(detail.contains("ambiguous component providers"), "{detail}");
        for identity in [
            OPERATION,
            "selected",
            "other",
            &first_digest,
            &second_digest,
        ] {
            assert!(detail.contains(identity), "{identity}: {detail}");
        }
    }
}

#[tokio::test]
async fn native_loader_checks_repeated_bytes_before_cache_access() {
    let first = component("transform", HANDLER, None, 37);
    let altered = NativeComponent {
        fact: first.fact.clone(),
        bytes: component("transform", HANDLER, None, 91).bytes,
    };
    let error = load(vec![first, altered])
        .await
        .expect_err("altered repeat refuses");
    assert!(format!("{error:#}").contains("native-component-bytes-digest-mismatch"));
}

#[tokio::test]
async fn native_mixed_workload_keeps_each_component_lifetime() {
    use wash_runtime::engine::ctx::SharedCtx;
    use wash_runtime::engine::dispatch::{GuestCall, GuestCallFuture};
    use wash_runtime::wasmtime::component::{Accessor, Instance};

    fn counter(name: &str, export: &str) -> NativeComponent {
        let mut input = component(name, export, None, 0);
        input.bytes = wat::parse_str(format!(
            r#"(component
            (core module $code
                (global $calls (mut i32) (i32.const 0))
                (func (export "run") (result i32)
                    global.get $calls i32.const 1 i32.add global.set $calls global.get $calls))
            (core instance $code (instantiate $code))
            (func $run (result u32) (canon lift (core func $code "run")))
            (instance $exports (export "run" (func $run)))
            (export "{export}" (instance $exports)))"#
        ))
        .expect("counter fixture");
        input.fact.component_digest = crate::component_admission::component_digest(&input.bytes);
        input
    }

    struct Count {
        export: String,
        reply: tokio::sync::oneshot::Sender<u32>,
    }
    impl GuestCall for Count {
        fn describe(&self) -> &'static str {
            "mixed workload counter"
        }
        fn deadline(&self) -> std::time::Duration {
            std::time::Duration::from_secs(2)
        }
        fn call(
            self: Box<Self>,
            accessor: &Accessor<SharedCtx>,
            instance: Instance,
        ) -> GuestCallFuture<'_> {
            Box::pin(async move {
                let run = accessor.with(|mut access| {
                    let interface = instance
                        .get_export_index(&mut access, None, &self.export)
                        .expect("counter interface");
                    let index = instance
                        .get_export_index(&mut access, Some(&interface), "run")
                        .expect("counter export");
                    instance.get_typed_func::<(), (u32,)>(&mut access, index)
                })?;
                let (count,) = run.call_concurrent(accessor, ()).await?;
                self.reply.send(count).expect("counter receiver");
                Ok(None)
            })
        }
    }

    let warm = counter("warm", HANDLER);
    let fresh = counter("fresh", OPERATION);
    let reuse =
        crate::warm_reuse::WarmReuse::new(std::slice::from_ref(&warm.fact.component_digest), 1, 60)
            .expect("deployment trust");
    let workload = load_with_reuse(vec![warm, fresh], reuse, Vec::new())
        .await
        .expect("mixed native workload");
    for (id, fact) in &workload.facts_by_component_id {
        let export = fact.operations.keys().next().expect("export");
        let target = workload
            .dispatch_target(id, "mixed-workload-test")
            .await
            .expect("target");
        let mut counts = Vec::new();
        for _ in 0..2 {
            let (reply, response) = tokio::sync::oneshot::channel();
            target
                .dispatch(Count {
                    export: export.clone(),
                    reply,
                })
                .await
                .expect("counter dispatch");
            counts.push(response.await.expect("counter result"));
        }
        assert_eq!(
            counts,
            if fact.component == "warm" {
                vec![1, 2]
            } else {
                vec![1, 1]
            },
            "{} native lifetime",
            fact.component
        );
    }
    workload
        .unbind_all_plugins()
        .await
        .expect("close mixed pools");
}
