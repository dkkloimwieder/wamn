//! Native event transport and derived publication.

use std::collections::HashSet;
use std::time::Duration;

use async_nats::HeaderMap;
use async_nats::header::NATS_MESSAGE_ID;
use async_nats::jetstream::Context;
#[cfg(test)]
use async_nats::jetstream::consumer::AckPolicy;
use async_nats::jetstream::consumer::pull::Config as PullConfig;
mod admission;
use anyhow::Context as _;
#[cfg(test)]
use futures_util::StreamExt as _;
use tracing::Instrument as _;
use wamn_broker::events::{
    advisory_stream_config, consumer_config_matches, source_stream_config, stream_config_matches,
};
use wamn_capability_authority::EffectOutcome;
use wamn_event_wire::{Causation, DerivedEvent, Op, derived_msg_id, stream_name, subject};
use wamn_registry::Triple;

use wash_runtime::plugin::HostPlugin;
use wash_runtime::wit::WitWorld;

use wamn_capability_authority::effect_span::{
    EFFECT_OPERATION, EffectIdentity, EffectOutcomeGuard, EffectWiring, JETSTREAM_DURATION_MS,
    effect_span, record_effect_ms, record_wiring,
};
pub const WAMN_JETSTREAM_ID: &str = "wamn-jetstream";

/// An admitted Emit terminal. Event coordinates come from host construction.
/// The caller supplies its package selector, payload and trace position.
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
    UnboundIdentity,
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
    fn new(type_: DerivedPublishErrorType, detail: impl Into<Box<str>>) -> Self {
        Self {
            type_,
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

// ---------------------------------------------------------------------------
// Plugin configuration
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct WamnJetstreamConfig {
    /// The host's data connection, with the environment certificate
    /// (docs/plan/alignment.md R5). `None` ⇒ the plugin registers but every
    /// call returns `connection-unavailable`.
    pub nats: Option<async_nats::Client>,
    /// Trusted event coordinates, separate from project and database authority.
    pub event_target: Option<Triple>,
    /// Declared NATS stream copies, separate from workload instances.
    pub stream_replicas: Option<usize>,
    /// Declared duplicate detection window in seconds.
    pub dup_window_secs: Option<u64>,
}

impl WamnJetstreamConfig {
    /// Read stream declarations. The host supplies its event coordinate and data
    /// connection with [`Self::with_nats`].
    pub fn from_env() -> Self {
        Self {
            nats: None,
            event_target: None,
            stream_replicas: std::env::var("WAMN_EVT_STREAM_REPLICAS")
                .ok()
                .and_then(|value| value.parse().ok()),
            dup_window_secs: std::env::var("WAMN_EVT_DUP_WINDOW_SECS")
                .ok()
                .and_then(|value| value.parse().ok()),
        }
    }

    /// Use `client` for every event call.
    #[must_use]
    pub fn with_nats(mut self, client: async_nats::Client) -> Self {
        self.nats = Some(client);
        self
    }
}

// ---------------------------------------------------------------------------
// Plugin
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct WamnJetstream {
    stream_replicas: Option<usize>,
    dup_window_secs: Option<u64>,
    /// Event coordinates come from the platform bootstrap, separately from DB authority.
    event_coordinates: EventCoordinates,
    /// Validated once when the host constructs the plugin.
    ctx: Option<Context>,
    stream: Option<async_nats::jetstream::stream::Stream>,
}

/// Host identity for event publication and tracing.
#[derive(Clone, Debug)]
struct JetstreamIdentity<'a> {
    project_env_key: Box<str>,
    project: &'a str,
    environment: &'a str,
}

#[derive(Debug, Default)]
struct EventCoordinates {
    org: Box<str>,
    project: Box<str>,
    environment: Box<str>,
}

fn require_event_coordinates(coordinates: &EventCoordinates) -> Result<(), DerivedPublishError> {
    for (name, value) in [
        ("org", coordinates.org.as_ref()),
        ("project", coordinates.project.as_ref()),
        ("environment", coordinates.environment.as_ref()),
    ] {
        if !wamn_event_wire::valid_subject_token(value) {
            return Err(DerivedPublishError::new(
                DerivedPublishErrorType::UnboundIdentity,
                format!("event coordinate {name} is absent or not one NATS subject token"),
            ));
        }
    }
    Ok(())
}

#[derive(Debug)]
struct PreparedDerivedPublication<'a> {
    component_id: String,
    identity: JetstreamIdentity<'a>,
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

fn prepare_derived_publication<'a>(
    identity: JetstreamIdentity<'a>,
    coordinates: &EventCoordinates,
    request: DerivedPublishRequest,
) -> Result<PreparedDerivedPublication<'a>, DerivedPublishError> {
    if identity.environment != coordinates.environment.as_ref() {
        return Err(DerivedPublishError::new(
            DerivedPublishErrorType::UnboundIdentity,
            "derived event environment differs from the platform event binding",
        ));
    }
    if !wamn_event_wire::valid_subject_token(&request.package_id) {
        return Err(DerivedPublishError::new(
            DerivedPublishErrorType::InvalidInput,
            "derived event package-id is empty or noncanonical",
        ));
    }
    if !wamn_event_wire::valid_subject_token(&request.entity) {
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
        identity.project_env_key.to_string(),
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
        &identity.project_env_key,
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
    let source = wamn_event_wire::SourceEvent::Derived(event);
    let body = serde_json::to_vec(&source).map_err(|error| {
        DerivedPublishError::new(
            DerivedPublishErrorType::Serialization,
            format!("serialize derived event: {error}"),
        )
    })?;
    let wamn_event_wire::SourceEvent::Derived(event) = source else {
        unreachable!("the publisher constructed a derived event")
    };
    Ok(PreparedDerivedPublication {
        component_id: request.component_id,
        identity,
        subject: event_subject,
        message_id,
        expected_stream,
        wiring: DerivedPublishWiring {
            package_id: event.package_id,
            wiring_id: request.wiring_id,
            wiring_version: request.wiring_version,
            node_id: request.node_id,
        },
        body,
    })
}

/// Trace a derived publication using the host identity and admitted wiring position.
fn js_span(
    identity: &JetstreamIdentity<'_>,
    component_id: &str,
    operation: &'static str,
    wiring: Option<&DerivedPublishWiring>,
) -> tracing::Span {
    let span = effect_span!(
        "wamn.jetstream",
        EffectIdentity {
            project_env_key: &identity.project_env_key,
            project: identity.project,
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
    pub async fn new(cfg: WamnJetstreamConfig) -> anyhow::Result<Self> {
        let has_target = cfg.event_target.is_some();
        let mut plugin = Self {
            stream_replicas: cfg.stream_replicas,
            dup_window_secs: cfg.dup_window_secs,
            event_coordinates: cfg
                .event_target
                .map_or_else(EventCoordinates::default, |target| EventCoordinates {
                    org: target.org.into_boxed_str(),
                    project: target.project.into_boxed_str(),
                    environment: target.env.as_str().into(),
                }),
            ctx: None,
            stream: None,
        };
        anyhow::ensure!(
            has_target || cfg.nats.is_none(),
            "event connection requires host coordinates"
        );
        let expected = if has_target {
            require_event_coordinates(&plugin.event_coordinates)?;
            Some(plugin.expected_event_streams()?)
        } else {
            None
        };
        if let Some(client) = cfg.nats {
            let expected = expected.expect("the event connection has a validated target");
            let context = async_nats::jetstream::new(client);
            for config in expected {
                let stream = context
                    .get_stream(&config.name)
                    .await
                    .with_context(|| format!("read declared event stream {}", config.name))?;
                if !stream_config_matches(&config, &stream.cached_info().config) {
                    return Err(anyhow::anyhow!(format!(
                        "event stream {} differs from its declaration",
                        config.name
                    )));
                }
                if plugin.stream.is_none() {
                    plugin.stream = Some(stream);
                }
            }
            plugin.ctx = Some(context);
        }
        Ok(plugin)
    }

    fn expected_event_streams(&self) -> anyhow::Result<[async_nats::jetstream::stream::Config; 2]> {
        let replicas = self
            .stream_replicas
            .filter(|replicas| (1..=5).contains(replicas))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "WAMN_EVT_STREAM_REPLICAS must declare one to five NATS stream copies",
                )
            })?;
        let duplicate_window =
            self.dup_window_secs
                .filter(|seconds| *seconds > 0)
                .map(Duration::from_secs)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "WAMN_EVT_DUP_WINDOW_SECS must declare a positive duplicate window",
                    )
                })?;
        let target = Triple::new(
            self.event_coordinates.org.as_ref(),
            self.event_coordinates.environment.as_ref(),
            self.event_coordinates.project.as_ref(),
        );
        Ok([
            source_stream_config(&target, replicas, duplicate_window),
            advisory_stream_config(&target, replicas),
        ])
    }

    /// Publish an admitted Emit terminal and return only after JetStream's
    /// server acknowledgement resolves.
    pub async fn publish_derived(
        &self,
        request: DerivedPublishRequest,
    ) -> Result<DerivedPublishAck, DerivedPublishError> {
        if self.event_coordinates.org.is_empty() {
            return Err(DerivedPublishError::new(
                DerivedPublishErrorType::UnboundIdentity,
                "event target is absent",
            ));
        }
        let identity = JetstreamIdentity {
            project_env_key: wamn_registry::project_env_key(
                &self.event_coordinates.org,
                &self.event_coordinates.project,
                &self.event_coordinates.environment,
            )
            .into(),
            project: &self.event_coordinates.project,
            environment: &self.event_coordinates.environment,
        };
        let publication = prepare_derived_publication(identity, &self.event_coordinates, request)?;
        let mut headers = HeaderMap::new();
        headers.insert(NATS_MESSAGE_ID, publication.message_id.as_str());

        let span = js_span(
            &publication.identity,
            &publication.component_id,
            "publish-derived",
            Some(&publication.wiring),
        );
        let mut observed = EffectOutcomeGuard::new(&span, None);
        let started = std::time::Instant::now();
        let result = async {
            let ctx = self
                .ctx
                .as_ref()
                .ok_or(anyhow::anyhow!("event connection unavailable"))
                .map_err(|error| {
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
            publication.identity.project,
            started.elapsed(),
        );
        let outcome = match &result {
            Ok(_) => EffectOutcome::Responded,
            Err(error) => match error.error_type() {
                DerivedPublishErrorType::UnboundIdentity
                | DerivedPublishErrorType::InvalidInput
                | DerivedPublishErrorType::ConnectionUnavailable
                | DerivedPublishErrorType::Serialization => EffectOutcome::RefusedBeforeDispatch,
                DerivedPublishErrorType::PublishRejected => EffectOutcome::EffectUncertain,
                DerivedPublishErrorType::UnexpectedStream => EffectOutcome::Responded,
            },
        };
        observed.settle(outcome, result.is_err());
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
            imports: HashSet::new(),
            exports: HashSet::new(),
        }
    }
}

const CONSUMER_CONFIG_DRIFT: &str = "event-consumer-config-drift";

#[cfg(test)]
mod tests {

    use super::*;

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

    /// A bound publisher without a broker connection.
    async fn span_test_plugin() -> WamnJetstream {
        WamnJetstream::new(WamnJetstreamConfig {
            event_target: Some(Triple::new("fixture", "dev", "widgets")),
            stream_replicas: Some(1),
            dup_window_secs: Some(120),
            ..Default::default()
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn derived_publication_refuses_without_a_broker() {
        let plugin = span_test_plugin().await;
        let error = plugin
            .publish_derived(derived_request("component-1", "author:orders:7"))
            .await
            .expect_err("publication needs a broker");
        assert_eq!(
            error.error_type(),
            DerivedPublishErrorType::ConnectionUnavailable
        );
    }

    #[test]
    fn derived_publication_uses_only_the_bound_identity_and_admitted_selector() {
        let dangerous_author_id = "author\r\nNats-Msg-Id: forged";
        let publication = prepare_derived_publication(
            JetstreamIdentity {
                project_env_key: "fixture-route-auth".into(),
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
        .expect("trusted target and admitted selector prepare");

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
        assert_eq!(event.project_env_key, "fixture-route-auth");
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
        let identity = JetstreamIdentity {
            project_env_key: "fixture".into(),
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
        let error = prepare_derived_publication(identity, &coordinates, request)
            .expect_err("a noncanonical package identity must refuse");
        assert_eq!(error.error_type(), DerivedPublishErrorType::InvalidInput);
    }

    #[tokio::test]
    async fn construction_refuses_invalid_event_coordinates_before_connect() {
        for target in [
            Triple::new("", "dev", "widgets"),
            Triple::new("fixture", "dev", ""),
            Triple::new("fixture.>", "dev", "widgets"),
        ] {
            assert!(
                WamnJetstream::new(WamnJetstreamConfig {
                    event_target: Some(target),
                    ..Default::default()
                })
                .await
                .is_err()
            );
        }
    }

    #[tokio::test]
    async fn construction_requires_declared_stream_limits() {
        for (replicas, window) in [
            (None, None),
            (Some(0), Some(120)),
            (Some(6), Some(120)),
            (Some(1), Some(0)),
        ] {
            assert!(
                WamnJetstream::new(WamnJetstreamConfig {
                    event_target: Some(Triple::new("fixture", "dev", "widgets")),
                    stream_replicas: replicas,
                    dup_window_secs: window,
                    ..Default::default()
                })
                .await
                .is_err()
            );
        }
        WamnJetstream::new(WamnJetstreamConfig {
            event_target: Some(Triple::new("fixture", "dev", "widgets")),
            stream_replicas: Some(3),
            dup_window_secs: Some(120),
            ..Default::default()
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    #[ignore = "requires: env:WAMN_NATS_URL, env:WAMN_NATS_TLS_CA, env:WAMN_NATS_TLS_CERT, env:WAMN_NATS_TLS_KEY"]
    async fn live_derived_publish_replay_converges_through_jetstream_dedup() {
        wamn_test_postgres::require_ignored_prerequisites(
            "requires: env:WAMN_NATS_URL, env:WAMN_NATS_TLS_CA, env:WAMN_NATS_TLS_CERT, env:WAMN_NATS_TLS_KEY",
        );
        let client = wamn_broker::nats::PlatformNats::from_env()
            .expect("set WAMN_NATS_URL and its certificate to this test's disposable broker")
            .connect()
            .await
            .expect("connect");
        let ctx = async_nats::jetstream::new(client.clone());
        let stream = "EVT_13_wamnjsderived_3_dev_3_app";
        let event_subject = "evt.wamnjsderived.dev.app.orders.update";
        let _ = ctx.delete_stream(stream).await;
        let target = Triple::new("wamnjsderived", "dev", "app");
        let advisory_config = advisory_stream_config(&target, 1);
        let advisory_name = advisory_config.name.clone();
        let _ = ctx.delete_stream(&advisory_name).await;
        ctx.create_stream(source_stream_config(&target, 1, Duration::from_secs(120)))
            .await
            .expect("provision derived stream");
        ctx.create_stream(advisory_config)
            .await
            .expect("provision advisory stream");

        let plugin = WamnJetstream::new(WamnJetstreamConfig {
            nats: Some(client),
            event_target: Some(target),
            stream_replicas: Some(1),
            dup_window_secs: Some(120),
        })
        .await
        .unwrap();
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

/// One message from one entity consumer. Target and release come from the host.
#[derive(Debug)]
pub struct EventBatchAdmission<'a> {
    pub application: &'a str,
    pub source_package: &'a str,
    pub entity: &'a str,
    pub envelope: &'a str,
    pub sequence: u64,
    pub source_event_id: &'a str,
}
