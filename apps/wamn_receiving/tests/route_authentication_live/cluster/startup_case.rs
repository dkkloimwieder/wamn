//! Native start demand and progress through the retained Receiving fixture.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, DirBuilder};
use std::os::unix::fs::DirBuilderExt as _;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use base64::Engine as _;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::process::Child;
use wamn_ctl::print_release_env::ReleaseCarrier;

use super::{ReceivingCluster, checked, kubectl};

pub(super) async fn assert_startup(
    cluster: &ReceivingCluster,
    carrier: &ReleaseCarrier,
) -> anyhow::Result<()> {
    let private = cluster.resources.work.join("startup-burst");
    DirBuilder::new().mode(0o700).create(&private)?;
    let evidence = cluster.resources.evidence.join("startup-burst");
    fs::create_dir(&evidence)?;
    let redactions = redactions(cluster)?;
    let mut final_result = json!({"source":cluster.resources.source,"verdict":"fail","cleanup":[]});
    let mut forwards = Vec::new();
    let result = async {
        let deployed: Value = serde_json::from_slice(&fs::read(cluster.resources.evidence.join("host-deployment.json"))?)?;
        let host = deployed["spec"]["template"]["spec"]["containers"].as_array()
            .context("the host deployment has containers")?.iter().find(|container| container["name"] == "host")
            .context("the deployment has its native host")?;
        let limits = host["env"].as_array().context("the native host has its declared environment")?.iter()
            .filter(|entry| entry["name"] == "WASH_MAX_CONCURRENT_STARTS")
            .map(|entry| entry["value"].as_str()).collect::<Vec<_>>();
        ensure!(limits.len() == 1 && limits[0].is_some_and(|limit| !limit.is_empty() && limit.bytes().all(|byte| byte.is_ascii_digit()))
            && !host["args"].as_array().into_iter().flatten().any(|argument| argument.as_str()
                .is_some_and(|argument| argument.split('=').next() == Some("--max-concurrent-starts"))),
            "the deployed host must have one explicit chart native start limit");
        let limit = limits[0].context("the native start limit is literal")?.parse::<usize>()?;
        ensure!(limit > 0, "the deployed native start limit must be positive");
        final_result["deployment_resources"] = host["resources"].clone();
        final_result["local_process_resource_limit"] = json!("inherits runner cgroup; not the Kubernetes 6-CPU quota");
        final_result["host_binary_sha256"] = json!(hex::encode(Sha256::digest(fs::read(cluster.artifacts.target.join("release/wamn-host"))?)));
        final_result["profile"] = json!("release");
        final_result["source_requirements"] = json!({"wasmtime_parallel_compilation":"Cargo.toml workspace Wasmtime features, captured by source SHA",
            "guest_memory_mode":"count","meter_mode":"duration","start_limit":limit});
        for (name,path) in [("Cargo.toml",cluster.resources.repository.join("Cargo.toml")),
            ("Cargo.lock",cluster.resources.repository.join("Cargo.lock")),
            ("host-deployment.json",cluster.resources.evidence.join("host-deployment.json")),
            ("production-workload.json",cluster.resources.evidence.join("flow-http-workload.json"))] {
            final_result["input_sha256"][name] = json!(hex::encode(Sha256::digest(fs::read(path)?)));
        }
        let scheduler = forward(cluster, &private, "scheduler", "service/nats", 4222, &mut forwards).await?;
        let otlp = forward(cluster, &private, "otlp", "deployment/otel-collector", 4317, &mut forwards).await?;
        let inputs = super::super::startup_burst::Inputs {
            source:cluster.resources.source.clone(), host_binary:cluster.artifacts.target.join("release/wamn-host"),
            host_secrets:cluster.inputs.host_secret_directory.clone(), registry_auth:cluster.inputs.registry_auth_file.clone(),
            workload:cluster.resources.evidence.join("flow-http-workload.json"), pat_secret:cluster.inputs.route_caller_secret_output.clone(),
            private_dir:private.clone(), evidence_dir:evidence.clone(), nats_url:cluster.nats_url.clone(),
            scheduler_nats_url:format!("nats://127.0.0.1:{scheduler}"), otlp_endpoint:format!("http://127.0.0.1:{otlp}"),
            proof_id:format!("startup-{}",uuid::Uuid::new_v4().simple()),
            component_artifact_base:cluster.inputs.component_artifact_base.clone(), release_artifact_base:carrier.artifact_base.clone(),
            manifest_digest:carrier.manifest_digest.to_string(), org:super::super::ORG.to_owned(), project:super::super::PROJECT.to_owned(),
            schema:"receiving".to_owned(), environment:cluster.resources.name.clone(), route_host:cluster.inputs.route_host.clone(),
            route_path:"/purchase_order/get".to_owned(), probe_body:json!({"id":"00000000-0000-0000-0000-000000000301"}), max_concurrent_starts:limit,
        };
        let scope = wamn_control_registry::Triple::new(super::super::ORG,super::super::PROJECT,super::super::ENVIRONMENT);
        let budget = 2 * 120 + 2 * 30 + 4 * limit as u64 * 30 + 70 + 5;
        tokio::time::timeout(Duration::from_secs(budget), super::super::startup_burst::assert_startup(
            &inputs, &cluster.broker.runtime, &scope, &cluster.source)).await.context("the retained native startup test exceeded its combined operation budgets")??;
        final_result["test_exit_code"] = json!(0);
        let protocol: Value = serde_json::from_slice(&fs::read(evidence.join("protocol.json"))?)?;
        ensure!(protocol["verdict"] == "protocol-pass-awaiting-trace-exposure", "the native startup protocol did not pass");
        let spans = collect(cluster, &inputs.proof_id, &protocol, &evidence, &redactions).await?;
        final_result["phases"] = phases(&protocol, &spans, limit)?;
        let raw = fs::read(private.join("host.raw.log"))?;
        let raw = strip_ansi(&String::from_utf8_lossy(&raw));
        let marker = format!("max_concurrent_starts={limit}");
        ensure!(raw.match_indices(&marker).any(|(start,_)| raw.as_bytes().get(start+marker.len())
            .is_none_or(|next| !next.is_ascii_alphanumeric() && *next != b'_')),
            "the native host log did not confirm the declared start limit");
        for name in ["cache_scope","phase_attribution","host_ready_seconds"] { final_result[name] = protocol[name].clone(); }
        final_result["first_success_since_process_start_seconds"] = json!(
            (number(&protocol["cold"]["started_unix_ns"])? - number(&protocol["process_started_unix_ns"])?) as f64 / 1e9
                + protocol["cold"]["first_success_seconds"].as_f64().context("cold progress has a first success time")?);
        final_result["occupancy_limit"] = json!("workload_start begins before the permit; overlap proves queued demand, not active permit occupancy or CPU-core use");
        final_result["comparison_limit"] = json!("Distinct from a herd of different cold digests; local host cgroup and new native probe semantics differ from historical in-cluster timings. Existing performance modes are unchanged.");
        Ok::<(),anyhow::Error>(())
    }.await;
    let mut cleanup_errors = Vec::new();
    for (pid, child) in forwards.iter_mut().rev() {
        let stopped = async {
            if let Some(status) = child.try_wait()? {
                return Ok::<_, anyhow::Error>(json!({"pid":pid,"exit_code":status.code(),"already_exited":true}));
            }
            let signaled = tokio::process::Command::new("kill")
                .args(["-TERM", "--", &format!("-{pid}")]).status().await?;
            let outcome = tokio::time::timeout(Duration::from_secs(70), child.wait()).await;
            let forced = outcome.is_err();
            let status = match outcome {
                Ok(status) => status?,
                Err(_) => {
                    child.start_kill()?;
                    tokio::time::timeout(Duration::from_secs(5), child.wait()).await??
                }
            };
            let entry = json!({"pid":pid,"exit_code":status.code(),"signal_sent":signaled.success(),"wait_exceeded_host_grace":forced});
            ensure!(!forced, "an owned port forward exceeded its shutdown grace: {entry}");
            Ok(entry)
        }.await;
        match stopped {
            Ok(entry) => final_result["cleanup"]
                .as_array_mut()
                .context("startup cleanup is an array")?
                .push(entry),
            Err(error) => {
                final_result["cleanup"]
                    .as_array_mut()
                    .context("startup cleanup is an array")?
                    .push(json!({"pid":pid,"failure":format!("{error:#}")}));
                cleanup_errors.push(format!("{error:#}"));
            }
        }
    }
    let result = match (result, cleanup_errors.is_empty()) {
        (result, true) => result,
        (Ok(()), false) => Err(anyhow::anyhow!(
            "port-forward cleanup failed: {}",
            cleanup_errors.join("; ")
        )),
        (Err(error), false) => Err(error.context(format!(
            "port-forward cleanup also failed: {}",
            cleanup_errors.join("; ")
        ))),
    };
    final_result["verdict"] = json!(if result.is_ok() { "pass" } else { "fail" });
    final_result["failure"] = json!(
        result
            .as_ref()
            .err()
            .map(|error| redact(&format!("{error:#}"), &redactions))
    );
    for (raw, public) in [
        ("host.raw.log", "host.log"),
        ("failure.raw.log", "failure.log"),
        ("otlp-port-forward.log", "otlp-port-forward.log"),
        ("scheduler-port-forward.log", "scheduler-port-forward.log"),
    ] {
        let path = private.join(raw);
        if path.exists() {
            fs::write(
                evidence.join(public),
                redact(&String::from_utf8_lossy(&fs::read(path)?), &redactions),
            )?;
        }
    }
    fs::write(
        evidence.join("result.json"),
        serde_json::to_vec_pretty(&final_result)?,
    )?;
    let mut paths = fs::read_dir(&evidence)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    let mut hashes = String::new();
    for path in paths {
        if path.is_file() {
            hashes.push_str(&format!(
                "{}  {}\n",
                hex::encode(Sha256::digest(fs::read(&path)?)),
                path.file_name()
                    .context("evidence has a filename")?
                    .to_string_lossy()
            ));
        }
    }
    fs::write(evidence.join("evidence.sha256"), hashes)?;
    result
}

async fn forward(
    cluster: &ReceivingCluster,
    private: &Path,
    label: &str,
    target: &str,
    port: u16,
    children: &mut Vec<(u32, Child)>,
) -> anyhow::Result<u16> {
    let path = private.join(format!("{label}-port-forward.log"));
    let log = fs::File::create(&path)?;
    let child = kubectl(&cluster.resources)
        .args([
            "-n",
            "wamn-system",
            "port-forward",
            target,
            &format!(":{port}"),
            "--address=127.0.0.1",
        ])
        .process_group(0)
        .stdout(log.try_clone()?)
        .stderr(log)
        .kill_on_drop(true)
        .spawn()?;
    let pid = child.id().context("the owned forward has a process ID")?;
    children.push((pid, child));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    loop {
        ensure!(
            children
                .last_mut()
                .context("the forward was stored")?
                .1
                .try_wait()?
                .is_none(),
            "the owned port forward exited"
        );
        for line in fs::read_to_string(&path)?.lines() {
            if let Some(value) = line
                .strip_prefix("Forwarding from 127.0.0.1:")
                .and_then(|value| value.strip_suffix(&format!(" -> {port}")))
            {
                let value = value.parse::<u16>()?;
                ensure!(value > 0, "the owned port forward has an allocated port");
                return Ok(value);
            }
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "the owned port forward did not become ready within 120 seconds"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn collect(
    cluster: &ReceivingCluster,
    identity: &str,
    protocol: &Value,
    evidence: &Path,
    redactions: &[String],
) -> anyhow::Result<BTreeMap<(String, String), (Value, String)>> {
    let expected = protocol["owned_workloads"]
        .as_array()
        .context("the startup protocol names its owned workloads")?
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .context("the owned workload ID is text")
        })
        .collect::<anyhow::Result<BTreeSet<_>>>()?;
    let proxy = "/api/v1/namespaces/wamn-system/services/http:tempo:3200/proxy";
    let mut search = reqwest::Url::parse(&format!("http://tempo{proxy}/api/search"))?;
    search
        .query_pairs_mut()
        .append_pair(
            "q",
            &format!(
                "{{ resource.wamn.startup.proof = \"{identity}\" && name = \"workload_start\" }}"
            ),
        )
        .append_pair("limit", "1000");
    let search = format!(
        "{}?{}",
        search.path(),
        search.query().context("the trace query is present")?
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
    let mut spans = BTreeMap::new();
    let mut attempt = 0;
    loop {
        attempt += 1;
        let found = capture(
            cluster,
            &search,
            &evidence.join(format!("trace-search-{attempt:03}.json")),
            redactions,
        )
        .await?;
        for trace in found["traces"].as_array().into_iter().flatten() {
            let id = trace_id(
                trace["traceID"]
                    .as_str()
                    .context("Tempo returned a trace ID")?,
            )?;
            let file = format!("native-trace-{attempt:03}-{id}.json");
            let value = capture(
                cluster,
                &format!("{proxy}/api/traces/{id}"),
                &evidence.join(&file),
                redactions,
            )
            .await?;
            for span in value["batches"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|batch| batch["scopeSpans"].as_array().into_iter().flatten())
                .flat_map(|scope| scope["spans"].as_array().into_iter().flatten())
            {
                let actual = span["traceId"]
                    .as_str()
                    .context("the span has its trace ID")?;
                let actual =
                    if actual.len() == 32 && actual.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                        actual.to_ascii_lowercase()
                    } else {
                        hex::encode(base64::prelude::BASE64_STANDARD.decode(actual)?)
                    };
                ensure!(actual == id, "Tempo returned a span from another trace");
                spans.insert(
                    (
                        id.clone(),
                        span["spanId"]
                            .as_str()
                            .context("the span has its identity")?
                            .to_owned(),
                    ),
                    (span.clone(), file.clone()),
                );
            }
        }
        let observed = spans
            .values()
            .filter(|(span, _)| span["name"] == "workload_start")
            .filter_map(|(span, _)| attribute(span, "workload_id"))
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        if expected.is_subset(&observed) {
            return Ok(spans);
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "native start spans did not arrive for every owned workload within 120 seconds"
        );
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn capture(
    cluster: &ReceivingCluster,
    path: &str,
    file: &Path,
    redactions: &[String],
) -> anyhow::Result<Value> {
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        checked(kubectl(&cluster.resources).args(["get", "--raw", path])),
    )
    .await??;
    let text = redact(&String::from_utf8_lossy(&output), redactions);
    fs::write(file, &text)?;
    Ok(serde_json::from_str(&text)?)
}

fn phases(
    protocol: &Value,
    spans: &BTreeMap<(String, String), (Value, String)>,
    limit: usize,
) -> anyhow::Result<Value> {
    let mut output = serde_json::Map::new();
    for phase in ["cold", "warm"] {
        let ids = protocol[phase]["starts"]
            .as_array()
            .context("the startup phase names its starts")?
            .iter()
            .map(|entry| {
                entry["id"]
                    .as_str()
                    .map(str::to_owned)
                    .context("the startup request names its ID")
            })
            .collect::<anyhow::Result<BTreeSet<_>>>()?;
        let mut starts = Vec::new();
        for ((trace, id), (span, file)) in spans {
            if span["name"] != "workload_start" {
                continue;
            }
            let Some(workload) = attribute(span, "workload_id") else {
                continue;
            };
            if !ids.contains(workload) {
                continue;
            }
            let start = number(&span["startTimeUnixNano"])?;
            let end = number(&span["endTimeUnixNano"])?;
            ensure!(end > start, "native start span has no positive duration");
            starts.push(json!({"id":workload,"trace_id":trace,"span_id":id,"source_file":file,"start_ns":start,"end_ns":end}));
        }
        ensure!(
            starts.len() == ids.len()
                && starts
                    .iter()
                    .map(|span| span["id"].as_str())
                    .collect::<BTreeSet<_>>()
                    .len()
                    == ids.len(),
            "native start span identity is missing or duplicated"
        );
        let mut events = Vec::new();
        for span in &starts {
            events.push((number(&span["start_ns"])?, 1i64));
            events.push((number(&span["end_ns"])?, -1i64));
        }
        events.sort();
        let (mut active, mut maximum) = (0i64, 0i64);
        for (_, change) in events {
            active += change;
            maximum = maximum.max(active);
        }
        ensure!(
            maximum > limit as i64,
            "insufficient measured native queued-start exposure"
        );
        starts.sort_by_key(|span| span["start_ns"].as_u64());
        let mut intervals: Vec<(u64, u64)> = Vec::new();
        for span in &starts {
            let start = number(&span["start_ns"])?;
            let end = number(&span["end_ns"])?;
            match intervals.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => intervals.push((start, end)),
            }
        }
        let origin = number(&protocol[phase]["started_unix_ns"])?;
        let mut progress = 0;
        for observation in protocol[phase]["observations"]
            .as_array()
            .context("the startup protocol has progress observations")?
        {
            let start = origin
                + (observation["started_seconds"]
                    .as_f64()
                    .context("progress has a start time")?
                    * 1e9) as u64;
            let end = origin
                + (observation["finished_seconds"]
                    .as_f64()
                    .context("progress has an end time")?
                    * 1e9) as u64;
            if intervals
                .iter()
                .any(|&(first, last)| first <= start && end <= last)
                && observation["native_live_status"] == 200
                && observation["native_ready_status"] == 200
                && (phase == "cold" || observation["application"]["status"] == 200)
            {
                progress += 1;
            }
        }
        ensure!(
            progress > 0,
            "no measured native serving progress during server start intervals"
        );
        output.insert(phase.into(),json!({"native_starts":starts,"max_overlapping_start_handlers":maximum,
            "complete_progress_observations_within_continuous_start_intervals":progress,"continuous_start_intervals_ns":intervals,
            "first_success_seconds":protocol[phase]["first_success_seconds"],"all_running_seconds":protocol[phase]["all_running_seconds"]}));
    }
    Ok(Value::Object(output))
}

fn number(value: &Value) -> anyhow::Result<u64> {
    value.as_u64().map(Ok).unwrap_or_else(|| {
        value
            .as_str()
            .context("a time value is numeric text")?
            .parse()
            .context("a time value is valid")
    })
}
fn attribute<'a>(span: &'a Value, name: &str) -> Option<&'a str> {
    span["attributes"]
        .as_array()?
        .iter()
        .find(|entry| entry["key"] == name)?["value"]["stringValue"]
        .as_str()
}
fn trace_id(value: &str) -> anyhow::Result<String> {
    ensure!(
        !value.is_empty()
            && value.len() <= 32
            && value.bytes().all(|byte| byte.is_ascii_hexdigit())
            && value.bytes().any(|byte| byte != b'0'),
        "Tempo returned an invalid trace identity"
    );
    Ok(format!("{:0>32}", value.to_ascii_lowercase()))
}
fn redact(text: &str, values: &[String]) -> String {
    let mut text = text.to_owned();
    for value in values {
        text = text.replace(value, "<redacted>");
    }
    text
}
fn strip_ansi(text: &str) -> String {
    let mut result = String::new();
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            while chars
                .peek()
                .is_some_and(|next| next.is_ascii_digit() || *next == ';')
            {
                chars.next();
            }
            if chars.peek() == Some(&'m') {
                chars.next();
            }
        } else {
            result.push(character);
        }
    }
    result
}

fn redactions(cluster: &ReceivingCluster) -> anyhow::Result<Vec<String>> {
    let mut values = BTreeSet::new();
    for name in [
        "identity-reader",
        "guest-sql",
        "executor-platform",
        "http-admitter",
        "event-materializer",
    ] {
        let secret: Value = serde_json::from_slice(&fs::read(
            cluster
                .inputs
                .host_secret_directory
                .join(format!("{name}.json")),
        )?)?;
        let url = secret["stringData"]["url"]
            .as_str()
            .context("the host authority has its private URL")?;
        values.insert(url.to_owned());
        if let Some(password) = reqwest::Url::parse(url)?.password() {
            values.insert(password.to_owned());
            values.insert(wamn_test_infrastructure::executor::decoded_password(
                password,
            )?);
        }
    }
    values.insert(fs::read_to_string(&cluster.broker.runtime.password_file)?);
    let secret: Value =
        serde_json::from_slice(&fs::read(&cluster.inputs.route_caller_secret_output)?)?;
    values.insert(
        secret["stringData"]["token"]
            .as_str()
            .context("the route credential has its token")?
            .to_owned(),
    );
    let auth: Value = serde_json::from_slice(&fs::read(&cluster.inputs.registry_auth_file)?)?;
    for entry in auth["auths"]
        .as_object()
        .context("the registry credential has its authorities")?
        .values()
    {
        for key in ["auth", "password", "identitytoken", "registrytoken"] {
            if let Some(value) = entry[key].as_str() {
                values.insert(value.to_owned());
            }
        }
        if let Some(auth) = entry["auth"].as_str() {
            let pair = String::from_utf8(base64::prelude::BASE64_STANDARD.decode(auth)?)?;
            values.insert(pair.clone());
            if let Some((_, password)) = pair.split_once(':') {
                values.insert(password.to_owned());
            }
        }
    }
    let mut values = values
        .into_iter()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observed() -> (Value, BTreeMap<(String, String), (Value, String)>) {
        let mut spans = BTreeMap::new();
        let mut protocol = json!({});
        for (phase, origin) in [("cold", 100u64), ("warm", 1_000)] {
            let ids = [format!("{phase}-one"), format!("{phase}-two")];
            protocol[phase] = json!({"starts":[{"id":ids[0]},{"id":ids[1]}],"started_unix_ns":origin.to_string(),
                "observations":[{"started_seconds":0.000000030,"finished_seconds":0.000000040,
                    "native_live_status":200,"native_ready_status":200,"application":{"status":200}}],
                "first_success_seconds":0.000000040,"all_running_seconds":0.000000080});
            for (id, start, end) in [
                (&ids[0], origin + 10, origin + 60),
                (&ids[1], origin + 20, origin + 80),
            ] {
                spans.insert(
                    (phase.to_owned(), id.clone()),
                    (
                        json!({"name":"workload_start",
                    "startTimeUnixNano":start.to_string(),"endTimeUnixNano":end.to_string(),
                    "attributes":[{"key":"workload_id","value":{"stringValue":id}}]}),
                        format!("{id}.json"),
                    ),
                );
            }
        }
        (protocol, spans)
    }

    #[test]
    fn requires_progress_inside_measured_overlapping_starts() {
        let (mut protocol, spans) = observed();
        let result =
            phases(&protocol, &spans, 1).expect("overlapping starts and enclosed progress");
        assert_eq!(result["cold"]["max_overlapping_start_handlers"], 2);
        assert_eq!(
            result["warm"]["complete_progress_observations_within_continuous_start_intervals"],
            1
        );
        protocol["warm"]["observations"][0]["started_seconds"] = json!(0.000000100);
        protocol["warm"]["observations"][0]["finished_seconds"] = json!(0.000000110);
        assert!(phases(&protocol, &spans, 1).is_err());
    }

    #[test]
    fn rejects_missing_duplicate_and_nonpositive_start_spans() {
        let (protocol, spans) = observed();
        let key = spans.keys().next().expect("one start").clone();
        let mut missing = spans.clone();
        missing.remove(&key);
        assert!(phases(&protocol, &missing, 1).is_err());
        let mut duplicate = spans.clone();
        duplicate.insert(("duplicate".into(), "span".into()), spans[&key].clone());
        assert!(phases(&protocol, &duplicate, 1).is_err());
        let mut empty = spans.clone();
        let span = &mut empty.get_mut(&key).expect("one start").0;
        span["endTimeUnixNano"] = span["startTimeUnixNano"].clone();
        assert!(phases(&protocol, &empty, 1).is_err());
    }

    #[test]
    fn requires_queued_demand_beyond_the_declared_limit_and_live_warm_route() {
        let (mut protocol, spans) = observed();
        assert!(phases(&protocol, &spans, 2).is_err());
        protocol["warm"]["observations"][0]["application"]["status"] = json!(503);
        assert!(phases(&protocol, &spans, 1).is_err());
    }

    #[test]
    fn preserves_tempo_trace_id_normalization_and_refusals() {
        assert_eq!(trace_id("aB").unwrap(), "000000000000000000000000000000ab");
        for invalid in ["", "0", "000000", "g", "000000000000000000000000000000001"] {
            assert!(trace_id(invalid).is_err());
        }
    }
}
