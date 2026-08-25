//! The one identity vocabulary every host-plugin effect span carries, and the
//! one body that records an effect's duration.
//!
//! # Why the shared thing is a macro and not a function
//!
//! Every guest-visible effect this host performs — a DB call, an outbound HTTP
//! request, a JetStream publish or ack — leaves the guest and touches
//! something outside it. Before `wamn-0h0g.24.3` only `wamn:postgres`
//! said so in a span, so "what did this component do off box" had no answer.
//!
//! What every one of those spans should agree on is the ENRICHMENT: which
//! tenant, project and component, resolved host-side and unspoofable. What they
//! must NOT share is the name — `wamn.postgres` is a published identifier that
//! deployed Grafana/Tempo panels and `tests/integration/src/metricbench.rs`
//! match on, and the other surfaces need names of their own.
//!
//! A shared *function* cannot do that. `tracing::info_span!` puts the span name
//! into a `static META` (tracing 0.1 `callsite2!`), so the name must be a const
//! expression and can never be a `&'static str` parameter. [`effect_span`] is
//! therefore a `macro_rules!` taking `$name:literal`: one definition of the
//! shared field block, a per-surface constant name at each call site. This is
//! M-MACRO-LAST-RESORT's actual last resort — the limitation is the language's,
//! not a preference — and it is what keeps `wamn-0h0g.24.12`'s wiring/node
//! enrichment a ONE-line edit here rather than four edits that only a gate
//! could notice going out of step.
//!
//! # The vocabulary
//!
//! - `wamn.tenant` / `wamn.project` / `wamn.component` — the executing
//!   component's identity, resolved HOST-side from bind-time claim maps and
//!   frozen plugin config. The guest supplies none of them and cannot spoof
//!   them. An empty string means the surface holds no such claim.
//! - `wamn.run_id` / `wamn.node_id` / `wamn.occurrence` / `wamn.requirement` —
//!   declared `Empty`, filled by [`record_run`] on the surfaces whose contract
//!   carries run coordinates. Today that is only the trusted HTTP effect.
//!
//! Each surface adds its own leading fields — `db.system` / `db.operation` for
//! `wamn:postgres` (OTel DB semantic conventions, frozen), `effect.operation`
//! for the rest, whose span name already names the surface.
//!
//! # Why resolved strings and not a plugin handle
//!
//! Each plugin holds its identity differently — `WamnPostgres` in `pub(super)`
//! claim maps, `ConnectionHttp` in frozen `Box<str>` fields, and `WamnJetstream`
//! in its bind-time map. [`EffectIdentity`] takes
//! the resolved `&str` triple so this module needs no access to any of them, and
//! adding a surface here never widens a plugin's private claim API.
//!
//! # What is deliberately absent
//!
//! These spans carry no wiring or node identity of their own. They are not
//! orphaned: an effect raised inside a node runs under `wamn.component.invoke`,
//! the span `crates/execution/host/src/router_driver.rs` instruments each
//! `Step::Invoke` with, which already carries `wamn.wiring_id`,
//! `wamn.wiring_version`, `wamn.node_id` and `wamn.component_digest`. Copying
//! those down so one effect span is self-describing is `wamn-0h0g.24.12`;
//! `wamn-0h0g.24.2` landed the invocation span itself and did not cover it.
//! (`wamn_router::NodeInvoker` has only test implementors because its `invoke`
//! is synchronous; `RouterDriver` drives `wiring.next` / `Step::Invoke` directly
//! and is the production driver.)

use std::time::{Duration, SystemTime};

/// Host-resolved identity of the component performing one effect.
///
/// Every field is a claim the platform registered at workload bind or froze
/// into the plugin at construction; an empty string means "this surface holds
/// no such claim", never "the guest declined to send one".
#[derive(Clone, Copy, Debug)]
pub(crate) struct EffectIdentity<'a> {
    pub tenant: &'a str,
    pub project: &'a str,
    pub component: &'a str,
}

/// The run coordinates of one effect, for a surface whose contract carries them.
///
/// Guest-supplied, and therefore only ever a trace label — the authority checks
/// that make these coordinates load-bearing live in the plugin, not here.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EffectRun<'a> {
    pub run_id: &'a str,
    pub node_id: &'a str,
    pub occurrence: u32,
    pub requirement: &'a str,
}

/// [9.8] The duration histogram of each surface `wamn-0h0g.24.3` newly
/// instrumented, one per surface so the instrument name identifies it exactly as
/// the older `wamn.postgres.query.duration_ms` does. Nothing consumed these
/// surfaces before, so unlike the postgres one they carry no frozen contract.
///
/// They live HERE rather than beside their plugins because `tools/repo-lint`
/// refuses ANY `static` / `LazyLock` / `OnceLock` in `connection_http.rs` — a
/// deliberately coarse rule that keeps a credentialed HTTP client from ever
/// being cached process-wide. Keeping them together honours it without
/// weakening it: that file still holds no process-wide cell of any kind.
///
/// `$instrument` is an expression, not a literal, so an instrument whose name is
/// itself a published identifier can be declared from the `const` that pins it
/// (see [`JETSTREAM_ACK_LAG_INSTRUMENT`]) instead of repeating the string.
macro_rules! effect_histogram {
    ($ident:ident, $meter:literal, $instrument:expr, $description:literal) => {
        pub(crate) static $ident: std::sync::LazyLock<opentelemetry::metrics::Histogram<f64>> =
            std::sync::LazyLock::new(|| {
                opentelemetry::global::meter($meter)
                    .f64_histogram($instrument)
                    .with_description($description)
                    .build()
            });
    };
}

effect_histogram!(
    HTTP_EFFECT_DURATION_MS,
    "wamn-connection-http",
    "wamn.http_effect.duration_ms",
    "trusted HTTP connection effect latency in ms, by effect.operation"
);
effect_histogram!(
    JETSTREAM_DURATION_MS,
    "wamn-jetstream",
    "wamn.jetstream.duration_ms",
    "wamn:jetstream effect latency in ms, by effect.operation"
);

/// [9.8] `wamn-0h0g.24.8`'s ack-lag series: how long a JetStream message waited
/// between the server publishing it and this host acking it.
///
/// The name is pinned as a `const` for the same reason `DELIVERY_ATTEMPTS` is in
/// `crates/execution/host/src/router_delivery.rs` — the Prometheus exporter
/// rewrites the dots to underscores, so a chart's `grep` for the series never
/// finds the literal, and a rename looks free at the call site.
pub(crate) const JETSTREAM_ACK_LAG_INSTRUMENT: &str = "wamn.jetstream.ack_lag_ms";

effect_histogram!(
    JETSTREAM_ACK_LAG_MS,
    "wamn-jetstream",
    JETSTREAM_ACK_LAG_INSTRUMENT,
    "delay in ms between a JetStream message being published and this host acking it"
);

/// The `effect.operation` label the non-postgres surfaces record their duration
/// under.
///
/// `wamn:postgres` keeps `db.operation` instead: it is the OTel DB semantic
/// convention AND a frozen published label — see [`record_effect_ms`].
pub(crate) const EFFECT_OPERATION: &str = "effect.operation";

/// Fill the run-coordinate fields [`effect_span`] declared `Empty`.
///
/// Called from the macro's expansion, so it must be reachable from every plugin
/// module rather than private to this one.
pub(crate) fn record_run(span: &tracing::Span, run: Option<EffectRun<'_>>) {
    if let Some(run) = run {
        span.record("wamn.run_id", run.run_id);
        span.record("wamn.node_id", run.node_id);
        span.record("wamn.occurrence", run.occurrence);
        span.record("wamn.requirement", run.requirement);
    }
}

/// [9.1] Open one effect span: a per-surface constant name, that surface's own
/// leading fields, then the shared identity vocabulary.
///
/// ```ignore
/// effect_span!(
///     "wamn.postgres",
///     EffectIdentity { tenant: &tenant, project: &project, component: component_id },
///     None,
///     db.system = "postgresql",
///     db.operation = op,      // <- the trailing comma is required
/// )
/// ```
///
/// The surface's own fields come FIRST in the emitted span, so `wamn:postgres`
/// keeps the exact field order it published. They are spliced as raw tokens, so
/// the last one must carry a trailing comma.
///
/// The caller instruments the awaited effect with the returned span
/// (`future.instrument(span).await`); entering it around a synchronous prelude
/// would not cover the await that is the effect.
///
/// The span is emitted through the process's global `tracing` subscriber, which
/// the fork's `initialize_observability` bridges to OTel and exports over OTLP
/// when `OTEL_*` is set — so it nests under whatever span is current and threads
/// into that trace. Enriching a host-created span keeps 9.1 wamn-side, with no
/// fork patch.
macro_rules! effect_span {
    (
        $name:literal,
        $identity:expr,
        $run:expr,
        $($surface_field:tt)*
    ) => {{
        // Named and typed so the macro cannot lie about what it accepts
        // (M-MACROS-DONT-LIE): a wrong second argument fails here rather than
        // inside an inscrutable `tracing` expansion.
        let identity: $crate::plugins::effect_span::EffectIdentity<'_> = $identity;
        let span = tracing::info_span!(
            $name,
            $($surface_field)*
            wamn.tenant = %identity.tenant,
            wamn.project = %identity.project,
            wamn.component = %identity.component,
            wamn.run_id = tracing::field::Empty,
            wamn.node_id = tracing::field::Empty,
            wamn.occurrence = tracing::field::Empty,
            wamn.requirement = tracing::field::Empty,
        );
        $crate::plugins::effect_span::record_run(&span, $run);
        span
    }};
}

pub(crate) use effect_span;

/// [9.8] Record one effect's wall time on the calling surface's histogram.
///
/// The histogram and the operation-label KEY belong to the caller because they
/// are published identifiers, not implementation detail: `wamn:postgres` exports
/// `wamn.postgres.query.duration_ms` labelled `db.operation`, which
/// `tests/integration/src/metricbench.rs` polls and asserts and which
/// `docs/archive/observability/dashboards.md` slices a Grafana panel by. Only
/// the recording body is shared.
pub(crate) fn record_effect_ms(
    duration_ms: &opentelemetry::metrics::Histogram<f64>,
    operation_key: &'static str,
    operation: &'static str,
    project: &str,
    elapsed: Duration,
) {
    duration_ms.record(
        elapsed.as_secs_f64() * 1000.0,
        &[
            opentelemetry::KeyValue::new(operation_key, operation),
            opentelemetry::KeyValue::new("wamn.project", project.to_string()),
        ],
    );
}

/// The registration one ack-lag sample is attributed to.
///
/// Only a consumer bound through `bind_registration` carries these: the plain
/// `bind` path registers no dead-letter identity, so there is no registration to
/// name and the call site passes `None`. Following the [`EffectIdentity`]
/// convention, the keys are still emitted then, empty — one stable series shape,
/// and an empty value reads as "this bind holds no such claim" rather than "the
/// label was dropped".
#[derive(Clone, Copy, Debug)]
pub(crate) struct AckLagRegistration<'a> {
    pub tenant: &'a str,
    pub environment: &'a str,
    pub catalog_id: &'a str,
    pub registration_id: &'a str,
}

/// The label vector one ack-lag sample carries, in a fixed order.
fn ack_lag_labels(
    project: &str,
    registration: Option<AckLagRegistration<'_>>,
) -> [opentelemetry::KeyValue; 5] {
    // DELIBERATE, not a placeholder: the plain `bind` path emits these four
    // keys empty rather than omitting them, per [`AckLagRegistration`] and the
    // [`EffectIdentity`] convention. Dropping them would fork the series shape
    // by bind path and make "no plain-bind traffic" indistinguishable from
    // "plain-bind acks not measured" (owner ruling, wamn-0h0g.24.8).
    let registration = registration.unwrap_or(AckLagRegistration {
        tenant: "",
        environment: "",
        catalog_id: "",
        registration_id: "",
    });
    [
        opentelemetry::KeyValue::new("wamn.project", project.to_string()),
        opentelemetry::KeyValue::new("wamn.tenant", registration.tenant.to_string()),
        opentelemetry::KeyValue::new("wamn.environment", registration.environment.to_string()),
        opentelemetry::KeyValue::new("wamn.catalog_id", registration.catalog_id.to_string()),
        opentelemetry::KeyValue::new(
            "wamn.registration_id",
            registration.registration_id.to_string(),
        ),
    ]
}

/// Wall time between a message being published and this host acking it.
///
/// Clamped at zero. `published` is the NATS server's clock and `now` is this
/// host's; an unsynchronised pair puts the publish in the future, which is a
/// clock fact and not a measurement, and a duration histogram cannot hold it.
fn ack_lag_ms(published: SystemTime, now: SystemTime) -> f64 {
    now.duration_since(published)
        .unwrap_or(Duration::ZERO)
        .as_secs_f64()
        * 1000.0
}

/// [9.8] Record how long one JetStream message waited between publish and ack.
///
/// Sibling of [`record_effect_ms`], and injected the same way: the histogram
/// arrives BY REFERENCE, so a test passes one built over its own
/// `SdkMeterProvider` and reads the sample back instead of reaching into the
/// process-global meter. `now` is a parameter for the same reason — the whole
/// recorded value is then the caller's, and the clamp is testable.
pub(crate) fn record_ack_lag_ms(
    ack_lag: &opentelemetry::metrics::Histogram<f64>,
    project: &str,
    registration: Option<AckLagRegistration<'_>>,
    published: SystemTime,
    now: SystemTime,
) {
    ack_lag.record(
        ack_lag_ms(published, now),
        &ack_lag_labels(project, registration),
    );
}

#[cfg(test)]
mod tests {
    use opentelemetry::metrics::MeterProvider as _;
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};

    use super::*;

    /// A fixed instant, so a recorded lag is an exact `f64` and not a race with
    /// the wall clock.
    fn published_at() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    /// One ack-lag histogram over a test-owned in-memory exporter. The provider
    /// is the test's, not `opentelemetry::global`'s, so each test reads back
    /// exactly the samples its own call emitted — this is the injected Meter
    /// [`record_ack_lag_ms`]'s by-reference histogram parameter exists for.
    struct AckLagHarness {
        exporter: InMemoryMetricExporter,
        provider: SdkMeterProvider,
    }

    impl AckLagHarness {
        fn install() -> Self {
            let exporter = InMemoryMetricExporter::default();
            let provider = SdkMeterProvider::builder()
                .with_reader(PeriodicReader::builder(exporter.clone()).build())
                .build();
            Self { exporter, provider }
        }

        fn histogram(&self) -> opentelemetry::metrics::Histogram<f64> {
            self.provider
                .meter("ack-lag-test")
                .f64_histogram(JETSTREAM_ACK_LAG_INSTRUMENT)
                .build()
        }

        /// Every `(name, sorted labels, sum, count)` the exporter holds, so an
        /// assertion names the whole emitted surface and a label that should not
        /// be there cannot hide.
        fn series(&self) -> Vec<(String, Vec<(String, String)>, f64, u64)> {
            self.provider
                .force_flush()
                .expect("test metrics must flush");
            let mut series = Vec::new();
            for resource in self
                .exporter
                .get_finished_metrics()
                .expect("test metric exporter must remain readable")
            {
                for scope in resource.scope_metrics() {
                    for metric in scope.metrics() {
                        let AggregatedMetrics::F64(MetricData::Histogram(histogram)) =
                            metric.data()
                        else {
                            panic!("{} must stay an f64 histogram", metric.name())
                        };
                        for point in histogram.data_points() {
                            let mut labels: Vec<(String, String)> = point
                                .attributes()
                                .map(|kv| (kv.key.to_string(), kv.value.to_string()))
                                .collect();
                            labels.sort();
                            series.push((
                                metric.name().to_owned(),
                                labels,
                                point.sum(),
                                point.count(),
                            ));
                        }
                    }
                }
            }
            series
        }
    }

    fn labels(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut labels: Vec<(String, String)> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        labels.sort();
        labels
    }

    /// A consumer bound through `bind_registration` carries the registration, so
    /// the sample is attributable to one registration of one catalog.
    #[test]
    fn ack_lag_records_the_message_age_under_its_registration() {
        let harness = AckLagHarness::install();
        record_ack_lag_ms(
            &harness.histogram(),
            "orders",
            Some(AckLagRegistration {
                tenant: "acme",
                environment: "prod",
                catalog_id: "cat-7",
                registration_id: "orders-changed",
            }),
            published_at(),
            published_at() + Duration::from_millis(250),
        );
        assert_eq!(
            harness.series(),
            vec![(
                JETSTREAM_ACK_LAG_INSTRUMENT.to_owned(),
                labels(&[
                    ("wamn.project", "orders"),
                    ("wamn.tenant", "acme"),
                    ("wamn.environment", "prod"),
                    ("wamn.catalog_id", "cat-7"),
                    ("wamn.registration_id", "orders-changed"),
                ]),
                250.0,
                1,
            )],
        );
    }

    /// The plain `bind` path registers no dead-letter identity. The sample is
    /// still recorded, with the registration keys present and empty, so the
    /// series shape does not depend on which bind path a consumer took.
    #[test]
    fn ack_lag_on_the_plain_bind_path_leaves_the_registration_keys_empty() {
        let harness = AckLagHarness::install();
        record_ack_lag_ms(
            &harness.histogram(),
            "orders",
            None,
            published_at(),
            published_at() + Duration::from_millis(40),
        );
        assert_eq!(
            harness.series(),
            vec![(
                JETSTREAM_ACK_LAG_INSTRUMENT.to_owned(),
                labels(&[
                    ("wamn.project", "orders"),
                    ("wamn.tenant", ""),
                    ("wamn.environment", ""),
                    ("wamn.catalog_id", ""),
                    ("wamn.registration_id", ""),
                ]),
                40.0,
                1,
            )],
        );
    }

    /// `published` is the server's clock. Unsynchronised, it can sit in this
    /// host's future; the sample is then zero, never negative.
    #[test]
    fn ack_lag_clamps_a_publish_stamped_in_the_future() {
        let harness = AckLagHarness::install();
        record_ack_lag_ms(
            &harness.histogram(),
            "orders",
            None,
            published_at() + Duration::from_secs(5),
            published_at(),
        );
        let series = harness.series();
        assert_eq!(series.len(), 1, "one sample was recorded");
        assert_eq!(series[0].2, 0.0, "a future publish clamps to zero lag");
    }

    /// Proving the recorder alone would prove a function nothing calls. The one
    /// production caller is `JsMessage::ack`, and it is not reachable from a
    /// test: `ack` needs a real delivered `async_nats` message in the resource
    /// table, which has no in-process fake, so both ack-exercising tests are
    /// `WAMN_EVT_NATS_URL`-gated and skip when unset. This reads the call site
    /// instead.
    #[test]
    fn the_jetstream_ack_site_records_the_lag() {
        const JETSTREAM: &str = include_str!("wamn_jetstream.rs");
        let ack = JETSTREAM
            .split_once("    async fn ack(")
            .expect("wamn_jetstream.rs defines `ack`")
            .1
            .split_once("    async fn nack(")
            .expect("`nack` follows `ack`")
            .0;
        assert!(
            ack.contains("record_ack_lag_ms("),
            "`JsMessage::ack` no longer records the ack lag, and no runtime test can \
             notice: the ack path needs a live NATS. The body was:\n{ack}"
        );
        assert!(
            ack.contains("JETSTREAM_ACK_LAG_MS"),
            "`JsMessage::ack` records a lag on some other histogram than the one \
             {JETSTREAM_ACK_LAG_INSTRUMENT:?} names"
        );
    }
}
