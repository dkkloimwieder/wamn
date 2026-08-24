//! The one identity vocabulary every host-plugin effect span carries, and the
//! one body that records an effect's duration.
//!
//! # Why the shared thing is a macro and not a function
//!
//! Every guest-visible effect this host performs — a DB call, an outbound HTTP
//! request, a JetStream publish or ack, a flow invocation — leaves the guest and
//! touches something outside it. Before `wamn-0h0g.24.3` only `wamn:postgres`
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
//! not a preference — and it is what keeps `wamn-0h0g.24.2`'s coming
//! wiring/node enrichment a ONE-line edit here rather than four edits that only
//! a gate could notice going out of step.
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
//! those down so one effect span is self-describing is a separate refinement
//! that `wamn-0h0g.24.2` (landed) did not cover.
//! (`wamn_router::NodeInvoker` has only test implementors because its `invoke`
//! is synchronous; `RouterDriver` drives `wiring.next` / `Step::Invoke` directly
//! and is the production driver.)

use std::time::Duration;

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
/// being cached process-wide. Keeping all three together honours it without
/// weakening it: that file still holds no process-wide cell of any kind.
macro_rules! effect_histogram {
    ($ident:ident, $meter:literal, $instrument:literal, $description:literal) => {
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
