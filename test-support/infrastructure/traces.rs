//! Request trace assertions shared by application cluster tests.

use serde::Deserialize;

/// A request trace returned by Tempo.
#[derive(Debug, Deserialize)]
pub struct TraceDocument {
    #[serde(default)]
    batches: Vec<TraceBatch>,
}

#[derive(Debug, Deserialize)]
struct TraceBatch {
    #[serde(default, rename = "scopeSpans")]
    scopes: Vec<TraceScope>,
}

#[derive(Debug, Deserialize)]
struct TraceScope {
    #[serde(default)]
    spans: Vec<TraceSpan>,
}

#[derive(Debug, Deserialize)]
struct TraceSpan {
    name: String,
    #[serde(default)]
    attributes: Vec<TraceAttribute>,
}

#[derive(Debug, Deserialize)]
struct TraceAttribute {
    key: String,
    value: TraceAttributeValue,
}

#[derive(Debug, Deserialize)]
struct TraceAttributeValue {
    #[serde(default, rename = "stringValue")]
    string: Option<String>,
}

/// Require the serving spans and reject component loading during a request.
///
/// The route declares its statement count. The caller declares whether the
/// request acquires executor authority.
pub fn request_trace_is_complete(
    trace: &TraceDocument,
    expect_executor: bool,
    expected_statements: usize,
) -> bool {
    let spans = trace
        .batches
        .iter()
        .flat_map(|batch| &batch.scopes)
        .flat_map(|scope| &scope.spans)
        .collect::<Vec<_>>();
    let count_named = |name: &str| spans.iter().filter(|span| span.name == name).count();
    let count_acquired = |class: &str| {
        spans
            .iter()
            .filter(|span| {
                span.name == "wamn.postgres.acquire"
                    && span
                        .attributes
                        .iter()
                        .find(|attribute| attribute.key == "wamn.authority_class")
                        .and_then(|attribute| attribute.value.string.as_deref())
                        == Some(class)
            })
            .count()
    };

    count_named("wamn.route.authenticate") == 1
        && count_named("wamn.router.resolve") == 1
        && count_named("wamn.component.invoke") == 1
        && count_named("wamn.component.pull") == 0
        && count_named("load_component_bytes") == 0
        && count_named("resolve_workload") == 0
        && count_named("link_components") == 0
        && count_acquired("callable-http") == 1
        && count_acquired("guest-sql") == 1
        && count_named("wamn.postgres") > 0
        && count_named("wamn.postgres.statement") == expected_statements
        && count_acquired("executor-platform") == usize::from(expect_executor)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{TraceDocument, request_trace_is_complete};

    fn span(name: &str, authority: Option<&str>) -> Value {
        let attributes = authority
            .map(|authority| {
                vec![json!({
                    "key": "wamn.authority_class",
                    "value": { "stringValue": authority },
                })]
            })
            .unwrap_or_default();
        json!({ "name": name, "attributes": attributes })
    }

    fn trace(
        statements: usize,
        executor: usize,
        omit: &str,
        extra: Option<Value>,
    ) -> TraceDocument {
        let mut spans = Vec::new();
        for name in [
            "wamn.route.authenticate",
            "wamn.router.resolve",
            "wamn.component.invoke",
        ] {
            if name != omit {
                spans.push(span(name, None));
            }
        }
        for class in ["callable-http", "guest-sql"] {
            if omit != format!("acquire:{class}") {
                spans.push(span("wamn.postgres.acquire", Some(class)));
            }
        }
        for _ in 0..executor {
            spans.push(span("wamn.postgres.acquire", Some("executor-platform")));
        }
        if omit != "wamn.postgres" {
            spans.push(span("wamn.postgres", None));
        }
        for _ in 0..statements {
            spans.push(span("wamn.postgres.statement", None));
        }
        spans.extend(extra);
        serde_json::from_value(json!({ "batches": [{ "scopeSpans": [{ "spans": spans }] }] }))
            .expect("valid trace fixture")
    }

    #[test]
    fn complete_request_with_or_without_executor_authority() {
        assert!(request_trace_is_complete(&trace(1, 1, "", None), true, 1));
        assert!(request_trace_is_complete(&trace(1, 0, "", None), false, 1));
    }

    #[test]
    fn route_declares_the_exact_statement_count() {
        assert!(request_trace_is_complete(&trace(8, 1, "", None), true, 8));
        assert!(!request_trace_is_complete(&trace(8, 1, "", None), true, 1));
        assert!(!request_trace_is_complete(&trace(1, 1, "", None), true, 8));
    }

    #[test]
    fn missing_or_unexpected_executor_authority_is_refused() {
        assert!(!request_trace_is_complete(&trace(1, 1, "", None), false, 1));
        assert!(!request_trace_is_complete(&trace(1, 0, "", None), true, 1));
        assert!(!request_trace_is_complete(&trace(1, 2, "", None), true, 1));
    }

    #[test]
    fn every_required_request_span_must_exist() {
        for name in [
            "wamn.route.authenticate",
            "wamn.router.resolve",
            "wamn.component.invoke",
            "wamn.postgres",
        ] {
            assert!(
                !request_trace_is_complete(&trace(1, 1, name, None), true, 1),
                "{name}"
            );
        }
        for class in ["callable-http", "guest-sql"] {
            let omitted = format!("acquire:{class}");
            assert!(
                !request_trace_is_complete(&trace(1, 1, &omitted, None), true, 1),
                "{class}",
            );
        }
    }

    #[test]
    fn request_cannot_load_or_link_components() {
        for name in [
            "wamn.component.pull",
            "load_component_bytes",
            "resolve_workload",
            "link_components",
        ] {
            let extra = span(name, None);
            assert!(
                !request_trace_is_complete(&trace(1, 1, "", Some(extra)), true, 1),
                "{name}",
            );
        }
    }

    #[test]
    fn each_single_request_span_and_authority_count_is_exact() {
        for name in [
            "wamn.route.authenticate",
            "wamn.router.resolve",
            "wamn.component.invoke",
        ] {
            let extra = span(name, None);
            assert!(
                !request_trace_is_complete(&trace(1, 1, "", Some(extra)), true, 1),
                "{name}",
            );
        }
        for class in ["callable-http", "guest-sql"] {
            let extra = span("wamn.postgres.acquire", Some(class));
            assert!(
                !request_trace_is_complete(&trace(1, 1, "", Some(extra)), true, 1),
                "{class}",
            );
        }
    }

    #[test]
    fn empty_or_malformed_trace_is_refused() {
        let empty = serde_json::from_str("{}").expect("empty document");
        assert!(!request_trace_is_complete(&empty, true, 1));
        assert!(serde_json::from_str::<TraceDocument>("not JSON").is_err());
    }
}
