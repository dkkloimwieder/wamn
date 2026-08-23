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
//!   Generic event publication and the doorbell hint are not release-gated.
//!   The reserved `dlq.*` namespace is host-only: exact registration bind ties
//!   a fetched message to its release identity before dead-letter publication.
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
use std::time::Duration;

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
use tokio::sync::Mutex;
use tracing::Instrument as _;
use wamn_catalog::ServingManifest;
use wamn_control_registry::identifiers::{
    ExecutionTargetId, doorbell_subject, mvp_execution_target_id,
};
use wamn_event_wire::{
    DEAD_LETTER_STREAM, DeadLetter, DeadLetterHeader, dead_letter_message_id, dead_letter_subject,
    subject_token,
};

use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::{Linker, Resource};
use wash_runtime::wit::{WitInterface, WitWorld};

use crate::plugins::effect_span::{
    EFFECT_OPERATION, EffectIdentity, JETSTREAM_DURATION_MS, effect_span, record_effect_ms,
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
    /// Per-component tenant/project claim, registered at workload bind from the
    /// same trusted `wamn.tenant` / `wamn.project` config the `wamn:postgres`
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
}

#[derive(Clone, Debug)]
struct DeadLetterIdentity {
    tenant: Box<str>,
    environment: Box<str>,
    catalog_id: Box<str>,
    registration_id: Box<str>,
    subject: Box<str>,
}

#[derive(Clone, Debug)]
struct DeadLetterDepthSample {
    identity: DeadLetterIdentity,
    depth: u64,
}

#[derive(Debug, Default)]
struct DeadLetterDepth {
    by_subject: std::sync::Mutex<HashMap<Box<str>, DeadLetterDepthSample>>,
}

impl DeadLetterDepth {
    fn register(depth: &Arc<Self>) {
        let weak = Arc::downgrade(depth);
        let _ = opentelemetry::global::meter("wamn-jetstream")
            .u64_observable_gauge("wamn.jetstream.dlq.depth")
            .with_description("retained dead-letter messages for one release registration")
            .with_callback(move |observer| {
                let Some(depth) = weak.upgrade() else {
                    return;
                };
                if let Ok(samples) = depth.by_subject.lock() {
                    for sample in samples.values() {
                        observer.observe(
                            sample.depth,
                            &[
                                KeyValue::new("wamn.tenant", sample.identity.tenant.to_string()),
                                KeyValue::new(
                                    "wamn.environment",
                                    sample.identity.environment.to_string(),
                                ),
                                KeyValue::new(
                                    "wamn.catalog",
                                    sample.identity.catalog_id.to_string(),
                                ),
                                KeyValue::new(
                                    "wamn.registration",
                                    sample.identity.registration_id.to_string(),
                                ),
                            ],
                        );
                    }
                }
            })
            .build();
    }

    fn update(&self, identity: DeadLetterIdentity, depth: u64) {
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
        let dlq_depth = Arc::new(DeadLetterDepth::default());
        DeadLetterDepth::register(&dlq_depth);
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

    /// Register a component's bind-time tenant/project claim for span
    /// enrichment. Both come from the trusted workload config; neither is
    /// validated here, because nothing but a trace label depends on them.
    fn set_claim(&self, component_id: &str, tenant: Option<&str>, project: Option<&str>) {
        self.claims
            .write()
            .expect("jetstream claims lock poisoned")
            .insert(
                component_id.to_string(),
                JetstreamClaim {
                    tenant: tenant.unwrap_or_default().into(),
                    project: project.unwrap_or(DEFAULT_PROJECT).into(),
                },
            );
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
            })
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
        let (tenant, project) = {
            let config = &item.local_resources().config;
            (
                config.get(TENANT_CONFIG_KEY).cloned(),
                config.get(PROJECT_CONFIG_KEY).cloned(),
            )
        };
        self.set_claim(item.id(), tenant.as_deref(), project.as_deref());
        if let Some(tenant) = tenant {
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
            "{UNREGISTERED_SOURCE}: no registration in release {} of catalog {:?} \
             sources entity {entity:?} op {op:?}",
            manifest.release.catalog_version, manifest.release.catalog_id
        ));
    }
    None
}

fn exact_registration_identity(
    release: Option<&ServingManifest>,
    catalog_id: &str,
    registration_id: &str,
    filter_subject: &str,
) -> Result<DeadLetterIdentity, String> {
    let manifest = release.ok_or_else(|| {
        format!(
            "{UNREGISTERED_SOURCE}: this host carries no release, so registration \
            {registration_id:?} cannot be resolved"
        )
    })?;
    if manifest.release.catalog_id != catalog_id {
        return Err(format!(
            "{UNREGISTERED_SOURCE}: release catalog {:?} does not match requested catalog {catalog_id:?}",
            manifest.release.catalog_id
        ));
    }
    let registration = manifest.registrations.get(registration_id).ok_or_else(|| {
        format!(
            "{UNREGISTERED_SOURCE}: release {} of catalog {:?} has no registration \
                 {registration_id:?}",
            manifest.release.catalog_version, manifest.release.catalog_id
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
        &manifest.release.catalog_id,
        registration_id,
    );
    Ok(DeadLetterIdentity {
        tenant: manifest.release.tenant_id.clone().into_boxed_str(),
        environment: manifest.release.environment.clone().into_boxed_str(),
        catalog_id: manifest.release.catalog_id.clone().into_boxed_str(),
        registration_id: registration_id.into(),
        subject: subject.into_boxed_str(),
    })
}

fn is_reserved_dead_letter_subject(subject: &str) -> bool {
    subject == "dlq" || subject.starts_with("dlq.")
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
        Some((catalog_id, registration_id)) => Some(
            exact_registration_identity(
                plugin.serving_manifest(),
                catalog_id,
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
        catalog_id: String,
        registration_id: String,
        config: consumer::ConsumerConfig,
    ) -> wash_runtime::wasmtime::Result<Result<Resource<JsConsumer>, JsError>> {
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
        let claim = plugin.claim_for(&component_id);
        let span = js_span(&claim, &component_id, "bind-registration");
        let started = std::time::Instant::now();
        let bound = bind_consumer(&plugin, &config, Some((&catalog_id, &registration_id)))
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
        let msg = self.table.get(&rep)?.msg.clone();
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

impl producer::Host for ActiveCtx<'_> {
    async fn publish(
        &mut self,
        subject: String,
        headers: Vec<Header>,
        body: Vec<u8>,
    ) -> wash_runtime::wasmtime::Result<Result<producer::PublishAck, JsError>> {
        let plugin = plugin_of(self)?;
        let component_id = self.component_id.to_string();
        let claim = plugin.claim_for(&component_id);
        let span = js_span(&claim, &component_id, "publish");
        let started = std::time::Instant::now();
        let result = async {
            if is_reserved_dead_letter_subject(&subject) {
                return Err(JsError::PublishRejected(format!(
                    "{RESERVED_DEAD_LETTER_SUBJECT}: use a bound message's dead-letter method"
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
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use wamn_catalog::{
        ServingRegistration, ServingRegistrationInput, ServingRelease, ServingWiring,
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
            wiring_id: "event-handler".into(),
            wiring_version: 1,
            entity: entity.to_string(),
            ops: ops.iter().copied().map(String::from).collect(),
            input: ServingRegistrationInput::Event,
        };
        ServingManifest::new(
            ServingRelease {
                tenant_id: "t1".into(),
                catalog_id: "cat".into(),
                catalog_version: 7,
                environment: "prod".into(),
            },
            BTreeSet::new(),
            BTreeSet::from([ServingWiring {
                wiring_id: "event-handler".into(),
                wiring_version: 1,
                graph_hash:
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                        .into(),
            }]),
            BTreeMap::new(),
            BTreeMap::from([("r1".to_string(), registration)]),
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
                "other-catalog",
                "r1",
                "evt.acme.proj.prod.receipts.>"
            )
            .is_err(),
            "registration ids are catalog-scoped and must not collide across releases"
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
}
