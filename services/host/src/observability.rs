//! The host's tracing subscriber.
//!
//! It installs the layers of `wash_runtime::observability::initialize_observability`
//! through the embedder path that function documents, and wraps them in
//! [`SampledSpans`]: a trace that the OpenTelemetry sampler did not sample
//! creates no further span (docs/architecture/execution.md).

use std::any::TypeId;

use anyhow::Context as _;
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::Interest;
use tracing::{Event, Level, Metadata, Subscriber};
use tracing_subscriber::filter::{EnvFilter, LevelFilter};
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{Layer as _, Registry};
use wash_runtime::observability::{flush, install_providers, log_configuration};

/// Install the process subscriber and return the flush to run on exit.
///
/// The layers and their filters decide what wash-runtime's do.
pub fn initialize(level: Level) -> anyhow::Result<Box<dyn FnOnce()>> {
    let fmt_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(level.as_str()))
        .add_directive("async_nats=error".parse()?)
        .add_directive("wasm_pkg_client=error".parse()?)
        .add_directive("wasm_pkg_core=error".parse()?);
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_ansi(false)
        .with_filter(fmt_filter);

    // Telemetry that cannot be set up is worth a warning, not an exit.
    let providers = match install_providers() {
        Ok(providers) => providers,
        Err(error) => {
            SampledSpans(Registry::default().with(fmt_layer))
                .try_init()
                .context("failed to install the tracing subscriber")?;
            tracing::warn!("failed to set up OTLP export: {error:#}; continuing without it");
            log_configuration();
            return Ok(Box::new(flush));
        }
    };
    // wash-runtime gives the exported signals an `EnvFilter` of the bare level.
    // A `LevelFilter` decides the same, without a span lookup on every enter.
    let level = LevelFilter::from_level(level);
    let otel_logs_layer = providers.logs_layer().map(|layer| layer.with_filter(level));
    let otel_tracer_layer = providers
        .tracing_layer()
        .map(|layer| layer.with_filter(level));
    SampledSpans(
        Registry::default()
            .with(fmt_layer)
            .with(otel_logs_layer)
            .with(otel_tracer_layer),
    )
    .try_init()
    .context("failed to install the tracing subscriber")?;
    log_configuration();
    Ok(Box::new(flush))
}

/// Whether the active OpenTelemetry span lets a new span be created.
///
/// The sampler decides when the OpenTelemetry layer starts a trace's entry
/// span, and entering that span makes its context active. A local span the
/// sampler did not sample refuses. A remote context is only the caller's flag,
/// so the entry span is still created and the sampler still decides. Without a
/// valid context, spans are created as before.
fn sampled_or_undecided() -> bool {
    use opentelemetry::trace::TraceContextExt as _;

    opentelemetry::Context::map_current(|context| {
        let span = context.span();
        let span = span.span_context();
        !span.is_valid() || span.is_sampled() || span.is_remote()
    })
}

/// A subscriber that creates no span in a trace the sampler did not sample.
///
/// It asks [`sampled_or_undecided`] before the wrapped subscriber sees a span,
/// so a refused span reaches no layer and the registry stores nothing for it.
/// Span call sites, WAMN and upstream, do not check sampling. Tasks that carry
/// the entered entry span read the same decision. Events are never refused.
///
/// A wrapper rather than a layer: in tracing-subscriber 0.3 a layer over
/// per-layer filters turns their `never` interest into `sometimes`, and a span
/// that every per-layer filter refuses is still stored in the registry.
#[derive(Debug)]
struct SampledSpans<S>(S);

impl<S: Subscriber> Subscriber for SampledSpans<S> {
    fn register_callsite(&self, metadata: &'static Metadata<'static>) -> Interest {
        let interest = self.0.register_callsite(metadata);
        // A span the wrapped subscriber would always create is now decided per
        // span. A refused callsite stays refused.
        if metadata.is_span() && interest.is_always() {
            Interest::sometimes()
        } else {
            interest
        }
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        self.0.max_level_hint()
    }

    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        (!metadata.is_span() || sampled_or_undecided()) && self.0.enabled(metadata)
    }

    fn event_enabled(&self, event: &Event<'_>) -> bool {
        self.0.event_enabled(event)
    }

    fn new_span(&self, span: &Attributes<'_>) -> Id {
        self.0.new_span(span)
    }

    fn record(&self, span: &Id, values: &Record<'_>) {
        self.0.record(span, values);
    }

    fn record_follows_from(&self, span: &Id, follows: &Id) {
        self.0.record_follows_from(span, follows);
    }

    fn event(&self, event: &Event<'_>) {
        self.0.event(event);
    }

    fn enter(&self, span: &Id) {
        self.0.enter(span);
    }

    fn exit(&self, span: &Id) {
        self.0.exit(span);
    }

    fn clone_span(&self, id: &Id) -> Id {
        self.0.clone_span(id)
    }

    fn try_close(&self, id: Id) -> bool {
        self.0.try_close(id)
    }

    fn current_span(&self) -> tracing_core::span::Current {
        self.0.current_span()
    }

    fn on_register_dispatch(&self, subscriber: &tracing::Dispatch) {
        self.0.on_register_dispatch(subscriber);
    }

    unsafe fn downcast_raw(&self, id: TypeId) -> Option<*const ()> {
        if id == TypeId::of::<Self>() {
            return Some(std::ptr::from_ref(self).cast());
        }
        // SAFETY: forwarded unchanged; the wrapped subscriber upholds the
        // contract for the type it answers. This lets `tracing-opentelemetry`
        // find its layer, as it does through `Layered`.
        unsafe { self.0.downcast_raw(id) }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use opentelemetry::context::FutureExt as _;
    use opentelemetry::trace::{
        SpanContext, SpanId, TraceContextExt as _, TraceFlags, TraceId, TraceState,
        TracerProvider as _,
    };
    use opentelemetry_sdk::trace::{InMemorySpanExporter, Sampler, SdkTracerProvider};
    use tracing::Instrument as _;
    use tracing_subscriber::filter::LevelFilter;
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::{Layer, Registry};
    use wamn_engine::invocation_trace::InvocationTrace;

    use super::SampledSpans;

    /// Counts the spans the registry stores.
    struct CountSpans(Arc<AtomicUsize>);

    impl<S: tracing::Subscriber> Layer<S> for CountSpans {
        fn on_new_span(
            &self,
            _: &tracing::span::Attributes<'_>,
            _: &tracing::span::Id,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// What one request produced.
    struct Outcome {
        created: usize,
        exported: Vec<String>,
        entry_sampled: bool,
    }

    /// One request: an entry span, an instrumented child, a span in a task that
    /// carries the current span (wash-runtime's `in_current_span`), and a span
    /// in a task restored by the native invocation carrier.
    async fn request(sampler: Sampler, caller: Option<TraceFlags>) -> Outcome {
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_sampler(sampler)
            .with_simple_exporter(exporter.clone())
            .build();
        let created = Arc::new(AtomicUsize::new(0));
        let _default = tracing::subscriber::set_default(SampledSpans(
            Registry::default()
                .with(CountSpans(created.clone()).with_filter(LevelFilter::INFO))
                .with(
                    tracing_opentelemetry::layer()
                        .with_tracer(provider.tracer("sampled-spans"))
                        .with_filter(LevelFilter::INFO),
                ),
        ));

        let remote = caller.map_or_else(opentelemetry::Context::new, |flags| {
            opentelemetry::Context::new().with_remote_span_context(SpanContext::new(
                TraceId::from_bytes([1; 16]),
                SpanId::from_bytes([2; 8]),
                flags,
                true,
                TraceState::default(),
            ))
        });
        // wash-runtime makes the caller's context current around the entry
        // span, as here.
        let entry_sampled = async {
            let entry = tracing::info_span!("handle_http_request");
            async {
                let child = tracing::info_span!("invoke_component_handler");
                async {
                    tokio::spawn(
                        async { drop(tracing::info_span!("wamn.task.current").entered()) }
                            .in_current_span(),
                    )
                    .await
                    .expect("current-span task");
                    tokio::spawn(InvocationTrace::capture().run(async {
                        drop(tracing::info_span!("wamn.task.carried").entered());
                    }))
                    .await
                    .expect("carried task");
                }
                .instrument(child)
                .await;
                opentelemetry::Context::map_current(|context| {
                    context.span().span_context().is_sampled()
                })
            }
            .instrument(entry)
            .await
        }
        .with_context(remote)
        .await;

        provider.force_flush().expect("flush");
        let mut names: Vec<String> = exporter
            .get_finished_spans()
            .expect("read exported spans")
            .into_iter()
            .map(|span| span.name.into_owned())
            .collect();
        names.sort();
        Outcome {
            created: created.load(Ordering::Relaxed),
            exported: names,
            entry_sampled,
        }
    }

    const ALL: [&str; 4] = [
        "handle_http_request",
        "invoke_component_handler",
        "wamn.task.carried",
        "wamn.task.current",
    ];

    #[tokio::test]
    async fn an_unsampled_request_stores_only_its_entry_span() {
        let outcome = request(Sampler::AlwaysOff, None).await;
        assert!(!outcome.entry_sampled);
        assert_eq!((outcome.created, outcome.exported.len()), (1, 0));
    }

    #[tokio::test]
    async fn a_sampled_request_keeps_every_span() {
        let outcome = request(Sampler::AlwaysOn, None).await;
        assert!(outcome.entry_sampled);
        assert_eq!(outcome.created, 4);
        assert_eq!(outcome.exported, ALL);
    }

    #[tokio::test]
    async fn the_sampler_decides_at_the_entry_not_the_callers_flag() {
        // A parent-based sampler follows an unsampled caller: nothing below the
        // entry span is stored.
        let follows = request(
            Sampler::ParentBased(Box::new(Sampler::AlwaysOn)),
            Some(TraceFlags::NOT_SAMPLED),
        )
        .await;
        assert_eq!((follows.created, follows.exported.len()), (1, 0));

        // A sampler that ignores the caller samples it, and every span exists.
        let overrides = request(Sampler::AlwaysOn, Some(TraceFlags::NOT_SAMPLED)).await;
        assert!(overrides.entry_sampled);
        assert_eq!(overrides.exported, ALL);
    }
}
