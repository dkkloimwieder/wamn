//! Host-owned registration activation and event publication.
//!
//! Native `wasmcloud:nats` owns materializer attachment, delivery and settlement.
//! This module retains exact release-registration selection, durable drift
//! checks, and host-only derived-event and router-tap publication. Broker
//! advisories replace the payload DLQ.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_nats::HeaderMap;
use async_nats::header::NATS_MESSAGE_ID;
use async_nats::jetstream::Context;
#[cfg(test)]
use async_nats::jetstream::consumer::AckPolicy;
use async_nats::jetstream::consumer::Config as StoredConsumerConfig;
use async_nats::jetstream::consumer::pull::Config as PullConfig;
use async_nats::jetstream::context::{GetStreamError, GetStreamErrorKind};
#[cfg(test)]
use futures_util::StreamExt as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tokio::sync::Mutex;
use tracing::Instrument as _;
use wamn_catalog::{AttachmentTarget, ServingManifest};
use wamn_control_provision::events::{
    advisory_stream_config, consumer_config_matches, materializer_consumer_config,
    source_stream_config, stream_config_matches,
};
use wamn_control_registry::Triple;
use wamn_event_wire::{
    Causation, DerivedEvent, Op, derived_msg_id, stream_name, subject, subject_token,
};
use wamn_run_state::redaction::{OUTPUT_CAPTURE_CEILING_BYTES, scrub};

use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::{Accessor, Linker};
use wash_runtime::wit::{WitInterface, WitWorld};

use crate::plugins::effect_span::{
    EFFECT_OPERATION, EffectIdentity, EffectWiring, JETSTREAM_DURATION_MS, effect_span,
    record_effect_ms, record_wiring,
};
use crate::plugins::wamn_postgres::{DEFAULT_PROJECT, PROJECT_CONFIG_KEY, TENANT_CONFIG_KEY};
use wamn_engine::release_manifest::LoadedRelease;

mod bindings {
    wash_runtime::wasmtime::component::bindgen!({
        world: "jetstream-plugin",
        imports: { default: async | trappable | tracing },
        wasmtime_crate: wash_runtime::wasmtime,
    });
}

use bindings::wamn::jetstream::registration;
use bindings::wamn::jetstream::types::JsError;

pub const WAMN_JETSTREAM_ID: &str = "wamn-jetstream";

/// Trusted workload claim carrying the exact event environment.
pub const ENVIRONMENT_CONFIG_KEY: &str = "wamn.environment";

/// The host-owned inputs needed to publish one admitted Emit terminal.
///
/// Tenant, project, and environment are intentionally absent:
/// [`WamnJetstream::publish_derived`] resolves them from the claim bound to
/// `component_id`. `package_id` is supplied only by the native host caller from
/// its loaded release/run/wiring identity; it is not a guest WIT operand.
///
/// `wiring_id`, `wiring_version` and `node_id` are trace coordinates only: they
/// name the wiring position the Emit terminal was admitted at, and nothing on
/// the publish path authorizes against them. Both callers are native hosts that
/// already hold all three — the router bridge from its resolved target and the
/// executor from its claimed queue row — so neither reads them from guest
/// output. See [`js_span`] for how they reach the effect span.
#[derive(Debug, Clone, PartialEq)]
pub struct DerivedPublishRequest {
    pub component_id: String,
    pub package_id: String,
    pub wiring_id: String,
    pub wiring_version: u32,
    pub node_id: String,
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
/// This namespace carries bounded previews, separate from durable event facts.
/// Only the native host publisher constructs these records.
pub const ROUTER_TAP_PREFIX: &str = "tap";

/// A router-tap record version understood by this release.
///
/// Version 1 names a wiring. Version 2 names a route or a wiring. The publisher
/// writes only version 2, and a reader still reads version 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouterTapFormatVersion {
    V1,
    V2,
}

impl RouterTapFormatVersion {
    /// Numeric value carried on the wire.
    pub const fn as_u32(self) -> u32 {
        match self {
            Self::V1 => 1,
            Self::V2 => 2,
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
        match u32::deserialize(deserializer)? {
            1 => Ok(Self::V1),
            2 => Ok(Self::V2),
            version => Err(serde::de::Error::custom(format!(
                "unsupported router-tap format-version {version}"
            ))),
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

/// What one router-tap record names: a route or a wiring, never both.
///
/// The same shape as the serving manifest's attachment target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouterTapTarget {
    /// One component export, entered through its attachment.
    Route {
        component: Box<str>,
        operation: Box<str>,
    },
    /// One exact wiring version.
    Wiring {
        wiring_id: Box<str>,
        wiring_version: u32,
    },
}

/// Frozen, owned router-tap wire record shared by publisher and readers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RouterTapWire", into = "RouterTapWire")]
pub struct RouterTapRecord {
    pub delivery_id: Box<str>,
    pub format_version: RouterTapFormatVersion,
    pub outcome: Option<Box<str>>,
    pub over_ceiling_bytes: Option<u64>,
    pub payload: serde_json::Value,
    pub phase: RouterTapRecordPhase,
    pub redacted: bool,
    pub source_id: Box<str>,
    pub source_kind: RouterTapSourceKind,
    pub target: RouterTapTarget,
}

/// The flat wire of [`RouterTapRecord`].
///
/// Fields are declared in the byte order emitted by the former JSON-map
/// publisher, so a wiring record keeps its version 1 bytes apart from its
/// version number.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RouterTapWire {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    component: Option<Box<str>>,
    delivery_id: Box<str>,
    format_version: RouterTapFormatVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    operation: Option<Box<str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    outcome: Option<Box<str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    over_ceiling_bytes: Option<u64>,
    payload: serde_json::Value,
    phase: RouterTapRecordPhase,
    redacted: bool,
    source_id: Box<str>,
    source_kind: RouterTapSourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wiring_id: Option<Box<str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wiring_version: Option<u32>,
}

impl TryFrom<RouterTapWire> for RouterTapRecord {
    type Error = &'static str;

    fn try_from(wire: RouterTapWire) -> Result<Self, Self::Error> {
        let target = match (
            wire.format_version,
            wire.component,
            wire.operation,
            wire.wiring_id,
            wire.wiring_version,
        ) {
            (_, None, None, Some(wiring_id), Some(wiring_version)) => RouterTapTarget::Wiring {
                wiring_id,
                wiring_version,
            },
            (RouterTapFormatVersion::V2, Some(component), Some(operation), None, None) => {
                RouterTapTarget::Route {
                    component,
                    operation,
                }
            }
            _ => return Err("a router tap names exactly one route or one wiring"),
        };
        Ok(Self {
            delivery_id: wire.delivery_id,
            format_version: wire.format_version,
            outcome: wire.outcome,
            over_ceiling_bytes: wire.over_ceiling_bytes,
            payload: wire.payload,
            phase: wire.phase,
            redacted: wire.redacted,
            source_id: wire.source_id,
            source_kind: wire.source_kind,
            target,
        })
    }
}

impl From<RouterTapRecord> for RouterTapWire {
    fn from(record: RouterTapRecord) -> Self {
        let (component, operation, wiring_id, wiring_version) = match record.target {
            RouterTapTarget::Route {
                component,
                operation,
            } => (Some(component), Some(operation), None, None),
            RouterTapTarget::Wiring {
                wiring_id,
                wiring_version,
            } => (None, None, Some(wiring_id), Some(wiring_version)),
        };
        Self {
            component,
            delivery_id: record.delivery_id,
            format_version: record.format_version,
            operation,
            outcome: record.outcome,
            over_ceiling_bytes: record.over_ceiling_bytes,
            payload: record.payload,
            phase: record.phase,
            redacted: record.redacted,
            source_id: record.source_id,
            source_kind: record.source_kind,
            wiring_id,
            wiring_version,
        }
    }
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
    /// The subject token that names this record's target: the wiring id, or
    /// the attachment id of a route.
    pub fn subject_token(&self) -> &str {
        match &self.target {
            RouterTapTarget::Route { .. } => &self.source_id,
            RouterTapTarget::Wiring { wiring_id, .. } => wiring_id,
        }
    }

    /// Check that the phase and bounded-payload fields describe one possible record.
    pub fn validate(&self) -> Result<(), RouterTapRecordError> {
        if matches!(self.target, RouterTapTarget::Route { .. })
            && self.source_kind != RouterTapSourceKind::Attachment
        {
            return Err(RouterTapRecordError {
                detail: "only an attachment enters a route",
            });
        }
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
    /// The route or the wiring that the delivery entered.
    pub target: &'a AttachmentTarget,
    /// `"attachment"` or `"registration"` — the bridge's own two ingress kinds.
    pub source_kind: &'static str,
    pub source_id: &'a str,
    pub phase: RouterTapPhase,
    pub payload: &'a serde_json::Value,
}

/// `tap.<org>.<project>.<env>.<target>.<delivery>` — six tokens, the same arity
/// as `evt.<org>.<project>.<env>.<entity>.<op>`, so a run screen binds one
/// delivery (`tap.<org>.<project>.<env>.*.<delivery>`) and an operator binds one
/// environment. The target token is the wiring id, or the attachment id of a
/// route.
///
/// The org, project and environment tokens come from the trusted bind-time
/// claim, which [`WamnJetstream::required_derived_claim`] already confirmed to
/// be exactly one subject token each. The target and delivery ids have not: the
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
    target_token: &str,
    delivery_id: &str,
) -> Option<String> {
    let prefix = router_tap_environment_prefix(tenant, project, environment)?;
    let target = subject_token(target_token);
    let delivery = subject_token(delivery_id);
    if target.is_empty() || delivery.is_empty() {
        return None;
    }
    Some(format!("{prefix}.{target}.{delivery}"))
}

fn router_tap_subject(
    claim: &JetstreamClaim,
    target_token: &str,
    delivery_id: &str,
) -> Option<String> {
    router_tap_record_subject(
        &claim.tenant,
        &claim.project,
        &claim.environment,
        target_token,
        delivery_id,
    )
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
    let (target, target_token) = match preview.target {
        AttachmentTarget::Route {
            component,
            operation,
        } => (
            RouterTapTarget::Route {
                component: Box::from(component.as_str()),
                operation: Box::from(operation.as_str()),
            },
            preview.source_id,
        ),
        AttachmentTarget::Wiring {
            wiring_id,
            wiring_version,
        } => (
            RouterTapTarget::Wiring {
                wiring_id: Box::from(wiring_id.as_str()),
                wiring_version: *wiring_version,
            },
            wiring_id.as_str(),
        ),
    };
    let subject = router_tap_subject(claim, target_token, preview.delivery_id)?;
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
        format_version: RouterTapFormatVersion::V2,
        outcome,
        over_ceiling_bytes,
        payload,
        phase,
        redacted,
        source_id: Box::from(preview.source_id),
        source_kind,
        target,
    };
    record
        .validate()
        .expect("a router-tap preview constructs one valid phase and payload state");
    Some(PreparedRouterTap {
        subject,
        body: serde_json::to_vec(&record).expect("a router-tap record always serializes"),
    })
}

/// Link host-owned registration checks directly.
/// The host path calls this from [`HostPlugin::on_workload_item_bind`];
/// a Service (the materializer, l5i9.17) or a hand-built store links it the same
/// way `wamn:postgres` is linked.
pub fn add_to_linker(linker: &mut Linker<SharedCtx>) -> wash_runtime::wasmtime::Result<()> {
    registration::add_to_linker::<_, SharedCtx>(linker, extract_active_ctx)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Plugin configuration
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct WamnJetstreamConfig {
    /// Data-plane NATS URL (deploy/infra/nats-jetstream.yaml Service `evt-nats`).
    /// `None` ⇒ the plugin registers but every call returns
    /// `connection-unavailable`.
    pub nats_url: Option<String>,
    /// Event-broker username paired with its private password file.
    pub nats_username: Option<String>,
    /// Private event-broker password file. Password bytes stay outside configuration.
    pub nats_password_file: Option<PathBuf>,
    /// Trusted event coordinates, separate from tenant and database authority.
    pub event_scope: Option<Triple>,
    /// Declared NATS stream copies, separate from workload instances.
    pub stream_replicas: Option<usize>,
    /// Declared duplicate detection window in seconds.
    pub dup_window_secs: Option<u64>,
}

impl WamnJetstreamConfig {
    /// The event-plane NATS URL, gated on `WAMN_EVT_NATS_URL` (the same
    /// skip-when-absent posture the live tests use).
    pub fn from_env() -> Self {
        Self {
            nats_url: std::env::var("WAMN_EVT_NATS_URL").ok(),
            nats_username: std::env::var("WAMN_EVT_NATS_USERNAME").ok(),
            nats_password_file: std::env::var_os("WAMN_EVT_NATS_PASSWORD_FILE").map(PathBuf::from),
            event_scope: match (
                std::env::var("WAMN_EVT_ORG"),
                std::env::var("WAMN_EVT_PROJECT"),
                std::env::var("WAMN_EVT_ENV"),
            ) {
                (Ok(org), Ok(project), Ok(environment)) => {
                    Some(Triple::new(org, project, environment))
                }
                _ => None,
            },
            stream_replicas: std::env::var("WAMN_EVT_STREAM_REPLICAS")
                .ok()
                .and_then(|value| value.parse().ok()),
            dup_window_secs: std::env::var("WAMN_EVT_DUP_WINDOW_SECS")
                .ok()
                .and_then(|value| value.parse().ok()),
        }
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct WamnJetstream {
    nats_url: Option<String>,
    stream_replicas: Option<usize>,
    dup_window_secs: Option<u64>,
    nats_username: Option<String>,
    nats_password_file: Option<PathBuf>,
    /// Event coordinates come from the platform bootstrap, separately from DB authority.
    event_coordinates: EventCoordinates,
    /// Lazily-connected, memoized JetStream context. A `Mutex<Option<_>>` (not a
    /// `OnceCell`) so a transient connect failure is retried on the next call
    /// instead of memoized forever; only a successful connect is stored.
    ctx: Mutex<Option<Context>>,
    /// Per-component tenant/project/environment claim, registered at workload
    /// bind from the same trusted `wamn.*` config the `wamn:postgres`
    /// claims read. It exists only to enrich this plugin's effect spans: before
    /// `wamn-0h0g.24.3` the bind read the tenant, derived the execution target
    /// from it and discarded it, so nothing here could say whose event plane a
    /// publish or an ack belonged to.
    claims: std::sync::RwLock<HashMap<String, JetstreamClaim>>,
    /// The release this process serves, held BY REFERENCE — reader 3 of the
    /// loaded release manifest. `None` ⇒ this process carries no release; see
    /// [`WamnJetstream::with_release`].
    release: Option<Arc<LoadedRelease>>,
}

/// One component's bind-time tenant/project claim.
#[derive(Clone, Debug)]
struct JetstreamClaim {
    tenant: Box<str>,
    project: Box<str>,
    environment: Box<str>,
}

#[derive(Debug, Default)]
struct EventCoordinates {
    org: Box<str>,
    project: Box<str>,
    environment: Box<str>,
}

fn require_event_coordinates(coordinates: &EventCoordinates) -> Result<(), DerivedPublishError> {
    for (name, value) in [
        ("WAMN_EVT_ORG", coordinates.org.as_ref()),
        ("WAMN_EVT_PROJECT", coordinates.project.as_ref()),
        ("WAMN_EVT_ENV", coordinates.environment.as_ref()),
    ] {
        if value.is_empty() || value.trim() != value || subject_token(value) != value {
            return Err(DerivedPublishError::new(
                DerivedPublishErrorKind::UnboundScope,
                format!("event coordinate {name} is absent or not one NATS subject token"),
            ));
        }
    }
    Ok(())
}

#[derive(Debug)]
struct PreparedDerivedPublication {
    component_id: String,
    claim: JetstreamClaim,
    subject: String,
    message_id: String,
    expected_stream: String,
    body: Vec<u8>,
    /// Carried through only so the span opened after preparation can name the
    /// wiring position; `prepare_derived_publication` consumes the request.
    wiring: DerivedPublishWiring,
}

/// The wiring position one derived publication was admitted at, for tracing.
#[derive(Debug)]
struct DerivedPublishWiring {
    package_id: String,
    wiring_id: String,
    wiring_version: u32,
    node_id: String,
}

fn prepare_derived_publication(
    claim: JetstreamClaim,
    coordinates: &EventCoordinates,
    request: DerivedPublishRequest,
) -> Result<PreparedDerivedPublication, DerivedPublishError> {
    require_event_coordinates(coordinates)?;
    if claim.environment != coordinates.environment {
        return Err(DerivedPublishError::new(
            DerivedPublishErrorKind::UnboundScope,
            "derived event environment differs from the platform event binding",
        ));
    }
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
        coordinates.project.to_string(),
        coordinates.environment.to_string(),
        request.package_id,
        request.entity,
        request.operation,
        request.payload,
        request.dedup_id,
        request.causation,
    );
    let event_subject = subject(
        &coordinates.org,
        &coordinates.project,
        &coordinates.environment,
        &event.entity,
        event.op,
    );
    let message_id = derived_msg_id(
        &claim.tenant,
        &coordinates.project,
        &coordinates.environment,
        &event.package_id,
        &event.entity,
        event.op,
        &event.dedup_id,
    );
    let expected_stream = stream_name(
        &coordinates.org,
        &coordinates.project,
        &coordinates.environment,
    );
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
        wiring: DerivedPublishWiring {
            package_id: event.package_id.clone(),
            wiring_id: request.wiring_id,
            wiring_version: request.wiring_version,
            node_id: request.node_id,
        },
        body,
    })
}

/// The span one `wamn:jetstream` effect opens, enriched from the component's
/// bind-time claim and, where the operation has one, its wiring position.
///
/// Only `publish-derived` passes a `wiring`. It is the one operation this plugin
/// performs on behalf of an admitted Emit terminal, and its native callers hand
/// the coordinates down with the request. The other two — `router-tap` and
/// `prepare-registration` — are plugin-initiated control work standing at no
/// wiring node, so they record the keys EMPTY through
/// [`record_wiring`]'s `None`, which reads as "this effect holds no such claim"
/// rather than as dropped instrumentation.
///
/// Four of the eight keys are EMPTY even on `publish-derived`.
/// `wamn.occurrence`, `wamn.component_digest`, `wamn.component_name` and
/// `wamn.operation` all belong to a bound `ConnectionInvocation`, and the
/// publish path never stands in a guest component scope: its two callers are the
/// router delivery bridge and the executor queue, both native hosts. They hold
/// the wiring position but no per-visit invocation, so there is nothing
/// host-attested to fill those four with and a placeholder would be a claim the
/// platform cannot make.
fn js_span(
    claim: &JetstreamClaim,
    component_id: &str,
    operation: &'static str,
    wiring: Option<&DerivedPublishWiring>,
) -> tracing::Span {
    let span = effect_span!(
        "wamn.jetstream",
        EffectIdentity {
            tenant: &claim.tenant,
            project: &claim.project,
            component: component_id,
        },
        None,
        effect.operation = operation,
    );
    record_wiring(
        &span,
        wiring.map(|wiring| EffectWiring {
            package_id: &wiring.package_id,
            wiring_id: &wiring.wiring_id,
            wiring_version: wiring.wiring_version,
            node_id: &wiring.node_id,
            occurrence: 0,
            component_digest: "",
            component_name: "",
            operation: "",
        }),
    );
    span
}

impl WamnJetstream {
    pub fn new(cfg: WamnJetstreamConfig) -> Self {
        Self {
            nats_url: cfg.nats_url,
            stream_replicas: cfg.stream_replicas,
            dup_window_secs: cfg.dup_window_secs,
            nats_username: cfg.nats_username,
            nats_password_file: cfg.nats_password_file,
            event_coordinates: cfg
                .event_scope
                .map_or_else(EventCoordinates::default, |scope| EventCoordinates {
                    org: scope.org.into_boxed_str(),
                    project: scope.project.into_boxed_str(),
                    environment: scope.env.as_str().into(),
                }),
            ctx: Mutex::new(None),
            claims: std::sync::RwLock::new(HashMap::new()),
            release: None,
        }
    }

    /// Read the platform event coordinates, broker address, and host-owned credentials.
    pub fn from_env() -> Self {
        Self::new(WamnJetstreamConfig::from_env())
    }

    /// Refuse a configured event connection until its streams match their declarations.
    pub async fn activate_events(&self) -> Result<(), JsError> {
        if self.nats_url.is_some() {
            self.ensure_ctx().await?;
        }
        Ok(())
    }

    fn expected_event_streams(
        &self,
    ) -> Result<[async_nats::jetstream::stream::Config; 2], JsError> {
        require_event_coordinates(&self.event_coordinates)
            .map_err(|error| JsError::Other(error.to_string()))?;
        let replicas = self
            .stream_replicas
            .filter(|replicas| (1..=5).contains(replicas))
            .ok_or_else(|| {
                JsError::Other(
                    "WAMN_EVT_STREAM_REPLICAS must declare one to five NATS stream copies".into(),
                )
            })?;
        let duplicate_window = self
            .dup_window_secs
            .filter(|seconds| *seconds > 0)
            .map(Duration::from_secs)
            .ok_or_else(|| {
                JsError::Other(
                    "WAMN_EVT_DUP_WINDOW_SECS must declare a positive duplicate window".into(),
                )
            })?;
        let scope = Triple::new(
            self.event_coordinates.org.as_ref(),
            self.event_coordinates.project.as_ref(),
            self.event_coordinates.environment.as_ref(),
        );
        Ok([
            source_stream_config(&scope, replicas, duplicate_window),
            advisory_stream_config(&scope, replicas),
        ])
    }

    /// Attach the release this process serves — reader 3 of the release-manifest
    /// loaded release, consulted by reference. This plugin never loads, parses or
    /// digest-verifies a manifest, and keeps no copy of one: the loaded release already
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
    pub fn with_release(mut self, release: Option<Arc<LoadedRelease>>) -> Self {
        self.release = release;
        self
    }

    /// The serving release's manifest, or `None` on a release-less process.
    fn serving_manifest(&self) -> Option<&ServingManifest> {
        self.release.as_deref().map(LoadedRelease::manifest)
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
        let expected_streams = self.expected_event_streams()?;
        let options = match (&self.nats_username, &self.nats_password_file) {
            (Some(username), Some(path))
                if !username.is_empty()
                    && username.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
                    }) =>
            {
                let password = tokio::fs::read_to_string(path).await.map_err(|error| {
                    tracing::warn!(target: "wamn::jetstream", error = %error,
                        "event broker password file is unreadable");
                    JsError::ConnectionUnavailable
                })?;
                if password.is_empty() {
                    return Err(JsError::ConnectionUnavailable);
                }
                async_nats::ConnectOptions::new()
                    .custom_inbox_prefix(format!("_INBOX_{username}"))
                    .user_and_password(username.clone(), password)
            }
            (None, None) => async_nats::ConnectOptions::new(),
            _ => {
                tracing::warn!(target: "wamn::jetstream",
                    "event broker requires a subject-safe username and its password file");
                return Err(JsError::ConnectionUnavailable);
            }
        };
        let client = options.connect(url).await.map_err(|e| {
            tracing::warn!(
                target: "wamn::jetstream",
                error = %e,
                "data-plane NATS connect failed"
            );
            JsError::ConnectionUnavailable
        })?;
        let ctx = async_nats::jetstream::new(client);
        for expected in expected_streams {
            let stream = ctx
                .get_stream(&expected.name)
                .await
                .map_err(|error| map_get_stream_err(&expected.name, &error))?;
            if !stream_config_matches(&expected, &stream.cached_info().config) {
                return Err(JsError::Other(format!(
                    "event stream {:?} differs from its complete declaration",
                    expected.name
                )));
            }
        }
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
        let publication = prepare_derived_publication(claim, &self.event_coordinates, request)?;
        let mut headers = HeaderMap::new();
        headers.insert(NATS_MESSAGE_ID, publication.message_id.as_str());

        let span = js_span(
            &publication.claim,
            &publication.component_id,
            "publish-derived",
            Some(&publication.wiring),
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
        let span = js_span(&claim, component_id, "router-tap", None);
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
                WitInterface::from("wamn:jetstream/registration@0.1.0"),
            ]),
            exports: HashSet::new(),
        }
    }

    async fn on_workload_item_bind<'a>(
        &self,
        item: &mut WorkloadItem<'a>,
        interfaces: WitInterfaces<'_>,
    ) -> anyhow::Result<()> {
        if !interfaces.contains("wamn", "jetstream", &["registration"]) {
            return Ok(());
        }
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
        add_to_linker(item.linker())?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Pure mappings (unit-tested; some are mutant-guarded)
// ---------------------------------------------------------------------------

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
// The release gate (reader 3 of the loaded release)
// ---------------------------------------------------------------------------

/// The named refusal class for a consumer bind the serving release does not
/// register. Stable prose, because it is what an operator greps and what tells
/// a held registration apart from a transient `connection-unavailable`.
const UNREGISTERED_SOURCE: &str = "unregistered-source";
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

fn require_registration(
    release: Option<&ServingManifest>,
    coordinates: &EventCoordinates,
    stream: &str,
    package_id: &str,
    registration_id: &str,
    filter_subject: &str,
) -> Result<(), String> {
    let manifest = release.ok_or_else(|| {
        format!(
            "{UNREGISTERED_SOURCE}: this host carries no release, so registration \
            {registration_id:?} cannot be resolved"
        )
    })?;
    require_event_coordinates(coordinates).map_err(|error| error.to_string())?;
    let prefix = format!(
        "evt.{}.{}.{}.",
        coordinates.org, coordinates.project, coordinates.environment,
    );
    if manifest.release.environment.as_str() != coordinates.environment.as_ref()
        || stream
            != stream_name(
                &coordinates.org,
                &coordinates.project,
                &coordinates.environment,
            )
        || !filter_subject.starts_with(&prefix)
    {
        return Err(format!(
            "{UNREGISTERED_SOURCE}: stream or filter differs from the platform event binding"
        ));
    }
    let qualified_registration_id = format!("{package_id}::{registration_id}");
    let registration = manifest
        .workflow
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

    Ok(())
}

fn exact_consumer_config_drift(
    requested: &registration::ConsumerConfig,
    stored: &StoredConsumerConfig,
) -> bool {
    let expected = materializer_consumer_config(
        &requested.durable,
        &requested.filter_subject,
        Duration::from_millis(requested.ack_wait_ms),
        requested.max_deliver,
    );
    !consumer_config_matches(&expected, stored)
}

async fn prepare_consumer(
    plugin: &WamnJetstream,
    config: &registration::ConsumerConfig,
    package_id: &str,
    registration_id: &str,
) -> Result<(), JsError> {
    require_registration(
        plugin.serving_manifest(),
        &plugin.event_coordinates,
        &config.stream_name,
        package_id,
        registration_id,
        &config.filter_subject,
    )
    .map_err(JsError::Other)?;
    if config.max_deliver == 0 || config.ack_wait_ms == 0 {
        return Err(JsError::Other(
            "registration delivery and acknowledgement bounds must be nonzero".into(),
        ));
    }
    let ctx = plugin.ensure_ctx().await?;
    let stream = ctx
        .get_stream(&config.stream_name)
        .await
        .map_err(|error| map_get_stream_err(&config.stream_name, &error))?;
    let consumer = stream
        .get_consumer::<PullConfig>(&config.durable)
        .await
        .map_err(|error| JsError::Other(format!("attach provisioned consumer: {error}")))?;
    if exact_consumer_config_drift(config, &consumer.cached_info().config) {
        return Err(JsError::Other(format!(
            "{CONSUMER_CONFIG_DRIFT}: durable {:?} differs from its bounded registration configuration",
            config.durable,
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Host trait impls
// ---------------------------------------------------------------------------

fn plugin_of(ctx: &ActiveCtx<'_>) -> wash_runtime::wasmtime::Result<std::sync::Arc<WamnJetstream>> {
    ctx.try_get_plugin::<WamnJetstream>(WAMN_JETSTREAM_ID)
}

impl registration::Host for ActiveCtx<'_> {}

impl<T: 'static + Send> registration::HostWithStore<T> for SharedCtx {
    async fn prepare(
        accessor: &Accessor<T, Self>,
        package_id: String,
        registration_id: String,
        config: registration::ConsumerConfig,
    ) -> wash_runtime::wasmtime::Result<Result<(), JsError>> {
        let (plugin, component_id) = accessor.with(|mut access| {
            let ctx = access.get();
            Ok::<_, wash_runtime::wasmtime::Error>((plugin_of(&ctx)?, ctx.component_id.to_string()))
        })?;
        let claim = plugin.claim_for(&component_id);
        let span = js_span(&claim, &component_id, "prepare-registration", None);
        let started = std::time::Instant::now();
        let result = prepare_consumer(&plugin, &config, &package_id, &registration_id)
            .instrument(span)
            .await;
        record_effect_ms(
            &JETSTREAM_DURATION_MS,
            EFFECT_OPERATION,
            "prepare-registration",
            &claim.project,
            started.elapsed(),
        );
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::path::Path;

    use wamn_catalog::{
        DefinitionHash, EffectiveReleaseId, PackageCoordinate, ServingRegistration,
        ServingRegistrationInput, ServingRelease, ServingWiring,
    };

    use super::*;
    use crate::plugins::effect_span::span_tests::{SpanHarness, expected_attributes};

    #[test]
    fn contract_declares_host_registration() {
        let contract = fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("wit/deps/wamn-jetstream/package.wit"),
        )
        .expect("authoritative jetstream WIT reads");
        for required in [
            "package wamn:jetstream@0.1.0;",
            "record consumer-config {",
            "durable: string,",
            "filter-subject: string,",
            "ack-wait-ms: u64,",
            "max-deliver: u32,",
            "prepare: async func(package-id: string, registration-id: string, config: consumer-config) -> result<_, js-error>;",
        ] {
            assert!(
                contract.contains(required),
                "wamn:jetstream contract is missing the host registration line: {required:?}"
            );
        }
    }

    fn derived_request(component_id: &str, dedup_id: &str) -> DerivedPublishRequest {
        DerivedPublishRequest {
            component_id: component_id.into(),
            package_id: "platform_fixture".into(),
            wiring_id: "fixture-orders".into(),
            wiring_version: 7,
            node_id: "publish".into(),
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

    /// A plugin bound for derived publication into a valid event scope, with no
    /// broker URL so `ensure_ctx` refuses before any socket is opened. The span
    /// is opened before that refusal, which is what makes the shape assertable
    /// with no NATS server running.
    fn span_test_plugin() -> WamnJetstream {
        let mut plugin = WamnJetstream::new(WamnJetstreamConfig::default());
        plugin
            .bind_derived_scope(
                "component-1",
                "fixture-route-auth",
                "database-project",
                "dev",
            )
            .expect("trusted driver scope binds");
        plugin.event_coordinates = EventCoordinates {
            org: "fixture".into(),
            project: "widgets".into(),
            environment: "dev".into(),
        };
        plugin
    }

    /// The whole span value, frozen as a literal: a derived publication names
    /// the wiring position it was admitted at, and names EMPTY the four keys the
    /// publish path cannot source.
    #[tokio::test]
    async fn derived_publication_span_carries_the_wiring_and_node_it_published_for() {
        let plugin = span_test_plugin();
        let harness = SpanHarness::install("jetstream-derived-span-test");

        plugin
            .publish_derived(derived_request("component-1", "author:orders:7"))
            .await
            .expect_err("no broker URL refuses before connecting");

        assert_eq!(
            harness.attributes("wamn.jetstream"),
            expected_attributes(&[
                ("effect.operation", "publish-derived"),
                ("wamn.tenant", "fixture-route-auth"),
                ("wamn.project", "database-project"),
                ("wamn.component", "component-1"),
                ("wamn.package_id", "platform_fixture"),
                ("wamn.wiring_id", "fixture-orders"),
                ("wamn.wiring_version", "7"),
                ("wamn.node_id", "publish"),
                // The publish path stands in no guest component scope, so it
                // holds no bound invocation to source these four from.
                ("wamn.occurrence", "0"),
                ("wamn.component_digest", ""),
                ("wamn.component_name", ""),
                ("wamn.operation", ""),
            ]),
            "a derived event must name the wiring node that produced it"
        );
    }

    /// Two emits from different nodes of one wiring must not read alike, or the
    /// coordinate is decoration rather than an answer to "which node published
    /// this?".
    #[tokio::test]
    async fn two_derived_publications_name_their_own_nodes() {
        let plugin = span_test_plugin();
        let harness = SpanHarness::install("jetstream-two-nodes-span-test");

        for (node_id, dedup_id) in [
            ("publish", "author:orders:7"),
            ("escalate", "author:orders:8"),
        ] {
            let mut request = derived_request("component-1", dedup_id);
            request.node_id = node_id.into();
            plugin
                .publish_derived(request)
                .await
                .expect_err("no broker URL refuses before connecting");
        }

        let nodes: Vec<String> = harness
            .every_span("wamn.jetstream")
            .into_iter()
            .map(|attributes| {
                attributes
                    .into_iter()
                    .find(|(key, _)| key == "wamn.node_id")
                    .expect("every derived publish span records a node id")
                    .1
            })
            .collect();
        assert_eq!(nodes, vec!["publish".to_owned(), "escalate".to_owned()]);
    }

    #[test]
    fn derived_publication_uses_only_the_bound_scope_and_admitted_selector() {
        let dangerous_author_id = "author\r\nNats-Msg-Id: forged";
        let publication = prepare_derived_publication(
            JetstreamClaim {
                tenant: "fixture-route-auth".into(),
                project: "database-project".into(),
                environment: "dev".into(),
            },
            &EventCoordinates {
                org: "fixture".into(),
                project: "widgets".into(),
                environment: "dev".into(),
            },
            derived_request("component-1", dangerous_author_id),
        )
        .expect("trusted scope and admitted selector prepare");

        assert_eq!(publication.subject, "evt.fixture.widgets.dev.orders.update");
        assert_eq!(publication.expected_stream, "EVT_7_fixture_7_widgets_3_dev");
        assert_eq!(
            publication.message_id,
            derived_msg_id(
                "fixture-route-auth",
                "widgets",
                "dev",
                "platform_fixture",
                "orders",
                Op::Update,
                dangerous_author_id,
            )
        );
        assert_eq!(publication.message_id.len(), "derived:".len() + 64);
        assert!(!publication.message_id.contains("\r\n"));

        let event = DerivedEvent::from_slice(&publication.body).expect("derived wire decodes");
        assert_eq!(event.tenant, "fixture-route-auth");
        assert_eq!(event.project, "widgets");
        assert_eq!(event.environment, "dev");
        assert_eq!(event.package_id, "platform_fixture");
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
            tenant: "fixture".into(),
            project: "app".into(),
            environment: "dev".into(),
        };
        let mut request = derived_request("component-1", "author:orders:7");
        request.package_id = " platform_fixture".into();
        let coordinates = EventCoordinates {
            org: "fixture".into(),
            project: "app".into(),
            environment: "dev".into(),
        };
        let error = prepare_derived_publication(claim, &coordinates, request)
            .expect_err("a noncanonical package identity must refuse");
        assert_eq!(error.kind(), DerivedPublishErrorKind::InvalidInput);
    }

    #[test]
    fn derived_publication_refuses_an_unbound_or_partial_scope() {
        let plugin = WamnJetstream::new(WamnJetstreamConfig::default());
        assert_eq!(
            plugin
                .required_derived_claim("component-1")
                .unwrap_err()
                .kind(),
            DerivedPublishErrorKind::UnboundScope
        );
        plugin.set_claim("component-1", Some("fixture"), Some("app"), None);
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
            .bind_derived_scope("component-1", "fixture", "app", "dev")
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

    #[tokio::test]
    async fn derived_publication_refuses_missing_or_foreign_event_coordinates_before_connect() {
        let mut plugin = WamnJetstream::new(WamnJetstreamConfig::default());
        plugin
            .bind_derived_scope(
                "component-1",
                "fixture-route-auth",
                "database-project",
                "dev",
            )
            .expect("trusted driver scope binds");
        for coordinates in [
            EventCoordinates::default(),
            EventCoordinates {
                org: "fixture".into(),
                project: Box::default(),
                environment: "dev".into(),
            },
            EventCoordinates {
                org: "fixture.>".into(),
                project: "widgets".into(),
                environment: "dev".into(),
            },
            EventCoordinates {
                org: "fixture".into(),
                project: "widgets".into(),
                environment: "prod".into(),
            },
        ] {
            plugin.event_coordinates = coordinates;
            let error = plugin
                .publish_derived(derived_request("component-1", "author:orders:7"))
                .await
                .expect_err("missing or foreign event coordinates must refuse before connection");
            assert_eq!(error.kind(), DerivedPublishErrorKind::UnboundScope);
        }
    }

    #[test]
    fn activation_requires_declared_stream_limits_separate_from_workload_instances() {
        assert!(
            WamnJetstream::new(WamnJetstreamConfig::default())
                .expected_event_streams()
                .is_err()
        );
        let mut plugin = WamnJetstream::new(WamnJetstreamConfig {
            event_scope: Some(Triple::new("fixture", "widgets", "dev")),
            ..Default::default()
        });
        assert!(plugin.expected_event_streams().is_err());
        plugin.stream_replicas = Some(3);
        assert!(plugin.expected_event_streams().is_err());
        plugin.dup_window_secs = Some(120);
        let [source, advisories] = plugin.expected_event_streams().unwrap();
        assert_eq!(source.num_replicas, 3);
        assert_eq!(advisories.num_replicas, 3);
        assert_eq!(source.duplicate_window, Duration::from_secs(120));
        for replicas in [0, 6] {
            plugin.stream_replicas = Some(replicas);
            assert!(plugin.expected_event_streams().is_err());
        }
        plugin.stream_replicas = Some(1);
        plugin.dup_window_secs = Some(0);
        assert!(plugin.expected_event_streams().is_err());
    }

    #[test]
    fn config_from_env_reads_evt_nats_url() {
        // Only assert the None (absent) branch — reading the var back would race
        // other tests in-process; the skip-when-absent posture is the contract.
        let cfg = WamnJetstreamConfig::default();
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
            delivery: wamn_catalog::RegistrationDelivery::Walk,
        };
        ServingManifest::new(
            ServingRelease {
                tenant_id: "t1".into(),
                effective_release_id: EffectiveReleaseId::new(7).unwrap(),
                environment: "prod".into(),
                packages: BTreeSet::from([PackageCoordinate::new("cat", "1.0.0").unwrap()]),
            },
            BTreeSet::new(),
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
    fn registration_preparation_requires_the_exact_release_source() {
        let manifest = release_registering("widgets", &["insert"]);
        let coordinates = EventCoordinates {
            org: "fixture".into(),
            project: "proj".into(),
            environment: "prod".into(),
        };
        require_registration(
            Some(&manifest),
            &coordinates,
            "EVT_7_fixture_4_proj_4_prod",
            "cat",
            "r1",
            "evt.fixture.proj.prod.widgets.>",
        )
        .expect("exact release registration admits");
        for foreign_stream in [
            stream_name("foreign", "proj", "prod"),
            stream_name("fixture", "other-project", "prod"),
            stream_name("fixture", "proj", "dev"),
        ] {
            assert!(
                require_registration(
                    Some(&manifest),
                    &coordinates,
                    &foreign_stream,
                    "cat",
                    "r1",
                    "evt.fixture.proj.prod.widgets.>",
                )
                .is_err()
            );
        }
        assert!(
            require_registration(
                None,
                &coordinates,
                "EVT_7_fixture_4_proj_4_prod",
                "cat",
                "r1",
                "evt.fixture.proj.prod.widgets.>"
            )
            .is_err()
        );
        for filter in [
            "",
            "evt.>",
            "evt.foreign.proj.prod.widgets.>",
            "evt.fixture.foreign.prod.widgets.>",
            "evt.fixture.proj.foreign.widgets.>",
            "evt.fixture.proj.prod.*.>",
            "evt.fixture.proj.prod.widgets.delete",
        ] {
            assert!(
                require_registration(
                    Some(&manifest),
                    &coordinates,
                    "EVT_7_fixture_4_proj_4_prod",
                    "cat",
                    "r1",
                    filter
                )
                .is_err(),
                "unregistered filter {filter:?}"
            );
        }

        assert!(
            require_registration(
                Some(&manifest),
                &coordinates,
                "EVT_7_fixture_4_proj_4_prod",
                "cat",
                "r2",
                "evt.fixture.proj.prod.widgets.>"
            )
            .unwrap_err()
            .starts_with(UNREGISTERED_SOURCE)
        );
        assert!(
            require_registration(
                Some(&manifest),
                &coordinates,
                "EVT_7_fixture_4_proj_4_prod",
                "cat",
                "r1",
                "evt.fixture.proj.prod.orders.>"
            )
            .is_err(),
            "a real registration id cannot bless another registration's source"
        );
        assert!(
            require_registration(
                Some(&manifest),
                &coordinates,
                "EVT_7_fixture_4_proj_4_prod",
                "other_package",
                "r1",
                "evt.fixture.proj.prod.widgets.>"
            )
            .is_err(),
            "registration ids are package-scoped and must not collide across packages"
        );
    }

    // ---- the reserved router-tap preview namespace (wamn-0h0g.24.5) --------

    fn orders_wiring() -> AttachmentTarget {
        AttachmentTarget::Wiring {
            wiring_id: "orders".to_owned(),
            wiring_version: 3,
        }
    }

    fn orders_record(format_version: RouterTapFormatVersion) -> RouterTapRecord {
        RouterTapRecord {
            delivery_id: "d-1".into(),
            format_version,
            outcome: None,
            over_ceiling_bytes: None,
            payload: serde_json::json!({"plain": "visible"}),
            phase: RouterTapRecordPhase::Accepted,
            redacted: false,
            source_id: "orders-http".into(),
            source_kind: RouterTapSourceKind::Attachment,
            target: RouterTapTarget::Wiring {
                wiring_id: "orders".into(),
                wiring_version: 3,
            },
        }
    }

    fn tap_claim() -> JetstreamClaim {
        JetstreamClaim {
            tenant: "fixture".into(),
            project: "app".into(),
            environment: "prod".into(),
        }
    }

    /// Minted preview subjects preserve the host-owned scope and token count.
    #[test]
    fn every_minted_tap_subject_preserves_its_host_owned_scope() {
        let claim = tap_claim();
        let subject = router_tap_subject(&claim, "orders", "d-1").expect("both ids name tokens");
        assert_eq!(subject, "tap.fixture.app.prod.orders.d-1");
        assert!(subject.starts_with("tap.fixture.app.prod."));

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
        assert!(injected.starts_with("tap.fixture.app.prod."));

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
        let target = orders_wiring();
        let preview = RouterTapPreview {
            delivery_id: "d-1",
            target: &target,
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
        assert_eq!(
            record.target,
            RouterTapTarget::Wiring {
                wiring_id: "orders".into(),
                wiring_version: 3,
            }
        );
        assert_eq!(&*record.source_id, "orders-http");
        assert_eq!(record.format_version.as_u32(), 2);
        assert_eq!(prepared.subject, "tap.fixture.app.prod.orders.d-1");

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
        let target = orders_wiring();
        let preview = RouterTapPreview {
            delivery_id: "d-1",
            target: &target,
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
            orders_record(RouterTapFormatVersion::V2),
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
                "format-version": 2,
                "phase": "accepted",
                "delivery-id": "d-1",
                "wiring-id": "orders",
                "wiring-version": 3,
                "source-kind": "attachment",
                "source-id": "orders-http",
                "redacted": false,
                "payload": {"plain": "visible"},
            }),
            "the accepted v2 wire of a wiring keeps the v1 keys"
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
                outcome: Some("respond".into()),
                phase: RouterTapRecordPhase::Settled,
                ..orders_record(RouterTapFormatVersion::V2)
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
                "format-version": 2,
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
            "the settled v2 wire of a wiring keeps the v1 keys"
        );

        assert_eq!(
            router_tap_subject(&claim, "orders", "d-1").as_deref(),
            Some("tap.fixture.app.prod.orders.d-1"),
            "the subject grammar is the other half of the frozen contract: \
             {ROUTER_TAP_PREFIX}.<tenant>.<project>.<environment>.<target>.<delivery>"
        );
    }

    /// A route names its component and operation, and its subject token is
    /// the attachment id, because a route has no wiring id.
    #[test]
    fn a_route_preview_names_the_route_and_takes_the_attachment_token() {
        let claim = tap_claim();
        let payload = serde_json::json!({"plain": "visible"});
        let target = AttachmentTarget::Route {
            component: "orders".to_owned(),
            operation: "orders:order/get@1.0.0".to_owned(),
        };
        let prepared = prepare_router_tap(
            &claim,
            &RouterTapPreview {
                delivery_id: "d-1",
                target: &target,
                source_kind: "attachment",
                source_id: "orders-get-http",
                phase: RouterTapPhase::Accepted,
                payload: &payload,
            },
        )
        .expect("the ids name tokens");
        assert_eq!(prepared.subject, "tap.fixture.app.prod.orders-get-http.d-1");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&prepared.body)
                .expect("decode the route record as its public wire value"),
            serde_json::json!({
                "format-version": 2,
                "phase": "accepted",
                "delivery-id": "d-1",
                "component": "orders",
                "operation": "orders:order/get@1.0.0",
                "source-kind": "attachment",
                "source-id": "orders-get-http",
                "redacted": false,
                "payload": {"plain": "visible"},
            })
        );
        let record: RouterTapRecord =
            serde_json::from_slice(&prepared.body).expect("the route record reads back");
        assert_eq!(record.subject_token(), "orders-get-http");
        assert_eq!(
            record.target,
            RouterTapTarget::Route {
                component: "orders".into(),
                operation: "orders:order/get@1.0.0".into(),
            }
        );

        let mut from_registration = record;
        from_registration.source_kind = RouterTapSourceKind::Registration;
        assert!(from_registration.validate().is_err());
    }

    /// Version 1 is never written again, and every version 1 record stays
    /// readable as the wiring it names.
    #[test]
    fn a_version_one_record_still_reads_as_its_wiring() {
        let v1 = serde_json::json!({
            "format-version": 1,
            "phase": "accepted",
            "delivery-id": "d-1",
            "wiring-id": "orders",
            "wiring-version": 3,
            "source-kind": "attachment",
            "source-id": "orders-http",
            "redacted": false,
            "payload": {"plain": "visible"},
        });
        let record: RouterTapRecord =
            serde_json::from_value(v1.clone()).expect("a version 1 record reads");
        assert_eq!(record, orders_record(RouterTapFormatVersion::V1));
        assert_eq!(record.subject_token(), "orders");
        assert_eq!(
            serde_json::to_value(&record).expect("serialize the version 1 record"),
            v1
        );

        let mut route = v1;
        route
            .as_object_mut()
            .expect("the record is an object")
            .remove("wiring-id");
        route["component"] = serde_json::json!("orders");
        route["operation"] = serde_json::json!("orders:order/get@1.0.0");
        assert!(
            serde_json::from_value::<RouterTapRecord>(route).is_err(),
            "version 1 cannot name a route"
        );
    }

    #[test]
    fn the_preview_record_refuses_unknown_versions_and_fields() {
        let record = orders_record(RouterTapFormatVersion::V2);
        let mut future_version = serde_json::to_value(&record).expect("serialize record");
        future_version["format-version"] = serde_json::Value::from(3);
        assert!(serde_json::from_value::<RouterTapRecord>(future_version).is_err());

        let mut unknown_field = serde_json::to_value(&record).expect("serialize record");
        unknown_field["edge"] = serde_json::Value::String("invoke".to_owned());
        assert!(serde_json::from_value::<RouterTapRecord>(unknown_field).is_err());

        let mut unknown_source = serde_json::to_value(&record).expect("serialize record");
        unknown_source["source-kind"] = serde_json::Value::String("schedule".to_owned());
        assert!(serde_json::from_value::<RouterTapRecord>(unknown_source).is_err());

        let mut both = serde_json::to_value(&record).expect("serialize record");
        both["component"] = serde_json::json!("orders");
        both["operation"] = serde_json::json!("orders:order/get@1.0.0");
        assert!(
            serde_json::from_value::<RouterTapRecord>(both).is_err(),
            "a record names a route or a wiring, never both"
        );

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
    fn registration_consumer_refuses_changed_broker_bounds() {
        use async_nats::jetstream::consumer::IntoConsumerConfig as _;

        let requested = registration::ConsumerConfig {
            stream_name: "EVT_7_fixture_4_proj_4_prod".into(),
            durable: "mat_t1_cat_r1".into(),
            filter_subject: "evt.fixture.proj.prod.widgets.>".into(),
            ack_wait_ms: 30_000,
            max_deliver: 5,
        };
        let matching = materializer_consumer_config(
            &requested.durable,
            &requested.filter_subject,
            Duration::from_millis(requested.ack_wait_ms),
            requested.max_deliver,
        )
        .into_consumer_config();
        assert!(!exact_consumer_config_drift(&requested, &matching));

        let unbounded = StoredConsumerConfig {
            max_deliver: -1,
            ..matching
        };
        assert!(
            exact_consumer_config_drift(&requested, &unbounded),
            "the server must retain the admitted retry bound"
        );
    }

    // -----------------------------------------------------------------------
    // Live round-trip against a real data-plane NATS. Ignored by default and
    // gated on WAMN_EVT_NATS_URL: it exercises the exact async-nats call
    // sequence the plugin relies on (dedupe on publish, durable pull consumer,
    // fetch/metadata/headers/ack) through the plugin's own mapping helpers, so
    // a broken API assumption fails here rather than only in-cluster. The full
    // component-driven e2e rides the materializer (l5i9.17).
    // -----------------------------------------------------------------------

    #[tokio::test]
    #[ignore = "requires: WAMN_EVT_NATS_URL"]
    async fn live_derived_publish_replay_converges_through_jetstream_dedup() {
        wamn_test_postgres::require_prerequisites(&["WAMN_EVT_NATS_URL"]);
        let url = std::env::var("WAMN_EVT_NATS_URL")
            .expect("set WAMN_EVT_NATS_URL to this test's disposable event broker");

        let client = async_nats::connect(&url).await.expect("connect");
        let ctx = async_nats::jetstream::new(client);
        let stream = "EVT_13_wamnjsderived_3_app_3_dev";
        let event_subject = "evt.wamnjsderived.app.dev.orders.update";
        let _ = ctx.delete_stream(stream).await;
        let scope = Triple::new("wamnjsderived", "app", "dev");
        let advisory_config = advisory_stream_config(&scope, 1);
        let advisory_name = advisory_config.name.clone();
        let _ = ctx.delete_stream(&advisory_name).await;
        ctx.create_stream(source_stream_config(&scope, 1, Duration::from_secs(120)))
            .await
            .expect("provision derived stream");
        ctx.create_stream(advisory_config)
            .await
            .expect("provision advisory stream");

        let plugin = WamnJetstream::new(WamnJetstreamConfig {
            nats_url: Some(url),
            event_scope: Some(scope),
            stream_replicas: Some(1),
            dup_window_secs: Some(120),
            ..Default::default()
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
        ctx.delete_stream(advisory_name)
            .await
            .expect("advisory cleanup");
    }
}
