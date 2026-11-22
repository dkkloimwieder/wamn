//! The workflow service serves an entry call while its `wasi:cli/run` task waits.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{Notify, oneshot};
use tokio::time::timeout;
use wash_runtime::engine::ctx::SharedCtx;
use wash_runtime::engine::dispatch::{DispatchTarget, GuestCall, GuestCallFuture};
use wash_runtime::engine::workload::{ResolvedWorkload, WorkloadItem};
use wash_runtime::host::{HostApi, HostBuilder};
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::types::{LocalResources, Service, Workload, WorkloadStartRequest};
use wash_runtime::wasmtime::component::types::ComponentItem;
use wash_runtime::wasmtime::component::{Accessor, Instance, ResourceType};
use wash_runtime::wasmtime::format_err;
use wash_runtime::wit::{WitInterface, WitWorld};

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        path: "../../wit/wamn-workflow",
        world: "service",
        imports: { default: async | trappable },
        exports: { default: async },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::exports::wamn::workflow::entry;
use bindings::wamn::workflow::run::{Refusal, Workflow};

const ID: &str = "wamn-workflow";
const RUN: &str = "wamn:workflow/run@0.1.0";
const DEADLINE: Duration = Duration::from_secs(60);

/// Serves the service's run loop imports as calls that never finish.
#[derive(Default)]
struct Imports {
    waiting: Arc<Notify>,
    target: Mutex<Option<DispatchTarget>>,
}

#[async_trait::async_trait]
impl HostPlugin for Imports {
    fn id(&self) -> &'static str {
        ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from("wamn:workflow/run,node@0.1.0")]),
            exports: HashSet::new(),
        }
    }

    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        _interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        let component = item.component().clone();
        let engine = component.engine();
        let linker = item.linker()?;
        // The host links WASI. Every other import gets a stub here.
        for (name, import) in component.component_type().imports(engine) {
            let ComponentItem::ComponentInstance(instance) = import.ty else {
                continue;
            };
            if name.starts_with("wasi:") {
                continue;
            }
            let mut linker = linker.instance(name)?;
            for (export, item) in instance.exports(engine) {
                match item.ty {
                    ComponentItem::Resource(_) => {
                        linker.resource(export, ResourceType::host::<()>(), |_, _| Ok(()))?;
                    }
                    // `serve` polls these two first. Each leaves `cli/run` waiting.
                    ComponentItem::ComponentFunc(_)
                        if name == RUN && export == "admit-schedules" =>
                    {
                        linker.func_new_concurrent(export, |_, _, _, _| {
                            Box::pin(std::future::pending())
                        })?;
                    }
                    ComponentItem::ComponentFunc(_)
                        if name == RUN && export == "data-change-consumers" =>
                    {
                        let waiting = Arc::clone(&self.waiting);
                        linker.func_new_concurrent(export, move |_, _, _, _| {
                            waiting.notify_one();
                            Box::pin(std::future::pending())
                        })?;
                    }
                    ComponentItem::ComponentFunc(func) if func.async_() => {
                        linker.func_new_concurrent(export, |_, _, _, _| {
                            Box::pin(async { Err(format_err!("the test serves no such import")) })
                        })?;
                    }
                    ComponentItem::ComponentFunc(_) => {
                        linker.func_new(export, |_, _, _, _| {
                            Err(format_err!("the test serves no such import"))
                        })?;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    async fn on_workload_resolved(
        &self,
        workload: &ResolvedWorkload,
        item: &str,
    ) -> anyhow::Result<()> {
        let target = workload.dispatch_target(item, ID).await?;
        *self.target.lock().expect("target lock") = Some(target);
        Ok(())
    }
}

struct Call {
    reply: oneshot::Sender<Result<entry::Response, Refusal>>,
}

impl GuestCall for Call {
    fn describe(&self) -> &'static str {
        "wamn:workflow/entry#call"
    }

    fn call(
        self: Box<Self>,
        accessor: &Accessor<SharedCtx>,
        instance: Instance,
    ) -> GuestCallFuture<'_> {
        Box::pin(async move {
            let service =
                accessor.with(|mut access| bindings::Service::new(&mut access, &instance))?;
            let input = entry::Request {
                handle: "request".to_owned(),
                run_id: None,
                definition: Workflow {
                    id: "workflow".to_owned(),
                    version: 1,
                    graph_json: "{}".to_owned(),
                },
                input_json: "{}".to_owned(),
                remaining_ms: 10_000,
            };
            let result = service
                .wamn_workflow_entry()
                .call_call(accessor, input)
                .await?;
            let _ = self.reply.send(result);
            Ok(None)
        })
    }
}

#[tokio::test]
async fn entry_call_returns_while_cli_run_waits() {
    let path = std::env::var_os("WAMN_WORKFLOW_SERVICE_COMPONENT")
        .expect("set WAMN_WORKFLOW_SERVICE_COMPONENT to the built workflow service");
    let bytes = std::fs::read(&path).expect("read the workflow service component");
    let imports = Arc::new(Imports::default());
    let engine = wamn_engine::engine::build_engine(&[]).expect("production WAMN engine");
    let host = HostBuilder::new()
        .with_engine(engine)
        .with_plugin(Arc::clone(&imports) as Arc<dyn HostPlugin>)
        .expect("register the import plugin")
        .build()
        .expect("build the host")
        .start()
        .await
        .expect("start the host");
    host.workload_start(WorkloadStartRequest {
        workload_id: "workflow".to_owned(),
        workload: Workload {
            namespace: "test".to_owned(),
            name: "workflow".to_owned(),
            annotations: HashMap::new(),
            service: Some(Service {
                bytes: bytes.into(),
                digest: None,
                local_resources: LocalResources::default(),
                max_restarts: 0,
            }),
            components: Vec::new(),
            host_interfaces: vec![WitInterface::from("wamn:workflow/run,node@0.1.0")],
            volumes: Vec::new(),
        },
    })
    .await
    .expect("start the workflow service");
    timeout(DEADLINE, imports.waiting.notified())
        .await
        .expect("cli/run reaches its first waiting import");
    let target = imports
        .target
        .lock()
        .expect("target lock")
        .clone()
        .expect("the service resolved a dispatch target");
    let (reply, answer) = oneshot::channel();
    timeout(DEADLINE, target.dispatch(Call { reply }))
        .await
        .expect("the entry call finishes within the deadline")
        .expect("the entry call returns instead of trapping");
    answer
        .await
        .expect("the entry call replies")
        .expect_err("an empty graph is refused");
}
