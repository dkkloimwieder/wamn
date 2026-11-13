use super::{
    DEV_OBSERVATION_LIMIT, DevObservationError, DevTraceObservation, Duration, TraceAttribute,
    TraceExport, TraceReader, TraceSummary,
};

pub(super) fn attribute<'a>(attributes: &'a [TraceAttribute], key: &str) -> Option<&'a str> {
    attributes
        .iter()
        .find(|attribute| attribute.key == key)?
        .value["stringValue"]
        .as_str()
}

impl TraceReader {
    pub(super) fn merge(&mut self, bytes: &[u8]) -> Result<(), DevObservationError> {
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
                    let scoped = attribute(&span.attributes, "wamn.project_env_key")
                        == Some(self.identity.project_env_key().as_str())
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
}
