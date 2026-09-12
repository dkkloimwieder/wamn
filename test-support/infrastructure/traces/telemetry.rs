//! Collect the two request traces and service metrics from an owned cluster.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use base64::Engine as _;
use serde_json::{Value, json};
use tokio::process::Command;
use tokio::time::{Instant, sleep, timeout};

fn hex_identifier(value: &str, size: usize) -> bool {
    value.len() == size * 2 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn identifier(value: &str, size: usize) -> anyhow::Result<String> {
    if hex_identifier(value, size) {
        return Ok(value.to_ascii_lowercase());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value)
        .context("decode trace identifier")?;
    ensure!(
        bytes.len() == size,
        "trace identifier has an invalid length"
    );
    Ok(hex::encode(bytes))
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
    let spans = document["batches"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|batch| batch["scopeSpans"].as_array().into_iter().flatten())
        .flat_map(|scope| scope["spans"].as_array().into_iter().flatten())
        .collect::<Vec<_>>();
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
        if span["name"] != "wamn.component.invoke" {
            continue;
        }
        let fields = attributes(span);
        let text = |key: &str| fields.get(key).and_then(Value::as_str).unwrap_or_default();
        let digest = text("wamn.component_digest")
            .strip_prefix("sha256:")
            .unwrap_or_default();
        ensure!(
            text("wamn.tenant") == tenant
                && text("wamn.project") == project
                && text("wamn.environment") == environment
                && hex_identifier(digest, 32)
                && digest.bytes().all(|byte| !byte.is_ascii_uppercase())
                && text("wamn.caller_credential_kind") == "pat"
                && [
                    "wamn.wiring_id",
                    "wamn.node_id",
                    "wamn.operation",
                    "wamn.caller_principal_id"
                ]
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

/// Read Tempo and collector results for two sampled requests in the owned cluster.
pub async fn collect(
    cluster: &str,
    work: &Path,
    namespace: &str,
    source: &str,
    tenant: &str,
    project: &str,
    environment: &str,
    requests: [(&str, &str); 2],
    evidence: &Path,
) -> anyhow::Result<()> {
    ensure!(
        work.join("kubeconfig").is_file(),
        "telemetry requires the private kubeconfig"
    );
    ensure!(
        hex_identifier(source, 20),
        "telemetry requires the full source commit"
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
    let mut paths = requests
        .iter()
        .map(|(name, id)| (*name, format!("{prefix}tempo:3200/proxy/api/traces/{id}")))
        .collect::<Vec<_>>();
    paths.push((
        "metrics",
        format!("{prefix}otel-collector:8889/proxy/metrics"),
    ));
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
                let parsed = if *name == "metrics" { metrics(std::str::from_utf8(&output.stdout)?,namespace,project)? } else {
                    let trace = requests.iter().find(|(label,_)| label == name).context("request trace is declared")?.1;
                    request_trace(&serde_json::from_slice(&output.stdout)?,trace,tenant,project,environment)?
                };
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
    let result = json!({"verdict":if passed {"pass"} else {"fail"},"source":source,"context":context,"namespace":namespace,"traces":requests.into_iter().collect::<BTreeMap<_,_>>(),"collected":completed,"failure":if passed {None} else {Some(failure.as_str())}});
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
    const TRACE: &str = include_str!(
        "../fixtures/traces/receiving-update.json"
    );
    const ID: &str = "655833bf599cf31d4f5422f6e06c7347";

    fn observed(document: &Value) -> anyhow::Result<Value> {
        request_trace(document, ID, "receiving-route-auth", "receiving", "dev")
    }

    #[test]
    fn retained_request_requires_its_identity_completed_spans_and_effect_ancestry() {
        let original: Value = serde_json::from_str(TRACE).unwrap();
        assert!(observed(&original).is_ok());
        for field_name in ["traceId", "spanId", "parentSpanId", "endTimeUnixNano"] {
            let mut changed = original.clone();
            let spans = changed["batches"]
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
        assert!(request_trace(&original, ID, "foreign", "receiving", "dev").is_err());
        assert!(request_trace(&original, ID, "receiving-route-auth", "foreign", "dev").is_err());
        assert!(request_trace(&original, ID, "receiving-route-auth", "receiving", "prod").is_err());
    }

    #[test]
    fn metrics_require_each_scoped_counter_and_refuse_invalid_counts() {
        let valid = "guest_invocation_duration_count{workload_namespace=\"owned\",component=\"flow-http\",plugin=\"wasi-http\",http_request_method=\"POST\"} 2\nwamn_postgres_query_duration_ms_count{wamn_project=\"receiving\"} 1\nwamn_jetstream_duration_ms_count{wamn_project=\"receiving\"} 1\n";
        assert!(metrics(valid, "owned", "receiving").is_ok());
        for omitted in 0..3 {
            let changed = valid
                .lines()
                .enumerate()
                .filter(|(index, _)| *index != omitted)
                .map(|(_, line)| line)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(metrics(&changed, "owned", "receiving").is_err());
        }
        assert!(metrics(valid, "foreign", "receiving").is_err());
        for count in ["NaN", "+Inf", "-1", "0"] {
            assert!(
                metrics(
                    &valid.replace("} 1", &format!("}} {count}")),
                    "owned",
                    "receiving"
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
    fn identifiers_accept_hex_or_base64_with_the_exact_size() {
        assert_eq!(
            identifier("aabbccddeeff0011", 8).unwrap(),
            "aabbccddeeff0011"
        );
        assert_eq!(identifier("qrvM3e7/ABE=", 8).unwrap(), "aabbccddeeff0011");
        assert!(identifier("qrvM3e7/ABE=", 16).is_err());
    }
}
