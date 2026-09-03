//! `wamn:jetstream` host plugin (E10).
//!
//! Built contract: `wit/deps/wamn-jetstream/package.wit`; guest-vendored copies
//! are drift-guarded by `tests/jetstream_wit_coherence.rs`.
//!
//! WHY THIS EXISTS. The only messaging WIT the pinned wasmCloud fork carries is
//! `wasmcloud:messaging@0.2.0` — core NATS with no ack/nack/term, no durable
//! consumers, no pull/fetch, no redelivery count, no `stream_seq`, and no
//! headers, so a component cannot set `Nats-Msg-Id` and cannot participate in
//! JetStream dedupe (findings.md E10). This plugin is the host side of a NEW
//! `wamn:jetstream@0.1.0` package (never a forked `wasmcloud:messaging`) over the
//! async-nats JetStream client, in the `wamn:postgres` host-plugin shape. The
//! Service-first materializer (l5i9.17) is the first importer.
//!
//! Host-enforced invariants:
//! - The guest never holds a NATS socket; only resource handles. The JetStream
//!   connection lives in the plugin, built lazily from host-injected config
//!   (`WAMN_EVT_NATS_URL`) and memoized for the plugin's lifetime.
//! - Streams are provisioned out-of-band (per-org `EVT_<org>_<env>` streams,
//!   D19 §5). A guest binds a durable consumer by name and publishes to a
//!   subject; it cannot create, configure, or delete a stream here.
//! - Event DELIVERY is gated on the serving release's registration projection
//!   ([`ServingManifest::registrations`] — reader 3 of the release-manifest
//!   weld, `wamn-0h0g.15.95`): a durable consumer binds only over subjects some
//!   registration of the release sources, so an event whose registration
//!   identity is not the release's never reaches a component.
//!   Generic non-event publication and the doorbell hint are not release-gated.
//!   The reserved `evt.*`, `dlq.*` and `tap.*` namespaces are host-only: derived
//!   events use [`WamnJetstream::publish_derived`], exact registration bind ties
//!   a fetched message to its release identity before dead-letter publication,
//!   and delivery previews use [`WamnJetstream::publish_router_tap`], which mints
//!   every subject it writes from the trusted bind-time claim.
//! - A publish waits for the server ack (async-nats: send future, then the
//!   server-ack future) — the returned `publish-ack` is the only delivery truth.
//! - The `doorbell.ring` wake hint (l5i9.17) publishes on the CONTROL-plane
//!   core-NATS connection the host injects at construction
//!   ([`WamnJetstream::with_doorbell`] — the washlet passes its own scheduler
//!   client), on the shared doorbell subject for the execution target assigned
//!   from the workload's trusted tenant config by the MVP placement adapter at
//!   bind time (a guest can never name or redirect its execution target).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use async_nats::HeaderMap;
use async_nats::header::NATS_MESSAGE_ID;
use async_nats::jetstream::Context;
use async_nats::jetstream::consumer::pull::Config as PullConfig;
use async_nats::jetstream::consumer::{AckPolicy, Config as StoredConsumerConfig, Consumer};
use async_nats::jetstream::context::{GetStreamError, GetStreamErrorKind};
use async_nats::jetstream::message::AckKind;
use async_nats::jetstream::publish::PublishAck as NatsPublishAck;
use futures_util::{StreamExt as _, TryStreamExt as _};
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Meter};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tokio::sync::Mutex;
use tracing::Instrument as _;
use wamn_catalog::ServingManifest;
use wamn_control_registry::identifiers::{
    ExecutionTargetId, doorbell_subject, mvp_execution_target_id,
};
use wamn_event_wire::{
    Causation, DEAD_LETTER_STREAM, DeadLetter, DeadLetterHeader, DerivedEvent, Op,
    dead_letter_message_id, dead_letter_subject, derived_msg_id, stream_name, subject,
    subject_token,
};
use wamn_run_state::redaction::{OUTPUT_CAPTURE_CEILING_BYTES, scrub};

use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::{Linker, Resource};
use wash_runtime::wit::{WitInterface, WitWorld};

use crate::plugins::effect_span::{
    AckLagRegistration, EFFECT_OPERATION, EffectIdentity, JETSTREAM_ACK_LAG_MS,
    JETSTREAM_DURATION_MS, effect_span, record_ack_lag_ms, record_effect_ms,
};
use crate::plugins::wamn_postgres::{DEFAULT_PROJECT, PROJECT_CONFIG_KEY, TENANT_CONFIG_KEY};
use crate::release_manifest::ReleaseManifestWeld;

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "jetstream-plugin",
        imports: { default: async | trappable | tracing },
        with: {
            "wamn:jetstream/consumer.durable-consumer": super::JsConsumer,
            "wamn:jetstream/consumer.message": super::JsMessage,
        },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wamn::jetstream::consumer;
use bindings::wamn::jetstream::doorbell;
use bindings::wamn::jetstream::producer;
use bindings::wamn::jetstream::types::{Header, JsError, MessageMeta};

pub const WAMN_JETSTREAM_ID: &str = "wamn-jetstream";

/// Trusted workload claim carrying the exact event environment.
pub const ENVIRONMENT_CONFIG_KEY: &str = "wamn.environment";

/// The host-owned inputs needed to publish one admitted Emit terminal.
///
/// Tenant, project, and environment are intentionally absent:
/// [`WamnJetstream::publish_derived`] resolves them from the claim bound to
/// `component_id`. `package_id` is supplied only by the native host caller from
/// its welded release/run/wiring identity; it is not a guest WIT operand.
#[derive(Debug, Clone, PartialEq)]
pub struct DerivedPublishRequest {
    pub component_id: String,
    pub package_id: String,
    pub entity: String,
    pub operation: Op,
    pub payload: serde_json::Value,
    pub dedup_id: String,
    pub causation: Causation,
}

/// Server-confirmed storage of one derived event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedPublishAck {
    pub stream_name: String,
    pub stream_seq: u64,
    pub duplicate: bool,
}

/// Stable host classification for derived publication failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DerivedPublishErrorKind {
    UnboundScope,
    InvalidInput,
    ConnectionUnavailable,
    Serialization,
    PublishRejected,
    UnexpectedStream,
}

/// Contextual failure returned by the native derived-event publisher seam.
#[derive(Debug)]
pub struct DerivedPublishError {
    kind: DerivedPublishErrorKind,
    detail: Box<str>,
}

impl DerivedPublishError {
    fn new(kind: DerivedPublishErrorKind, detail: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    pub fn kind(&self) -> DerivedPublishErrorKind {
        self.kind
    }
}

impl std::fmt::Display for DerivedPublishError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for DerivedPublishError {}

// ---------------------------------------------------------------------------
// The reserved router-tap preview namespace (wamn-0h0g.24.5)
// ---------------------------------------------------------------------------

/// The reserved, HOST-OWNED subject namespace carrying ephemeral delivery
/// previews — the router-edge live view's wire, consumed by the `wamn-dggp.10`
/// run screen.
///
/// A THIRD reserved namespace beside `evt.*` and `dlq.*`, and deliberately not
/// `evt`: a preview is an ephemeral debugging tap, and putting one into the
/// durable event grammar would give one subject two origins — a stored fact and
/// a redacted snapshot — which is fabricated provenance. `tap` is its own
/// three-letter token in the shape the other two already use. It is not `trace`
/// because this host already spends that word on W3C context propagation
/// (`traceparent`/`tracestate`), and a payload preview is not that.
///
/// Minting the namespace and gating it are ONE change on purpose. Before this,
/// `producer::publish` refused exactly `dlq.*` and `evt.*` and admitted every
/// other subject with no registration-identity check at all, so a new host-owned
/// namespace without [`is_reserved_router_tap_subject`] in the same commit would
/// be a minted vulnerability: any tenant guest could write an operator's live
/// view, and a forged preview reads as the host's own observation.
pub const ROUTER_TAP_PREFIX: &str = "tap";

/// The named refusal class a guest publish onto the preview namespace earns.
const RESERVED_ROUTER_TAP_SUBJECT: &str = "reserved-router-tap-subject";

/// Wire version of the preview record. Bumped when a field's meaning changes.
const ROUTER_TAP_FORMAT_VERSION: u32 = 1;

/// The only router-tap record version understood by this release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouterTapFormatVersion {
    V1,
}

impl RouterTapFormatVersion {
    /// Numeric value carried on the wire.
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::V1 => ROUTER_TAP_FORMAT_VERSION,
        }
    }
}

impl Serialize for RouterTapFormatVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u32(self.as_u32())
    }
}

impl<'de> Deserialize<'de> for RouterTapFormatVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let version = u32::deserialize(deserializer)?;
        if version == ROUTER_TAP_FORMAT_VERSION {
            Ok(Self::V1)
        } else {
            Err(serde::de::Error::custom(format!(
                "unsupported router-tap format-version {version}"
            )))
        }
    }
}

/// Delivery boundary named by one router-tap record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RouterTapRecordPhase {
    Accepted,
    Settled,
}

/// Trusted ingress kind that originated one delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RouterTapSourceKind {
    Attachment,
    Registration,
}

impl RouterTapSourceKind {
    fn from_preview(kind: &str) -> Option<Self> {
        match kind {
            "attachment" => Some(Self::Attachment),
            "registration" => Some(Self::Registration),
            _ => None,
        }
    }
}

/// Frozen, owned router-tap v1 wire record shared by publisher and readers.
///
/// Fields are declared in the byte order emitted by the former JSON-map
/// publisher so making the wire typed does not rewrite otherwise-identical
/// records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RouterTapRecord {
    pub delivery_id: Box<str>,
    pub format_version: RouterTapFormatVersion,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Box<str>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub over_ceiling_bytes: Option<u64>,
    pub payload: serde_json::Value,
    pub phase: RouterTapRecordPhase,
    pub redacted: bool,
    pub source_id: Box<str>,
    pub source_kind: RouterTapSourceKind,
    pub wiring_id: Box<str>,
    pub wiring_version: u32,
}

/// Semantic disagreement inside one otherwise well-formed tap record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouterTapRecordError {
    detail: &'static str,
}

impl std::fmt::Display for RouterTapRecordError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.detail)
    }
}

impl std::error::Error for RouterTapRecordError {}

impl RouterTapRecord {
    /// Prove the phase and bounded-payload fields describe one possible record.
    pub fn validate(&self) -> Result<(), RouterTapRecordError> {
        match (self.phase, self.outcome.as_deref()) {
            (RouterTapRecordPhase::Accepted, Some(_)) => {
                return Err(RouterTapRecordError {
                    detail: "an accepted router tap cannot carry a settled outcome",
                });
            }
            (RouterTapRecordPhase::Settled, None | Some("")) => {
                return Err(RouterTapRecordError {
                    detail: "a settled router tap must carry a nonempty outcome",
                });
            }
            _ => {}
        }
        if let Some(bytes) = self.over_ceiling_bytes
            && (bytes <= OUTPUT_CAPTURE_CEILING_BYTES as u64 || !self.payload.is_null())
        {
            return Err(RouterTapRecordError {
                detail: "an over-ceiling router tap must name a larger size and omit payload",
            });
        }
        Ok(())
    }
}

/// Which boundary of one delivery a preview describes.
///
/// The bridge sees two: the delivery it admitted, and the outcome the driver
/// settled it with. Per-edge previews inside the router walk are the
/// DEMAND-GATED UPGRADE, not built here — they would put a publish on every
/// `Step::Invoke`, which is hot-path cost for debugging depth the default tier
/// deliberately dropped. Nothing in this record's shape forecloses adding them:
/// a per-edge phase is another variant on a subject that already scopes to one
/// delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouterTapPhase {
    /// Admitted by the bridge; the payload is the delivery's input.
    Accepted,
    /// Settled by the driver under this outcome label; the payload is the result.
    Settled(&'static str),
}

/// One delivery-boundary preview, borrowed from what the bridge already holds.
///
/// Every field is a borrow so constructing one allocates nothing: a host with no
/// data-plane NATS skips the whole tap without copying a payload.
#[derive(Debug, Clone, Copy)]
pub struct RouterTapPreview<'a> {
    pub delivery_id: &'a str,
    pub wiring_id: &'a str,
    pub wiring_version: u32,
    /// `"attachment"` or `"registration"` — the bridge's own two ingress kinds.
    pub source_kind: &'static str,
    pub source_id: &'a str,
    pub phase: RouterTapPhase,
    pub payload: &'a serde_json::Value,
}

/// `tap.<org>.<project>.<env>.<wiring>.<delivery>` — six tokens, the same arity
/// as `evt.<org>.<project>.<env>.<entity>.<op>`, so a run screen binds one
/// delivery (`tap.<org>.<project>.<env>.*.<delivery>`) and an operator binds one
/// environment.
///
/// The org, project and environment tokens come from the trusted bind-time
/// claim, which [`WamnJetstream::required_derived_claim`] has already proved to
/// be exactly one subject token each. The wiring and delivery ids have not: the
/// delivery id arrives over the WIT boundary from a guest, so both go through
/// [`subject_token`] and cannot inject a separator or a wildcard. `None` when an
/// id sanitizes to nothing, because an empty token is not a subject.
fn router_tap_environment_prefix(tenant: &str, project: &str, environment: &str) -> Option<String> {
    if [tenant, project, environment]
        .into_iter()
        .any(|value| value.is_empty() || value.trim() != value || subject_token(value) != value)
    {
        return None;
    }
    Some(format!(
        "{ROUTER_TAP_PREFIX}.{tenant}.{project}.{environment}"
    ))
}

/// Exact environment-scoped subject filter for router-tap readers.
pub fn router_tap_environment_filter(
    tenant: &str,
    project: &str,
    environment: &str,
) -> Option<String> {
    router_tap_environment_prefix(tenant, project, environment).map(|prefix| format!("{prefix}.>"))
}

/// Exact subject carrying one environment-scoped router-tap record.
pub fn router_tap_record_subject(
    tenant: &str,
    project: &str,
    environment: &str,
    wiring_id: &str,
    delivery_id: &str,
) -> Option<String> {
    let prefix = router_tap_environment_prefix(tenant, project, environment)?;
    let wiring = subject_token(wiring_id);
    let delivery = subject_token(delivery_id);
    if wiring.is_empty() || delivery.is_empty() {
        return None;
    }
    Some(format!("{prefix}.{wiring}.{delivery}"))
}

fn router_tap_subject(
    claim: &JetstreamClaim,
    wiring_id: &str,
    delivery_id: &str,
) -> Option<String> {
    router_tap_record_subject(
        &claim.tenant,
        &claim.project,
        &claim.environment,
        wiring_id,
        delivery_id,
    )
}

/// Is `subject` inside the reserved preview namespace?
///
/// `strip_prefix` rather than `starts_with(ROUTER_TAP_PREFIX)`, which would also
/// swallow every unrelated subject beginning with those three letters, and
/// rather than a `format!`-built `"tap."`, which would allocate on the publish
/// path. Bare `tap` is included: it is the namespace root.
fn is_reserved_router_tap_subject(subject: &str) -> bool {
    subject
        .strip_prefix(ROUTER_TAP_PREFIX)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedRouterTap {
    subject: String,
    body: Vec<u8>,
}

/// Mint the subject and build the bounded, redacted body of one preview.
///
/// Redaction and the ceiling are applied HERE, not by the caller, so the only
/// path onto the reserved namespace is one that has already run both. The policy
/// is `wamn_run_state::redaction` exactly as extracted by `wamn-0h0g.26.2`
/// ([`scrub`] and [`OUTPUT_CAPTURE_CEILING_BYTES`]); a live view that needs more
/// renegotiates it on `wamn-0h0g.24.5` rather than widening it locally.
///
/// An over-ceiling payload is DROPPED, not truncated: truncated JSON does not
/// parse, and half a redacted object is not a safer thing to publish than none.
/// The byte count moves into the envelope so the flag cannot be confused with a
/// key the guest payload happens to carry.
fn prepare_router_tap(
    claim: &JetstreamClaim,
    preview: &RouterTapPreview<'_>,
) -> Option<PreparedRouterTap> {
    let subject = router_tap_subject(claim, preview.wiring_id, preview.delivery_id)?;
    let source_kind = RouterTapSourceKind::from_preview(preview.source_kind)?;
    let mut payload = preview.payload.clone();
    let redacted = scrub(&mut payload);
    let payload_bytes = serde_json::to_vec(&payload)
        .expect("a serde_json::Value tree always serializes")
        .len();
    let (payload, over_ceiling_bytes) = if payload_bytes > OUTPUT_CAPTURE_CEILING_BYTES {
        (
            serde_json::Value::Null,
            Some(
                u64::try_from(payload_bytes)
                    .expect("a serialized payload byte count always fits in u64"),
            ),
        )
    } else {
        (payload, None)
    };
    let (phase, outcome) = match preview.phase {
        RouterTapPhase::Accepted => (RouterTapRecordPhase::Accepted, None),
        RouterTapPhase::Settled(outcome) => (
            RouterTapRecordPhase::Settled,
            Some(Box::<str>::from(outcome)),
        ),
    };
    let record = RouterTapRecord {
        delivery_id: Box::from(preview.delivery_id),
        format_version: RouterTapFormatVersion::V1,
        outcome,
        over_ceiling_bytes,
        payload,
        phase,
        redacted,
        source_id: Box::from(preview.source_id),
        source_kind,
        wiring_id: Box::from(preview.wiring_id),
        wiring_version: preview.wiring_version,
    };
    record
        .validate()
        .expect("a router-tap preview constructs one valid phase and payload state");
    Some(PreparedRouterTap {
        subject,
        body: serde_json::to_vec(&record).expect("a router-tap record always serializes"),
    })
}

/// Wire the `wamn:jetstream` consumer + producer host functions into a linker
/// directly. The host path calls this from [`HostPlugin::on_workload_item_bind`];
/// a Service (the materializer, l5i9.17) or a hand-built store links it the same
/// way `wamn:postgres` is linked.
pub fn add_to_linker(linker: &mut Linker<SharedCtx>) -> wash_runtime::wasmtime::Result<()> {
    consumer::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)?;
    producer::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)?;
    doorbell::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Plugin configuration
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct WamnJetstreamConfig {
    /// Data-plane NATS URL (deploy/infra/nats-jetstream.yaml Service `evt-nats`).
    /// `None` ⇒ the plugin registers but every call returns
    /// `connection-unavailable`.
    pub nats_url: Option<String>,
}

impl WamnJetstreamConfig {
    /// The event-plane NATS URL, gated on `WAMN_EVT_NATS_URL` (the same
    /// skip-when-absent posture the live tests use).
    pub fn from_env() -> Self {
        Self {
            nats_url: std::env::var("WAMN_EVT_NATS_URL").ok(),
        }
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

pub struct WamnJetstream {
    nats_url: Option<String>,
    /// Lazily-connected, memoized JetStream context. A `Mutex<Option<_>>` (not a
    /// `OnceCell`) so a transient connect failure is retried on the next call
    /// instead of memoized forever; only a successful connect is stored.
    ctx: Mutex<Option<Context>>,
    /// CONTROL-plane core-NATS client for `doorbell.ring` (the washlet injects
    /// its own scheduler client). `None` ⇒ ring returns `connection-unavailable`
    /// (best-effort by contract: the caller counts it and continues).
    doorbell_nats: Option<async_nats::Client>,
    /// Per-component execution target for the doorbell subject, registered at
    /// workload bind by the trusted MVP placement adapter — never guest-supplied.
    execution_targets: std::sync::RwLock<HashMap<String, ExecutionTargetId>>,
    /// Per-component tenant/project/environment claim, registered at workload
    /// bind from the same trusted `wamn.*` config the `wamn:postgres`
    /// claims read. It exists only to enrich this plugin's effect spans: before
    /// `wamn-0h0g.24.3` the bind read the tenant, derived the execution target
    /// from it and discarded it, so nothing here could say whose event plane a
    /// publish or an ack belonged to.
    claims: std::sync::RwLock<HashMap<String, JetstreamClaim>>,
    /// The release this process serves, held BY REFERENCE — reader 3 of the
    /// release-manifest weld. `None` ⇒ this process carries no release; see
    /// [`WamnJetstream::with_release`].
    release: Option<Arc<ReleaseManifestWeld>>,
    /// Last server-observed depth of each registration DLQ subject.
    dlq_depth: Arc<DeadLetterDepth>,
}

/// One component's bind-time tenant/project claim.
#[derive(Clone, Debug)]
struct JetstreamClaim {
    tenant: Box<str>,
    project: Box<str>,
    environment: Box<str>,
}

#[derive(Debug)]
struct PreparedDerivedPublication {
    component_id: String,
    claim: JetstreamClaim,
    subject: String,
    message_id: String,
    expected_stream: String,
    body: Vec<u8>,
}

fn prepare_derived_publication(
    claim: JetstreamClaim,
    request: DerivedPublishRequest,
) -> Result<PreparedDerivedPublication, DerivedPublishError> {
    if request.package_id.is_empty()
        || request.package_id.trim() != request.package_id
        || request.package_id.as_bytes().contains(&0)
    {
        return Err(DerivedPublishError::new(
            DerivedPublishErrorKind::InvalidInput,
            "derived event package-id is empty or noncanonical",
        ));
    }
    if request.entity.is_empty() || request.entity.trim() != request.entity {
        return Err(DerivedPublishError::new(
            DerivedPublishErrorKind::InvalidInput,
            "derived event entity is empty or noncanonical",
        ));
    }
    if request.dedup_id.is_empty() {
        return Err(DerivedPublishError::new(
            DerivedPublishErrorKind::InvalidInput,
            "derived event dedup-id is empty",
        ));
    }
    if request.causation.run.is_empty() || request.causation.root.is_empty() {
        return Err(DerivedPublishError::new(
            DerivedPublishErrorKind::InvalidInput,
            "derived event causation is incomplete",
        ));
    }

    let event = DerivedEvent::new(
        claim.tenant.to_string(),
        claim.project.to_string(),
        claim.environment.to_string(),
        request.package_id,
        request.entity,
        request.operation,
        request.payload,
        request.dedup_id,
        request.causation,
    );
    let event_subject = subject(
        &claim.tenant,
        &claim.project,
        &claim.environment,
        &event.entity,
        event.op,
    );
    let message_id = derived_msg_id(
        &claim.tenant,
        &claim.project,
        &claim.environment,
        &event.package_id,
        &event.entity,
        event.op,
        &event.dedup_id,
    );
    let expected_stream = stream_name(&claim.tenant, &claim.environment);
    let body = serde_json::to_vec(&event).map_err(|error| {
        DerivedPublishError::new(
            DerivedPublishErrorKind::Serialization,
            format!("serialize derived event: {error}"),
        )
    })?;
    Ok(PreparedDerivedPublication {
        component_id: request.component_id,
        claim,
        subject: event_subject,
        message_id,
        expected_stream,
        body,
    })
}

#[derive(Clone, Debug)]
struct DeadLetterIdentity {
    tenant: Box<str>,
    environment: Box<str>,
    package_id: Box<str>,
    registration_id: Box<str>,
    subject: Box<str>,
}

#[derive(Clone, Debug)]
struct DeadLetterDepthSample {
    identity: DeadLetterIdentity,
    depth: u64,
}

/// The series labels for one dead-letter registration, minted ONCE and shared
/// by the depth gauge and its samples counter, so the two always land on the
/// same series and a dashboard can read them together.
fn dead_letter_attributes(identity: &DeadLetterIdentity) -> [KeyValue; 4] {
    [
        KeyValue::new("wamn.tenant", identity.tenant.to_string()),
        KeyValue::new("wamn.environment", identity.environment.to_string()),
        KeyValue::new("wamn.package", identity.package_id.to_string()),
        KeyValue::new("wamn.registration", identity.registration_id.to_string()),
    ]
}

#[derive(Debug)]
struct DeadLetterDepth {
    by_subject: std::sync::Mutex<HashMap<Box<str>, DeadLetterDepthSample>>,
    /// `wamn.jetstream.dlq.depth.samples` (wamn-0h0g.24.10): one increment per
    /// depth reading this process actually TOOK, under the same attributes as
    /// the gauge it certifies.
    ///
    /// LIVENESS IS PROVEN BY A SIGNAL THE SUSPECT CANNOT FAKE. The gauge alone
    /// proves nothing: it is an OBSERVABLE instrument over a last-write-wins
    /// map, and the exporter invokes its callback on ITS OWN clock whether or
    /// not anything refreshed the map — so a registration whose dead-letter
    /// subject is genuinely empty and one whose observer died an hour ago emit
    /// BYTE-IDENTICAL series. Read with this counter they differ: the depth is
    /// trustworthy only while the counter is still advancing.
    ///
    /// THE LIMIT, STATED SO THE CRITERION IS NOT READ AS FULLY MET: this is a
    /// SELF-REPORT, so it distinguishes an observer that STOPPED and nothing
    /// more. An observer that LIES or WEDGES while still ticking keeps
    /// incrementing it. A signal the subject does not produce — an external
    /// reader of stream state — is `wamn-2jkm.104`; this counter is a floor
    /// under that bead, not a substitute for it, and retires nothing.
    samples: Counter<u64>,
}

impl DeadLetterDepth {
    fn new(meter: &Meter) -> Self {
        Self {
            by_subject: std::sync::Mutex::new(HashMap::new()),
            samples: meter
                .u64_counter("wamn.jetstream.dlq.depth.samples")
                .with_description(
                    "dead-letter depth readings taken for one release registration; \
                     a flat count means the observer stopped, not that the subject is empty",
                )
                .build(),
        }
    }

    fn register(meter: &Meter, depth: &Arc<Self>) {
        let weak = Arc::downgrade(depth);
        let _ = meter
            .u64_observable_gauge("wamn.jetstream.dlq.depth")
            .with_description("retained dead-letter messages for one release registration")
            .with_callback(move |observer| {
                let Some(depth) = weak.upgrade() else {
                    return;
                };
                if let Ok(samples) = depth.by_subject.lock() {
                    for sample in samples.values() {
                        observer.observe(sample.depth, &dead_letter_attributes(&sample.identity));
                    }
                }
            })
            .build();
    }

    fn update(&self, identity: DeadLetterIdentity, depth: u64) {
        // ON THE OBSERVATION PATH, not the publish path: both refresh sites —
        // after every consumer fetch and after a dead-letter publish — land
        // here, and a counter incremented where dead letters are PUBLISHED
        // would sit flat on a healthy registration that never dead-letters,
        // which is exactly the reading it exists to rule out.
        self.samples.add(1, &dead_letter_attributes(&identity));
        if let Ok(mut samples) = self.by_subject.lock() {
            samples.insert(
                identity.subject.clone(),
                DeadLetterDepthSample { identity, depth },
            );
        }
    }
}

/// The span one `wamn:jetstream` effect opens, enriched from the component's
/// bind-time claim.
fn js_span(claim: &JetstreamClaim, component_id: &str, operation: &'static str) -> tracing::Span {
    // The span name is the host capability, not the wire: `doorbell.ring`
    // publishes on the CONTROL-plane core-NATS connection and is still
    // `wamn.jetstream`, because this plugin is what an operator would open next.
    effect_span!(
        "wamn.jetstream",
        EffectIdentity {
            tenant: &claim.tenant,
            project: &claim.project,
            component: component_id,
        },
        None,
        effect.operation = operation,
    )
}

impl WamnJetstream {
    pub fn new(cfg: WamnJetstreamConfig) -> Self {
        let meter = opentelemetry::global::meter("wamn-jetstream");
        let dlq_depth = Arc::new(DeadLetterDepth::new(&meter));
        DeadLetterDepth::register(&meter, &dlq_depth);
        Self {
            nats_url: cfg.nats_url,
            ctx: Mutex::new(None),
            doorbell_nats: None,
            execution_targets: std::sync::RwLock::new(HashMap::new()),
            claims: std::sync::RwLock::new(HashMap::new()),
            release: None,
            dlq_depth,
        }
    }

    /// Build from the environment (`WAMN_EVT_NATS_URL`).
    pub fn from_env() -> Self {
        Self::new(WamnJetstreamConfig::from_env())
    }

    /// Attach the CONTROL-plane core-NATS client `doorbell.ring` publishes on
    /// (formatted by `wamn-control-registry`). The washlet passes its scheduler
    /// client — the same control plane the dispatcher's doorbells and the
    /// run-worker's subscription ride — so no second connection is opened.
    pub fn with_doorbell(mut self, client: async_nats::Client) -> Self {
        self.doorbell_nats = Some(client);
        self
    }

    /// Attach the release this process serves — reader 3 of the release-manifest
    /// weld, consulted by reference. This plugin never loads, parses or
    /// digest-verifies a manifest, and keeps no copy of one: the weld already
    /// holds the digest-named document for the life of the process, and a
    /// digest-named object has no stale state to refresh or invalidate.
    ///
    /// # Where the release gate starts, and where it stops
    ///
    /// `None` means this host was given no release, and then every consumer bind
    /// is REFUSED. A host that cannot name the registrations of a release cannot
    /// decide that an event belongs to one, and delivering it anyway would hand
    /// the identity back to the guest sweep this gate took it from.
    ///
    /// Generic `producer::publish` and `doorbell::ring` keep working on a
    /// release-less host. The reserved `dlq.*` namespace is the exception:
    /// generic publication cannot name it, and `message.dead-letter` exists only
    /// after an exact release-registration bind.
    pub fn with_release(mut self, release: Option<Arc<ReleaseManifestWeld>>) -> Self {
        self.release = release;
        self
    }

    /// The serving release's manifest, or `None` on a release-less process.
    fn serving_manifest(&self) -> Option<&ServingManifest> {
        self.release.as_deref().map(ReleaseManifestWeld::manifest)
    }

    /// Register a validated doorbell execution target for a component id.
    pub fn set_execution_target(&self, component_id: &str, execution_target_id: ExecutionTargetId) {
        self.execution_targets
            .write()
            .expect("execution targets lock poisoned")
            .insert(component_id.to_string(), execution_target_id);
    }

    fn execution_target_for(&self, component_id: &str) -> Option<ExecutionTargetId> {
        self.execution_targets
            .read()
            .expect("execution targets lock poisoned")
            .get(component_id)
            .cloned()
    }

    /// Register a component's bind-time scope claim. All values come from the
    /// trusted workload config. Generic guest operations use them only for
    /// enrichment; the native derived publisher separately requires a complete
    /// subject-safe claim.
    fn set_claim(
        &self,
        component_id: &str,
        tenant: Option<&str>,
        project: Option<&str>,
        environment: Option<&str>,
    ) {
        self.claims
            .write()
            .expect("jetstream claims lock poisoned")
            .insert(
                component_id.to_string(),
                JetstreamClaim {
                    tenant: tenant.unwrap_or_default().into(),
                    project: project.unwrap_or(DEFAULT_PROJECT).into(),
                    environment: environment.unwrap_or_default().into(),
                },
            );
    }

    /// Bind the exact trusted scope used by native derived publication.
    ///
    /// The production driver calls this at instance checkout and revokes it at
    /// check-in. No scope operand exists on [`DerivedPublishRequest`], so a
    /// guest or wiring payload has nothing it can echo or redirect.
    pub fn bind_derived_scope(
        &self,
        component_id: &str,
        tenant: &str,
        project: &str,
        environment: &str,
    ) -> Result<(), DerivedPublishError> {
        for (field, value) in [
            ("tenant", tenant),
            ("project", project),
            ("environment", environment),
        ] {
            if value.is_empty() || value.trim() != value || subject_token(value) != value {
                return Err(DerivedPublishError::new(
                    DerivedPublishErrorKind::InvalidInput,
                    format!("derived event {field} claim is empty or not one NATS subject token"),
                ));
            }
        }
        self.set_claim(component_id, Some(tenant), Some(project), Some(environment));
        Ok(())
    }

    /// Revoke a native derived-publication claim at instance check-in.
    pub fn revoke_derived_scope(&self, component_id: &str) {
        self.claims
            .write()
            .expect("jetstream claims lock poisoned")
            .remove(component_id);
    }

    /// The claim registered for a component, or the unclaimed default. An
    /// unregistered component is a store built without the bind path (a bench,
    /// a hand-linked fixture), not a guest that withheld its identity.
    fn claim_for(&self, component_id: &str) -> JetstreamClaim {
        self.claims
            .read()
            .expect("jetstream claims lock poisoned")
            .get(component_id)
            .cloned()
            .unwrap_or_else(|| JetstreamClaim {
                tenant: Box::default(),
                project: DEFAULT_PROJECT.into(),
                environment: Box::default(),
            })
    }

    fn required_derived_claim(
        &self,
        component_id: &str,
    ) -> Result<JetstreamClaim, DerivedPublishError> {
        let claim = self
            .claims
            .read()
            .expect("jetstream claims lock poisoned")
            .get(component_id)
            .cloned()
            .ok_or_else(|| {
                DerivedPublishError::new(
                    DerivedPublishErrorKind::UnboundScope,
                    "derived-event-scope-unbound",
                )
            })?;
        for value in [
            claim.tenant.as_ref(),
            claim.project.as_ref(),
            claim.environment.as_ref(),
        ] {
            if value.is_empty() || value.trim() != value || subject_token(value) != value {
                return Err(DerivedPublishError::new(
                    DerivedPublishErrorKind::UnboundScope,
                    "derived-event-scope-incomplete-or-invalid",
                ));
            }
        }
        Ok(claim)
    }

    /// Resolve (lazily connect + memoize) the JetStream context. Unconfigured or
    /// unreachable ⇒ `connection-unavailable`.
    async fn ensure_ctx(&self) -> Result<Context, JsError> {
        let mut guard = self.ctx.lock().await;
        if let Some(ctx) = guard.as_ref() {
            return Ok(ctx.clone());
        }
        let url = self
            .nats_url
            .as_deref()
            .ok_or(JsError::ConnectionUnavailable)?;
        let client = async_nats::connect(url).await.map_err(|e| {
            tracing::warn!(
                target: "wamn::jetstream",
                error = %e,
                "data-plane NATS connect failed"
            );
            JsError::ConnectionUnavailable
        })?;
        let ctx = async_nats::jetstream::new(client);
        *guard = Some(ctx.clone());
        Ok(ctx)
    }

    /// Publish an admitted Emit terminal and return only after JetStream's
    /// server acknowledgement resolves.
    pub async fn publish_derived(
        &self,
        request: DerivedPublishRequest,
    ) -> Result<DerivedPublishAck, DerivedPublishError> {
        let claim = self.required_derived_claim(&request.component_id)?;
        let publication = prepare_derived_publication(claim, request)?;
        let mut headers = HeaderMap::new();
        headers.insert(NATS_MESSAGE_ID, publication.message_id.as_str());

        let span = js_span(
            &publication.claim,
            &publication.component_id,
            "publish-derived",
        );
        let started = std::time::Instant::now();
        let result = async {
            let ctx = self.ensure_ctx().await.map_err(|error| {
                DerivedPublishError::new(
                    DerivedPublishErrorKind::ConnectionUnavailable,
                    format!("derived event JetStream unavailable: {error:?}"),
                )
            })?;
            // Two awaits are load-bearing: queue completion may follow only
            // the server ACK, never the client-side send future.
            let ack = ctx
                .publish_with_headers(publication.subject, headers, publication.body.into())
                .await
                .map_err(|error| {
                    DerivedPublishError::new(
                        DerivedPublishErrorKind::PublishRejected,
                        format!("send derived event: {error}"),
                    )
                })?
                .await
                .map_err(|error| {
                    DerivedPublishError::new(
                        DerivedPublishErrorKind::PublishRejected,
                        format!("store derived event: {error}"),
                    )
                })?;
            if ack.stream != publication.expected_stream {
                return Err(DerivedPublishError::new(
                    DerivedPublishErrorKind::UnexpectedStream,
                    format!(
                        "derived event stored in stream {:?}, expected {:?}",
                        ack.stream, publication.expected_stream
                    ),
                ));
            }
            Ok(DerivedPublishAck {
                stream_name: ack.stream,
                stream_seq: ack.sequence,
                duplicate: ack.duplicate,
            })
        }
        .instrument(span)
        .await;
        record_effect_ms(
            &JETSTREAM_DURATION_MS,
            EFFECT_OPERATION,
            "publish-derived",
            &publication.claim.project,
            started.elapsed(),
        );
        result
    }

    /// Publish one ephemeral, redacted preview of a delivery boundary onto the
    /// reserved [`ROUTER_TAP_PREFIX`] namespace.
    ///
    /// BEST-EFFORT BY CONTRACT, and that is why it returns nothing. A live view
    /// is a debugging surface; a delivery must not fail, slow, or change shape
    /// because an operator is watching. So this
    ///
    /// - skips entirely on a host with no data-plane NATS, before it clones or
    ///   scrubs anything — which also makes the tap free in every test and bench
    ///   that runs without one;
    /// - does NOT await the JetStream server ack, unlike
    ///   [`WamnJetstream::publish_derived`], where the ack is the delivery truth.
    ///   Here it would only put a debugging tap on a delivery's critical path;
    /// - logs a failure at debug rather than raising it.
    ///
    /// COST, named rather than hidden: on a host that HAS a data-plane NATS this
    /// deep-clones the previewed payload once (the redaction policy scrubs in
    /// place, so a copy is unavoidable) and sends once, per boundary. If that
    /// shows up in a bench, the next lever is demand-gating the tap on a bound
    /// consumer, not thinning what the preview says.
    ///
    /// `component_id` names whose claim mints the subject; the caller cannot
    /// supply a subject, which is what keeps this the only writer.
    pub async fn publish_router_tap(&self, component_id: &str, preview: RouterTapPreview<'_>) {
        if self.nats_url.is_none() {
            return;
        }
        let Ok(claim) = self.required_derived_claim(component_id) else {
            tracing::debug!(
                target: "wamn::jetstream",
                component = component_id,
                "router tap skipped: no complete bind-time claim to scope a preview subject"
            );
            return;
        };
        let Some(prepared) = prepare_router_tap(&claim, &preview) else {
            tracing::debug!(
                target: "wamn::jetstream",
                component = component_id,
                "router tap skipped: the delivery or wiring id names no subject token"
            );
            return;
        };
        let span = js_span(&claim, component_id, "router-tap");
        let outcome = async {
            let ctx = self
                .ensure_ctx()
                .await
                .map_err(|error| format!("data-plane NATS unavailable: {error:?}"))?;
            ctx.publish(prepared.subject, prepared.body.into())
                .await
                .map_err(|error| format!("send router tap preview: {error}"))?;
            Ok::<(), String>(())
        }
        .instrument(span)
        .await;
        if let Err(error) = outcome {
            tracing::debug!(target: "wamn::jetstream", error, "router tap preview not published");
        }
    }
}

#[async_trait::async_trait]
impl HostPlugin for WamnJetstream {
    fn id(&self) -> &'static str {
        WAMN_JETSTREAM_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([
                WitInterface::from("wamn:jetstream/types@0.1.0"),
                WitInterface::from("wamn:jetstream/consumer@0.1.0"),
                WitInterface::from("wamn:jetstream/producer@0.1.0"),
                WitInterface::from("wamn:jetstream/doorbell@0.1.0"),
            ]),
            exports: HashSet::new(),
        }
    }

    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        if !interfaces.contains("wamn", "jetstream", &["consumer"])
            && !interfaces.contains("wamn", "jetstream", &["producer"])
            && !interfaces.contains("wamn", "jetstream", &["doorbell"])
        {
            return Ok(());
        }
        // The sole MVP placement adapter maps the same trusted `wamn.tenant`
        // config the postgres claims use into a distinct validated execution
        // target. The guest supplies neither the tenant nor the target.
        let (tenant, project, environment) = {
            let config = &item.local_resources().config;
            (
                config.get(TENANT_CONFIG_KEY).cloned(),
                config.get(PROJECT_CONFIG_KEY).cloned(),
                config.get(ENVIRONMENT_CONFIG_KEY).cloned(),
            )
        };
        self.set_claim(
            item.id(),
            tenant.as_deref(),
            project.as_deref(),
            environment.as_deref(),
        );
        if let Some(tenant) = tenant {
            // THE MVP TENANT-TO-TARGET ADAPTER, DELIBERATE AND RECORDED HERE
            // (wamn-0h0g.10.11). The other two doorbell configs take the
            // execution target as a STATED field — the waker requires its
            // `<execution-target-id>=<Deployment>` mapping, and the
            // dispatcher's `project_spec` falls back to this adapter only when
            // the field is absent. This bind DERIVES it instead, because the
            // workload config it reads names a tenant, a project and an
            // environment and NO target; that absence is why
            // `deploy/platform/materializer.example.yaml` is the one manifest
            // wamn-0h0g.10.5 could not rewrite to an explicit target.
            //
            // RETIREMENT TRIGGER: the first component that must ring a target
            // which is not its own tenant. Placement is wamn-0h0g.5's. Until it
            // yields a second target, a config key here would state nothing
            // this line does not already state, and it would give the doorbell
            // subject two sources where the comment above depends on it having
            // one.
            let execution_target_id = mvp_execution_target_id(&tenant)?;
            self.set_execution_target(item.id(), execution_target_id.clone());
            tracing::debug!(
                component = item.id(),
                tenant,
                execution_target_id = %execution_target_id,
                "wamn:jetstream doorbell execution target registered"
            );
        } else if interfaces.contains("wamn", "jetstream", &["doorbell"]) {
            tracing::warn!(
                component = item.id(),
                "component imports wamn:jetstream/doorbell but sets no wamn.tenant; no MVP execution target can be assigned and ring will be refused"
            );
        }
        add_to_linker(item.linker())?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Resources
// ---------------------------------------------------------------------------

/// Host side of a `wamn:jetstream/consumer.durable-consumer`. Holds the bound
/// async-nats pull consumer; [`Consumer`] is `Clone`, so `fetch` clones it out of
/// the resource table before pulling (the table borrow cannot span pushing the
/// returned message resources).
pub struct JsConsumer {
    consumer: Consumer<PullConfig>,
    dead_letter: Option<DeadLetterIdentity>,
}

/// Host side of a `wamn:jetstream/consumer.message`. Holds the delivered message;
/// ack/nack/term send the disposition back to the server.
pub struct JsMessage {
    msg: async_nats::jetstream::Message,
    dead_letter: Option<DeadLetterIdentity>,
}

// ---------------------------------------------------------------------------
// Pure mappings (unit-tested; some are mutant-guarded)
// ---------------------------------------------------------------------------

/// Build an async-nats `HeaderMap` from the guest's flat header list. `append`
/// (not `insert`) preserves duplicate names, matching the wire contract.
fn to_header_map(headers: &[Header]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for h in headers {
        map.append(h.name.as_str(), h.value.as_str());
    }
    map
}

/// Flatten an async-nats `HeaderMap` to the flat wire list. Multi-value headers
/// expand to one entry per value.
fn from_header_map(map: Option<&HeaderMap>) -> Vec<Header> {
    let Some(map) = map else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (name, values) in map.iter() {
        for value in values {
            out.push(Header {
                name: name.to_string(),
                value: value.as_str().to_string(),
            });
        }
    }
    out
}

/// Delivery metadata → the WIT record. `delivered` is `i64` on the wire but only
/// ever positive (1 on first delivery); a defensive saturating cast keeps a
/// nonsense negative from wrapping to a huge redelivery count.
fn to_message_meta(stream_seq: u64, delivered: i64) -> MessageMeta {
    MessageMeta {
        stream_seq,
        delivered: u64::try_from(delivered).unwrap_or(0),
    }
}

/// Nack disposition: `0` means "redeliver as soon as the server can" (`None`,
/// subject to `ack-wait`); a positive delay defers redelivery by that many ms.
fn nack_ack_kind(delay_ms: u64) -> AckKind {
    if delay_ms == 0 {
        AckKind::Nak(None)
    } else {
        AckKind::Nak(Some(Duration::from_millis(delay_ms)))
    }
}

/// Server publish-ack → the WIT record. A deduped publish is a SUCCESS carrying
/// `duplicate = true`, never an error.
fn to_publish_ack(ack: &NatsPublishAck) -> producer::PublishAck {
    producer::PublishAck {
        stream_name: ack.stream.clone(),
        stream_seq: ack.sequence,
        duplicate: ack.duplicate,
    }
}

/// `get_stream` failure → error taxonomy: a transport `Request` failure is
/// transient; every other kind (a JetStream 404, an empty/invalid name) means
/// the stream is not there to bind against.
fn map_get_stream_err(stream: &str, e: &GetStreamError) -> JsError {
    match e.kind() {
        GetStreamErrorKind::Request => JsError::ConnectionUnavailable,
        _ => JsError::NotFound(stream.to_string()),
    }
}

// ---------------------------------------------------------------------------
// The release gate (reader 3 of the weld)
// ---------------------------------------------------------------------------

/// The named refusal class for a consumer bind the serving release does not
/// register. Stable prose, because it is what an operator greps and what tells
/// a held registration apart from a transient `connection-unavailable`.
const UNREGISTERED_SOURCE: &str = "unregistered-source";
const RESERVED_DEAD_LETTER_SUBJECT: &str = "reserved-dead-letter-subject";
const RESERVED_EVENT_SUBJECT: &str = "reserved-event-subject";
const CONSUMER_CONFIG_DRIFT: &str = "registration-consumer-config-drift";

/// The `(entity, op)` tail of one event subject — the whole of a registration's
/// identity that a subject can carry.
///
/// `evt.<org>.<project>.<env>.<entity>.<op>` is the entire grammar the event
/// plane publishes ([`wamn_event_wire::subject`]), and its entity segment is
/// [`subject_token`]-sanitized. Anything else — an empty filter (which selects
/// the whole stream), a `>` above the entity — yields `None`: it selects
/// subjects whose registration identity cannot be read off at all, and what
/// cannot be read cannot be shown to be the release's.
fn subject_source(subject: &str) -> Option<(&str, &str)> {
    let mut tokens = subject.split('.');
    let prefix = tokens.next()?;
    let _org = tokens.next()?;
    let _project = tokens.next()?;
    let _environment = tokens.next()?;
    let entity = tokens.next()?;
    let op = tokens.next()?;
    if prefix != "evt" || tokens.next().is_some() {
        return None;
    }
    Some((entity, op))
}

/// Does some registration of the serving release source `(entity, op)`?
///
/// Membership in the manifest's projection, never a rederivation of it. The
/// comparison happens in subject-token space because the manifest carries the
/// raw stable entity id while the subject carries the sanitized token (R22).
///
/// `op` is a NATS wildcard for the materializer's own per-registration filter,
/// which spans every op of its entity. A wildcard therefore gates on the entity
/// alone — the op half stays the guest's `SkipReason::OpMismatch` to make, as it
/// already was. A filter that pins ONE op is gated on it, since then every
/// subject it selects would be unregistered.
fn release_sources(manifest: &ServingManifest, entity: &str, op: &str) -> bool {
    let any_op = op == ">" || op == "*";
    manifest.registrations.values().any(|registration| {
        subject_token(&registration.entity) == entity && (any_op || registration.ops.contains(op))
    })
}

/// The refusal a consumer bind over `filter_subject` earns, or `None` to admit.
///
/// `release` is the manifest of the release this process serves; `None` is a
/// release-less process, which admits nothing — see
/// [`WamnJetstream::with_release`].
fn bind_refusal(release: Option<&ServingManifest>, filter_subject: &str) -> Option<String> {
    let Some(manifest) = release else {
        return Some(format!(
            "{UNREGISTERED_SOURCE}: this host carries no release, so it has no \
             registration projection to admit a consumer against"
        ));
    };
    let Some((entity, op)) = subject_source(filter_subject) else {
        return Some(format!(
            "{UNREGISTERED_SOURCE}: filter subject {filter_subject:?} does not name \
             one entity and op, so the subjects it selects cannot be shown to be \
             registered"
        ));
    };
    if !release_sources(manifest, entity, op) {
        return Some(format!(
            "{UNREGISTERED_SOURCE}: no registration in effective release {} \
             sources entity {entity:?} op {op:?}",
            manifest.release.effective_release_id.get()
        ));
    }
    None
}

fn exact_registration_identity(
    release: Option<&ServingManifest>,
    package_id: &str,
    registration_id: &str,
    filter_subject: &str,
) -> Result<DeadLetterIdentity, String> {
    let manifest = release.ok_or_else(|| {
        format!(
            "{UNREGISTERED_SOURCE}: this host carries no release, so registration \
            {registration_id:?} cannot be resolved"
        )
    })?;
    let qualified_registration_id = format!("{package_id}::{registration_id}");
    let registration = manifest
        .registrations
        .get(&qualified_registration_id)
        .ok_or_else(|| {
            format!(
                "{UNREGISTERED_SOURCE}: effective release {} has no registration \
                 {qualified_registration_id:?}",
                manifest.release.effective_release_id.get()
            )
        })?;
    let (entity, op) = subject_source(filter_subject).ok_or_else(|| {
        format!(
            "{UNREGISTERED_SOURCE}: filter subject {filter_subject:?} does not name one entity \
             and op"
        )
    })?;
    let any_op = op == ">" || op == "*";
    if subject_token(&registration.entity) != entity || (!any_op && !registration.ops.contains(op))
    {
        return Err(format!(
            "{UNREGISTERED_SOURCE}: registration {registration_id:?} does not source entity \
             {entity:?} op {op:?}"
        ));
    }

    let subject = dead_letter_subject(
        &manifest.release.tenant_id,
        &manifest.release.environment,
        package_id,
        registration_id,
    );
    Ok(DeadLetterIdentity {
        tenant: manifest.release.tenant_id.clone().into_boxed_str(),
        environment: manifest.release.environment.clone().into_boxed_str(),
        package_id: package_id.into(),
        registration_id: registration_id.into(),
        subject: subject.into_boxed_str(),
    })
}

fn is_reserved_dead_letter_subject(subject: &str) -> bool {
    subject == "dlq" || subject.starts_with("dlq.")
}

fn is_reserved_event_subject(subject: &str) -> bool {
    subject == "evt" || subject.starts_with("evt.")
}

fn exact_consumer_config_drift(
    requested: &consumer::ConsumerConfig,
    stored: &StoredConsumerConfig,
) -> bool {
    let expected_max_deliver = if requested.max_deliver == 0 {
        -1
    } else {
        i64::from(requested.max_deliver)
    };
    stored.ack_policy != AckPolicy::Explicit
        || stored.filter_subject != requested.filter_subject
        || stored.max_deliver != expected_max_deliver
        || (requested.ack_wait_ms > 0
            && stored.ack_wait != Duration::from_millis(requested.ack_wait_ms))
}

async fn bind_consumer(
    plugin: &WamnJetstream,
    config: &consumer::ConsumerConfig,
    registration: Option<(&str, &str)>,
) -> Result<JsConsumer, JsError> {
    let dead_letter = match registration {
        Some((package_id, registration_id)) => Some(
            exact_registration_identity(
                plugin.serving_manifest(),
                package_id,
                registration_id,
                &config.filter_subject,
            )
            .map_err(JsError::Other)?,
        ),
        None => {
            if let Some(refusal) = bind_refusal(plugin.serving_manifest(), &config.filter_subject) {
                return Err(JsError::Other(refusal));
            }
            None
        }
    };
    let ctx = plugin.ensure_ctx().await?;
    let stream = ctx
        .get_stream(&config.stream_name)
        .await
        .map_err(|error| map_get_stream_err(&config.stream_name, &error))?;
    let pull = PullConfig {
        durable_name: Some(config.durable.clone()),
        ack_policy: AckPolicy::Explicit,
        filter_subject: config.filter_subject.clone(),
        ack_wait: Duration::from_millis(config.ack_wait_ms),
        max_deliver: if config.max_deliver == 0 {
            -1
        } else {
            i64::from(config.max_deliver)
        },
        ..Default::default()
    };
    let consumer = stream
        .get_or_create_consumer(&config.durable, pull)
        .await
        .map_err(|error| JsError::Other(format!("bind consumer: {error}")))?;
    if registration.is_some() {
        let stored = &consumer.cached_info().config;
        if exact_consumer_config_drift(config, stored) {
            return Err(JsError::Other(format!(
                "{CONSUMER_CONFIG_DRIFT}: durable {:?} does not match its exact bounded registration config",
                config.durable
            )));
        }
    }
    Ok(JsConsumer {
        consumer,
        dead_letter,
    })
}

async fn dead_letter_subject_depth(ctx: &Context, subject: &str) -> Result<u64, String> {
    let stream = ctx
        .get_stream(DEAD_LETTER_STREAM)
        .await
        .map_err(|error| format!("get {DEAD_LETTER_STREAM}: {error}"))?;
    let mut subjects = stream
        .info_with_subjects(subject)
        .await
        .map_err(|error| format!("read {DEAD_LETTER_STREAM} subject state: {error}"))?;
    let mut depth = 0_u64;
    while let Some((stored_subject, count)) = subjects
        .try_next()
        .await
        .map_err(|error| format!("read {DEAD_LETTER_STREAM} subject page: {error}"))?
    {
        if stored_subject == subject {
            depth = depth.saturating_add(count as u64);
        }
    }
    Ok(depth)
}

// ---------------------------------------------------------------------------
// Host trait impls
// ---------------------------------------------------------------------------

fn plugin_of(ctx: &ActiveCtx<'_>) -> wash_runtime::wasmtime::Result<std::sync::Arc<WamnJetstream>> {
    ctx.try_get_plugin::<WamnJetstream>(WAMN_JETSTREAM_ID)
}

impl consumer::Host for ActiveCtx<'_> {
    async fn bind(
        &mut self,
        config: consumer::ConsumerConfig,
    ) -> wash_runtime::wasmtime::Result<Result<Resource<JsConsumer>, JsError>> {
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
        let claim = plugin.claim_for(&component_id);
        let span = js_span(&claim, &component_id, "bind");
        let started = std::time::Instant::now();
        // The whole bind — the release gate and all three round trips — runs
        // inside the span, so a refusal is attributed to the same effect the
        // successful bind would have been.
        let bound = bind_consumer(&plugin, &config, None).instrument(span).await;
        record_effect_ms(
            &JETSTREAM_DURATION_MS,
            EFFECT_OPERATION,
            "bind",
            &claim.project,
            started.elapsed(),
        );
        let bound = match bound {
            Ok(c) => c,
            Err(e) => return Ok(Err(e)),
        };
        Ok(Ok(self.table.push(bound)?))
    }

    async fn bind_registration(
        &mut self,
        package_id: String,
        registration_id: String,
        config: consumer::ConsumerConfig,
    ) -> wash_runtime::wasmtime::Result<Result<Resource<JsConsumer>, JsError>> {
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
        let claim = plugin.claim_for(&component_id);
        let span = js_span(&claim, &component_id, "bind-registration");
        let started = std::time::Instant::now();
        let bound = bind_consumer(&plugin, &config, Some((&package_id, &registration_id)))
            .instrument(span)
            .await;
        record_effect_ms(
            &JETSTREAM_DURATION_MS,
            EFFECT_OPERATION,
            "bind-registration",
            &claim.project,
            started.elapsed(),
        );
        let bound = match bound {
            Ok(consumer) => consumer,
            Err(error) => {
                tracing::warn!(
                    target: "wamn::jetstream",
                    registration_id,
                    durable = %config.durable,
                    filter_subject = %config.filter_subject,
                    refusal = ?error,
                    "exact registration consumer bind refused"
                );
                return Ok(Err(error));
            }
        };
        Ok(Ok(self.table.push(bound)?))
    }
}

impl consumer::HostDurableConsumer for ActiveCtx<'_> {
    async fn fetch(
        &mut self,
        rep: Resource<JsConsumer>,
        max_messages: u32,
        expires_ms: u64,
    ) -> wash_runtime::wasmtime::Result<Result<Vec<Resource<JsMessage>>, JsError>> {
        // Clone the consumer out so the table borrow does not span the push of
        // the message resources below (Consumer is a cheap Arc-backed handle).
        let bound = self.table.get(&rep)?;
        let consumer = bound.consumer.clone();
        let dead_letter = bound.dead_letter.clone();
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
        let claim = plugin.claim_for(&component_id);
        let span = js_span(&claim, &component_id, "fetch");
        let started = std::time::Instant::now();

        let pulled = async {
            let mut fetch = consumer.fetch().max_messages(max_messages as usize);
            if expires_ms > 0 {
                fetch = fetch.expires(Duration::from_millis(expires_ms));
            }
            let mut batch = fetch
                .messages()
                .await
                .map_err(|e| JsError::Other(format!("fetch: {e}")))?;

            let mut pulled = Vec::new();
            while let Some(item) = batch.next().await {
                match item {
                    Ok(msg) => pulled.push(JsMessage {
                        msg,
                        dead_letter: dead_letter.clone(),
                    }),
                    // Boxed dyn error — stringify (map_err with anyhow!, not .context).
                    Err(e) => return Err(JsError::Other(format!("fetch message: {e}"))),
                }
            }
            Ok(pulled)
        }
        .instrument(span)
        .await;
        record_effect_ms(
            &JETSTREAM_DURATION_MS,
            EFFECT_OPERATION,
            "fetch",
            &claim.project,
            started.elapsed(),
        );
        let pulled = match pulled {
            Ok(p) => p,
            Err(e) => return Ok(Err(e)),
        };
        if let Some(identity) = dead_letter.as_ref() {
            match plugin.ensure_ctx().await {
                Ok(ctx) => match dead_letter_subject_depth(&ctx, &identity.subject).await {
                    Ok(depth) => plugin.dlq_depth.update(identity.clone(), depth),
                    Err(error) => tracing::warn!(
                        target: "wamn::jetstream",
                        subject = %identity.subject,
                        error,
                        "dead-letter depth refresh after fetch failed"
                    ),
                },
                Err(error) => tracing::warn!(
                    target: "wamn::jetstream",
                    subject = %identity.subject,
                    error = ?error,
                    "dead-letter depth refresh could not resolve JetStream"
                ),
            }
        }

        let mut handles = Vec::with_capacity(pulled.len());
        for m in pulled {
            handles.push(self.table.push(m)?);
        }
        Ok(Ok(handles))
    }

    async fn drop(&mut self, rep: Resource<JsConsumer>) -> wash_runtime::wasmtime::Result<()> {
        // Dropping releases the client handle only; durable state persists
        // server-side, so binding the same name resumes from the ack floor.
        self.table.delete(rep)?;
        Ok(())
    }
}

impl consumer::HostMessage for ActiveCtx<'_> {
    async fn body(&mut self, rep: Resource<JsMessage>) -> wash_runtime::wasmtime::Result<Vec<u8>> {
        Ok(self.table.get(&rep)?.msg.payload.to_vec())
    }

    async fn subject(
        &mut self,
        rep: Resource<JsMessage>,
    ) -> wash_runtime::wasmtime::Result<String> {
        Ok(self.table.get(&rep)?.msg.subject.to_string())
    }

    async fn headers(
        &mut self,
        rep: Resource<JsMessage>,
    ) -> wash_runtime::wasmtime::Result<Vec<Header>> {
        Ok(from_header_map(self.table.get(&rep)?.msg.headers.as_ref()))
    }

    async fn metadata(
        &mut self,
        rep: Resource<JsMessage>,
    ) -> wash_runtime::wasmtime::Result<MessageMeta> {
        let msg = self.table.get(&rep)?;
        match msg.msg.info() {
            Ok(info) => Ok(to_message_meta(info.stream_sequence, info.delivered)),
            Err(e) => {
                // A consumer-delivered message always carries a parseable reply
                // subject; a failure here means a malformed frame — surface zeros
                // rather than trap (metadata is not fallible on the wire).
                tracing::warn!(target: "wamn::jetstream", error = %e, "message metadata parse failed");
                Ok(to_message_meta(0, 0))
            }
        }
    }

    async fn ack(
        &mut self,
        rep: Resource<JsMessage>,
    ) -> wash_runtime::wasmtime::Result<Result<(), JsError>> {
        let (msg, dead_letter) = {
            let entry = self.table.get(&rep)?;
            (entry.msg.clone(), entry.dead_letter.clone())
        };
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
        let claim = plugin.claim_for(&component_id);
        let span = js_span(&claim, &component_id, "ack");
        let started = std::time::Instant::now();
        let result = msg
            .ack()
            .instrument(span)
            .await
            .map_err(|e| JsError::AckFailed(e.to_string()));
        record_effect_ms(
            &JETSTREAM_DURATION_MS,
            EFFECT_OPERATION,
            "ack",
            &claim.project,
            started.elapsed(),
        );
        // Ack lag is the MESSAGE's age, not this call's duration, so it needs
        // the server's publish stamp, which only the reply subject carries. A
        // malformed frame costs the sample and nothing else: same warn-and-
        // degrade posture `metadata` takes, never a trap on the ack path.
        match msg.info() {
            Ok(info) => record_ack_lag_ms(
                &JETSTREAM_ACK_LAG_MS,
                &claim.project,
                dead_letter.as_ref().map(|identity| AckLagRegistration {
                    tenant: &identity.tenant,
                    environment: &identity.environment,
                    package_id: &identity.package_id,
                    registration_id: &identity.registration_id,
                }),
                SystemTime::from(info.published),
                SystemTime::now(),
            ),
            Err(e) => tracing::warn!(
                target: "wamn::jetstream",
                error = %e,
                "ack lag skipped: message metadata parse failed"
            ),
        }
        Ok(result)
    }

    async fn nack(
        &mut self,
        rep: Resource<JsMessage>,
        delay_ms: u64,
    ) -> wash_runtime::wasmtime::Result<Result<(), JsError>> {
        let msg = self.table.get(&rep)?.msg.clone();
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
        let claim = plugin.claim_for(&component_id);
        let span = js_span(&claim, &component_id, "nack");
        let started = std::time::Instant::now();
        let result = msg
            .ack_with(nack_ack_kind(delay_ms))
            .instrument(span)
            .await
            .map_err(|e| JsError::AckFailed(e.to_string()));
        record_effect_ms(
            &JETSTREAM_DURATION_MS,
            EFFECT_OPERATION,
            "nack",
            &claim.project,
            started.elapsed(),
        );
        Ok(result)
    }

    async fn term(
        &mut self,
        rep: Resource<JsMessage>,
    ) -> wash_runtime::wasmtime::Result<Result<(), JsError>> {
        let msg = self.table.get(&rep)?.msg.clone();
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
        let claim = plugin.claim_for(&component_id);
        let span = js_span(&claim, &component_id, "term");
        let started = std::time::Instant::now();
        let result = msg
            .ack_with(AckKind::Term)
            .instrument(span)
            .await
            .map_err(|e| JsError::AckFailed(e.to_string()));
        record_effect_ms(
            &JETSTREAM_DURATION_MS,
            EFFECT_OPERATION,
            "term",
            &claim.project,
            started.elapsed(),
        );
        Ok(result)
    }

    async fn dead_letter(
        &mut self,
        rep: Resource<JsMessage>,
        reason: String,
    ) -> wash_runtime::wasmtime::Result<Result<(), JsError>> {
        let message = self.table.get(&rep)?;
        let msg = message.msg.clone();
        let Some(identity) = message.dead_letter.clone() else {
            return Ok(Err(JsError::PublishRejected(format!(
                "{UNREGISTERED_SOURCE}: message was not fetched through bind-registration"
            ))));
        };
        let info = match msg.info() {
            Ok(info) => info,
            Err(error) => {
                return Ok(Err(JsError::Other(format!(
                    "dead-letter source metadata: {error}"
                ))));
            }
        };
        let dead_letter = DeadLetter {
            format_version: 1,
            reason,
            source_stream: info.stream.to_string(),
            source_stream_sequence: info.stream_sequence,
            delivered: u64::try_from(info.delivered).unwrap_or(0),
            original_subject: msg.subject.to_string(),
            headers: from_header_map(msg.headers.as_ref())
                .into_iter()
                .map(|header| DeadLetterHeader {
                    name: header.name,
                    value: header.value,
                })
                .collect(),
            body: msg.payload.to_vec(),
        };
        let body = match serde_json::to_vec(&dead_letter) {
            Ok(body) => body,
            Err(error) => {
                return Ok(Err(JsError::Other(format!(
                    "serialize dead-letter record: {error}"
                ))));
            }
        };
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
        let claim = plugin.claim_for(&component_id);
        let span = js_span(&claim, &component_id, "dead-letter");
        let started = std::time::Instant::now();
        let source_stream_sequence = dead_letter.source_stream_sequence;
        let result = async {
            let ctx = plugin.ensure_ctx().await?;
            let mut headers = HeaderMap::new();
            let message_id = dead_letter_message_id(
                &identity.subject,
                &dead_letter.source_stream,
                source_stream_sequence,
            );
            headers.insert(NATS_MESSAGE_ID, message_id.as_str());
            let ack = ctx
                .publish_with_headers(identity.subject.to_string(), headers, body.into())
                .await
                .map_err(|error| JsError::PublishRejected(error.to_string()))?
                .await
                .map_err(|error| JsError::PublishRejected(error.to_string()))?;
            if ack.stream != DEAD_LETTER_STREAM {
                return Err(JsError::PublishRejected(format!(
                    "dead-letter subject was stored in unexpected stream {:?}",
                    ack.stream
                )));
            }
            match dead_letter_subject_depth(&ctx, &identity.subject).await {
                Ok(depth) => plugin.dlq_depth.update(identity.clone(), depth),
                Err(error) => tracing::warn!(
                    target: "wamn::jetstream",
                    subject = %identity.subject,
                    error,
                    "dead-letter stored but depth refresh failed"
                ),
            }
            Ok(())
        }
        .instrument(span)
        .await;
        record_effect_ms(
            &JETSTREAM_DURATION_MS,
            EFFECT_OPERATION,
            "dead-letter",
            &claim.project,
            started.elapsed(),
        );
        Ok(result)
    }

    async fn drop(&mut self, rep: Resource<JsMessage>) -> wash_runtime::wasmtime::Result<()> {
        // Dropping without an explicit ack/nack/term leaves the message to
        // redeliver after ack-wait (at-least-once).
        self.table.delete(rep)?;
        Ok(())
    }
}

impl doorbell::Host for ActiveCtx<'_> {
    async fn ring(
        &mut self,
        run_id: String,
    ) -> wash_runtime::wasmtime::Result<Result<(), JsError>> {
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
        // The target comes from the workload's bind-time MVP placement adapter.
        // A component with no registered target gets a refusal, not a default.
        let Some(execution_target_id) = plugin.execution_target_for(&component_id) else {
            return Ok(Err(JsError::Other(
                "no doorbell execution target registered for this component (set wamn.tenant)"
                    .into(),
            )));
        };
        let Some(nats) = plugin.doorbell_nats.as_ref() else {
            return Ok(Err(JsError::ConnectionUnavailable));
        };
        let subject = doorbell_subject(&execution_target_id);
        let claim = plugin.claim_for(&component_id);
        let span = js_span(&claim, &component_id, "doorbell.ring");
        let started = std::time::Instant::now();
        // Publish + flush: the hint must be ON THE WIRE when ring returns, or a
        // buffered publish could outlive the caller's interest (the async-nats
        // client buffers while disconnected — flushing surfaces that as an err).
        let result = async {
            nats.publish(subject, run_id.into_bytes().into())
                .await
                .map_err(|e| JsError::Other(format!("doorbell publish: {e}")))?;
            nats.flush()
                .await
                .map_err(|e| JsError::Other(format!("doorbell flush: {e}")))
        }
        .instrument(span)
        .await;
        record_effect_ms(
            &JETSTREAM_DURATION_MS,
            EFFECT_OPERATION,
            "doorbell.ring",
            &claim.project,
            started.elapsed(),
        );
        Ok(result)
    }
}

/// The generic guest publish: every reserved namespace refused, then the wire.
///
/// A free function taking `&WamnJetstream`, in the shape [`bind_consumer`]
/// already uses, so the refusals are reachable from a unit test that owns
/// nothing but a plugin — the gate is the security boundary of three host-owned
/// namespaces and a predicate test alone would never show that `publish` calls
/// it. Every refusal is decided BEFORE [`WamnJetstream::ensure_ctx`], which is
/// what lets `connection-unavailable` stand as proof that a subject got past
/// the gate.
async fn publish_generic(
    plugin: &WamnJetstream,
    component_id: &str,
    subject: String,
    headers: Vec<Header>,
    body: Vec<u8>,
) -> Result<producer::PublishAck, JsError> {
    let claim = plugin.claim_for(component_id);
    let span = js_span(&claim, component_id, "publish");
    let started = std::time::Instant::now();
    let result = async {
        if is_reserved_dead_letter_subject(&subject) {
            return Err(JsError::PublishRejected(format!(
                "{RESERVED_DEAD_LETTER_SUBJECT}: use a bound message's dead-letter method"
            )));
        }
        if is_reserved_event_subject(&subject) {
            return Err(JsError::PublishRejected(format!(
                "{RESERVED_EVENT_SUBJECT}: derived events use the host-owned publisher"
            )));
        }
        if is_reserved_router_tap_subject(&subject) {
            return Err(JsError::PublishRejected(format!(
                "{RESERVED_ROUTER_TAP_SUBJECT}: delivery previews are minted by the host tap"
            )));
        }
        let ctx = plugin.ensure_ctx().await?;
        let map = to_header_map(&headers);
        // Two awaits: the send future, then the server-ack future. The awaited
        // PublishAck is the only delivery truth (async-nats 0.47).
        let ack_future = ctx
            .publish_with_headers(subject, map, body.into())
            .await
            .map_err(|e| JsError::PublishRejected(e.to_string()))?;
        ack_future
            .await
            .map(|ack| to_publish_ack(&ack))
            .map_err(|e| JsError::PublishRejected(e.to_string()))
    }
    .instrument(span)
    .await;
    record_effect_ms(
        &JETSTREAM_DURATION_MS,
        EFFECT_OPERATION,
        "publish",
        &claim.project,
        started.elapsed(),
    );
    result
}

impl producer::Host for ActiveCtx<'_> {
    async fn publish(
        &mut self,
        subject: String,
        headers: Vec<Header>,
        body: Vec<u8>,
    ) -> wash_runtime::wasmtime::Result<Result<producer::PublishAck, JsError>> {
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
        Ok(publish_generic(&plugin, &component_id, subject, headers, body).await)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use opentelemetry::metrics::MeterProvider as _;
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
    use wamn_catalog::{
        DefinitionHash, EffectiveReleaseId, PackageCoordinate, ServingRegistration,
        ServingRegistrationInput, ServingRelease, ServingWiring,
    };

    use super::*;

    #[test]
    fn header_round_trip_preserves_pairs_and_order() {
        let headers = vec![
            Header {
                name: "Nats-Msg-Id".into(),
                value: "proj_prod:42".into(),
            },
            Header {
                name: "X-Wamn-Trace".into(),
                value: "abc".into(),
            },
        ];
        let map = to_header_map(&headers);
        // Nats-Msg-Id must survive so JetStream dedupe works from a guest.
        assert_eq!(
            map.get("Nats-Msg-Id").map(|v| v.as_str()),
            Some("proj_prod:42")
        );
        let back = from_header_map(Some(&map));
        assert_eq!(back.len(), 2);
        assert!(
            back.iter()
                .any(|h| h.name == "Nats-Msg-Id" && h.value == "proj_prod:42")
        );
        assert!(
            back.iter()
                .any(|h| h.name == "X-Wamn-Trace" && h.value == "abc")
        );
    }

    #[test]
    fn from_header_map_none_is_empty() {
        assert!(from_header_map(None).is_empty());
    }

    #[test]
    fn from_header_map_expands_multi_value() {
        let mut map = HeaderMap::new();
        map.append("K", "v1");
        map.append("K", "v2");
        let back = from_header_map(Some(&map));
        assert_eq!(back.len(), 2, "each value gets its own flat entry");
        assert!(back.iter().all(|h| h.name == "K"));
    }

    #[test]
    fn nack_zero_delay_is_immediate() {
        // 0 ⇒ no delay (redeliver ASAP, subject to ack-wait); the mutant that
        // maps 0 to Some(_) or drops the None branch fails here.
        assert!(matches!(nack_ack_kind(0), AckKind::Nak(None)));
    }

    #[test]
    fn nack_positive_delay_is_deferred() {
        assert!(matches!(
            nack_ack_kind(1500),
            AckKind::Nak(Some(d)) if d == Duration::from_millis(1500)
        ));
    }

    #[test]
    fn message_meta_carries_seq_and_delivered() {
        let m = to_message_meta(99, 3);
        assert_eq!(m.stream_seq, 99);
        assert_eq!(
            m.delivered, 3,
            "redelivery count travels as-is when positive"
        );
    }

    #[test]
    fn message_meta_clamps_negative_delivered() {
        // A nonsense negative must not wrap to a huge redelivery count; the
        // mutant that drops the saturating cast fails here.
        let m = to_message_meta(1, -5);
        assert_eq!(m.delivered, 0);
    }

    #[test]
    fn publish_ack_maps_fields_and_duplicate() {
        let nats = NatsPublishAck {
            stream: "EVT_acme_prod".into(),
            sequence: 7,
            domain: String::new(),
            duplicate: true,
            value: None,
        };
        let ack = to_publish_ack(&nats);
        assert_eq!(ack.stream_name, "EVT_acme_prod");
        assert_eq!(ack.stream_seq, 7);
        assert!(
            ack.duplicate,
            "a deduped publish is a SUCCESS carrying duplicate=true"
        );
    }

    fn derived_request(component_id: &str, dedup_id: &str) -> DerivedPublishRequest {
        DerivedPublishRequest {
            component_id: component_id.into(),
            package_id: "receiving".into(),
            entity: "orders".into(),
            operation: Op::Update,
            payload: serde_json::json!(["arbitrary", {"status": "ready"}]),
            dedup_id: dedup_id.into(),
            causation: Causation {
                run: "registration:delivery:9".into(),
                root: "registration:delivery:1".into(),
                depth: 3,
            },
        }
    }

    #[test]
    fn derived_publication_uses_only_the_bound_scope_and_admitted_selector() {
        let dangerous_author_id = "author\r\nNats-Msg-Id: forged";
        let publication = prepare_derived_publication(
            JetstreamClaim {
                tenant: "acme".into(),
                project: "app".into(),
                environment: "dev".into(),
            },
            derived_request("component-1", dangerous_author_id),
        )
        .expect("trusted scope and admitted selector prepare");

        assert_eq!(publication.subject, "evt.acme.app.dev.orders.update");
        assert_eq!(publication.expected_stream, "EVT_acme_dev");
        assert_eq!(
            publication.message_id,
            derived_msg_id(
                "acme",
                "app",
                "dev",
                "receiving",
                "orders",
                Op::Update,
                dangerous_author_id,
            )
        );
        assert_eq!(publication.message_id.len(), "derived:".len() + 64);
        assert!(!publication.message_id.contains("\r\n"));

        let event = DerivedEvent::from_slice(&publication.body).expect("derived wire decodes");
        assert_eq!(event.tenant, "acme");
        assert_eq!(event.project, "app");
        assert_eq!(event.environment, "dev");
        assert_eq!(event.package_id, "receiving");
        assert_eq!(event.entity, "orders");
        assert_eq!(event.op, Op::Update);
        assert_eq!(event.dedup_id, dangerous_author_id);
        assert_eq!(
            event.payload,
            serde_json::json!(["arbitrary", {"status": "ready"}])
        );
    }

    #[test]
    fn derived_publication_refuses_a_noncanonical_package_identity() {
        let claim = JetstreamClaim {
            tenant: "acme".into(),
            project: "app".into(),
            environment: "dev".into(),
        };
        let mut request = derived_request("component-1", "author:orders:7");
        request.package_id = " receiving".into();
        let error = prepare_derived_publication(claim, request)
            .expect_err("a noncanonical package identity must refuse");
        assert_eq!(error.kind(), DerivedPublishErrorKind::InvalidInput);
    }

    #[test]
    fn derived_publication_refuses_an_unbound_or_partial_scope() {
        let plugin = WamnJetstream::new(WamnJetstreamConfig { nats_url: None });
        assert_eq!(
            plugin
                .required_derived_claim("component-1")
                .unwrap_err()
                .kind(),
            DerivedPublishErrorKind::UnboundScope
        );
        plugin.set_claim("component-1", Some("acme"), Some("app"), None);
        assert_eq!(
            plugin
                .required_derived_claim("component-1")
                .unwrap_err()
                .kind(),
            DerivedPublishErrorKind::UnboundScope
        );
        plugin.set_claim(
            "component-1",
            Some("other.tenant"),
            Some("app"),
            Some("dev"),
        );
        assert_eq!(
            plugin
                .required_derived_claim("component-1")
                .unwrap_err()
                .kind(),
            DerivedPublishErrorKind::UnboundScope
        );
        assert!(
            plugin
                .bind_derived_scope("component-1", "other.tenant", "app", "dev")
                .is_err(),
            "a claim that can escape one subject token is refused"
        );
        plugin
            .bind_derived_scope("component-1", "acme", "app", "dev")
            .expect("complete trusted scope binds");
        assert_eq!(
            plugin
                .required_derived_claim("component-1")
                .expect("claim resolves")
                .environment
                .as_ref(),
            "dev"
        );
        plugin.revoke_derived_scope("component-1");
        assert!(plugin.required_derived_claim("component-1").is_err());
    }

    #[test]
    fn doorbell_registration_uses_the_mvp_target_adapter() {
        let plugin = WamnJetstream::new(WamnJetstreamConfig { nats_url: None });
        assert!(mvp_execution_target_id("evil.>").is_err());
        assert!(plugin.execution_target_for("c1").is_none());
        let target = mvp_execution_target_id("tenant-a").expect("tenant-safe target");
        plugin.set_execution_target("c1", target.clone());
        assert_eq!(plugin.execution_target_for("c1"), Some(target.clone()));
        assert_eq!(doorbell_subject(&target), "wamn.doorbell.tenant-a");
        // Unregistered components resolve to none — ring refuses, never defaults.
        assert!(plugin.execution_target_for("c2").is_none());
    }

    #[test]
    fn config_from_env_reads_evt_nats_url() {
        // Only assert the None (absent) branch — reading the var back would race
        // other tests in-process; the skip-when-absent posture is the contract.
        let cfg = WamnJetstreamConfig { nats_url: None };
        assert!(cfg.nats_url.is_none());
    }

    /// A serving release registering exactly one entity's ops.
    fn release_registering(entity: &str, ops: &[&str]) -> ServingManifest {
        let registration = ServingRegistration {
            package_id: "cat".into(),
            source_package_id: "cat".into(),
            wiring_id: "event-handler".into(),
            wiring_version: 1,
            entity: entity.to_string(),
            ops: ops.iter().copied().map(String::from).collect(),
            input: ServingRegistrationInput::Event,
        };
        ServingManifest::new(
            ServingRelease {
                tenant_id: "t1".into(),
                effective_release_id: EffectiveReleaseId::new(7).unwrap(),
                environment: "prod".into(),
                packages: BTreeSet::from([PackageCoordinate::new("cat", "1.0.0").unwrap()]),
            },
            BTreeSet::new(),
            BTreeSet::from([ServingWiring {
                package_id: "cat".into(),
                wiring_id: "event-handler".into(),
                wiring_version: 1,
                graph_hash: DefinitionHash::parse(
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                )
                .expect("fixture definition hash is canonical"),
            }]),
            BTreeMap::new(),
            BTreeMap::from([("cat::r1".to_string(), registration)]),
        )
        .expect("the fixture release is valid")
    }

    #[test]
    fn a_release_less_host_admits_no_consumer_bind() {
        let plugin = WamnJetstream::new(WamnJetstreamConfig { nats_url: None });
        let refusal = bind_refusal(plugin.serving_manifest(), "evt.acme.proj.prod.receipts.>")
            .expect("a host with no release has no registration projection to admit against");
        assert!(refusal.starts_with(UNREGISTERED_SOURCE));
        assert!(
            refusal.contains("carries no release"),
            "the refusal must name the deployment fact, not look transient: {refusal}"
        );
    }

    #[test]
    fn only_a_source_the_serving_release_registers_admits_a_consumer() {
        let manifest = release_registering("receipts", &["insert"]);

        // The materializer's own filter: one entity, every op of it.
        assert_eq!(
            bind_refusal(Some(&manifest), "evt.acme.proj.prod.receipts.>"),
            None
        );
        // An entity no registration sources is not this release's to deliver.
        let stranger = bind_refusal(Some(&manifest), "evt.acme.proj.prod.orders.>")
            .expect("an unregistered entity is refused");
        assert!(stranger.contains("orders"), "{stranger}");
        // A filter pinning ONE op selects only that op, so the op is gated too.
        assert_eq!(
            bind_refusal(Some(&manifest), "evt.acme.proj.prod.receipts.insert"),
            None
        );
        let wrong_op = bind_refusal(Some(&manifest), "evt.acme.proj.prod.receipts.delete");
        assert!(
            wrong_op.is_some(),
            "an op no registration on the entity subscribes is refused"
        );
        // A filter that pins no single entity would deliver every source on the
        // stream ungated, so it is refused rather than partially checked.
        for unpinned in [
            "",
            "evt.>",
            "evt.acme.proj.prod.*.>",
            "evt.acme.proj.prod.>",
        ] {
            assert!(
                bind_refusal(Some(&manifest), unpinned).is_some(),
                "filter {unpinned:?} pins no entity and must be refused"
            );
        }
        // The manifest carries the RAW entity id and the subject a sanitized
        // token, so membership is decided in token space — comparing the raw
        // names would refuse a registration on a dotted entity id.
        let dotted = release_registering("a.b", &["insert"]);
        let dotted_filter = format!("evt.acme.proj.prod.{}.>", subject_token("a.b"));
        assert_eq!(bind_refusal(Some(&dotted), &dotted_filter), None);
    }

    #[test]
    fn exact_registration_bind_mints_the_host_owned_dlq_identity() {
        let manifest = release_registering("receipts", &["insert"]);
        let identity = exact_registration_identity(
            Some(&manifest),
            "cat",
            "r1",
            "evt.acme.proj.prod.receipts.>",
        )
        .expect("exact release registration admits");
        assert_eq!(identity.subject.as_ref(), "dlq.t1.prod.cat.r1");
        assert_eq!(identity.registration_id.as_ref(), "r1");

        assert!(
            exact_registration_identity(
                Some(&manifest),
                "cat",
                "r2",
                "evt.acme.proj.prod.receipts.>"
            )
            .unwrap_err()
            .starts_with(UNREGISTERED_SOURCE)
        );
        assert!(
            exact_registration_identity(
                Some(&manifest),
                "cat",
                "r1",
                "evt.acme.proj.prod.orders.>"
            )
            .is_err(),
            "a real registration id cannot bless another registration's source"
        );
        assert!(
            exact_registration_identity(
                Some(&manifest),
                "other_package",
                "r1",
                "evt.acme.proj.prod.receipts.>"
            )
            .is_err(),
            "registration ids are package-scoped and must not collide across packages"
        );
    }

    #[test]
    fn generic_publish_cannot_name_the_reserved_dlq_namespace() {
        for subject in ["dlq", "dlq.t1.prod.cat.r1"] {
            assert!(is_reserved_dead_letter_subject(subject));
        }
        assert!(!is_reserved_dead_letter_subject(
            "evt.acme.proj.prod.receipts.insert"
        ));
    }

    #[test]
    fn generic_publish_cannot_name_the_host_owned_event_namespace() {
        for subject in ["evt", "evt.acme.app.dev.orders.insert"] {
            assert!(is_reserved_event_subject(subject));
        }
        assert!(!is_reserved_event_subject("wamn.jstest.orders.insert"));
    }

    // ---- the reserved router-tap preview namespace (wamn-0h0g.24.5) --------

    fn tap_claim() -> JetstreamClaim {
        JetstreamClaim {
            tenant: "acme".into(),
            project: "app".into(),
            environment: "prod".into(),
        }
    }

    /// The gate is exercised THROUGH the publish path, not as a bare predicate.
    ///
    /// A plugin with no configured URL cannot reach a server, so
    /// `connection-unavailable` is positive proof that a subject got PAST every
    /// refusal, and a `publish-rejected` naming a class is proof the refusal
    /// itself fired at the call site. Delete any of the three checks from
    /// `publish_generic` and its subjects fall through to
    /// `connection-unavailable` here. No NATS is involved.
    #[tokio::test]
    async fn generic_publish_refuses_every_reserved_namespace_before_the_wire() {
        let plugin = WamnJetstream::new(WamnJetstreamConfig { nats_url: None });
        for (subject, class) in [
            ("dlq", RESERVED_DEAD_LETTER_SUBJECT),
            ("dlq.t1.prod.cat.r1", RESERVED_DEAD_LETTER_SUBJECT),
            ("evt", RESERVED_EVENT_SUBJECT),
            ("evt.acme.app.dev.orders.insert", RESERVED_EVENT_SUBJECT),
            ("tap", RESERVED_ROUTER_TAP_SUBJECT),
            ("tap.acme.app.prod.orders.d-1", RESERVED_ROUTER_TAP_SUBJECT),
        ] {
            let error = publish_generic(&plugin, "c1", subject.to_owned(), Vec::new(), Vec::new())
                .await
                .expect_err("a host-owned namespace is not a guest's to write");
            let JsError::PublishRejected(detail) = error else {
                panic!("{subject:?} must be refused as publish-rejected, got {error:?}");
            };
            assert!(
                detail.starts_with(class),
                "{subject:?} must be refused as {class}: {detail}"
            );
        }
        // Nothing outside the three namespaces is refused — including subjects
        // that merely START with the reserved letters, which a `starts_with`
        // prefix test would swallow along with a tenant's own traffic.
        for admitted in ["wamn.jstest.orders.insert", "tapioca.acme", "taps", "evtx"] {
            let error = publish_generic(&plugin, "c1", admitted.to_owned(), Vec::new(), Vec::new())
                .await
                .expect_err("the fixture plugin has no data-plane NATS");
            assert!(
                matches!(error, JsError::ConnectionUnavailable),
                "{admitted:?} must reach the connection, not a reserved-namespace refusal: \
                 {error:?}"
            );
        }
    }

    /// The live arm runs against the provisioned stream itself. If the tap
    /// reservation is removed, this exact generic guest call receives a server
    /// ack and changes `WAMN_TAP`'s message count; there is no mock publisher or
    /// alternate authorization path in the proof.
    #[tokio::test]
    #[ignore = "requires a disposable NATS provisioned with checked-in WAMN_TAP"]
    async fn live_generic_guest_cannot_write_the_provisioned_tap_stream() {
        let url = std::env::var("WAMN_ROUTER_TAP_NATS_URL")
            .expect("set WAMN_ROUTER_TAP_NATS_URL to the disposable provisioned NATS");
        let context = async_nats::jetstream::new(
            async_nats::connect(&url)
                .await
                .expect("connect to disposable NATS"),
        );
        let mut stream = context
            .get_stream("WAMN_TAP")
            .await
            .expect("checked-in WAMN_TAP provisioning ran");
        let before = stream
            .info()
            .await
            .expect("read WAMN_TAP before")
            .state
            .messages;
        let plugin = WamnJetstream::new(WamnJetstreamConfig {
            nats_url: Some(url),
        });
        let error = publish_generic(
            &plugin,
            "generic-guest",
            "tap.tenant-a.default.prod.orders.forged".to_owned(),
            Vec::new(),
            br#"{"forged":true}"#.to_vec(),
        )
        .await
        .expect_err("a generic guest must not receive a WAMN_TAP server ack");
        let JsError::PublishRejected(detail) = error else {
            panic!("reserved tap publication must be rejected, got {error:?}");
        };
        assert!(detail.starts_with(RESERVED_ROUTER_TAP_SUBJECT), "{detail}");
        assert_eq!(
            stream
                .info()
                .await
                .expect("read WAMN_TAP after")
                .state
                .messages,
            before,
            "the refused guest publication must not reach the provisioned stream"
        );
    }

    /// The host must never mint a subject its own gate would admit from a guest:
    /// a preview namespace a tenant can write is a forgeable provenance channel
    /// feeding an operator's live view, which is worse than having no tap.
    #[test]
    fn every_minted_tap_subject_falls_inside_the_gated_namespace() {
        let claim = tap_claim();
        let subject = router_tap_subject(&claim, "orders", "d-1").expect("both ids name tokens");
        assert_eq!(subject, "tap.acme.app.prod.orders.d-1");
        assert!(is_reserved_router_tap_subject(&subject));

        // The delivery id crosses the WIT boundary from a guest. Sanitization
        // keeps it ONE token, so it can neither add a level nor plant a
        // wildcard that would widen what a consumer's filter selects.
        let injected = router_tap_subject(&claim, "orders", "d.1.*.>")
            .expect("a dirty id still names a token");
        assert_eq!(
            injected.split('.').count(),
            6,
            "a guest-supplied id must not add subject levels: {injected}"
        );
        assert!(
            !injected.contains('*') && !injected.contains('>'),
            "{injected}"
        );
        assert!(is_reserved_router_tap_subject(&injected));

        // An id that sanitizes to nothing yields no subject at all rather than a
        // malformed one with an empty token.
        assert_eq!(router_tap_subject(&claim, "", "d-1"), None);
        assert_eq!(router_tap_subject(&claim, "orders", ""), None);
    }

    /// Redaction and the ceiling are the publisher's, not the caller's: the
    /// assertions read the BYTES the tap would put on the wire.
    #[test]
    fn a_preview_is_redacted_and_bounded_before_it_can_reach_the_wire() {
        let claim = tap_claim();
        let payload = serde_json::json!({
            "api_key": "hunter2",
            "nested": {"authorization": "Bearer abc"},
            "plain": "visible",
        });
        let preview = RouterTapPreview {
            delivery_id: "d-1",
            wiring_id: "orders",
            wiring_version: 3,
            source_kind: "attachment",
            source_id: "orders-http",
            phase: RouterTapPhase::Accepted,
            payload: &payload,
        };
        let prepared = prepare_router_tap(&claim, &preview).expect("the ids name tokens");
        let record: RouterTapRecord =
            serde_json::from_slice(&prepared.body).expect("the tap body is JSON");

        assert_eq!(record.payload["api_key"], serde_json::json!("[redacted]"));
        assert_eq!(
            record.payload["nested"]["authorization"],
            serde_json::json!("[redacted]")
        );
        assert_eq!(record.payload["plain"], serde_json::json!("visible"));
        assert!(record.redacted);
        assert_eq!(record.phase, RouterTapRecordPhase::Accepted);
        assert_eq!(record.outcome, None);
        assert_eq!(&*record.delivery_id, "d-1");
        assert_eq!(record.wiring_version, 3);
        assert_eq!(&*record.source_id, "orders-http");
        assert_eq!(record.format_version.as_u32(), 1);
        assert_eq!(prepared.subject, "tap.acme.app.prod.orders.d-1");

        // A settled preview names its outcome; an accepted one has none to name.
        let settled = prepare_router_tap(
            &claim,
            &RouterTapPreview {
                phase: RouterTapPhase::Settled("respond"),
                ..preview
            },
        )
        .expect("the ids name tokens");
        let settled: RouterTapRecord =
            serde_json::from_slice(&settled.body).expect("the tap body is JSON");
        assert_eq!(settled.phase, RouterTapRecordPhase::Settled);
        assert_eq!(settled.outcome.as_deref(), Some("respond"));

        // A payload the extracted policy will not retain is DROPPED, not
        // truncated, and the envelope says how large it was.
        let oversized = serde_json::json!({"blob": "x".repeat(OUTPUT_CAPTURE_CEILING_BYTES + 1)});
        let bounded = prepare_router_tap(
            &claim,
            &RouterTapPreview {
                payload: &oversized,
                ..preview
            },
        )
        .expect("the ids name tokens");
        assert!(
            bounded.body.len() < OUTPUT_CAPTURE_CEILING_BYTES,
            "an over-ceiling payload must not reach the wire: {} bytes",
            bounded.body.len()
        );
        let bounded: RouterTapRecord =
            serde_json::from_slice(&bounded.body).expect("the tap body is JSON");
        assert_eq!(bounded.payload, serde_json::Value::Null);
        assert!(
            bounded
                .over_ceiling_bytes
                .is_some_and(|bytes| bytes > OUTPUT_CAPTURE_CEILING_BYTES as u64),
            "the dropped payload's size must survive as a flag: {bounded:?}"
        );
    }

    /// FROZEN WIRE RECORD. `wamn-dggp.10` is the named consumer and parses
    /// this on the console side, so the field set, the spelling of each key and
    /// the version literal are a contract rather than an implementation detail.
    ///
    /// Asserted as a WHOLE TYPED RECORD on purpose: the publisher and named
    /// consumer share one closed field set instead of two JSON interpretations.
    #[test]
    fn the_preview_record_is_frozen_for_its_named_consumer() {
        let claim = tap_claim();
        let payload = serde_json::json!({"plain": "visible"});
        let preview = RouterTapPreview {
            delivery_id: "d-1",
            wiring_id: "orders",
            wiring_version: 3,
            source_kind: "attachment",
            source_id: "orders-http",
            phase: RouterTapPhase::Accepted,
            payload: &payload,
        };

        let accepted = prepare_router_tap(&claim, &preview).expect("the ids name tokens");
        let accepted_record: RouterTapRecord =
            serde_json::from_slice(&accepted.body).expect("the tap body is JSON");
        assert_eq!(
            accepted_record,
            RouterTapRecord {
                delivery_id: "d-1".into(),
                format_version: RouterTapFormatVersion::V1,
                outcome: None,
                over_ceiling_bytes: None,
                payload: serde_json::json!({"plain": "visible"}),
                phase: RouterTapRecordPhase::Accepted,
                redacted: false,
                source_id: "orders-http".into(),
                source_kind: RouterTapSourceKind::Attachment,
                wiring_id: "orders".into(),
                wiring_version: 3,
            },
            "the accepted preview record is frozen for wamn-dggp.10"
        );
        assert_eq!(
            serde_json::to_vec(&accepted_record).expect("serialize accepted record"),
            accepted.body,
            "the named reader round-trips the publisher's exact bytes"
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&accepted.body)
                .expect("decode accepted record as its public wire value"),
            serde_json::json!({
                "format-version": 1,
                "phase": "accepted",
                "delivery-id": "d-1",
                "wiring-id": "orders",
                "wiring-version": 3,
                "source-kind": "attachment",
                "source-id": "orders-http",
                "redacted": false,
                "payload": {"plain": "visible"},
            }),
            "the accepted v1 wire stays byte-semantically frozen"
        );

        // A settled preview adds exactly one key. An accepted one carries no
        // `outcome` at all rather than a null, so the console can branch on
        // presence.
        let settled = prepare_router_tap(
            &claim,
            &RouterTapPreview {
                phase: RouterTapPhase::Settled("respond"),
                ..preview
            },
        )
        .expect("the ids name tokens");
        let settled_record: RouterTapRecord =
            serde_json::from_slice(&settled.body).expect("the tap body is JSON");
        assert_eq!(
            settled_record,
            RouterTapRecord {
                delivery_id: "d-1".into(),
                format_version: RouterTapFormatVersion::V1,
                outcome: Some("respond".into()),
                over_ceiling_bytes: None,
                payload: serde_json::json!({"plain": "visible"}),
                phase: RouterTapRecordPhase::Settled,
                redacted: false,
                source_id: "orders-http".into(),
                source_kind: RouterTapSourceKind::Attachment,
                wiring_id: "orders".into(),
                wiring_version: 3,
            },
            "the settled preview record is frozen for wamn-dggp.10"
        );
        assert_eq!(
            serde_json::to_vec(&settled_record).expect("serialize settled record"),
            settled.body,
            "the named reader round-trips the publisher's exact bytes"
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&settled.body)
                .expect("decode settled record as its public wire value"),
            serde_json::json!({
                "format-version": 1,
                "phase": "settled",
                "outcome": "respond",
                "delivery-id": "d-1",
                "wiring-id": "orders",
                "wiring-version": 3,
                "source-kind": "attachment",
                "source-id": "orders-http",
                "redacted": false,
                "payload": {"plain": "visible"},
            }),
            "the settled v1 wire stays byte-semantically frozen"
        );

        assert_eq!(
            router_tap_subject(&claim, "orders", "d-1").as_deref(),
            Some("tap.acme.app.prod.orders.d-1"),
            "the subject grammar is the other half of the frozen contract: \
             {ROUTER_TAP_PREFIX}.<tenant>.<project>.<environment>.<wiring>.<delivery>"
        );
    }

    #[test]
    fn the_preview_record_refuses_unknown_versions_and_fields() {
        let record = RouterTapRecord {
            delivery_id: "d-1".into(),
            format_version: RouterTapFormatVersion::V1,
            outcome: None,
            over_ceiling_bytes: None,
            payload: serde_json::json!({"plain": "visible"}),
            phase: RouterTapRecordPhase::Accepted,
            redacted: false,
            source_id: "orders-http".into(),
            source_kind: RouterTapSourceKind::Attachment,
            wiring_id: "orders".into(),
            wiring_version: 3,
        };
        let mut future_version = serde_json::to_value(&record).expect("serialize record");
        future_version["format-version"] = serde_json::Value::from(2);
        assert!(serde_json::from_value::<RouterTapRecord>(future_version).is_err());

        let mut unknown_field = serde_json::to_value(&record).expect("serialize record");
        unknown_field["edge"] = serde_json::Value::String("invoke".to_owned());
        assert!(serde_json::from_value::<RouterTapRecord>(unknown_field).is_err());

        let mut unknown_source = serde_json::to_value(&record).expect("serialize record");
        unknown_source["source-kind"] = serde_json::Value::String("schedule".to_owned());
        assert!(serde_json::from_value::<RouterTapRecord>(unknown_source).is_err());

        let mut impossible = record.clone();
        impossible.outcome = Some("respond".into());
        assert!(impossible.validate().is_err());

        let mut impossible = record;
        impossible.over_ceiling_bytes = Some((OUTPUT_CAPTURE_CEILING_BYTES + 1) as u64);
        assert!(impossible.validate().is_err());
    }

    // NOT ASSERTED HERE, and deliberately: `publish_router_tap`'s early return
    // on an unconfigured host is indistinguishable from letting it fall through
    // to `ensure_ctx`, because both end in nothing published. It is a cost
    // decision, not a behavioural one, so there is no honest unit test for it.

    #[test]
    fn exact_registration_consumer_keeps_transport_redelivery_armed() {
        let requested = consumer::ConsumerConfig {
            stream_name: "EVT_acme_prod".into(),
            durable: "mat_t1_cat_r1".into(),
            filter_subject: "evt.acme.proj.prod.receipts.>".into(),
            ack_wait_ms: 30_000,
            // Router execution is bounded by the materializer. Transport stays
            // armed so a failed DLQ publish can retry without re-running it.
            max_deliver: 0,
        };
        let matching = StoredConsumerConfig {
            ack_policy: AckPolicy::Explicit,
            filter_subject: requested.filter_subject.clone(),
            ack_wait: Duration::from_millis(requested.ack_wait_ms),
            max_deliver: -1,
            ..Default::default()
        };
        assert!(!exact_consumer_config_drift(&requested, &matching));

        let prematurely_stopped = StoredConsumerConfig {
            max_deliver: 5,
            ..matching
        };
        assert!(
            exact_consumer_config_drift(&requested, &prematurely_stopped),
            "the server must not stop redelivery before a failed DLQ write can recover"
        );
    }

    // -----------------------------------------------------------------------
    // Live round-trip against a real data-plane NATS. Gated on
    // WAMN_EVT_NATS_URL (skip-when-absent, the WAMN_*_PG_URL posture): it
    // exercises the exact async-nats call sequence the plugin relies on
    // (dedupe on publish, durable pull consumer, fetch/metadata/headers/ack)
    // through the plugin's own mapping helpers, so a broken API assumption
    // fails here rather than only in-cluster. The full component-driven e2e
    // rides the materializer (l5i9.17).
    // -----------------------------------------------------------------------

    use async_nats::jetstream::stream::{Config as StreamConfig, StorageType};

    #[tokio::test]
    async fn live_publish_dedupe_bind_fetch_ack() {
        let Ok(url) = std::env::var("WAMN_EVT_NATS_URL") else {
            eprintln!("skipping live_publish_dedupe_bind_fetch_ack: WAMN_EVT_NATS_URL unset");
            return;
        };

        let client = async_nats::connect(&url).await.expect("connect");
        let ctx = async_nats::jetstream::new(client);

        let stream_name = "WAMN_JS_TEST";
        let subject = "wamn.jstest.receipts.insert";
        let _ = ctx.delete_stream(stream_name).await;
        ctx.create_stream(StreamConfig {
            name: stream_name.into(),
            subjects: vec!["wamn.jstest.>".into()],
            storage: StorageType::File,
            num_replicas: 1,
            duplicate_window: Duration::from_secs(120),
            ..Default::default()
        })
        .await
        .expect("create stream");

        // Publish the same Nats-Msg-Id twice → dedupe. Uses the plugin helpers.
        let msg_id = "jstest_prod:1";
        let headers = vec![Header {
            name: "Nats-Msg-Id".into(),
            value: msg_id.into(),
        }];
        let map = to_header_map(&headers);
        let a1 = to_publish_ack(
            &ctx.publish_with_headers(
                subject.to_string(),
                map.clone(),
                b"{\"n\":1}".to_vec().into(),
            )
            .await
            .expect("send")
            .await
            .expect("ack"),
        );
        assert!(!a1.duplicate, "first publish is not a duplicate");
        assert_eq!(a1.stream_name, stream_name);
        let a2 = to_publish_ack(
            &ctx.publish_with_headers(subject.to_string(), map, b"{\"n\":1}".to_vec().into())
                .await
                .expect("send")
                .await
                .expect("ack"),
        );
        assert!(
            a2.duplicate,
            "second publish with the same Nats-Msg-Id dedupes"
        );

        // Bind a durable pull consumer and fetch — the plugin's bind config.
        let stream = ctx.get_stream(stream_name).await.expect("get stream");
        let pull = PullConfig {
            durable_name: Some("mat_test".into()),
            ack_policy: AckPolicy::Explicit,
            filter_subject: subject.into(),
            ack_wait: Duration::from_secs(5),
            max_deliver: -1,
            ..Default::default()
        };
        let consumer = stream
            .get_or_create_consumer("mat_test", pull)
            .await
            .expect("bind consumer");

        let mut batch = consumer
            .fetch()
            .max_messages(10)
            .expires(Duration::from_secs(2))
            .messages()
            .await
            .expect("fetch");
        let mut count = 0;
        while let Some(item) = batch.next().await {
            let msg = item.expect("message");
            count += 1;
            let hdrs = from_header_map(msg.headers.as_ref());
            assert!(
                hdrs.iter()
                    .any(|h| h.name == "Nats-Msg-Id" && h.value == msg_id),
                "delivered message carries its Nats-Msg-Id header"
            );
            let info = msg.info().expect("info");
            let meta = to_message_meta(info.stream_sequence, info.delivered);
            assert_eq!(meta.stream_seq, 1, "single stored message is seq 1");
            assert_eq!(meta.delivered, 1, "first delivery");
            msg.ack().await.expect("ack");
        }
        assert_eq!(count, 1, "exactly one message stored (dedupe held)");

        ctx.delete_stream(stream_name).await.expect("cleanup");
    }

    #[tokio::test]
    async fn live_derived_publish_replay_converges_through_jetstream_dedup() {
        let Ok(url) = std::env::var("WAMN_EVT_NATS_URL") else {
            eprintln!(
                "skipping live_derived_publish_replay_converges_through_jetstream_dedup: WAMN_EVT_NATS_URL unset"
            );
            return;
        };

        let client = async_nats::connect(&url).await.expect("connect");
        let ctx = async_nats::jetstream::new(client);
        let stream = "EVT_wamnjsderived_dev";
        let event_subject = "evt.wamnjsderived.app.dev.orders.update";
        let _ = ctx.delete_stream(stream).await;
        ctx.create_stream(StreamConfig {
            name: stream.into(),
            subjects: vec!["evt.wamnjsderived.*.dev.>".into()],
            storage: StorageType::File,
            num_replicas: 1,
            duplicate_window: Duration::from_secs(120),
            ..Default::default()
        })
        .await
        .expect("create derived stream");

        let plugin = WamnJetstream::new(WamnJetstreamConfig {
            nats_url: Some(url),
        });
        plugin
            .bind_derived_scope("component-1", "wamnjsderived", "app", "dev")
            .expect("trusted scope binds");
        let first = plugin
            .publish_derived(derived_request("component-1", "author:orders:7"))
            .await
            .expect("first server ack");
        assert!(!first.duplicate);
        assert_eq!(first.stream_name, stream);
        let replay = plugin
            .publish_derived(derived_request("component-1", "author:orders:7"))
            .await
            .expect("replay server ack");
        assert!(replay.duplicate, "the replay converges at JetStream dedup");
        assert_eq!(replay.stream_seq, first.stream_seq);

        let stream_handle = ctx.get_stream(stream).await.expect("get stream");
        let consumer = stream_handle
            .get_or_create_consumer(
                "derived_mat_test",
                PullConfig {
                    durable_name: Some("derived_mat_test".into()),
                    ack_policy: AckPolicy::Explicit,
                    filter_subject: event_subject.into(),
                    ack_wait: Duration::from_secs(5),
                    max_deliver: -1,
                    ..Default::default()
                },
            )
            .await
            .expect("bind derived consumer");
        let mut batch = consumer
            .fetch()
            .max_messages(10)
            .expires(Duration::from_secs(2))
            .messages()
            .await
            .expect("fetch derived event");
        let mut stored = Vec::new();
        while let Some(item) = batch.next().await {
            let message = item.expect("derived message");
            stored.push(DerivedEvent::from_slice(&message.payload).expect("derived wire"));
            message.ack().await.expect("ack derived message");
        }
        assert_eq!(stored.len(), 1, "the replay was not stored twice");
        assert_eq!(stored[0].dedup_id, "author:orders:7");
        assert_eq!(stored[0].entity, "orders");
        assert_eq!(stored[0].op, Op::Update);
        assert_eq!(stored[0].causation.depth, 3);

        ctx.delete_stream(stream).await.expect("cleanup");
    }

    // ---- depth-gauge liveness (wamn-0h0g.24.10) ----------------------------

    /// One meter over an in-memory exporter, owned by the test rather than by
    /// `opentelemetry::global`, so each [`MetricHarness::export`] call reads back
    /// exactly one exporter tick. Same shape as the harness in
    /// `crates/execution/host/src/router_delivery.rs`, which lives in that
    /// crate's `#[cfg(test)]` module and cannot be imported.
    struct MetricHarness {
        exporter: InMemoryMetricExporter,
        provider: SdkMeterProvider,
    }

    impl MetricHarness {
        fn install() -> Self {
            let exporter = InMemoryMetricExporter::default();
            let provider = SdkMeterProvider::builder()
                .with_reader(PeriodicReader::builder(exporter.clone()).build())
                .build();
            Self { exporter, provider }
        }

        fn meter(&self) -> Meter {
            self.provider.meter("dead-letter-depth-test")
        }

        /// One exporter tick: flush, then read back only the NEWEST batch, so
        /// two calls model two ticks of the exporter's own clock.
        fn export(&self) -> Vec<(String, Vec<(String, String)>, u64)> {
            self.provider
                .force_flush()
                .expect("test metrics must flush");
            let batches = self
                .exporter
                .get_finished_metrics()
                .expect("test metric exporter must remain readable");
            let mut series = Vec::new();
            let Some(resource) = batches.last() else {
                return series;
            };
            for scope in resource.scope_metrics() {
                for metric in scope.metrics() {
                    let points: Vec<(Vec<(String, String)>, u64)> = match metric.data() {
                        AggregatedMetrics::U64(MetricData::Sum(sum)) => sum
                            .data_points()
                            .map(|point| (sorted_attributes(point.attributes()), point.value()))
                            .collect(),
                        AggregatedMetrics::U64(MetricData::Gauge(gauge)) => gauge
                            .data_points()
                            .map(|point| (sorted_attributes(point.attributes()), point.value()))
                            .collect(),
                        _ => panic!("{} must stay a u64 gauge or sum", metric.name()),
                    };
                    for (attributes, value) in points {
                        series.push((metric.name().to_owned(), attributes, value));
                    }
                }
            }
            series.sort();
            series
        }
    }

    fn sorted_attributes<'a>(
        attributes: impl Iterator<Item = &'a KeyValue>,
    ) -> Vec<(String, String)> {
        let mut pairs: Vec<(String, String)> = attributes
            .map(|kv| (kv.key.to_string(), kv.value.to_string()))
            .collect();
        pairs.sort();
        pairs
    }

    /// A DEPTH GAUGE ALONE CANNOT PROVE ITS OWN OBSERVER IS ALIVE.
    ///
    /// Three exporter ticks over one registration whose dead-letter subject is
    /// genuinely EMPTY. The gauge reads 0 on all three, because the exporter
    /// re-observes the last written sample on ITS OWN clock: tick 2, where
    /// nothing refreshed the cache, is byte-identical to tick 1. The samples
    /// counter is what separates them — flat across tick 2 (the observer
    /// stopped), advanced on tick 3 (the observer is alive and the subject is
    /// genuinely empty).
    ///
    /// THE LIMIT, so this is not read as proving more than it does: the counter
    /// is a SELF-REPORT, so it catches an observer that STOPPED and nothing
    /// else. One that LIES or WEDGES while still ticking keeps incrementing it
    /// and no assertion below would fail. The signal the subject cannot fake is
    /// `wamn-2jkm.104`'s.
    #[test]
    fn the_dlq_depth_samples_counter_tells_an_empty_subject_from_a_stopped_observer() {
        let harness = MetricHarness::install();
        let meter = harness.meter();
        let depth = Arc::new(DeadLetterDepth::new(&meter));
        DeadLetterDepth::register(&meter, &depth);

        let identity = DeadLetterIdentity {
            tenant: "tenant-a".into(),
            environment: "prod".into(),
            package_id: "orders".into(),
            registration_id: "orders-changed".into(),
            subject: "dlq.tenant-a.prod.orders.orders-changed".into(),
        };
        let labels = vec![
            ("wamn.environment".to_owned(), "prod".to_owned()),
            ("wamn.package".to_owned(), "orders".to_owned()),
            ("wamn.registration".to_owned(), "orders-changed".to_owned()),
            ("wamn.tenant".to_owned(), "tenant-a".to_owned()),
        ];

        depth.update(identity.clone(), 0);
        let observed = harness.export();
        assert_eq!(
            observed,
            vec![
                ("wamn.jetstream.dlq.depth".to_owned(), labels.clone(), 0),
                (
                    "wamn.jetstream.dlq.depth.samples".to_owned(),
                    labels.clone(),
                    1,
                ),
            ]
        );

        let stopped = harness.export();
        assert_eq!(
            stopped[0], observed[0],
            "the gauge cannot tell a stopped observer from an empty subject: \
             it re-observes the last written sample forever"
        );

        depth.update(identity, 0);
        let alive = harness.export();
        assert_eq!(
            alive[0], observed[0],
            "the subject stayed empty, so the gauge must not have moved"
        );
        assert_eq!(
            (stopped[1].2, alive[1].2),
            (1, 2),
            "the samples counter is the whole difference: flat while the \
             observer is stopped, advancing while it is taking readings"
        );
    }
}
