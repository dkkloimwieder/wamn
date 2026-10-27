//! Live, read-only adapter for the development loop's trace view.

use std::error::Error;
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use tokio::task::JoinHandle;

use super::config::DevConfig;
use super::read::{DEV_OBSERVATION_LIMIT, DevReadPublisher, DevTraceObservation};

const TRACE_POLL_INTERVAL: Duration = Duration::from_secs(1);

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

/// The background reader owned for exactly one development-loop session.
#[derive(Debug)]
pub(crate) struct DevObservationReaders {
    traces: JoinHandle<()>,
}

impl DevObservationReaders {
    /// Open the collector trace file before the first stage begins.
    pub(crate) async fn start(
        config: &DevConfig,
        publisher: DevReadPublisher,
    ) -> Result<Self, DevObservationError> {
        let path = config.local_artifacts().directory.join("otel/traces.jsonl");
        let offset = match tokio::fs::metadata(&path).await {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => {
                return Err(DevObservationError::with_source(
                    "open collector traces",
                    path.display().to_string(),
                    error,
                ));
            }
        };
        let started_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|source| {
                DevObservationError::with_source(
                    "start collector reader",
                    "the system clock precedes the Unix epoch",
                    source,
                )
            })?;
        let mut reader = TraceReader {
            path,
            offset,
            started_at: u64::try_from(started_at.as_nanos()).map_err(|source| {
                DevObservationError::with_source(
                    "start collector reader",
                    "the system clock exceeds OTLP timestamp range",
                    source,
                )
            })?,
            identity: config.activation_identity().clone(),
            traces: std::collections::BTreeMap::new(),
        };

        let traces = tokio::spawn(async move {
            let mut interval = tokio::time::interval(TRACE_POLL_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                if let Err(error) = reader.read().await {
                    tracing::warn!(error = %error, "collector observation read failed");
                }
                publisher.merge_traces(reader.observations());
            }
        });

        Ok(Self { traces })
    }
}

impl Drop for DevObservationReaders {
    fn drop(&mut self) {
        self.traces.abort();
    }
}

// The collector file exporter writes one OTLP JSON object per line. A read
// stops at an incomplete line and retries it after the next exporter flush.
#[derive(Debug)]
struct TraceReader {
    path: std::path::PathBuf,
    offset: u64,
    started_at: u64,
    identity: super::activation::DevActivationIdentity,
    traces: std::collections::BTreeMap<String, TraceSummary>,
}

#[derive(Debug)]
struct TraceSummary {
    observation: DevTraceObservation,
    end: u64,
    scoped: bool,
    has_root: bool,
}

impl TraceReader {
    async fn read(&mut self) -> Result<(), DevObservationError> {
        use tokio::io::{AsyncBufReadExt as _, AsyncSeekExt as _, BufReader};
        let file = match tokio::fs::File::open(&self.path).await {
            Ok(file) => file,
            // The exporter creates the file on the first sampled batch.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(self.file_error(error)),
        };
        let length = file
            .metadata()
            .await
            .map_err(|error| self.file_error(error))?
            .len();
        if length < self.offset {
            self.offset = 0;
        }
        let mut reader = BufReader::new(file);
        reader
            .seek(std::io::SeekFrom::Start(self.offset))
            .await
            .map_err(|error| self.file_error(error))?;
        let mut line = Vec::new();
        // Yield between groups of exporter batches even under sustained load.
        for _ in 0..64 {
            line.clear();
            let count = reader
                .read_until(b'\n', &mut line)
                .await
                .map_err(|error| self.file_error(error))?;
            if count == 0 || line.last() != Some(&b'\n') {
                break;
            }
            self.offset += count as u64;
            if let Err(error) = self.merge(&line) {
                tracing::warn!(error = %error, "collector trace record refused");
            }
        }
        Ok(())
    }

    fn file_error(&self, source: std::io::Error) -> DevObservationError {
        DevObservationError::with_source(
            "read collector traces",
            self.path.display().to_string(),
            source,
        )
    }

    fn merge(&mut self, bytes: &[u8]) -> Result<(), DevObservationError> {
        let record: TraceExport = serde_json::from_slice(bytes).map_err(|source| {
            DevObservationError::with_source(
                "decode collector traces",
                "expected an OTLP JSON trace export",
                source,
            )
        })?;
        for resource in record.resource_spans {
            let service =
                attribute(&resource.resource.attributes, "service.name").unwrap_or_default();
            for scope in resource.scope_spans {
                for span in scope.spans {
                    let start = span.start_time_unix_nano.parse::<u64>().map_err(|source| {
                        DevObservationError::with_source(
                            "decode collector traces",
                            "invalid span start time",
                            source,
                        )
                    })?;
                    let end = span.end_time_unix_nano.parse::<u64>().map_err(|source| {
                        DevObservationError::with_source(
                            "decode collector traces",
                            "invalid span end time",
                            source,
                        )
                    })?;
                    if end < start {
                        return Err(DevObservationError::new(
                            "decode collector traces",
                            "span ends before it starts",
                        ));
                    }
                    if start < self.started_at {
                        continue;
                    }
                    if span.trace_id.len() != 32
                        || !span.trace_id.bytes().all(|byte| byte.is_ascii_hexdigit())
                    {
                        return Err(DevObservationError::new(
                            "decode collector traces",
                            "invalid OTLP trace identity",
                        ));
                    }
                    let id = span.trace_id.to_ascii_lowercase();
                    let scoped = attribute(&span.attributes, "wamn.env_key")
                        == Some(self.identity.env_key().as_str())
                        && attribute(&span.attributes, "wamn.project")
                            == Some(self.identity.project.as_str())
                        && attribute(&span.attributes, "wamn.environment")
                            == Some(self.identity.environment.as_str());
                    let is_root = span.parent_span_id.bytes().all(|byte| byte == b'0');
                    let summary = self
                        .traces
                        .entry(id.clone())
                        .or_insert_with(|| TraceSummary {
                            observation: DevTraceObservation {
                                trace_id: id,
                                root_service_name: service.to_owned(),
                                root_trace_name: span.name.clone(),
                                start_time_unix_nanos: start,
                                duration: Duration::ZERO,
                            },
                            end,
                            scoped: false,
                            has_root: false,
                        });
                    if !summary.has_root
                        && (is_root || start < summary.observation.start_time_unix_nanos)
                    {
                        service.clone_into(&mut summary.observation.root_service_name);
                        summary.observation.root_trace_name = span.name;
                    }
                    summary.has_root |= is_root;
                    summary.scoped |= scoped;
                    summary.end = summary.end.max(end);
                    summary.observation.start_time_unix_nanos =
                        summary.observation.start_time_unix_nanos.min(start);
                    summary.observation.duration = Duration::from_nanos(
                        summary.end - summary.observation.start_time_unix_nanos,
                    );
                    if self.traces.len() > DEV_OBSERVATION_LIMIT {
                        let oldest = self
                            .traces
                            .iter()
                            .min_by_key(|(_, summary)| summary.observation.start_time_unix_nanos)
                            .map(|(id, _)| id.clone())
                            .expect("the cache exceeds its nonzero bound");
                        self.traces.remove(&oldest);
                    }
                }
            }
        }
        Ok(())
    }

    fn observations(&self) -> Vec<DevTraceObservation> {
        self.traces
            .values()
            .filter(|summary| summary.scoped)
            .map(|summary| summary.observation.clone())
            .collect()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TraceExport {
    resource_spans: Vec<ResourceSpans>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResourceSpans {
    #[serde(default)]
    resource: TraceResource,
    scope_spans: Vec<ScopeSpans>,
}

#[derive(Debug, Default, Deserialize)]
struct TraceResource {
    #[serde(default)]
    attributes: Vec<TraceAttribute>,
}

#[derive(Debug, Deserialize)]
struct ScopeSpans {
    spans: Vec<TraceSpan>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TraceSpan {
    trace_id: String,
    #[serde(default)]
    parent_span_id: String,
    name: String,
    start_time_unix_nano: String,
    end_time_unix_nano: String,
    #[serde(default)]
    attributes: Vec<TraceAttribute>,
}

#[derive(Debug, Deserialize)]
struct TraceAttribute {
    key: String,
    value: serde_json::Value,
}

fn attribute<'a>(attributes: &'a [TraceAttribute], key: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|attribute| attribute.key == key)?
        .value["stringValue"]
        .as_str()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn reader(path: std::path::PathBuf) -> TraceReader {
        let addresses = ["127.0.0.1:41000".parse().unwrap(); 11];
        let config = super::super::config::parse_config(
            &serde_json::to_vec(&super::super::config::tests::complete_document(&addresses))
                .unwrap(),
        )
        .unwrap();
        TraceReader {
            path,
            offset: 0,
            started_at: 0,
            identity: config.activation_identity().clone(),
            traces: std::collections::BTreeMap::new(),
        }
    }

    fn export(
        reader: &TraceReader,
        id: &str,
        name: &str,
        start: u64,
        end: u64,
        root: bool,
    ) -> serde_json::Value {
        let attributes = if root {
            vec![]
        } else {
            vec![
                json!({"key":"wamn.env_key", "value":{"stringValue":reader.identity.env_key()}}),
                json!({"key":"wamn.project", "value":{"stringValue":reader.identity.project}}),
                json!({"key":"wamn.environment", "value":{"stringValue":reader.identity.environment}}),
            ]
        };
        json!({"resourceSpans":[{
            "resource":{"attributes":[{"key":"service.name", "value":{"stringValue":"wamn-host"}}]},
            "scopeSpans":[{"spans":[{
                "traceId":id, "parentSpanId":if root {""} else {"0000000000000001"},
                "name":name, "startTimeUnixNano":start.to_string(), "endTimeUnixNano":end.to_string(),
                "attributes":attributes
            }]}]
        }]})
    }

    #[test]
    fn sampled_trace_joins_batches_and_filters_the_environment() {
        let mut reader = reader(std::path::PathBuf::new());
        let id = "11111111111111111111111111111111";
        let child = export(&reader, id, "wamn.component.invoke", 200, 300, false);
        reader.merge(&serde_json::to_vec(&child).unwrap()).unwrap();
        let root = export(&reader, id, "request", 100, 500, true);
        reader.merge(&serde_json::to_vec(&root).unwrap()).unwrap();
        // A later duplicate child must not replace the root or narrow duration.
        reader.merge(&serde_json::to_vec(&child).unwrap()).unwrap();
        let mut foreign = export(
            &reader,
            "22222222222222222222222222222222",
            "foreign",
            600,
            700,
            false,
        );
        foreign["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["attributes"][2]["value"]["stringValue"] =
            json!("another-environment");
        reader
            .merge(&serde_json::to_vec(&foreign).unwrap())
            .unwrap();
        let observations = reader.observations();
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].root_service_name(), "wamn-host");
        assert_eq!(observations[0].root_trace_name(), "request");
        assert_eq!(observations[0].start_time_unix_nanos(), 100);
        assert_eq!(observations[0].duration(), Duration::from_nanos(400));
    }

    #[test]
    fn collector_summaries_publish_oldest_first_with_exact_ids_and_durations() {
        let mut reader = reader(std::path::PathBuf::new());
        for (id, start, end) in [
            (
                "11111111111111111111111111111111",
                2_000_000_000,
                2_002_500_000,
            ),
            (
                "22222222222222222222222222222222",
                1_000_000_000,
                1_001_000_000,
            ),
        ] {
            let record = export(&reader, id, "request", start, end, false);
            reader.merge(&serde_json::to_vec(&record).unwrap()).unwrap();
        }
        let (publisher, handle) = super::super::read::dev_read_channel();
        publisher.merge_traces(reader.observations());
        let snapshot = handle.snapshot();
        assert_eq!(
            snapshot.traces()[0].trace_id(),
            "22222222222222222222222222222222"
        );
        assert_eq!(
            snapshot.traces()[1].trace_id(),
            "11111111111111111111111111111111"
        );
        assert_eq!(snapshot.traces()[0].duration(), Duration::from_millis(1));
        assert_eq!(
            snapshot.traces()[1].duration(),
            Duration::from_micros(2_500)
        );
        let mut overflow = export(
            &reader,
            "33333333333333333333333333333333",
            "request",
            1,
            2,
            false,
        );
        overflow["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["endTimeUnixNano"] =
            json!("18446744073709551616");
        assert!(
            reader
                .merge(&serde_json::to_vec(&overflow).unwrap())
                .is_err()
        );
    }

    #[tokio::test]
    async fn file_reader_waits_for_complete_lines_and_resumes_after_truncation() {
        use std::io::Write as _;
        let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
        let path = scratch.path().join("traces.jsonl");
        let mut reader = reader(path.clone());
        reader.read().await.unwrap();
        assert!(reader.observations().is_empty());
        let record = serde_json::to_vec(&export(
            &reader,
            "11111111111111111111111111111111",
            "request",
            100,
            200,
            false,
        ))
        .unwrap();
        std::fs::write(&path, &record[..record.len() / 2]).unwrap();
        reader.read().await.unwrap();
        assert_eq!(reader.offset, 0);
        assert!(reader.observations().is_empty());
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(&record[record.len() / 2..]).unwrap();
        file.write_all(b"\n").unwrap();
        reader.read().await.unwrap();
        assert_eq!(reader.observations().len(), 1);
        let offset = reader.offset;
        reader.read().await.unwrap();
        assert_eq!(reader.offset, offset);
        // The exporter can truncate its file when it restarts.
        std::fs::write(&path, b"{\"resourceSpans\":[]}\n").unwrap();
        reader.read().await.unwrap();
        assert!(reader.offset < offset);
        file.write_all(&record).unwrap();
        file.write_all(b"\n").unwrap();
        reader.read().await.unwrap();
        assert_eq!(reader.observations().len(), 1);
    }

    #[test]
    fn trace_reader_refuses_invalid_times_and_bounds_its_cache() {
        let mut reader = reader(std::path::PathBuf::new());
        let bad = export(
            &reader,
            "11111111111111111111111111111111",
            "request",
            200,
            100,
            false,
        );
        assert!(reader.merge(&serde_json::to_vec(&bad).unwrap()).is_err());
        reader.started_at = 100;
        let old = export(
            &reader,
            "11111111111111111111111111111111",
            "old",
            1,
            2,
            false,
        );
        reader.merge(&serde_json::to_vec(&old).unwrap()).unwrap();
        assert!(reader.observations().is_empty());
        for index in 1..=DEV_OBSERVATION_LIMIT + 1 {
            let record = export(
                &reader,
                &format!("{index:032x}"),
                "request",
                100 + index as u64,
                200 + index as u64,
                false,
            );
            reader.merge(&serde_json::to_vec(&record).unwrap()).unwrap();
        }
        assert_eq!(reader.traces.len(), DEV_OBSERVATION_LIMIT);
        assert!(
            !reader
                .traces
                .contains_key("00000000000000000000000000000001")
        );
    }
}
