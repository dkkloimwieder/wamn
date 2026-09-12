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
//! not a preference.
//!
//! What the shared block does NOT buy is enrichment for free. `wamn-0h0g.24.12`
//! was filed on the premise that widening this block enriches all four surfaces
//! at once, and an owner ruling refuted that for three of them on 2026-08-26:
//! DECLARING a field here is one edit, but FILLING it is one per surface, and
//! only a surface that can SOURCE the claim may fill it. So [`EffectWiring`] is
//! declared here once and filled by `wamn.connection_http` alone
//! (`wamn-0h0g.24.12`), the one surface holding a host-bound
//! `ConnectionInvocation`. The rest are separate beads with separate owners:
//! `wamn.jetstream` is `wamn-0h0g.24.15` (it holds a wiring id only as a
//! router-tap subject, and no node id at all), and `wamn:postgres` is
//! `wamn-0h0g.24.14`, blocked until `wamn-0h0g.7.9` builds the guest-to-host
//! run-context contract those coordinates would reach it through.
//!
//! # The vocabulary
//!
//! - `wamn.tenant` / `wamn.project` / `wamn.component` — the executing
//!   component's identity, resolved HOST-side from bind-time claim maps and
//!   frozen plugin config. The guest supplies none of them and cannot spoof
//!   them. An empty string means the surface holds no such claim.
//! - `wamn.package_id` / `wamn.wiring_id` / `wamn.wiring_version` /
//!   `wamn.node_id` / `wamn.occurrence` / `wamn.component_digest` /
//!   `wamn.component_name` / `wamn.operation` — the
//!   host-attested invocation the effect was raised under, declared `Empty` and
//!   filled by [`record_wiring`] on a surface holding one. The five coordinates
//!   `wamn.component.invoke` also carries are spelled exactly as that parent
//!   spells them, never a second vocabulary for the same position.
//!   `wamn.package_id` and `wamn.occurrence` come from the same bound
//!   invocation. The parent carries neither, so an effect span is the first
//!   place a reader finds them (`wamn-b2m6.2`).
//!   `wamn.component_name` names the executing component the way the catalog
//!   admitted it. It is NOT `wamn.component`, which every surface fills with the
//!   per-request pooled scope. That scope is an instance id and it changes on
//!   every call, so it names no component a reader can look up
//!   (`wamn-b2m6.7`).
//! - `wamn.run_id` / `wamn.requirement` — declared `Empty`,
//!   filled by [`record_run`] on the surfaces whose contract carries run
//!   coordinates. Nothing constructs an [`EffectRun`] on any surface today; the
//!   contract that would is `wamn-0h0g.7.9`.
//! - `effect.outcome` — what the platform says happened to the effect. One of
//!   six words and never a seventh, declared `Empty` and filled by
//!   [`EffectOutcomeGuard`] on the surfaces that run their effect through it.
//!   [`EffectOutcome`] states what each word claims (`wamn-b2m6.3`).
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
//! Three of the four surfaces still carry no wiring or node identity of their
//! own. They are not orphaned: an effect raised inside a node runs under
//! `wamn.component.invoke`, the span
//! `crates/execution/host/src/router_driver.rs` instruments each `Step::Invoke`
//! with, which already carries `wamn.wiring_id`, `wamn.wiring_version`,
//! `wamn.node_id`, `wamn.component_digest` and `wamn.operation` — the spelling
//! [`EffectWiring`] reuses rather than forking. `wamn.connection_http` copies
//! them down so that one effect span is self-describing without a parent walk
//! (`wamn-0h0g.24.12`); `wamn.jetstream` (`wamn-0h0g.24.15`) and `wamn:postgres`
//! (`wamn-0h0g.24.14`, itself blocked on `wamn-0h0g.7.9`) cannot source the
//! coordinates yet and leave the block empty. `wamn-0h0g.24.2` landed the
//! invocation span itself and covered none of them.
//! (`wamn_router::NodeInvoker` has only test implementors because its `invoke`
//! is synchronous; `RouterDriver` drives `wiring.next` / `Step::Invoke` directly
//! and is the production driver.)
//!
//! The EXECUTING COMPONENT IDENTITY and the NODE OPERATION arrive the same way,
//! and a reader has to know what each one answers. `ConnectionInvocation`, the
//! record the router driver binds per pooled instance, carries the admitted
//! component name and the operation the driver called (`wamn-b2m6.7`). Before
//! that widening an effect span named its component only by
//! `wamn.component_digest`, which is a manifest key, and named its call only by
//! `effect.operation`, which is the capability method. Neither answers "which
//! component, running which node operation, raised this effect".

use std::sync::{Arc, Mutex};
use std::time::Duration;

use wamn_execution_contract::EffectOutcome;

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

/// The wiring position one effect was raised at, and the package that executed
/// it.
///
/// Host-attested: copied down from the invocation the router driver bound before
/// entering the pooled component, never from anything the guest sent. An empty
/// field means "this effect holds no such claim" — the same convention
/// [`EffectIdentity`] follows.
///
/// `package_id` and `occurrence` sit beside the position because a reader
/// attributes an effect to one CALL. A wiring id is package-scoped, so two
/// packages name a wiring alike, and one walk visits the same node more than
/// once. Without these two fields those calls record identical spans
/// (`wamn-b2m6.2`).
///
/// `component_name` and `operation` say WHO ran the effect and WHICH node
/// operation raised it. The digest names the component by the key its manifest
/// stores it under, and `effect.operation` names the capability method. So
/// without these two a reader has neither the component name a person calls the
/// component by nor the operation the wiring ran (`wamn-b2m6.7`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct EffectWiring<'a> {
    pub package_id: &'a str,
    pub wiring_id: &'a str,
    pub wiring_version: u32,
    pub node_id: &'a str,
    pub occurrence: u32,
    pub component_digest: &'a str,
    pub component_name: &'a str,
    pub operation: &'a str,
}

/// The run coordinates of one effect, for a surface whose contract carries them.
///
/// Guest-supplied, and therefore only ever a trace label — the authority checks
/// that make these coordinates load-bearing live in the plugin, not here.
///
/// `node_id` and `occurrence` are NOT here: one coordinate has one owner, and
/// the owner is [`EffectWiring`], which the host attests rather than the guest
/// asserts (`wamn-0h0g.24.12`, `wamn-b2m6.2`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct EffectRun<'a> {
    pub run_id: &'a str,
    pub requirement: &'a str,
}

/// Failure evidence shared only by one invocation and its nested calls.
///
/// Successful capability attempts contribute no failure evidence. Exactly one
/// failed attempt supplies its observed outcome; two or more are ambiguous.
/// Dropping the last handle discards the evidence with its invocation.
#[derive(Debug, Clone, Default)]
pub struct EffectEvidence {
    state: Arc<Mutex<FailureEvidence>>,
}

#[derive(Debug, Default)]
enum FailureEvidence {
    #[default]
    None,
    One(EffectOutcome),
    Ambiguous,
}

impl EffectEvidence {
    /// Start an invocation with no observed failed capability attempt.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The sole failed attempt's outcome, absent when missing or ambiguous.
    #[must_use]
    pub fn outcome(&self) -> Option<EffectOutcome> {
        match *self.state.lock().ok()? {
            FailureEvidence::One(outcome) => Some(outcome),
            FailureEvidence::None | FailureEvidence::Ambiguous => None,
        }
    }

    fn record_failure(&self, outcome: EffectOutcome) {
        if let Ok(mut state) = self.state.lock() {
            *state = match *state {
                FailureEvidence::None => FailureEvidence::One(outcome),
                FailureEvidence::One(_) | FailureEvidence::Ambiguous => FailureEvidence::Ambiguous,
            };
        }
    }
}

impl PartialEq for EffectEvidence {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }
}

impl Eq for EffectEvidence {}

/// The span field one outcome is recorded under, beside [`EFFECT_OPERATION`].
pub(crate) const EFFECT_OUTCOME: &str = "effect.outcome";

/// Put one outcome on one effect span, whatever ends the effect.
///
/// A guard and not a plain call, because a dropped future records nothing. An
/// effect abandoned before it settles leaves through `Drop`, and `Drop` is the
/// only code that runs then. The guard therefore starts at
/// [`EffectOutcome::Cancelled`] and the call site settles it once the effect
/// returns. An effect span carries an outcome on every path out.
///
/// The guard holds its OWN clone of the span. `Instrument` drops the span it was
/// given as soon as the effect ends, and this clone keeps the span open until
/// the outcome is on it. So the guard has to outlive the instrumented future,
/// which the call sites arrange by declaring it first.
pub(crate) struct EffectOutcomeGuard {
    span: tracing::Span,
    outcome: EffectOutcome,
    failed: bool,
    evidence: Option<EffectEvidence>,
}

impl EffectOutcomeGuard {
    pub(crate) fn new(span: &tracing::Span, evidence: Option<EffectEvidence>) -> Self {
        Self {
            span: span.clone(),
            outcome: EffectOutcome::Cancelled,
            failed: true,
            evidence,
        }
    }

    /// Name the observed outcome and whether the capability returned an error.
    ///
    /// A successful response contributes no failure evidence, even though both
    /// success and some backend refusals are classified as `responded`.
    pub(crate) fn settle(&mut self, outcome: EffectOutcome, failed: bool) {
        self.outcome = outcome;
        self.failed = failed;
    }
}

impl Drop for EffectOutcomeGuard {
    fn drop(&mut self) {
        self.span.record(EFFECT_OUTCOME, self.outcome.label());
        if self.failed
            && let Some(evidence) = &self.evidence
        {
            evidence.record_failure(self.outcome);
        }
    }
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
/// instead of repeating the string.
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
    BLOBSTORE_DURATION_MS,
    "wamn-blobstore",
    "wamn.blobstore.duration_ms",
    "wasmcloud:blobstore effect latency in ms, by effect.operation"
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
        span.record("wamn.requirement", run.requirement);
    }
}

/// Copy one bound invocation onto the fields [`effect_span`] declared `Empty`,
/// so an effect span answers "which package, which wiring, which node, which
/// visit" on its own instead of only through its `wamn.component.invoke` parent.
///
/// Called by the surface AFTER the macro rather than from inside it. The macro
/// takes no wiring argument on purpose: widening its arity would force the two
/// surfaces that cannot source these coordinates to pass a placeholder they have
/// no way to compute, which is exactly the premise `wamn-0h0g.24.12`'s owner
/// ruling refuted.
///
/// `None` records the eight keys EMPTY rather than leaving them unfilled,
/// following [`EffectIdentity`]'s convention: one span shape per surface, where
/// an empty value reads as "this effect holds no such claim" and never as "the
/// enrichment was dropped".
pub(crate) fn record_wiring(span: &tracing::Span, wiring: Option<EffectWiring<'_>>) {
    let wiring = wiring.unwrap_or(EffectWiring {
        package_id: "",
        wiring_id: "",
        wiring_version: 0,
        node_id: "",
        occurrence: 0,
        component_digest: "",
        component_name: "",
        operation: "",
    });
    span.record("wamn.package_id", wiring.package_id);
    span.record("wamn.wiring_id", wiring.wiring_id);
    span.record("wamn.wiring_version", wiring.wiring_version);
    span.record("wamn.node_id", wiring.node_id);
    span.record("wamn.occurrence", wiring.occurrence);
    span.record("wamn.component_digest", wiring.component_digest);
    span.record("wamn.component_name", wiring.component_name);
    span.record("wamn.operation", wiring.operation);
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
/// A surface that can source its wiring position calls [`record_wiring`] on the
/// returned span; the macro declares those fields but never fills them, because
/// only the call site knows whether it holds an invocation. `effect.outcome` is
/// declared the same way and filled by an [`EffectOutcomeGuard`] the call site
/// builds from the span before it runs the effect.
///
/// The caller instruments the awaited effect with the returned span
/// (`future.instrument(span).await`); entering it around a synchronous prelude
/// would not cover the await that is the effect.
///
/// The span is emitted through the process's global `tracing` subscriber, which
/// the runtime's `initialize_observability` bridges to OTel and exports over OTLP
/// when `OTEL_*` is set — so it nests under whatever span is current and threads
/// into that trace. Enriching a host-created span keeps 9.1 wamn-side, with no
/// runtime patch.
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
            wamn.package_id = tracing::field::Empty,
            wamn.wiring_id = tracing::field::Empty,
            wamn.wiring_version = tracing::field::Empty,
            wamn.node_id = tracing::field::Empty,
            wamn.occurrence = tracing::field::Empty,
            wamn.component_digest = tracing::field::Empty,
            wamn.component_name = tracing::field::Empty,
            wamn.operation = tracing::field::Empty,
            wamn.run_id = tracing::field::Empty,
            wamn.requirement = tracing::field::Empty,
            effect.outcome = tracing::field::Empty,
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

/// Span-shape proof support for the surfaces that fill this vocabulary.
///
/// It lives beside the vocabulary and not beside one surface. Every surface
/// freezes the WHOLE span value as a literal, so a second copy of the reader
/// could disagree with the first about what a trace reader receives.
#[cfg(test)]
pub(crate) mod span_tests {
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};

    /// The spans one call exported, read back through an in-memory exporter, so
    /// an assertion names what a trace reader RECEIVES rather than what the call
    /// site wrote.
    ///
    /// The layer's own bookkeeping attributes — tracing target, source location,
    /// thread, and busy/idle timings — are switched off, so the exported set is
    /// exactly the span's declared fields and a field the enrichment should not
    /// carry cannot hide among them.
    pub(crate) struct SpanHarness {
        exporter: InMemorySpanExporter,
        provider: SdkTracerProvider,
        _guard: tracing::subscriber::DefaultGuard,
    }

    impl SpanHarness {
        pub(crate) fn install(tracer: &'static str) -> Self {
            use opentelemetry::trace::TracerProvider as _;
            use tracing_subscriber::layer::SubscriberExt as _;

            let exporter = InMemorySpanExporter::default();
            let provider = SdkTracerProvider::builder()
                .with_simple_exporter(exporter.clone())
                .build();
            let layer = tracing_opentelemetry::layer()
                .with_tracer(provider.tracer(tracer))
                .with_target(false)
                .with_location(false)
                .with_threads(false)
                .with_tracked_inactivity(false);
            let guard =
                tracing::subscriber::set_default(tracing_subscriber::registry().with(layer));
            Self {
                exporter,
                provider,
                _guard: guard,
            }
        }

        /// Every span of one name, in the order the exporter finished them, each
        /// as its sorted attribute set.
        ///
        /// A test that opens two spans reads both. Two calls that record the
        /// same attributes are one repeated value here, which is what makes
        /// "these two calls are indistinguishable" a failing assertion.
        pub(crate) fn every_span(&self, name: &str) -> Vec<Vec<(String, String)>> {
            self.provider.force_flush().expect("test spans must flush");
            self.exporter
                .get_finished_spans()
                .expect("test span exporter must remain readable")
                .iter()
                .filter(|span| span.name == name)
                .map(|span| {
                    let mut attributes: Vec<(String, String)> = span
                        .attributes
                        .iter()
                        .map(|attr| (attr.key.to_string(), attr.value.to_string()))
                        .collect();
                    attributes.sort();
                    attributes
                })
                .collect()
        }

        /// The one span of that name, for a test that opened exactly one.
        pub(crate) fn attributes(&self, name: &str) -> Vec<(String, String)> {
            let mut spans = self.every_span(name);
            assert_eq!(spans.len(), 1, "exactly one {name} span must be exported");
            spans.pop().expect("the one exported span")
        }
    }

    /// One expected attribute set, sorted the way [`SpanHarness`] sorts.
    pub(crate) fn expected_attributes(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        pairs.sort();
        pairs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settled_evidence(evidence: &EffectEvidence, outcome: EffectOutcome, failed: bool) {
        let mut guard = EffectOutcomeGuard::new(&tracing::Span::none(), Some(evidence.clone()));
        guard.settle(outcome, failed);
    }

    #[test]
    fn one_failure_survives_successes_but_never_another_failure() {
        let evidence = EffectEvidence::new();
        let nested = evidence.clone();
        assert_eq!(evidence, nested);
        assert_ne!(evidence, EffectEvidence::new());
        assert_eq!(evidence.outcome(), None);
        settled_evidence(&evidence, EffectOutcome::Responded, false);
        assert_eq!(evidence.outcome(), None);
        settled_evidence(&nested, EffectOutcome::Timeout, true);
        settled_evidence(&evidence, EffectOutcome::Responded, false);
        assert_eq!(evidence.outcome(), Some(EffectOutcome::Timeout));
        // Even equal labels are two attempts, not corroborating evidence.
        settled_evidence(&nested, EffectOutcome::Timeout, true);
        assert_eq!(evidence.outcome(), None);
        settled_evidence(&evidence, EffectOutcome::RefusedBeforeDispatch, true);
        assert_eq!(evidence.outcome(), None);
        assert_eq!(EffectEvidence::new().outcome(), None);
    }

    #[test]
    fn a_responded_backend_refusal_is_failed_evidence() {
        let evidence = EffectEvidence::new();
        settled_evidence(&evidence, EffectOutcome::Responded, true);
        assert_eq!(evidence.outcome(), Some(EffectOutcome::Responded));
    }

    #[test]
    fn a_dropped_unsettled_guard_contributes_one_cancelled_attempt() {
        let evidence = EffectEvidence::new();
        drop(EffectOutcomeGuard::new(
            &tracing::Span::none(),
            Some(evidence.clone()),
        ));
        assert_eq!(evidence.outcome(), Some(EffectOutcome::Cancelled));
        drop(EffectOutcomeGuard::new(
            &tracing::Span::none(),
            Some(evidence.clone()),
        ));
        assert_eq!(evidence.outcome(), None);
    }

    /// One effect span, opened and settled the way a surface settles it, read
    /// back as the value `effect.outcome` carries.
    fn observed_outcome(tracer: &'static str, settled: Option<EffectOutcome>) -> String {
        let harness = span_tests::SpanHarness::install(tracer);
        {
            let span = effect_span!(
                "wamn.outcome_test",
                EffectIdentity {
                    tenant: "tenant-a",
                    project: "project-a",
                    component: "component-a",
                },
                None,
                effect.operation = "probe",
            );
            let mut guard = EffectOutcomeGuard::new(&span, None);
            if let Some(outcome) = settled {
                guard.settle(outcome, true);
            }
        }
        harness
            .attributes("wamn.outcome_test")
            .into_iter()
            .find(|(key, _)| key.as_str() == EFFECT_OUTCOME)
            .expect("every effect span carries an outcome")
            .1
    }

    /// An effect the platform declined reads as refused before dispatch, which
    /// says the platform sent nothing.
    #[test]
    fn an_effect_the_platform_declined_records_refused_before_dispatch() {
        assert_eq!(
            observed_outcome(
                "outcome-refused-test",
                Some(EffectOutcome::RefusedBeforeDispatch)
            ),
            "refused-before-dispatch",
        );
    }

    /// An effect the far side answered reads as responded, whatever that answer
    /// said.
    #[test]
    fn an_effect_the_far_side_answered_records_responded() {
        assert_eq!(
            observed_outcome("outcome-responded-test", Some(EffectOutcome::Responded)),
            "responded",
        );
    }

    /// An effect whose deadline really elapsed reads as timeout, and a reader
    /// learns nothing about the far side from it.
    #[test]
    fn an_effect_whose_deadline_elapsed_records_timeout() {
        assert_eq!(
            observed_outcome("outcome-timeout-test", Some(EffectOutcome::Timeout)),
            "timeout",
        );
    }

    /// An effect abandoned before it settled reads as cancelled. Nothing calls
    /// `settle` on that path, so the guard's own `Drop` is what records it.
    #[test]
    fn an_effect_abandoned_before_it_settled_records_cancelled() {
        assert_eq!(
            observed_outcome("outcome-cancelled-test", None),
            "cancelled"
        );
    }

    /// An attempt the platform sent and holds no outcome for reads as
    /// effect-uncertain, which is the state the durable shelf contract names.
    #[test]
    fn an_attempt_with_no_recorded_outcome_records_effect_uncertain() {
        assert_eq!(
            observed_outcome(
                "outcome-uncertain-test",
                Some(EffectOutcome::EffectUncertain)
            ),
            "effect-uncertain",
        );
    }

    /// An answer the far side sent and the guest never received reads as
    /// response-lost, the word whose remedy is to re-read.
    #[test]
    fn an_answer_that_never_reached_the_guest_records_response_lost() {
        assert_eq!(
            observed_outcome(
                "outcome-response-lost-test",
                Some(EffectOutcome::ResponseLost)
            ),
            "response-lost",
        );
    }

    /// The six words are the whole set. None of them names a rollback, an undo
    /// or a revert, because no effect observation states what a command did.
    #[test]
    fn no_outcome_word_claims_a_command_was_undone() {
        for outcome in EffectOutcome::ALL {
            let label = outcome.label();
            for forbidden in ["rollback", "rolled", "undo", "undone", "revert"] {
                assert!(
                    !label.contains(forbidden),
                    "{label} claims {forbidden}, which the platform does not know"
                );
            }
        }
    }
}
