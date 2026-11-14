//! Restore the store's invocation context at the native logging callback.

use std::sync::Arc;

use wamn_engine::invocation_trace::invocation_trace;
use wash_runtime::engine::ctx::{SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::wasmtime::component::{ComponentType, Lift};

#[derive(Debug, ComponentType, Lift)]
#[expect(
    dead_code,
    reason = "The component ABI lifts these variants from guest values."
)]
#[repr(u8)]
#[component(enum)]
#[component(wasmtime_crate = wash_runtime::wasmtime)]
enum Level {
    #[component(name = "trace")]
    Trace,
    #[component(name = "debug")]
    Debug,
    #[component(name = "info")]
    Info,
    #[component(name = "warn")]
    Warn,
    #[component(name = "error")]
    Error,
    #[component(name = "critical")]
    Critical,
}

/// The public native hook installs the interface and its workload metadata.
/// Its private record callback has no context hook, so replace only that callback.
/// The trace comes from the invocation record selected by this store's active scope.
pub(super) fn bind(item: &mut WorkloadItem<'_>) -> anyhow::Result<()> {
    let component: Arc<str> = item.id().into();
    let name: Arc<str> = item.workload_name().into();
    let namespace: Arc<str> = item.workload_namespace().into();
    let linker = item.linker()?;
    linker.allow_shadowing(true);
    let result = (|| {
        linker
            .instance("wasi:logging/logging@0.1.0-draft")?
            .func_wrap(
                "log",
                move |mut store: wash_runtime::wasmtime::StoreContextMut<'_, SharedCtx>,
                      (level, context, message): (Level, String, String)| {
                    let trace = invocation_trace(&mut extract_active_ctx(store.data_mut()));
                    trace.in_scope(|| {
                        let span = tracing::info_span!("wasi.logging.log", ?level, context);
                        let _entered = span.enter();
                        macro_rules! record {
                            ($level:expr) => {
                                tracing::event!(
                                    $level,
                                    workload.component_id = component.as_ref(),
                                    workload.name = name.as_ref(),
                                    workload.namespace = namespace.as_ref(),
                                    context,
                                    "{message}"
                                )
                            };
                        }
                        match level {
                            Level::Trace => record!(tracing::Level::TRACE),
                            Level::Debug => record!(tracing::Level::DEBUG),
                            Level::Info => record!(tracing::Level::INFO),
                            Level::Warn => record!(tracing::Level::WARN),
                            Level::Error | Level::Critical => record!(tracing::Level::ERROR),
                        }
                    });
                    Ok(())
                },
            )
    })();
    linker.allow_shadowing(false);
    Ok(result?)
}
