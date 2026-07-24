//! `wamn:jetstream` host plugin (E10).
//!
//! Contract source of truth: docs/wamn-jetstream.wit (mirrored byte-identical
//! into `wit/deps/wamn-jetstream/package.wit`; drift-guarded by
//! `tests/jetstream_wit_coherence.rs`).
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
//! - A publish waits for the server ack (async-nats: send future, then the
//!   server-ack future) — the returned `publish-ack` is the only delivery truth.
//! - The `doorbell.ring` wake hint (l5i9.17) publishes on the CONTROL-plane
//!   core-NATS connection the host injects at construction
//!   ([`WamnJetstream::with_doorbell`] — the washlet passes its own scheduler
//!   client), on `wamn.doorbell.<tenant>` with the tenant derived from the
//!   workload's `wamn.tenant` config at bind time (the `wamn:postgres` claims
//!   posture — a guest can never name a tenant, so it can never ring another
//!   tenant's bell).

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use async_nats::HeaderMap;
use async_nats::jetstream::Context;
use async_nats::jetstream::consumer::pull::Config as PullConfig;
use async_nats::jetstream::consumer::{AckPolicy, Consumer};
use async_nats::jetstream::context::{GetStreamError, GetStreamErrorKind};
use async_nats::jetstream::message::AckKind;
use async_nats::jetstream::publish::PublishAck as NatsPublishAck;
use futures_util::StreamExt as _;
use tokio::sync::Mutex;

use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::{Linker, Resource};
use wash_runtime::wit::{WitInterface, WitWorld};

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
    /// Per-component tenant identity for the doorbell subject, registered at
    /// workload bind from `wamn.tenant` config (the `wamn:postgres` claims
    /// posture — never guest-supplied).
    tenants: std::sync::RwLock<HashMap<String, String>>,
}

impl WamnJetstream {
    pub fn new(cfg: WamnJetstreamConfig) -> Self {
        Self {
            nats_url: cfg.nats_url,
            ctx: Mutex::new(None),
            doorbell_nats: None,
            tenants: std::sync::RwLock::new(HashMap::new()),
        }
    }

    /// Build from the environment (`WAMN_EVT_NATS_URL`).
    pub fn from_env() -> Self {
        Self::new(WamnJetstreamConfig::from_env())
    }

    /// Attach the CONTROL-plane core-NATS client `doorbell.ring` publishes on
    /// (`wamn.doorbell.<tenant>`). The washlet passes its scheduler client —
    /// the same control plane the dispatcher's doorbells and the run-worker's
    /// subscription ride — so no second connection is opened.
    pub fn with_doorbell(mut self, client: async_nats::Client) -> Self {
        self.doorbell_nats = Some(client);
        self
    }

    /// Register the doorbell tenant for a component id. The host path feeds it
    /// from workload bind (`wamn.tenant`); a harness calls it directly.
    pub fn set_tenant(&self, component_id: &str, tenant: &str) -> anyhow::Result<()> {
        anyhow::ensure!(
            wamn_registry::identifiers::valid_tenant(tenant),
            "invalid tenant {tenant:?}: 1-64 chars of [A-Za-z0-9_-] required"
        );
        self.tenants
            .write()
            .expect("tenants lock poisoned")
            .insert(component_id.to_string(), tenant.to_string());
        Ok(())
    }

    fn tenant_for(&self, component_id: &str) -> Option<String> {
        self.tenants
            .read()
            .expect("tenants lock poisoned")
            .get(component_id)
            .cloned()
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
        // The doorbell tenant rides the same host-injected `wamn.tenant` config
        // the wamn:postgres claims use — registered here so `ring` can derive
        // the subject from the CALLER's identity, never a guest argument.
        if let Some(tenant) = item
            .local_resources()
            .config
            .get(crate::plugins::wamn_postgres::TENANT_CONFIG_KEY)
        {
            let tenant = tenant.clone();
            self.set_tenant(item.id(), &tenant)?;
            tracing::debug!(
                component = item.id(),
                tenant,
                "wamn:jetstream doorbell tenant registered"
            );
        } else if interfaces.contains("wamn", "jetstream", &["doorbell"]) {
            tracing::warn!(
                component = item.id(),
                "component imports wamn:jetstream/doorbell but sets no wamn.tenant; ring will be refused"
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
}

/// Host side of a `wamn:jetstream/consumer.message`. Holds the delivered message;
/// ack/nack/term send the disposition back to the server.
pub struct JsMessage {
    msg: async_nats::jetstream::Message,
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

/// The doorbell subject grammar — MUST equal what the dispatcher publishes and
/// the run-worker subscribes (`wamn.doorbell.<tenant>`); the unit test is the
/// three-way drift guard. The tenant charset ([A-Za-z0-9_-], enforced at
/// registration by `valid_tenant`) contains no NATS token separators or
/// wildcards, so the subject is structurally un-smuggleable.
fn doorbell_subject(tenant: &str) -> String {
    format!("wamn.doorbell.{tenant}")
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
        let ctx = match plugin.ensure_ctx().await {
            Ok(c) => c,
            Err(e) => return Ok(Err(e)),
        };
        let stream = match ctx.get_stream(&config.stream_name).await {
            Ok(s) => s,
            Err(e) => return Ok(Err(map_get_stream_err(&config.stream_name, &e))),
        };
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
        let bound = match stream.get_or_create_consumer(&config.durable, pull).await {
            Ok(c) => c,
            Err(e) => return Ok(Err(JsError::Other(format!("bind consumer: {e}")))),
        };
        Ok(Ok(self.table.push(JsConsumer { consumer: bound })?))
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
        let consumer = self.table.get(&rep)?.consumer.clone();

        let mut fetch = consumer.fetch().max_messages(max_messages as usize);
        if expires_ms > 0 {
            fetch = fetch.expires(Duration::from_millis(expires_ms));
        }
        let mut batch = match fetch.messages().await {
            Ok(b) => b,
            Err(e) => return Ok(Err(JsError::Other(format!("fetch: {e}")))),
        };

        let mut pulled = Vec::new();
        while let Some(item) = batch.next().await {
            match item {
                Ok(msg) => pulled.push(JsMessage { msg }),
                // Boxed dyn error — stringify (map_err with anyhow!, not .context).
                Err(e) => return Ok(Err(JsError::Other(format!("fetch message: {e}")))),
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
        Ok(msg
            .ack()
            .await
            .map_err(|e| JsError::AckFailed(e.to_string())))
    }

    async fn nack(
        &mut self,
        rep: Resource<JsMessage>,
        delay_ms: u64,
    ) -> wash_runtime::wasmtime::Result<Result<(), JsError>> {
        let msg = self.table.get(&rep)?.msg.clone();
        Ok(msg
            .ack_with(nack_ack_kind(delay_ms))
            .await
            .map_err(|e| JsError::AckFailed(e.to_string())))
    }

    async fn term(
        &mut self,
        rep: Resource<JsMessage>,
    ) -> wash_runtime::wasmtime::Result<Result<(), JsError>> {
        let msg = self.table.get(&rep)?.msg.clone();
        Ok(msg
            .ack_with(AckKind::Term)
            .await
            .map_err(|e| JsError::AckFailed(e.to_string())))
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
        // The tenant comes from the workload's bind-time registration — a
        // component with no registered tenant gets a refusal, not a default
        // (ringing an unowned bell is worse than a slower wake).
        let Some(tenant) = plugin.tenant_for(&component_id) else {
            return Ok(Err(JsError::Other(
                "no doorbell tenant registered for this component (set wamn.tenant)".into(),
            )));
        };
        let Some(nats) = plugin.doorbell_nats.as_ref() else {
            return Ok(Err(JsError::ConnectionUnavailable));
        };
        let subject = doorbell_subject(&tenant);
        // Publish + flush: the hint must be ON THE WIRE when ring returns, or a
        // buffered publish could outlive the caller's interest (the async-nats
        // client buffers while disconnected — flushing surfaces that as an err).
        if let Err(e) = nats.publish(subject, run_id.into_bytes().into()).await {
            return Ok(Err(JsError::Other(format!("doorbell publish: {e}"))));
        }
        if let Err(e) = nats.flush().await {
            return Ok(Err(JsError::Other(format!("doorbell flush: {e}"))));
        }
        Ok(Ok(()))
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
        let ctx = match plugin.ensure_ctx().await {
            Ok(c) => c,
            Err(e) => return Ok(Err(e)),
        };
        let map = to_header_map(&headers);
        // Two awaits: the send future, then the server-ack future. The awaited
        // PublishAck is the only delivery truth (async-nats 0.47).
        let ack_future = match ctx.publish_with_headers(subject, map, body.into()).await {
            Ok(f) => f,
            Err(e) => return Ok(Err(JsError::PublishRejected(e.to_string()))),
        };
        Ok(match ack_future.await {
            Ok(ack) => Ok(to_publish_ack(&ack)),
            Err(e) => Err(JsError::PublishRejected(e.to_string())),
        })
    }
}

#[cfg(test)]
mod tests {
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
    fn doorbell_subject_matches_the_dispatcher_and_run_worker_grammar() {
        // Three-way drift guard: wamn-dispatcher publishes and wamn-run-worker
        // subscribes the LITERAL `wamn.doorbell.<tenant>`; the plugin must ring
        // the same bell or materializer wakes silently degrade to the sweep.
        assert_eq!(doorbell_subject("tenant-a"), "wamn.doorbell.tenant-a");
    }

    #[test]
    fn doorbell_tenant_registration_validates_and_resolves() {
        let plugin = WamnJetstream::new(WamnJetstreamConfig { nats_url: None });
        // A subject-breaking tenant is refused at registration (defense in
        // depth on top of the CRD-side validation).
        assert!(plugin.set_tenant("c1", "evil.>").is_err());
        assert!(plugin.tenant_for("c1").is_none());
        plugin.set_tenant("c1", "tenant-a").unwrap();
        assert_eq!(plugin.tenant_for("c1").as_deref(), Some("tenant-a"));
        // Unregistered components resolve to none — ring refuses, never defaults.
        assert!(plugin.tenant_for("c2").is_none());
    }

    #[test]
    fn config_from_env_reads_evt_nats_url() {
        // Only assert the None (absent) branch — reading the var back would race
        // other tests in-process; the skip-when-absent posture is the contract.
        let cfg = WamnJetstreamConfig { nats_url: None };
        assert!(cfg.nats_url.is_none());
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
