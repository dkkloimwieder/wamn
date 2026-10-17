//! Collect service metrics from the owned cluster test collector.

use std::collections::BTreeMap;
use std::fs::DirBuilder;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::process::Command;
use tokio::time::{Instant, sleep, timeout};

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

/// The cluster and scope that one metric read covers.
#[derive(Debug, Clone, Copy)]
pub struct TelemetryInput<'a> {
    pub cluster: &'a str,
    pub work: &'a Path,
    pub namespace: &'a str,
    pub project: &'a str,
    pub evidence: &'a Path,
}

/// Read the metric test collector in the owned cluster.
pub async fn collect(input: &TelemetryInput<'_>) -> anyhow::Result<()> {
    let TelemetryInput {
        cluster,
        work,
        namespace,
        project,
        evidence,
    } = *input;
    ensure!(
        work.join("kubeconfig").is_file(),
        "telemetry requires the private kubeconfig"
    );
    DirBuilder::new()
        .mode(0o700)
        .create(evidence)
        .context("create telemetry output directory")?;
    let prefix = "/api/v1/namespaces/wamn-system/services/http:";
    let paths = [(
        "metrics",
        format!("{prefix}otel-collector:8889/proxy/metrics"),
    )];
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
            let filename = format!("{name}-{attempt:03}.prom");
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
    let result = json!({"verdict":if passed {"pass"} else {"fail"},"context":context,"namespace":namespace,"collected":completed,"failure":if passed {None} else {Some(failure.as_str())}});
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
}
