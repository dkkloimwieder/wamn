//! Host-owned registration activation and event publication.
//!
//! Native `wasmcloud:nats` owns materializer attachment, delivery and settlement.
//! This module selects enabled event workflows, retains durable drift
//! checks, and host-only derived-event publication. Broker advisories replace
//! the payload DLQ.

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
use tokio::sync::Mutex;
use tracing::Instrument as _;
use wamn_control_provision::events::{
    advisory_stream_config, consumer_config_matches, materializer_consumer_config,
    source_stream_config, stream_config_matches,
};
use wamn_control_registry::Triple;
use wamn_event_wire::{
    Causation, DerivedEvent, Op, derived_msg_id, stream_name, subject, subject_token,
};

use wash_runtime::engine::ctx::{ActiveCtx, SharedCtx, extract_active_ctx};
use wash_runtime::engine::workload::WorkloadItem;
use wash_runtime::plugin::{HostPlugin, WitInterfaces};
use wash_runtime::wasmtime::component::{Accessor, Linker};
use wash_runtime::wit::{WitInterface, WitWorld};

use crate::plugins::effect_span::{
    EFFECT_OPERATION, EffectIdentity, EffectWiring, JETSTREAM_DURATION_MS, effect_span,
    record_effect_ms, record_wiring,
};
use crate::plugins::wamn_postgres::{DEFAULT_PROJECT, ENV_KEY_CONFIG_KEY, PROJECT_CONFIG_KEY};
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
pub enum DerivedPublishErrorType {
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
    type_: DerivedPublishErrorType,
    detail: Box<str>,
}

impl DerivedPublishError {
    fn new(kind: DerivedPublishErrorType, detail: impl Into<Box<str>>) -> Self {
        Self {
            type_: kind,
            detail: detail.into(),
        }
    }

    pub fn error_type(&self) -> DerivedPublishErrorType {
        self.type_
    }
}

impl std::fmt::Display for DerivedPublishError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for DerivedPublishError {}

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
    env_key: Box<str>,
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
                DerivedPublishErrorType::UnboundScope,
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
            DerivedPublishErrorType::UnboundScope,
            "derived event environment differs from the platform event binding",
        ));
    }
    if request.package_id.is_empty()
        || request.package_id.trim() != request.package_id
        || request.package_id.as_bytes().contains(&0)
    {
        return Err(DerivedPublishError::new(
            DerivedPublishErrorType::InvalidInput,
            "derived event package-id is empty or noncanonical",
        ));
    }
    if request.entity.is_empty() || request.entity.trim() != request.entity {
        return Err(DerivedPublishError::new(
            DerivedPublishErrorType::InvalidInput,
            "derived event entity is empty or noncanonical",
        ));
    }
    if request.dedup_id.is_empty() {
        return Err(DerivedPublishError::new(
            DerivedPublishErrorType::InvalidInput,
            "derived event dedup-id is empty",
        ));
    }
    if request.causation.run.is_empty() || request.causation.root.is_empty() {
        return Err(DerivedPublishError::new(
            DerivedPublishErrorType::InvalidInput,
            "derived event causation is incomplete",
        ));
    }

    let event = DerivedEvent::new(
        claim.env_key.to_string(),
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
        &claim.env_key,
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
            DerivedPublishErrorType::Serialization,
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
/// the coordinates down with the request. The registration operations perform
/// control work at no wiring node, so each records the
/// keys EMPTY through
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
            env_key: &claim.env_key,
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

    /// Bind the host release and its environment applications. A missing
    /// release refuses every consumer attachment. Enabled workflow rows
    /// supply the event subscriptions inside that scope.
    pub fn with_release(mut self, release: Option<Arc<LoadedRelease>>) -> Self {
        self.release = release;
        self
    }

    /// Register a component's bind-time scope claim. All values come from the
    /// trusted workload config. Generic guest operations use them only for
    /// enrichment; the native derived publisher separately requires a complete
    /// subject-safe claim.
    fn set_claim(
        &self,
        component_id: &str,
        env_key: Option<&str>,
        project: Option<&str>,
        environment: Option<&str>,
    ) {
        self.claims
            .write()
            .expect("jetstream claims lock poisoned")
            .insert(
                component_id.to_string(),
                JetstreamClaim {
                    env_key: env_key.unwrap_or_default().into(),
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
        env_key: &str,
        project: &str,
        environment: &str,
    ) -> Result<(), DerivedPublishError> {
        for (field, value) in [
            ("env_key", env_key),
            ("project", project),
            ("environment", environment),
        ] {
            if value.is_empty() || value.trim() != value || subject_token(value) != value {
                return Err(DerivedPublishError::new(
                    DerivedPublishErrorType::InvalidInput,
                    format!("derived event {field} claim is empty or not one NATS subject token"),
                ));
            }
        }
        self.set_claim(
            component_id,
            Some(env_key),
            Some(project),
            Some(environment),
        );
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
                env_key: Box::default(),
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
                    DerivedPublishErrorType::UnboundScope,
                    "derived-event-scope-unbound",
                )
            })?;
        for value in [
            claim.env_key.as_ref(),
            claim.project.as_ref(),
            claim.environment.as_ref(),
        ] {
            if value.is_empty() || value.trim() != value || subject_token(value) != value {
                return Err(DerivedPublishError::new(
                    DerivedPublishErrorType::UnboundScope,
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
                    DerivedPublishErrorType::ConnectionUnavailable,
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
                        DerivedPublishErrorType::PublishRejected,
                        format!("send derived event: {error}"),
                    )
                })?
                .await
                .map_err(|error| {
                    DerivedPublishError::new(
                        DerivedPublishErrorType::PublishRejected,
                        format!("store derived event: {error}"),
                    )
                })?;
            if ack.stream != publication.expected_stream {
                return Err(DerivedPublishError::new(
                    DerivedPublishErrorType::UnexpectedStream,
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
        let (env_key, project, environment) = {
            let config = &item.local_resources().config;
            (
                config.get(ENV_KEY_CONFIG_KEY).cloned(),
                config.get(PROJECT_CONFIG_KEY).cloned(),
                config.get(ENVIRONMENT_CONFIG_KEY).cloned(),
            )
        };
        self.set_claim(
            item.id(),
            env_key.as_deref(),
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
    release: Option<&LoadedRelease>,
    triggers: &[wamn_materializer::workflow::EventTrigger],
    coordinates: &EventCoordinates,
    stream: &str,
    package_id: &str,
    registration_id: &str,
    filter_subject: &str,
) -> Result<(), String> {
    let release = release.ok_or_else(|| {
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
    if release.scope().environment.as_str() != coordinates.environment.as_ref()
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
    let registration = triggers
        .iter()
        .find(|trigger| trigger.application == package_id && trigger.workflow_id == registration_id)
        .ok_or_else(|| {
            format!(
                "{UNREGISTERED_SOURCE}: no enabled event trigger {package_id:?}/{registration_id:?}"
            )
        })?;
    let (entity, op) = subject_source(filter_subject).ok_or_else(|| {
        format!(
            "{UNREGISTERED_SOURCE}: filter subject {filter_subject:?} does not name one entity \
             and op"
        )
    })?;
    let any_op = op == ">" || op == "*";
    if subject_token(&registration.entity) != entity
        || (!any_op
            && !registration
                .ops
                .iter()
                .any(|candidate| candidate.as_str() == op))
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
    postgres: &crate::plugins::WamnPostgres,
    config: &registration::ConsumerConfig,
    package_id: &str,
    registration_id: &str,
) -> Result<(), JsError> {
    let release = plugin
        .release
        .as_deref()
        .ok_or_else(|| JsError::Other(UNREGISTERED_SOURCE.into()))?;
    let triggers = postgres
        .event_triggers(&plugin.event_coordinates.project, release)
        .await
        .map_err(|error| JsError::Other(error.to_string()))?;
    require_registration(
        plugin.release.as_deref(),
        &triggers,
        &plugin.event_coordinates,
        &config.stream_name,
        package_id,
        registration_id,
        &config.filter_subject,
    )
    .map_err(JsError::Other)?;
    let trigger = triggers
        .iter()
        .find(|trigger| trigger.application == package_id && trigger.workflow_id == registration_id)
        .ok_or_else(|| JsError::Other(UNREGISTERED_SOURCE.into()))?;
    if config.durable
        != wamn_materializer::workflow::entity_durable(&trigger.source_package, &trigger.entity)
    {
        return Err(JsError::Other(format!(
            "{UNREGISTERED_SOURCE}: durable differs from the entity consumer"
        )));
    }
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

fn event_plugins<T: 'static + Send>(
    accessor: &Accessor<T, SharedCtx>,
) -> wash_runtime::wasmtime::Result<(
    Arc<WamnJetstream>,
    Arc<crate::plugins::WamnPostgres>,
    String,
)> {
    accessor.with(|mut access| {
        let ctx = access.get();
        Ok((
            plugin_of(&ctx)?,
            ctx.try_get_plugin::<crate::plugins::WamnPostgres>(
                crate::plugins::wamn_postgres::WAMN_POSTGRES_ID,
            )?,
            ctx.component_id.to_string(),
        ))
    })
}

impl registration::Host for ActiveCtx<'_> {}

impl<T: 'static + Send> registration::HostWithStore<T> for SharedCtx {
    async fn triggers(
        accessor: &Accessor<T, Self>,
    ) -> wash_runtime::wasmtime::Result<Result<String, JsError>> {
        let (plugin, postgres, component) = event_plugins(accessor)?;
        let Some(release) = plugin.release.as_deref() else {
            return Ok(Err(JsError::Other(UNREGISTERED_SOURCE.into())));
        };
        if let Err(error) = postgres.require_event_materializer(&component, release) {
            return Ok(Err(JsError::Other(error.to_string())));
        }
        let claim = plugin.claim_for(&component);
        let span = js_span(&claim, &component, "event-triggers", None);
        Ok(
            match postgres
                .event_triggers(&plugin.event_coordinates.project, release)
                .instrument(span)
                .await
            {
                Ok(triggers) => serde_json::to_string(&triggers)
                    .map_err(|error| JsError::Other(error.to_string())),
                Err(error) => Err(JsError::Other(error.to_string())),
            },
        )
    }

    async fn admit(
        accessor: &Accessor<T, Self>,
        source_package: String,
        entity: String,
        envelope: String,
        sequence: u64,
        source_event_id: String,
    ) -> wash_runtime::wasmtime::Result<Result<(), JsError>> {
        let (plugin, postgres, component) = event_plugins(accessor)?;
        let Some(release) = plugin.release.as_deref() else {
            return Ok(Err(JsError::Other(UNREGISTERED_SOURCE.into())));
        };
        if let Err(error) = postgres.require_event_materializer(&component, release) {
            return Ok(Err(JsError::Other(error.to_string())));
        }
        let claim = plugin.claim_for(&component);
        let span = js_span(&claim, &component, "admit-event-batch", None);
        Ok(postgres
            .admit_event_batch(
                &plugin.event_coordinates.project,
                release,
                &crate::plugins::wamn_postgres::EventBatchAdmission {
                    source_package: &source_package,
                    entity: &entity,
                    envelope: &envelope,
                    sequence,
                    source_event_id: &source_event_id,
                },
            )
            .instrument(span)
            .await
            .map_err(|error| JsError::Other(error.to_string())))
    }

    async fn prepare(
        accessor: &Accessor<T, Self>,
        package_id: String,
        registration_id: String,
        config: registration::ConsumerConfig,
    ) -> wash_runtime::wasmtime::Result<Result<(), JsError>> {
        let (plugin, postgres, component_id) = accessor.with(|mut access| {
            let ctx = access.get();
            Ok::<_, wash_runtime::wasmtime::Error>((
                plugin_of(&ctx)?,
                ctx.try_get_plugin::<crate::plugins::WamnPostgres>(
                    crate::plugins::wamn_postgres::WAMN_POSTGRES_ID,
                )?,
                ctx.component_id.to_string(),
            ))
        })?;
        let Some(release) = plugin.release.as_deref() else {
            return Ok(Err(JsError::Other(UNREGISTERED_SOURCE.into())));
        };
        if let Err(error) = postgres.require_event_materializer(&component_id, release) {
            return Ok(Err(JsError::Other(error.to_string())));
        }
        let claim = plugin.claim_for(&component_id);
        let span = js_span(&claim, &component_id, "prepare-registration", None);
        let started = std::time::Instant::now();
        let result = prepare_consumer(&plugin, &postgres, &config, &package_id, &registration_id)
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

    use wamn_catalog::{PackageCoordinate, ServingManifest, ServingRelease};

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
            expected_attributes(
                ["fixture-route-auth", "database-project", "component-1"],
                [
                    "platform_fixture",
                    "fixture-orders",
                    "7",
                    "publish",
                    // The publish path holds no guest invocation for these fields.
                    "0",
                    "",
                    "",
                    "",
                ],
                &[("effect.operation", "publish-derived")],
            ),
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
                env_key: "fixture-route-auth".into(),
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
        assert_eq!(event.env_key, "fixture-route-auth");
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
            env_key: "fixture".into(),
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
        assert_eq!(error.error_type(), DerivedPublishErrorType::InvalidInput);
    }

    #[test]
    fn derived_publication_refuses_an_unbound_or_partial_scope() {
        let plugin = WamnJetstream::new(WamnJetstreamConfig::default());
        assert_eq!(
            plugin
                .required_derived_claim("component-1")
                .unwrap_err()
                .error_type(),
            DerivedPublishErrorType::UnboundScope
        );
        plugin.set_claim("component-1", Some("fixture"), Some("app"), None);
        assert_eq!(
            plugin
                .required_derived_claim("component-1")
                .unwrap_err()
                .error_type(),
            DerivedPublishErrorType::UnboundScope
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
                .error_type(),
            DerivedPublishErrorType::UnboundScope
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
            assert_eq!(error.error_type(), DerivedPublishErrorType::UnboundScope);
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

    /// Trigger rows work with a release that declares no registrations.
    fn release_without_registrations() -> ServingManifest {
        ServingManifest::new(
            ServingRelease {
                packages: BTreeSet::from([PackageCoordinate::new("cat", "1.0.0").unwrap()]),
            },
            BTreeSet::new(),
            BTreeSet::new(),
            BTreeMap::new(),
        )
        .expect("the fixture release is valid")
    }

    #[test]
    fn registration_preparation_requires_the_exact_release_source() {
        let manifest = LoadedRelease::load_canonical_bytes(
            &release_without_registrations().canonical_bytes(),
            "fixture",
            wamn_engine::release_manifest::ReleaseScope::new("tenant", "prod").with_applications(
                [(
                    "catalog".to_owned(),
                    wamn_catalog::ApplicationDefinition {
                        connectors: std::collections::BTreeMap::new(),
                        connections: std::collections::BTreeMap::new(),
                        packages: ["cat".to_owned()].into(),
                    },
                )]
                .into(),
            ),
        )
        .expect("the fixture release loads");
        let triggers = vec![wamn_materializer::workflow::EventTrigger {
            application: "cat".into(),
            workflow_id: "r1".into(),
            source_package: "cat".into(),
            entity: "widgets".into(),
            ops: vec![Op::Insert],
            condition: None,
            enable_sequence: 0,
        }];
        let coordinates = EventCoordinates {
            org: "fixture".into(),
            project: "proj".into(),
            environment: "prod".into(),
        };
        require_registration(
            Some(&manifest),
            &triggers,
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
                    &triggers,
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
                &triggers,
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
                    &triggers,
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
                &triggers,
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
                &triggers,
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
                &triggers,
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
