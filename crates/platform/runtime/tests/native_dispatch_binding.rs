//! Public native dispatch binding checkpoint for wamn-0ct2.2.
//! The scalar fixtures isolate host binding from the application wire contract.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use anyhow::Context as _;
use tokio::sync::oneshot;
use wash_runtime::engine::ctx::SharedCtx;
use wash_runtime::engine::dispatch::{DispatchTarget, GuestCall, GuestCallFuture};
use wash_runtime::engine::workload::{ResolvedWorkload, WorkloadItem};
use wash_runtime::host::http::NullServer;
use wash_runtime::observability::{MeterKind, Meters};
use wash_runtime::plugin::{HostPlugin, PluginBindings, WitInterfaces};
use wash_runtime::types::{Component, Workload};
use wash_runtime::wasmtime::component::{Accessor, Instance, TypedFunc};
use wash_runtime::wit::{WitInterface, WitWorld};

const ROOT: &str = "root:entry/run@1.0.0";
const CHILD: &str = "child:entry/run@1.0.0";
const ROOT_WAT: &str = r#"
(component
  (import "child:entry/run@1.0.0" (instance $child
    (export "run" (func (result u32)))))
  (alias export $child "run" (func $child-run))
  (core func $lowered-child (canon lower (func $child-run)))
  (core module $module
    (import "child" "run" (func $child-run (result i32)))
    (func (export "run") (result i32) call $child-run))
  (core instance $imports (export "run" (func $lowered-child)))
  (core instance $root (instantiate $module (with "child" (instance $imports))))
  (func $run (result u32) (canon lift (core func $root "run")))
  (instance $exports (export "run" (func $run)))
  (export "root:entry/run@1.0.0" (instance $exports)))
"#;

fn child_wat(value: u32) -> String {
    format!(
        r#"(component
          (core module $module
            (func $start unreachable)
            (start $start)
            (func (export "run") (result i32) i32.const {value}))
          (core instance $child (instantiate $module))
          (func $run (result u32) (canon lift (core func $child "run")))
          (instance $exports (export "run" (func $run)))
          (export "{CHILD}" (instance $exports)))"#
    )
}

#[derive(Debug)]
struct OperationPlugin {
    allowed: Arc<AtomicBool>,
    shim_calls: Arc<AtomicUsize>,
    root_binds: AtomicUsize,
}

#[async_trait::async_trait]
impl HostPlugin for OperationPlugin {
    fn id(&self) -> &'static str {
        "native-operation-binding-test"
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from(CHILD)]),
            exports: HashSet::from([WitInterface::from(ROOT), WitInterface::from(CHILD)]),
        }
    }

    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        _interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        if item.component().get_export(None, ROOT).is_some() {
            self.root_binds.fetch_add(1, Ordering::SeqCst);
            let allowed = Arc::clone(&self.allowed);
            let calls = Arc::clone(&self.shim_calls);
            item.linker().instance(CHILD)?.func_wrap(
                "run",
                move |_store: wasmtime::StoreContextMut<'_, SharedCtx>, (): ()| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    if !allowed.load(Ordering::SeqCst) {
                        return Err(wasmtime::format_err!("nested-operation-denied"));
                    }
                    Ok((37_u32,))
                },
            )?;
        }
        Ok(())
    }
}

struct Run {
    operation: &'static str,
    reply: oneshot::Sender<u32>,
}

impl GuestCall for Run {
    fn describe(&self) -> &str {
        self.operation
    }

    fn call(
        self: Box<Self>,
        accessor: &Accessor<SharedCtx>,
        instance: Instance,
    ) -> GuestCallFuture<'_> {
        Box::pin(async move {
            let run: TypedFunc<(), (u32,)> = accessor
                .with(|mut access| {
                    let handler = instance
                        .get_export_index(&mut access, None, self.operation)
                        .ok_or_else(|| wasmtime::format_err!("missing operation export"))?;
                    let run = instance
                        .get_export_index(&mut access, Some(&handler), "run")
                        .ok_or_else(|| wasmtime::format_err!("missing run export"))?;
                    instance.get_typed_func(&mut access, run)
                })
                .map_err(|error| anyhow::anyhow!("{error:#}"))?;
            let (value,) = run
                .call_concurrent(accessor, ())
                .await
                .map_err(|error| anyhow::anyhow!("{error:#}"))?;
            self.reply
                .send(value)
                .map_err(|_| anyhow::anyhow!("reply abandoned"))?;
            Ok(None)
        })
    }
}

fn component(name: &str, source: &str) -> Component {
    Component {
        name: name.to_owned(),
        bytes: wat::parse_str(source)
            .expect("valid component fixture")
            .into(),
        ..Component::default()
    }
}

async fn resolve(
    duplicate_child: bool,
) -> (Arc<OperationPlugin>, anyhow::Result<ResolvedWorkload>) {
    let plugin = Arc::new(OperationPlugin {
        allowed: Arc::new(AtomicBool::new(true)),
        shim_calls: Arc::new(AtomicUsize::new(0)),
        root_binds: AtomicUsize::new(0),
    });
    let mut components = vec![
        component("root", ROOT_WAT),
        component("child", &child_wat(91)),
    ];
    if duplicate_child {
        components.push(component("other-child", &child_wat(92)));
    }
    let engine = wamn_engine::build_engine(&[]).expect("production engine");
    let workload = engine
        .initialize_workload(
            "native-binding-test",
            Workload {
                namespace: "test".to_owned(),
                name: "native-binding-test".to_owned(),
                annotations: HashMap::new(),
                service: None,
                components,
                host_interfaces: vec![WitInterface::from(ROOT), WitInterface::from(CHILD)],
                volumes: Vec::new(),
            },
        )
        .expect("native component loading");
    let plugins: HashMap<&'static str, Arc<dyn HostPlugin>> =
        HashMap::from([(plugin.id(), Arc::clone(&plugin) as Arc<dyn HostPlugin>)]);
    let resolved = workload
        .resolve(
            Some(&plugins),
            &PluginBindings::new(),
            Arc::new(NullServer::default()),
            &Meters::new(MeterKind::Off),
        )
        .await;
    (plugin, resolved)
}

async fn target(workload: &ResolvedWorkload, name: &str) -> DispatchTarget {
    let components = workload.components();
    let id = components
        .read()
        .await
        .values()
        .find(|component| component.name() == name)
        .expect("named component exists")
        .id()
        .to_owned();
    workload
        .dispatch_target(&id, "native-operation-binding-test")
        .await
        .expect("native dispatch target")
}

async fn run(target: &DispatchTarget, operation: &'static str) -> anyhow::Result<u32> {
    let (reply, receive) = oneshot::channel();
    target.dispatch(Run { operation, reply }).await?;
    receive.await.context("guest returned a value")
}

#[tokio::test]
async fn host_export_binding_keeps_the_nested_policy_shim() {
    let (plugin, workload) = resolve(false).await;
    let workload = workload.expect("two-component native workload resolves");
    assert_eq!(plugin.root_binds.load(Ordering::SeqCst), 1);
    let root = target(&workload, "root").await;
    assert_eq!(run(&root, ROOT).await.expect("host allows nested call"), 37);
    plugin.allowed.store(false, Ordering::SeqCst);
    let denied = run(&root, ROOT)
        .await
        .expect_err("host refuses nested call");
    assert!(format!("{denied:#}").contains("nested-operation-denied"));
    assert_eq!(plugin.shim_calls.load(Ordering::SeqCst), 2);
    let child = target(&workload, "child").await;
    let trapped = run(&child, CHILD).await.expect_err("child start must trap");
    assert!(format!("{trapped:#}").contains("unreachable"));
    assert_eq!(plugin.shim_calls.load(Ordering::SeqCst), 2);
    workload
        .unbind_all_plugins()
        .await
        .expect("unbind test workload");
}

#[tokio::test]
async fn native_resolution_refuses_ambiguous_imported_providers() {
    let (plugin, result) = resolve(true).await;
    assert_eq!(plugin.root_binds.load(Ordering::SeqCst), 1);
    let error = result.expect_err("native resolution rejects two child exporters");
    assert!(format!("{error:#}").contains("cannot disambiguate the provider"));
    assert_eq!(plugin.shim_calls.load(Ordering::SeqCst), 0);
}
