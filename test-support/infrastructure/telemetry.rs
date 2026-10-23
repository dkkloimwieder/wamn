//! Read collector OTLP JSON traces and service metrics from owned cluster tests.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::process::Command;
use tokio::time::{Instant, sleep, timeout};

/// Fixed request IDs with sampled parent contexts (`traceparent` ends in `01`).
/// The host keeps sampled parents. The collector forwards every received trace.
pub const SAMPLED_TRACE_IDS: [&str; 5] = [
    "00000000000000000000000000000014",
    "00000000000000000000000000000015",
    "00000000000000000000000000000016",
    "00000000000000000000000000000017",
    "0000000000000000000000000000004c",
];

/// Render the cluster test collector from the deployed receiver and processors.
/// Only file exporters and the retained Prometheus metric pipeline differ.
pub fn render_collector(repository: &Path, work: &Path) -> anyhow::Result<std::path::PathBuf> {
    use serde::Deserialize as _;
    let deployed = std::fs::read_to_string(repository.join("deploy/platform/otel-collector.yaml"))?;
    let documents = serde_yaml::Deserializer::from_str(&deployed)
        .map(Value::deserialize)
        .collect::<Result<Vec<_>, _>>()?;
    let source = documents
        .iter()
        .find(|document| document["kind"] == "ConfigMap")
        .context("deployed collector has a ConfigMap")?;
    let mut configuration: Value = serde_yaml::from_str(
        source["data"]["config.yaml"]
            .as_str()
            .context("deployed collector has configuration")?,
    )?;
    let image = documents
        .iter()
        .find(|document| document["kind"] == "Deployment")
        .context("deployed collector has a Deployment")?["spec"]["template"]["spec"]["containers"]
        [0]["image"]
        .as_str()
        .context("deployed collector has an image")?;
    let template = std::fs::read_to_string(repository.join("deploy/infra/otel-collector.yaml"))?;
    let mut documents = serde_yaml::Deserializer::from_str(&template)
        .map(Value::deserialize)
        .collect::<Result<Vec<_>, _>>()?;
    for document in &mut documents {
        match document["kind"].as_str() {
            Some("ConfigMap") => {
                let metrics: Value = serde_yaml::from_str(
                    document["data"]["config.yaml"]
                        .as_str()
                        .context("metric collector has configuration")?,
                )?;
                configuration["exporters"] = json!({
                    "file/traces":{"path":"/var/lib/otelcol/traces.jsonl","format":"json"},
                    "file/logs":{"path":"/var/lib/otelcol/logs.jsonl","format":"json"},
                    "prometheus":metrics["exporters"]["prometheus"]
                });
                for signal in ["traces", "logs"] {
                    configuration["service"]["pipelines"][signal]["exporters"] =
                        json!([format!("file/{signal}")]);
                }
                configuration["processors"]["batch/metrics"] =
                    metrics["processors"]["batch/metrics"].clone();
                configuration["service"]["pipelines"]["metrics"] =
                    metrics["service"]["pipelines"]["metrics"].clone();
                configuration["service"]["telemetry"] = metrics["service"]["telemetry"].clone();
                document["data"]["config.yaml"] = json!(serde_yaml::to_string(&configuration)?);
            }
            Some("Deployment") => {
                let containers = document["spec"]["template"]["spec"]["containers"]
                    .as_array_mut()
                    .context("collector deployment has containers")?;
                containers[0]["image"] = json!(image);
                containers[1]["image"] = json!(crate::workload::HTTP_PROBE_IMAGE);
            }
            _ => {}
        }
    }
    let path = work.join("otel-collector.yaml");
    std::fs::write(
        &path,
        documents
            .iter()
            .map(serde_yaml::to_string)
            .collect::<Result<Vec<_>, _>>()?
            .join("---\n"),
    )?;
    Ok(path)
}

/// Select one trace across all complete OTLP JSON lines, including split batches.
fn file_trace(bytes: &[u8], trace_id: &str) -> anyhow::Result<Value> {
    let mut resources = Vec::new();
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        if !line.ends_with(b"\n") || line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let document: Value =
            serde_json::from_slice(line).context("parse collector OTLP JSON line")?;
        for resource in document["resourceSpans"].as_array().into_iter().flatten() {
            let mut resource = resource.clone();
            for scope in resource["scopeSpans"]
                .as_array_mut()
                .context("collector resource has scopes")?
            {
                scope["spans"]
                    .as_array_mut()
                    .context("collector scope has spans")?
                    .retain(|span| span["traceId"].as_str().is_some_and(|id| id == trace_id));
            }
            resources.push(resource);
        }
    }
    Ok(json!({"resourceSpans":resources}))
}

/// Read the collector file through its read-only sidecar, with a two-batch budget.
/// Require the completed HTTP root as well as the caller's trace assertions.
pub async fn read_trace(
    cluster: &str,
    work: &Path,
    trace_id: &str,
    evidence: &Path,
    complete: impl Fn(&Value) -> bool,
) -> anyhow::Result<Value> {
    identifier(trace_id, 16)?;
    let deadline = Instant::now() + Duration::from_secs(130);
    let mut failure = format!("collector trace {trace_id} did not finish");
    while Instant::now() < deadline {
        let mut command = Command::new("kubectl");
        command
            .arg("--kubeconfig")
            .arg(work.join("kubeconfig"))
            .args([
                "--context",
                &format!("kind-{cluster}"),
                "--request-timeout=10s",
                "-n",
                "wamn-system",
                "exec",
                "deployment/otel-collector",
                "-c",
                "trace-reader",
                "--",
                "cat",
                "/var/lib/otelcol/traces.jsonl",
            ])
            .kill_on_drop(true);
        let result: anyhow::Result<Value> = async {
            let output = timeout(Duration::from_secs(12), command.output())
                .await
                .context("collector file read timed out")??;
            std::fs::write(
                evidence.join(format!("collector-{trace_id}.jsonl")),
                &output.stdout,
            )?;
            std::fs::write(
                evidence.join(format!("collector-{trace_id}.stderr")),
                &output.stderr,
            )?;
            ensure!(
                output.status.success(),
                "kubectl refused collector file read: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let document = file_trace(&output.stdout, trace_id)?;
            ensure!(
                trace_spans(&document).any(|span| span["name"] == "handle_http_request")
                    && complete(&document),
                "collector trace {trace_id} is incomplete or violates its assertions"
            );
            Ok(document)
        }
        .await;
        match result {
            Ok(document) => return Ok(document),
            Err(error) => failure = format!("{error:#}"),
        }
        sleep(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_secs(2)),
        )
        .await;
    }
    anyhow::bail!("{failure}")
}

fn hex_identifier(value: &str, size: usize) -> bool {
    value.len() == size * 2 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn identifier(value: &str, size: usize) -> anyhow::Result<String> {
    ensure!(
        hex_identifier(value, size) && value.bytes().any(|byte| byte != b'0'),
        "collector returned an invalid trace identifier"
    );
    Ok(value.to_ascii_lowercase())
}

fn trace_spans(document: &Value) -> impl Iterator<Item = &Value> {
    document["resourceSpans"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|batch| batch["scopeSpans"].as_array().into_iter().flatten())
        .flat_map(|scope| scope["spans"].as_array().into_iter().flatten())
}

fn field<'a>(value: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("trace lacks {key}"))
}

fn attributes(span: &Value) -> BTreeMap<String, Value> {
    span["attributes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            Some((
                item["key"].as_str()?.to_owned(),
                item["value"].as_object()?.values().next()?.clone(),
            ))
        })
        .collect()
}

fn duration(span: &Value) -> anyhow::Result<u64> {
    let start = field(span, "startTimeUnixNano")?.parse::<u64>()?;
    let end = field(span, "endTimeUnixNano")?.parse::<u64>()?;
    ensure!(end > start, "request span did not finish");
    Ok(end - start)
}

fn request_trace(
    document: &Value,
    trace_id: &str,
    tenant: &str,
    project: &str,
    environment: &str,
) -> anyhow::Result<Value> {
    let spans = trace_spans(document).collect::<Vec<_>>();
    ensure!(!spans.is_empty(), "request trace has no spans");
    let mut by_id = BTreeMap::new();
    for span in spans {
        ensure!(
            identifier(field(span, "traceId")?, 16)? == trace_id,
            "trace contains another request identifier"
        );
        let id = identifier(field(span, "spanId")?, 8)?;
        ensure!(
            by_id.insert(id, span).is_none(),
            "trace repeats a span identifier"
        );
    }
    let mut invocations = BTreeMap::new();
    let mut effects = Vec::new();
    for (id, span) in &by_id {
        // The composed-entry route path emits wamn.operation.http without the
        // host identity, so its PostgreSQL effect carries the identity check
        // (owner ruling 2026-10-23). It reverts to wamn.component.invoke alone
        // when the host emits that span there (wamn-2g4rw).
        if span["name"] == "wamn.operation.http" {
            invocations.insert(
                id.clone(),
                json!({"span_id":id,"duration_ns":duration(span)?,"attributes":attributes(span)}),
            );
            continue;
        }
        if span["name"] != "wamn.component.invoke" {
            continue;
        }
        let fields = attributes(span);
        let text = |key: &str| fields.get(key).and_then(Value::as_str).unwrap_or_default();
        let digest = text("wamn.component_digest")
            .strip_prefix("sha256:")
            .unwrap_or_default();
        // A route span names its attachment. A workflow span names its wiring
        // and node instead.
        let route = !text("wamn.attachment_id").is_empty();
        let workflow = !text("wamn.wiring_id").is_empty() && !text("wamn.node_id").is_empty();
        ensure!(
            text("wamn.tenant") == tenant
                && text("wamn.project") == project
                && text("wamn.environment") == environment
                && hex_identifier(digest, 32)
                && digest.bytes().all(|byte| !byte.is_ascii_uppercase())
                && text("wamn.caller_credential_type") == "pat"
                && route != workflow
                && ["wamn.operation", "wamn.caller_principal_id"]
                    .iter()
                    .all(|key| !text(key).is_empty()),
            "invocation does not carry the expected host identity"
        );
        invocations.insert(
            id.clone(),
            json!({"span_id":id,"duration_ns":duration(span)?,"attributes":fields}),
        );
    }
    for (id, span) in &by_id {
        if span["name"] != "wamn.postgres" {
            continue;
        }
        let fields = attributes(span);
        let text = |key: &str| fields.get(key).and_then(Value::as_str).unwrap_or_default();
        ensure!(
            text("wamn.tenant") == tenant
                && text("wamn.project") == project
                && !text("wamn.component").is_empty()
                && text("db.system") == "postgresql"
                && !text("db.operation").is_empty(),
            "PostgreSQL effect does not carry its host identity"
        );
        let elapsed = duration(span)?;
        let mut parent = identifier(field(span, "parentSpanId")?, 8)?;
        let mut visited = BTreeSet::new();
        while !invocations.contains_key(&parent) && visited.insert(parent.clone()) {
            let Some(ancestor) = by_id.get(&parent) else {
                break;
            };
            parent = identifier(field(ancestor, "parentSpanId")?, 8)?;
        }
        ensure!(
            invocations.contains_key(&parent),
            "PostgreSQL effect has no invocation ancestor in this trace"
        );
        effects.push(json!({"span_id":id,"invocation_span_id":parent,"duration_ns":elapsed,"attributes":fields}));
    }
    ensure!(
        !invocations.is_empty() && !effects.is_empty(),
        "request has no complete invocation and PostgreSQL effect"
    );
    Ok(
        json!({"trace_id":trace_id,"spans":by_id.len(),"invocations":invocations.into_values().collect::<Vec<_>>(),"postgres_effects":effects}),
    )
}

fn labels(mut input: &str) -> anyhow::Result<BTreeMap<String, String>> {
    let mut output = BTreeMap::new();
    while !input.is_empty() {
        let (key, rest) = input
            .split_once('=')
            .context("metric label lacks a value")?;
        ensure!(
            !key.is_empty()
                && key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
            "metric label has an invalid name"
        );
        let mut value = serde_json::Deserializer::from_str(rest).into_iter::<String>();
        let decoded = value.next().context("metric label lacks quoted text")??;
        output.insert(key.to_owned(), decoded);
        input = &rest[value.byte_offset()..];
        if !input.is_empty() {
            input = input
                .strip_prefix(',')
                .context("metric labels lack a comma")?;
        }
    }
    Ok(output)
}

fn metrics(text: &str, namespace: &str, project: &str) -> anyhow::Result<Value> {
    let required = [
        (
            "guest_invocation_duration_count",
            vec![
                ("workload_namespace", namespace),
                ("component", "flow-http"),
                ("plugin", "wasi-http"),
                ("http_request_method", "POST"),
            ],
            2.0,
        ),
        (
            "wamn_postgres_query_duration_ms_count",
            vec![("wamn_project", project)],
            1.0,
        ),
        (
            "wamn_jetstream_duration_ms_count",
            vec![("wamn_project", project)],
            1.0,
        ),
    ];
    let mut found = BTreeMap::<&str, Vec<Value>>::new();
    for line in text.lines() {
        let Some((name, rest)) = line.split_once('{') else {
            continue;
        };
        let Some((_, expected, _)) = required.iter().find(|(required, _, _)| name == *required)
        else {
            continue;
        };
        let (raw_labels, rest) = rest
            .rsplit_once('}')
            .context("metric lacks its closing brace")?;
        let fields = labels(raw_labels)?;
        if expected
            .iter()
            .any(|(key, value)| fields.get(*key).map(String::as_str) != Some(*value))
        {
            continue;
        }
        let count = rest
            .split_whitespace()
            .next()
            .context("metric lacks a count")?
            .parse::<f64>()?;
        ensure!(
            count.is_finite() && count >= 0.0,
            "collector returned an invalid histogram count"
        );
        found
            .entry(name)
            .or_default()
            .push(json!({"labels":fields,"count":count}));
    }
    for (name, _, minimum) in required {
        let total = found
            .get(name)
            .into_iter()
            .flatten()
            .map(|series| series["count"].as_f64().unwrap_or_default())
            .sum::<f64>();
        ensure!(
            total >= minimum,
            "collector does not yet expose enough {name} samples"
        );
    }
    Ok(serde_json::to_value(found)?)
}

/// Which cluster, identity, and sampled requests one telemetry read covers.
#[derive(Debug, Clone, Copy)]
pub struct TelemetryInput<'a> {
    pub cluster: &'a str,
    pub work: &'a Path,
    pub namespace: &'a str,
    pub tenant: &'a str,
    pub project: &'a str,
    pub environment: &'a str,
    pub requests: [(&'a str, &'a str); 2],
    pub evidence: &'a Path,
}

/// Require request identity, PostgreSQL ancestry, and scoped service metrics.
pub async fn collect(input: &TelemetryInput<'_>) -> anyhow::Result<()> {
    let TelemetryInput {
        cluster,
        work,
        namespace,
        tenant,
        project,
        environment,
        requests,
        evidence,
    } = *input;
    ensure!(
        work.join("kubeconfig").is_file(),
        "telemetry requires the private kubeconfig"
    );
    ensure!(
        requests[0].0 != requests[1].0 && requests[0].1 != requests[1].1,
        "telemetry requires two distinct requests"
    );
    for (name, trace) in requests {
        ensure!(
            name != "metrics"
                && !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
            "trace output name is invalid"
        );
        ensure!(
            hex_identifier(trace, 16)
                && trace.bytes().any(|byte| byte != b'0')
                && trace.bytes().all(|byte| !byte.is_ascii_uppercase()),
            "telemetry requires a nonzero lowercase trace identifier"
        );
    }
    DirBuilder::new()
        .mode(0o700)
        .create(evidence)
        .context("create telemetry output directory")?;
    let prefix = "/api/v1/namespaces/wamn-system/services/http:";
    let paths = [(
        "metrics",
        format!("{prefix}otel-collector:8889/proxy/metrics"),
    )];
    let mut traces = BTreeMap::new();
    for (name, id) in requests {
        let document = read_trace(cluster, work, id, evidence, |document| {
            request_trace(document, id, tenant, project, environment).is_ok()
        })
        .await?;
        let parsed = request_trace(&document, id, tenant, project, environment)?;
        let filename = format!("trace-{name}.json");
        std::fs::write(
            evidence.join(&filename),
            serde_json::to_vec_pretty(&document)?,
        )?;
        traces.insert(name, json!({"file":filename,"evidence":parsed}));
    }
    let context = format!("kind-{cluster}");
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut completed = BTreeMap::new();
    let mut attempt = 0;
    let mut failure = "telemetry did not arrive within 120 seconds".to_owned();
    while Instant::now() < deadline && completed.len() != paths.len() {
        attempt += 1;
        for (name, path) in &paths {
            if completed.contains_key(name) || Instant::now() >= deadline {
                continue;
            }
            let filename = format!(
                "{name}-{attempt:03}.{}",
                if *name == "metrics" { "prom" } else { "json" }
            );
            let mut command = Command::new("kubectl");
            command
                .arg("--kubeconfig")
                .arg(work.join("kubeconfig"))
                .args([
                    "--context",
                    &context,
                    "--request-timeout=10s",
                    "get",
                    "--raw",
                    path,
                ])
                .kill_on_drop(true);
            let result: anyhow::Result<Value> = async {
                let budget = (deadline - Instant::now()).min(Duration::from_secs(12));
                let output = timeout(budget,command.output()).await.context("telemetry request timed out")??;
                std::fs::write(evidence.join(&filename),&output.stdout)?;
                std::fs::write(evidence.join(format!("{name}-{attempt:03}.stderr")),&output.stderr)?;
                ensure!(output.status.success(), "kubectl refused telemetry request {name}");
                let parsed = metrics(std::str::from_utf8(&output.stdout)?,namespace,project)?;
                Ok(json!({"file":filename,"sha256":hex::encode(ring::digest::digest(&ring::digest::SHA256,&output.stdout).as_ref()),"evidence":parsed}))
            }.await;
            match result {
                Ok(result) => {
                    completed.insert(*name, result);
                }
                Err(error) => {
                    failure = format!("{name}: {error:#}");
                    std::fs::write(
                        evidence.join(format!("{name}-{attempt:03}.refusal.txt")),
                        format!("{failure}\n"),
                    )?;
                }
            }
        }
        if completed.len() != paths.len() {
            sleep(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_secs(2)),
            )
            .await;
        }
    }
    let passed = completed.len() == paths.len();
    let result = json!({"verdict":if passed {"pass"} else {"fail"},"context":context,"namespace":namespace,"traces":requests.into_iter().collect::<BTreeMap<_,_>>(),"collected":completed,"request_evidence":traces,"failure":if passed {None} else {Some(failure.as_str())}});
    std::fs::write(
        evidence.join("result.json"),
        serde_json::to_vec_pretty(&result)?,
    )?;
    ensure!(passed, "{failure}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const TRACE: &str = include_str!("fixtures/traces/fixture-update.json");
    const ID: &str = "655833bf599cf31d4f5422f6e06c7347";

    fn observed(document: &Value) -> anyhow::Result<Value> {
        request_trace(document, ID, "fixture-route-auth", "fixture", "dev")
    }

    #[test]
    fn collector_lines_join_batches_isolate_requests_and_ignore_partial_writes() {
        let document: Value = serde_json::from_str(TRACE).unwrap();
        let mut lines = Vec::new();
        for resource in document["resourceSpans"].as_array().unwrap() {
            lines.extend(serde_json::to_vec(&json!({"resourceSpans":[resource]})).unwrap());
            lines.push(b'\n');
        }
        let mut foreign = document.clone();
        for resource in foreign["resourceSpans"].as_array_mut().unwrap() {
            for scope in resource["scopeSpans"].as_array_mut().unwrap() {
                for span in scope["spans"].as_array_mut().unwrap() {
                    span["traceId"] = json!("00000000000000000000000000000017");
                }
            }
        }
        lines.extend(serde_json::to_vec(&foreign).unwrap());
        lines.push(b'\n');
        lines.extend(b"{\"resourceSpans\":");
        let trace = file_trace(&lines, ID).unwrap();
        observed(&trace).unwrap();
        assert_eq!(trace_spans(&trace).count(), trace_spans(&document).count());
        assert!(
            observed(&file_trace(&lines, "00000000000000000000000000000018").unwrap()).is_err()
        );
        assert!(file_trace(b"invalid\n", ID).is_err());
        let mut repeated = lines[..lines.len() - b"{\"resourceSpans\":".len()].to_vec();
        repeated.extend(serde_json::to_vec(&document).unwrap());
        repeated.push(b'\n');
        assert!(observed(&file_trace(&repeated, ID).unwrap()).is_err());
    }

    #[test]
    fn rendered_cluster_collector_keeps_deployed_trace_processing_and_sampled_ids() {
        use serde::Deserialize as _;
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let scratch = crate::scratch::ScratchRoot::create().unwrap();
        let rendered = render_collector(&repository, scratch.path()).unwrap();
        let contents = std::fs::read_to_string(rendered).unwrap();
        let documents = serde_yaml::Deserializer::from_str(&contents)
            .map(|document| Value::deserialize(document).unwrap())
            .collect::<Vec<_>>();
        let rendered: Value =
            serde_yaml::from_str(documents[0]["data"]["config.yaml"].as_str().unwrap()).unwrap();
        let source =
            std::fs::read_to_string(repository.join("deploy/platform/otel-collector.yaml"))
                .unwrap();
        let deployed = serde_yaml::Deserializer::from_str(&source)
            .map(|document| Value::deserialize(document).unwrap())
            .find(|document| document["kind"] == "ConfigMap")
            .unwrap();
        let deployed: Value =
            serde_yaml::from_str(deployed["data"]["config.yaml"].as_str().unwrap()).unwrap();
        assert_eq!(rendered["receivers"], deployed["receivers"]);
        for (name, processor) in deployed["processors"].as_object().unwrap() {
            assert_eq!(rendered["processors"][name], *processor, "{name}");
        }
        let mut expected = deployed["service"]["pipelines"]["traces"].clone();
        expected["exporters"] = json!(["file/traces"]);
        assert_eq!(rendered["service"]["pipelines"]["traces"], expected);
        assert!(
            rendered["processors"]
                .get("probabilistic_sampler")
                .is_none()
        );
        assert!(
            rendered["service"]["pipelines"]["traces"]["processors"]
                .as_array()
                .unwrap()
                .iter()
                .all(|name| name != "probabilistic_sampler")
        );
    }

    #[test]
    fn retained_request_requires_its_identity_completed_spans_and_effect_ancestry() {
        let original: Value = serde_json::from_str(TRACE).unwrap();
        assert!(observed(&original).is_ok());
        for field_name in ["traceId", "spanId", "parentSpanId", "endTimeUnixNano"] {
            let mut changed = original.clone();
            let spans = changed["resourceSpans"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .flat_map(|batch| batch["scopeSpans"].as_array_mut().unwrap())
                .flat_map(|scope| scope["spans"].as_array_mut().unwrap());
            for span in spans {
                if span["name"] == "wamn.postgres" {
                    span[field_name] = if field_name == "endTimeUnixNano" {
                        json!("0")
                    } else {
                        json!("00000000000000000000000000000000")
                    };
                    break;
                }
            }
            assert!(observed(&changed).is_err(), "{field_name}");
        }
        assert!(request_trace(&original, ID, "foreign", "fixture", "dev").is_err());
        assert!(request_trace(&original, ID, "fixture-route-auth", "foreign", "dev").is_err());
        assert!(request_trace(&original, ID, "fixture-route-auth", "fixture", "prod").is_err());
    }

    #[test]
    fn a_composed_entry_request_checks_identity_on_its_postgres_effect() {
        let mut composed: Value = serde_json::from_str(TRACE).unwrap();
        for resource in composed["resourceSpans"].as_array_mut().unwrap() {
            for scope in resource["scopeSpans"].as_array_mut().unwrap() {
                for span in scope["spans"].as_array_mut().unwrap() {
                    if span["name"] == "wamn.component.invoke" {
                        span["name"] = json!("wamn.operation.http");
                        span["attributes"] = json!([]);
                    }
                }
            }
        }
        let parsed = observed(&composed).unwrap();
        assert!(!parsed["postgres_effects"].as_array().unwrap().is_empty());
        assert!(request_trace(&composed, ID, "foreign", "fixture", "dev").is_err());
        assert!(request_trace(&composed, ID, "fixture-route-auth", "foreign", "dev").is_err());
    }

    #[test]
    fn metrics_require_each_scoped_counter_and_refuse_invalid_counts() {
        let valid = "guest_invocation_duration_count{workload_namespace=\"owned\",component=\"flow-http\",plugin=\"wasi-http\",http_request_method=\"POST\"} 2\nwamn_postgres_query_duration_ms_count{wamn_project=\"fixture\"} 1\nwamn_jetstream_duration_ms_count{wamn_project=\"fixture\"} 1\n";
        assert!(metrics(valid, "owned", "fixture").is_ok());
        for omitted in 0..3 {
            let changed = valid
                .lines()
                .enumerate()
                .filter(|(index, _)| *index != omitted)
                .map(|(_, line)| line)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(metrics(&changed, "owned", "fixture").is_err());
        }
        assert!(metrics(valid, "foreign", "fixture").is_err());
        for count in ["NaN", "+Inf", "-1", "0"] {
            assert!(
                metrics(
                    &valid.replace("} 1", &format!("}} {count}")),
                    "owned",
                    "fixture"
                )
                .is_err()
            );
        }
        assert_eq!(
            labels(r#"name="a\"b",path="a\\b""#).unwrap()["name"],
            "a\"b"
        );
    }

    #[test]
    fn identifiers_require_full_size_and_nonzero_hex() {
        assert_eq!(
            identifier("aAbBcCdDeEfF0011", 8).unwrap(),
            "aabbccddeeff0011"
        );
        for invalid in ["ab", "qrvM3e7/ABE=", "0000000000000000", "not a trace id"] {
            assert!(identifier(invalid, 8).is_err(), "{invalid}");
        }
    }
}

/// Require the serving spans and reject component loading during a request.
///
/// The route declares its statement count. The caller declares whether the
/// request acquires executor authority.
pub fn request_trace_is_complete(
    trace: &Value,
    expect_executor: bool,
    expected_statements: usize,
) -> bool {
    let spans = trace_spans(trace).collect::<Vec<_>>();
    let count_named = |name: &str| spans.iter().filter(|span| span["name"] == name).count();
    let count_acquired = |class: &str| {
        spans
            .iter()
            .filter(|span| {
                span["name"] == "wamn.postgres.acquire"
                    && attributes(span)
                        .get("wamn.authority_class")
                        .and_then(Value::as_str)
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
mod request_shape_tests {
    use serde_json::{Value, json};

    use super::request_trace_is_complete;

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

    fn trace(statements: usize, executor: usize, omit: &str, extra: Option<Value>) -> Value {
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
        serde_json::from_value(json!({ "resourceSpans": [{ "scopeSpans": [{ "spans": spans }] }] }))
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
        assert!(serde_json::from_str::<Value>("not JSON").is_err());
    }
}
