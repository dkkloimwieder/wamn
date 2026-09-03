//! Live, read-only adapters for the development loop's tap and trace views.

use std::collections::VecDeque;
use std::error::Error;
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt as _;
use serde::Deserialize;
use tokio::task::JoinHandle;
use url::Url;
use wamn_runtime::plugins::wamn_jetstream::{
    RouterTapRecord, router_tap_environment_filter, router_tap_record_subject,
};

use super::config::DevConfig;
use super::read::{
    DEV_OBSERVATION_LIMIT, DevReadPublisher, DevTapObservation, DevTraceObservation,
};

const TEMPO_POLL_INTERVAL: Duration = Duration::from_secs(1);
const TEMPO_FETCH_LIMIT: usize = DEV_OBSERVATION_LIMIT + 1;
const TEMPO_MAX_QUERY_PAGES: usize = 64;
const TEMPO_SETTLE_DELAY_SECONDS: u64 = 15;
const OBSERVATION_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Failure to start or decode one development observation source.
#[derive(Debug)]
pub(crate) struct DevObservationError {
    operation: &'static str,
    detail: Box<str>,
    source: Option<anyhow::Error>,
}

impl DevObservationError {
    fn new(operation: &'static str, detail: impl Into<Box<str>>) -> Self {
        Self {
            operation,
            detail: detail.into(),
            source: None,
        }
    }

    fn with_source(
        operation: &'static str,
        detail: impl Into<Box<str>>,
        source: impl Into<anyhow::Error>,
    ) -> Self {
        Self {
            operation,
            detail: detail.into(),
            source: Some(source.into()),
        }
    }
}

impl fmt::Display for DevObservationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.detail)
    }
}

impl Error for DevObservationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_ref().map(AsRef::as_ref)
    }
}

/// Background readers owned for exactly one development-loop session.
#[derive(Debug)]
pub(crate) struct DevObservationReaders {
    tap: JoinHandle<()>,
    tempo: JoinHandle<()>,
}

impl DevObservationReaders {
    /// Connect both sources before the first stage begins.
    pub(crate) async fn start(
        config: &DevConfig,
        publisher: DevReadPublisher,
    ) -> Result<Self, DevObservationError> {
        let identity = config.activation_identity();
        let tap_filter = router_tap_environment_filter(
            &identity.tenant,
            &identity.project,
            &identity.environment,
        )
        .ok_or_else(|| {
            DevObservationError::new(
                "construct router-tap subscription",
                "tenant, project, and environment must each be one NATS subject token",
            )
        })?;
        let tap_client = tokio::time::timeout(
            OBSERVATION_REQUEST_TIMEOUT,
            async_nats::connect(config.event_nats_url()),
        )
        .await
        .map_err(|source| {
            DevObservationError::with_source(
                "connect router-tap reader",
                format!(
                    "event_nats_url did not complete a connection within {}s",
                    OBSERVATION_REQUEST_TIMEOUT.as_secs()
                ),
                source,
            )
        })?
        .map_err(|source| {
            DevObservationError::with_source(
                "connect router-tap reader",
                "event_nats_url refused the configured connection",
                source,
            )
        })?;
        let mut taps = tokio::time::timeout(
            OBSERVATION_REQUEST_TIMEOUT,
            tap_client.subscribe(tap_filter),
        )
        .await
        .map_err(|source| {
            DevObservationError::with_source(
                "subscribe router-tap reader",
                format!(
                    "event_nats_url did not accept the environment subject within {}s",
                    OBSERVATION_REQUEST_TIMEOUT.as_secs()
                ),
                source,
            )
        })?
        .map_err(|source| {
            DevObservationError::with_source(
                "subscribe router-tap reader",
                "event_nats_url refused the environment-scoped subject",
                source,
            )
        })?;

        let tempo_endpoint = tempo_search_endpoint(config.tempo_query_url())?;
        let tempo_query = tempo_query(config);
        let started_at = unix_seconds("start Tempo reader")?;
        let http = reqwest::Client::builder()
            .timeout(OBSERVATION_REQUEST_TIMEOUT)
            .build()
            .map_err(|source| {
                DevObservationError::with_source(
                    "construct Tempo reader",
                    "build the bounded HTTP client",
                    source,
                )
            })?;
        fetch_tempo_page(
            &http,
            &tempo_endpoint,
            &tempo_query,
            started_at.saturating_sub(1),
            started_at,
        )
        .await?;

        let tap_identity = config.activation_identity().clone();
        let tap_publisher = publisher.clone();
        let tap = tokio::spawn(async move {
            let _client = tap_client;
            while let Some(message) = taps.next().await {
                match decode_tap(
                    &tap_identity.tenant,
                    &tap_identity.project,
                    &tap_identity.environment,
                    message.subject.as_str(),
                    &message.payload,
                ) {
                    Ok(observation) => tap_publisher.push_tap(observation),
                    Err(error) => tracing::warn!(error = %error, "router-tap observation refused"),
                }
            }
        });

        let tempo_publisher = publisher;
        let tempo = tokio::spawn(async move {
            let mut window_start = started_at;
            let mut interval = tokio::time::interval(TEMPO_POLL_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // The initial read above owns the immediate tick.
            interval.tick().await;
            loop {
                interval.tick().await;
                match fetch_tempo(&http, &tempo_endpoint, &tempo_query, window_start).await {
                    Ok((ended_at, traces)) => {
                        tempo_publisher.merge_traces(traces);
                        window_start = next_tempo_window_start(started_at, ended_at);
                    }
                    Err(error) => tracing::warn!(error = %error, "Tempo observation read failed"),
                }
            }
        });

        Ok(Self { tap, tempo })
    }
}

impl Drop for DevObservationReaders {
    fn drop(&mut self) {
        self.tap.abort();
        self.tempo.abort();
    }
}

fn tempo_search_endpoint(base: &str) -> Result<Url, DevObservationError> {
    let mut endpoint = Url::parse(base).map_err(|source| {
        DevObservationError::with_source(
            "construct Tempo search endpoint",
            "tempo_query_url is not an HTTP URL",
            source,
        )
    })?;
    let path = format!("{}/api/search", endpoint.path().trim_end_matches('/'));
    endpoint.set_path(&path);
    endpoint.set_query(None);
    endpoint.set_fragment(None);
    Ok(endpoint)
}

fn tempo_query(config: &DevConfig) -> String {
    let identity = config.activation_identity();
    let tenant = serde_json::to_string(&identity.tenant)
        .expect("a Rust string always has a JSON string spelling");
    let project = serde_json::to_string(&identity.project)
        .expect("a Rust string always has a JSON string spelling");
    let environment = serde_json::to_string(&identity.environment)
        .expect("a Rust string always has a JSON string spelling");
    format!(
        "{{ span.\"wamn.tenant\" = {tenant} && span.\"wamn.project\" = {project} && span.\"wamn.environment\" = {environment} }}"
    )
}

async fn fetch_tempo(
    http: &reqwest::Client,
    endpoint: &Url,
    query: &str,
    window_start: u64,
) -> Result<(u64, Vec<DevTraceObservation>), DevObservationError> {
    let ended_at = unix_seconds("query Tempo")?.max(window_start);
    let traces = fetch_tempo_interval(http, endpoint, query, window_start, ended_at).await?;
    Ok((ended_at, traces))
}

async fn fetch_tempo_interval(
    http: &reqwest::Client,
    endpoint: &Url,
    query: &str,
    started_at: u64,
    ended_at: u64,
) -> Result<Vec<DevTraceObservation>, DevObservationError> {
    let mut intervals = VecDeque::from([(started_at, ended_at)]);
    let mut pages = 0_usize;
    let mut traces = Vec::new();
    while let Some((page_start, page_end)) = intervals.pop_front() {
        pages += 1;
        if pages > TEMPO_MAX_QUERY_PAGES {
            tracing::warn!(
                limit = TEMPO_MAX_QUERY_PAGES,
                "Tempo observation interval exceeded the bounded query budget; the client view retains the observations already read"
            );
            break;
        }
        let page = fetch_tempo_page(http, endpoint, query, page_start, page_end).await?;
        if page.len() < TEMPO_FETCH_LIMIT {
            traces.extend(page);
            continue;
        }
        if page_start == page_end {
            tracing::warn!(
                second = page_start,
                limit = TEMPO_FETCH_LIMIT,
                "Tempo observation second saturated; the client view retains the bounded observations returned"
            );
            traces.extend(page);
            continue;
        }
        let midpoint = page_start + (page_end - page_start) / 2;
        intervals.push_front((midpoint + 1, page_end));
        intervals.push_front((page_start, midpoint));
    }
    Ok(traces)
}

async fn fetch_tempo_page(
    http: &reqwest::Client,
    endpoint: &Url,
    query: &str,
    started_at: u64,
    ended_at: u64,
) -> Result<Vec<DevTraceObservation>, DevObservationError> {
    let response = http
        .get(endpoint.clone())
        .query(&[
            ("q", query.to_owned()),
            ("start", started_at.to_string()),
            ("end", ended_at.to_string()),
            ("limit", TEMPO_FETCH_LIMIT.to_string()),
        ])
        .send()
        .await
        .map_err(|source| {
            DevObservationError::with_source(
                "query Tempo",
                "tempo_query_url did not answer within the observation budget",
                source,
            )
        })?;
    if !response.status().is_success() {
        return Err(DevObservationError::new(
            "query Tempo",
            format!("tempo_query_url returned HTTP {}", response.status()),
        ));
    }
    let bytes = response.bytes().await.map_err(|source| {
        DevObservationError::with_source(
            "read Tempo response",
            "tempo_query_url returned an unreadable body",
            source,
        )
    })?;
    parse_tempo_search(&bytes)
}

fn next_tempo_window_start(session_start: u64, ended_at: u64) -> u64 {
    ended_at
        .saturating_sub(TEMPO_SETTLE_DELAY_SECONDS)
        .saturating_add(1)
        .max(session_start)
}

fn unix_seconds(operation: &'static str) -> Result<u64, DevObservationError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|source| {
            DevObservationError::with_source(
                operation,
                "the system clock precedes the Unix epoch",
                source,
            )
        })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TempoSearchResponse {
    traces: Vec<TempoTrace>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TempoTrace {
    #[serde(rename = "traceID")]
    trace_id: String,
    root_service_name: String,
    root_trace_name: String,
    start_time_unix_nano: String,
    duration_ms: f64,
}

fn parse_tempo_search(bytes: &[u8]) -> Result<Vec<DevTraceObservation>, DevObservationError> {
    let response: TempoSearchResponse = serde_json::from_slice(bytes).map_err(|source| {
        DevObservationError::with_source(
            "decode Tempo response",
            "tempo_query_url returned a malformed search result",
            source,
        )
    })?;
    if response.traces.len() > TEMPO_FETCH_LIMIT {
        return Err(DevObservationError::new(
            "decode Tempo response",
            format!(
                "tempo_query_url returned {} traces above the declared limit {}",
                response.traces.len(),
                TEMPO_FETCH_LIMIT
            ),
        ));
    }
    let mut observations = response
        .traces
        .into_iter()
        .map(|trace| {
            let start_time_unix_nanos =
                trace
                    .start_time_unix_nano
                    .parse::<u64>()
                    .map_err(|source| {
                        DevObservationError::with_source(
                            "decode Tempo response",
                            format!("trace {} has an invalid start time", trace.trace_id),
                            source,
                        )
                    })?;
            let duration =
                Duration::try_from_secs_f64(trace.duration_ms / 1_000.0).map_err(|source| {
                    DevObservationError::with_source(
                        "decode Tempo response",
                        format!("trace {} has an invalid duration", trace.trace_id),
                        source,
                    )
                })?;
            Ok(DevTraceObservation {
                trace_id: trace.trace_id,
                root_service_name: trace.root_service_name,
                root_trace_name: trace.root_trace_name,
                start_time_unix_nanos,
                duration,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    observations.sort_by_key(DevTraceObservation::start_time_unix_nanos);
    Ok(observations)
}

fn decode_tap(
    tenant: &str,
    project: &str,
    environment: &str,
    subject: &str,
    bytes: &[u8],
) -> Result<DevTapObservation, DevObservationError> {
    let record: RouterTapRecord = serde_json::from_slice(bytes).map_err(|source| {
        DevObservationError::with_source(
            "decode router tap",
            "event_nats_url delivered a malformed router-tap record",
            source,
        )
    })?;
    record.validate().map_err(|source| {
        DevObservationError::with_source(
            "decode router tap",
            "event_nats_url delivered an impossible router-tap field combination",
            source,
        )
    })?;
    let expected = router_tap_record_subject(
        tenant,
        project,
        environment,
        &record.wiring_id,
        &record.delivery_id,
    )
    .ok_or_else(|| {
        DevObservationError::new(
            "decode router tap",
            "the record identity cannot form one router-tap subject",
        )
    })?;
    if subject != expected {
        return Err(DevObservationError::new(
            "decode router tap",
            "the record identity disagrees with its NATS subject",
        ));
    }
    Ok(DevTapObservation {
        subject: subject.to_owned(),
        record,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wamn_runtime::plugins::wamn_jetstream::{
        RouterTapFormatVersion, RouterTapRecordPhase, RouterTapSourceKind,
    };

    use super::*;

    #[test]
    fn typed_tap_reader_requires_subject_and_body_to_name_the_same_delivery() {
        let record = RouterTapRecord {
            delivery_id: "delivery-1".into(),
            format_version: RouterTapFormatVersion::V1,
            outcome: None,
            over_ceiling_bytes: None,
            payload: json!({"receipt": "r-1"}),
            phase: RouterTapRecordPhase::Accepted,
            redacted: false,
            source_id: "route-1".into(),
            source_kind: RouterTapSourceKind::Attachment,
            wiring_id: "receipt".into(),
            wiring_version: 1,
        };
        let bytes = serde_json::to_vec(&record).expect("serialize typed tap");
        let observation = decode_tap(
            "tenant-a",
            "project-a",
            "dev",
            "tap.tenant-a.project-a.dev.receipt.delivery-1",
            &bytes,
        )
        .expect("matching typed tap is admitted");
        assert_eq!(observation.delivery_id(), "delivery-1");

        let error = decode_tap(
            "tenant-a",
            "project-a",
            "dev",
            "tap.tenant-a.project-a.dev.other.delivery-1",
            &bytes,
        )
        .expect_err("a mismatched subject must be refused");
        assert_eq!(error.operation, "decode router tap");
    }

    #[test]
    fn tempo_reader_returns_oldest_first_typed_summaries() {
        let response = json!({
            "traces": [
                {
                    "traceID": "trace-2",
                    "rootServiceName": "wash-runtime",
                    "rootTraceName": "request",
                    "startTimeUnixNano": "2000000000",
                    "durationMs": 2.5,
                    "spanSet": {"spans": []}
                },
                {
                    "traceID": "trace-1",
                    "rootServiceName": "wash-runtime",
                    "rootTraceName": "request",
                    "startTimeUnixNano": "1000000000",
                    "durationMs": 1.0
                }
            ],
            "metrics": {"inspectedTraces": 2}
        });
        let observations =
            parse_tempo_search(&serde_json::to_vec(&response).expect("serialize Tempo fixture"))
                .expect("decode external Tempo fields into owned summaries");

        assert_eq!(observations.len(), 2);
        assert_eq!(observations[0].trace_id(), "trace-1");
        assert_eq!(observations[1].duration(), Duration::from_micros(2_500));
    }

    #[test]
    fn tempo_cursor_retains_the_declared_settlement_window() {
        let started_at = 1_000;

        assert_eq!(
            next_tempo_window_start(started_at, started_at + 10),
            started_at
        );
        assert_eq!(
            next_tempo_window_start(started_at, started_at + 20),
            started_at + 6
        );
    }

    #[test]
    fn tempo_reader_refuses_unrepresentable_durations() {
        let response = json!({
            "traces": [{
                "traceID": "trace-overflow",
                "rootServiceName": "wamn-host",
                "rootTraceName": "request",
                "startTimeUnixNano": "1000000000",
                "durationMs": 1e300
            }]
        });

        let error =
            parse_tempo_search(&serde_json::to_vec(&response).expect("serialize Tempo fixture"))
                .expect_err("an unrepresentable external duration must be refused");

        assert_eq!(error.operation, "decode Tempo response");
        assert!(error.detail.contains("trace-overflow"));
    }
}
