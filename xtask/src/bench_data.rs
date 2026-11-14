//! Summaries of retained benchmark measurements.

use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use tokio::io::AsyncWriteExt as _;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub fn read(path: &Path) -> anyhow::Result<Value> {
    serde_json::from_slice(
        &std::fs::read(path).with_context(|| format!("read {}", path.display()))?,
    )
    .with_context(|| format!("parse {}", path.display()))
}
pub fn write(path: &Path, value: &Value) -> anyhow::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    writeln!(file)?;
    Ok(())
}
fn number(value: &Value) -> anyhow::Result<f64> {
    value.as_f64().context("measurement is not numeric")
}
fn f64_count(value: usize) -> f64 {
    serde_json::Value::from(value)
        .as_f64()
        .expect("count is numeric")
}

pub fn result(work: &Path, output: &Path, concurrency: usize) -> anyhow::Result<()> {
    let mut result = json!({"concurrency":concurrency});
    for call in [
        "get",
        "update",
        "query",
        "receipt-query",
        "create",
        "record_receipt",
        "mix",
        "app-path",
    ] {
        let path = work.join(format!("{call}.json"));
        if !path.exists() {
            continue;
        }
        let run = read(&path)?;
        for key in [
            "throughput_per_sec",
            "p50_ms",
            "p95_ms",
            "p99_ms",
            "calls",
            "errors",
            "lines",
            "statement_ms_per_row",
            "trigger_ms_per_row",
            "trigger_share",
        ] {
            if let Some(value) = run.get(key) {
                result[call][key] = value.clone();
            }
        }
        let counts = work.join(format!("{call}.pgss"));
        result[call]["pg_stat_statements_calls_per_request"] = if counts.is_file() {
            let mut total = 0.0;
            for line in std::fs::read_to_string(counts)?.lines() {
                total += (line.parse::<f64>()? / number(&run["calls"])?).round_ties_even();
            }
            json!(total)
        } else {
            run["statements_per_request"].clone()
        };
        let cpu = work.join(format!("{call}.host-cpu"));
        if cpu.is_file() {
            let values = std::fs::read_to_string(cpu)?
                .split_whitespace()
                .map(str::parse::<f64>)
                .collect::<Result<Vec<_>, _>>()?;
            ensure!(
                values.len() == 3,
                "CPU sample requires start, end, ticks per second"
            );
            result[call]["host_cpu_ms_per_request"] =
                json!((values[1] - values[0]) * 1000.0 / values[2] / number(&run["calls"])?);
        }
    }
    write(output, &result)?;
    println!(
        "{}\n{}",
        serde_json::to_string_pretty(&result)?,
        output.display()
    );
    Ok(())
}

fn delta(before: &Value, after: &Value) -> anyhow::Result<Value> {
    ensure!(
        before["stats_reset"] == after["stats_reset"],
        "PostgreSQL counters reset during the run"
    );
    let mut result = json!({});
    for (name, value) in after.as_object().context("counter object")? {
        if let (Some(before), Some(after)) = (before[name].as_f64(), value.as_f64()) {
            result[name] = json!(after - before);
        }
    }
    Ok(result)
}
pub fn database_delta(work: &Path, call: &str) -> anyhow::Result<()> {
    let run = work.join(format!("{call}.json"));
    let (before, after) = if run.is_file() {
        let run = read(&run)?;
        (
            run["postgres"]["before"]["cluster"].clone(),
            run["postgres"]["after"]["cluster"].clone(),
        )
    } else {
        (
            read(&work.join(format!("{call}.database-before.json")))?,
            read(&work.join(format!("{call}.database-after.json")))?,
        )
    };
    let mut result = json!({});
    for name in ["wal", "checkpointer"] {
        result[name] = delta(&before[name], &after[name])?;
    }
    let mut io = Vec::new();
    for row in after["io"].as_array().into_iter().flatten() {
        let earlier = before["io"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|entry| {
                ["backend_type", "object", "context"]
                    .iter()
                    .all(|key| entry[key] == row[key])
            })
            .context("I/O counter absent before run")?;
        let mut value = delta(earlier, row)?;
        for key in ["backend_type", "object", "context"] {
            value[key] = row[key].clone();
        }
        io.push(value);
    }
    result["io"] = io.into();
    write(&work.join(format!("{call}.database-delta.json")), &result)
}

pub fn app_result(
    work: &Path,
    output: &Path,
    http: &str,
    edge: bool,
    scale: usize,
) -> anyhow::Result<()> {
    let mut run = read(&work.join("app-path.json"))?;
    ensure!(run["errors"] == 0, "application path returned errors");
    let configuration = read(&work.join("host-telemetry.json"))?;
    ensure!(
        configuration["OTEL_TRACES_SAMPLER"] == "parentbased_traceidratio",
        "application path requires the deployed host sampler"
    );
    run["sampling_percentage"] = json!(
        100.0
            * configuration["OTEL_TRACES_SAMPLER_ARG"]
                .as_str()
                .context("sampler ratio")?
                .parse::<f64>()?
    );
    let mut traces: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
    for line in std::fs::read_to_string(work.join("env/local-artifacts/otel/traces.jsonl"))?.lines()
    {
        let data: Value = serde_json::from_str(line)?;
        for resource in data["resourceSpans"].as_array().into_iter().flatten() {
            for scope in resource["scopeSpans"].as_array().into_iter().flatten() {
                for span in scope["spans"].as_array().into_iter().flatten() {
                    traces
                        .entry(span["traceId"].as_str().context("trace id")?.to_owned())
                        .or_default()
                        .insert(
                            span["spanId"].as_str().context("span id")?.to_owned(),
                            span.clone(),
                        );
                }
            }
        }
    }
    let nano = |span: &Value, key: &str| -> anyhow::Result<u64> {
        span[key].as_u64().map_or_else(
            || {
                span[key]
                    .as_str()
                    .context("span timestamp")?
                    .parse()
                    .context("span timestamp")
            },
            Ok,
        )
    };
    let mut totals = Vec::new();
    let mut parts: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for spans in traces.values() {
        for root in spans
            .values()
            .filter(|span| span["name"] == "invoke_component_handler")
        {
            let start = nano(root, "startTimeUnixNano")?;
            if start < run["started_at_unix_nanos"].as_u64().context("run start")?
                || start >= run["ended_at_unix_nanos"].as_u64().context("run end")?
            {
                continue;
            }
            let mut selected = Vec::new();
            let mut pending = vec![root];
            while let Some(span) = pending.pop() {
                selected.push(span);
                pending.extend(
                    spans
                        .values()
                        .filter(|child| child["parentSpanId"] == span["spanId"]),
                );
            }
            if !selected.iter().any(|span| {
                span["attributes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|attr| {
                        attr["key"] == "wamn.operation"
                            && attr["value"]["stringValue"]
                                .as_str()
                                .is_some_and(|value| value.contains("receipt/get@"))
                    })
            }) {
                continue;
            }
            totals.push(number(&json!(nano(root, "endTimeUnixNano")? - start))? / 1e6);
            for span in selected {
                let start = nano(span, "startTimeUnixNano")?;
                let end = nano(span, "endTimeUnixNano")?;
                let mut intervals = Vec::new();
                for child in spans
                    .values()
                    .filter(|child| child["parentSpanId"] == span["spanId"])
                {
                    intervals.push((
                        start.max(nano(child, "startTimeUnixNano")?),
                        end.min(nano(child, "endTimeUnixNano")?),
                    ));
                }
                intervals.sort_unstable();
                let mut covered = 0;
                let mut cursor = start;
                for (left, right) in intervals {
                    covered += right.saturating_sub(left.max(cursor));
                    cursor = cursor.max(right);
                }
                parts
                    .entry(span["name"].as_str().context("span name")?.to_owned())
                    .or_default()
                    .push(number(&json!(end - start - covered))? / 1e6);
            }
        }
    }
    ensure!(
        !totals.is_empty(),
        "no sampled receipt traces in the measured interval"
    );
    run["sampled_trace_count"] = totals.len().into();
    run["edge"] = edge.into();
    run["seed_scale"] = scale.into();
    run["http_route_sha256"] = http.into();
    run["host_mean_ms"] = json!(totals.iter().sum::<f64>() / f64_count(totals.len()));
    let mut postgres = 0.0;
    for (name, values) in parts {
        let mean = values.iter().sum::<f64>() / f64_count(totals.len());
        if name == "wamn.postgres" || name.starts_with("wamn.postgres.") {
            postgres += mean;
        }
        run["span_parts"][name] = json!({"mean_ms":mean,"span_count":values.len(),"per_span_mean_ms":values.iter().sum::<f64>()/f64_count(values.len())});
    }
    run["postgres_mean_ms"] = json!(postgres);
    write(output, &run)
}

pub async fn name_guests(root: &Path, artifacts: &Path, stacks: &Path) -> anyhow::Result<()> {
    let pattern = regex::Regex::new(r#"^\s*\(func \$([^\s"]+) \(;(\d+);\)"#)?;
    let mut candidates: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    for file in std::fs::read_dir(artifacts)? {
        let path = file?.path();
        if path.extension().is_none_or(|ext| ext != "wasm") {
            continue;
        }
        let text = crate::process::output(
            root,
            "wasm-tools",
            &["print", path.to_str().context("artifact path")?],
        )
        .await?;
        for line in text.lines() {
            if let Some(row) = pattern.captures(line)
                && row[1].bytes().all(|b| b.is_ascii_graphic())
            {
                candidates
                    .entry((row[2].to_owned(), row[1].chars().take(96).collect()))
                    .or_default()
                    .insert(row[1].to_owned());
            }
        }
    }
    let mut names = BTreeMap::new();
    for values in candidates.values().filter(|values| values.len() == 1) {
        let name = values.first().expect("one candidate");
        let mut child = tokio::process::Command::new("rustfilt")
            .current_dir(root)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .context("rustfilt stdin")?
            .write_all(name.as_bytes())
            .await?;
        let result = child.wait_with_output().await?;
        ensure!(result.status.success(), "demangle guest name");
        names.insert(name, String::from_utf8(result.stdout)?.trim().to_owned());
    }
    let text = std::fs::read_to_string(stacks)?;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(stacks.with_extension("raw.stacks"))?
        .write_all(text.as_bytes())?;
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let pattern = regex::Regex::new(r"(wasm\[\d+\]::function\[(\d+)\]::)(\S+)")?;
    let renamed = pattern.replace_all(&text, |row: &regex::Captures<'_>| {
        let (symbol, offset) = row[3].split_once("+0x").unwrap_or((&row[3], ""));
        let found = candidates.get(&(row[2].to_owned(), symbol.to_owned()));
        if let Some(found) = found.filter(|found| found.len() == 1) {
            *counts.entry("resolved").or_default() += 1;
            format!(
                "{}{}{}",
                &row[1],
                names[found.first().expect("one candidate")],
                if offset.is_empty() {
                    String::new()
                } else {
                    format!("+0x{offset}")
                }
            )
        } else {
            *counts
                .entry(if found.is_some() {
                    "ambiguous"
                } else {
                    "unmatched"
                })
                .or_default() += 1;
            row[0].to_owned()
        }
    });
    std::fs::write(stacks, renamed.as_bytes())?;
    write(
        &stacks.with_extension("names.json"),
        &serde_json::to_value(counts)?,
    )
}

/// Read pgbench's completed transactions and PostgreSQL statement counters.
pub fn pg_result(work: &Path, output: &Path, concurrency: usize) -> anyhow::Result<()> {
    let mut result = json!({"concurrency":concurrency});
    let mut calls = vec![
        "select-only".to_owned(),
        "simple-update".into(),
        "get".into(),
        "update".into(),
        "update-no-triggers".into(),
    ];
    let mut receipts = Vec::new();
    for file in std::fs::read_dir(work)? {
        let path = file?.path();
        if let Some(call) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| stem.starts_with("receipt-") && !stem.contains('.'))
            && path.extension().is_some_and(|ext| ext == "log")
        {
            receipts.push(call.to_owned());
        }
    }
    receipts.sort_by_key(|call| call[8..].parse::<usize>().unwrap_or(0));
    calls.extend(receipts);
    for call in calls {
        let log = work.join(format!("{call}.log"));
        if !log.is_file() {
            continue;
        }
        let log = std::fs::read_to_string(log)?;
        let field = |prefix: &str| -> anyhow::Result<&str> {
            log.lines()
                .find_map(|line| line.strip_prefix(prefix))
                .and_then(|value| value.split_whitespace().next())
                .context("pgbench result field")
        };
        let failures = field("number of failed transactions: ")?.parse::<u64>()?;
        ensure!(failures == 0, "{call}: failed transactions");
        let mut latencies = Vec::new();
        for file in std::fs::read_dir(work)? {
            let path = file?.path();
            if path.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .starts_with(&format!("{call}.latencies."))
            }) {
                for line in std::fs::read_to_string(path)?.lines() {
                    latencies.push(
                        line.split_whitespace()
                            .nth(2)
                            .context("pgbench latency")?
                            .parse::<u64>()?,
                    );
                }
            }
        }
        latencies.sort_unstable();
        ensure!(!latencies.is_empty(), "{call}: no completed transactions");
        let calls = f64_count(latencies.len());
        let rows = csv_rows(&work.join(format!("{call}.statements.csv")))?;
        let statements = rows
            .iter()
            .map(|row| {
                row["calls"]
                    .parse::<f64>()
                    .map(|n| (n / calls).round_ties_even())
            })
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .sum::<f64>();
        result[&call] = json!({"throughput_per_sec":field("tps = ")?.parse::<f64>()?,"calls":latencies.len(),"errors":failures,"pg_stat_statements_calls_per_request":statements});
        for percent in [50, 95, 99] {
            let index = (latencies.len() * percent).div_ceil(100) - 1;
            result[&call][format!("p{percent}_ms")] =
                json!(number(&json!(latencies[index]))? / 1000.0);
        }
        if let Some(lines) = call.strip_prefix("receipt-") {
            let lines = lines.parse::<usize>()?;
            let counts = std::fs::read_to_string(work.join(format!("{call}.receipt-counts")))?;
            let counts = counts
                .trim()
                .split('|')
                .map(str::parse::<usize>)
                .collect::<Result<Vec<_>, _>>()?;
            ensure!(
                counts == [latencies.len(), latencies.len() * lines],
                "{call}: receipt and line counts differ from completed calls"
            );
            let statement = rows
                .iter()
                .filter(|row| {
                    row.get("toplevel").is_some_and(|v| v == "t")
                        && row
                            .get("query")
                            .is_some_and(|v| v.starts_with("WITH requested AS"))
                })
                .map(|row| row["total_exec_time"].parse::<f64>())
                .collect::<Result<Vec<_>, _>>()?
                .iter()
                .sum::<f64>();
            let functions = csv_rows(&work.join(format!("{call}.functions.csv")))?;
            let triggers = functions
                .iter()
                .filter(|row| {
                    row.get("schemaname").is_some_and(|v| v == "wamn_history")
                        && row
                            .get("funcname")
                            .is_some_and(|v| v.starts_with("receiving__"))
                })
                .map(|row| row["total_time"].parse::<f64>())
                .collect::<Result<Vec<_>, _>>()?
                .iter()
                .sum::<f64>();
            result[&call]["lines"] = lines.into();
            result[&call]["statement_ms_per_row"] = json!(statement / (calls * f64_count(lines)));
            result[&call]["trigger_ms_per_row"] = json!(triggers / (calls * f64_count(lines)));
            result[&call]["trigger_share"] = json!(triggers / statement);
        }
    }
    let updates = result
        .as_object()
        .expect("result object")
        .iter()
        .filter(|(name, _)| name.starts_with("update"))
        .map(|(_, value)| value["calls"].as_u64().unwrap_or(0))
        .sum::<u64>();
    if updates > 0 {
        let counts = std::fs::read_to_string(work.join("update-counts"))?;
        let counts = counts
            .trim()
            .split('|')
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()?;
        ensure!(
            counts == [updates, updates],
            "update replay differs from completed writes"
        );
    }
    write(output, &result)
}
fn csv_rows(path: &Path) -> anyhow::Result<Vec<BTreeMap<String, String>>> {
    csv::Reader::from_path(path)?
        .deserialize()
        .collect::<Result<Vec<_>, _>>()
        .context("read PostgreSQL counters")
}

pub fn profile_summary(source: &Path, output: &Path) -> anyhow::Result<()> {
    type Frame = (String, String);
    let frame = regex::Regex::new(r"^\s+[0-9a-f]+\s+(.+)\s+\((.*)\)\s*$")?;
    let header = regex::Regex::new(r"\s(\d+)\s+cpu-clock:u:")?;
    let offset = regex::Regex::new(r"\+0x[0-9a-f]+$")?;
    let allocator = regex::Regex::new(
        r"(^|::)(__libc_|dl)?(malloc|free|cfree|calloc|realloc|memalign|aligned_alloc|posix_memalign)(@.*)?$|^__libc_malloc2$|^unlink_chunk(?:[.]|$)|^malloc_consolidate$|^_int_(malloc|free|realloc)|(^|::)(__rust_|__rdl_)(alloc|dealloc|realloc)|^(<?)(alloc::alloc::|alloc::raw_vec::|std::alloc::|std::sys::alloc::)",
    )?;
    let hash = regex::Regex::new(r"\[[0-9a-f]{16}\]")?;
    let prefix = regex::Regex::new(r"^wasm\[\d+\]::function\[\d+\]::")?;
    let is_allocator =
        |symbol: &str| allocator.is_match(&prefix.replace(&hash.replace_all(symbol, ""), ""));
    let category = |frame: &Frame| {
        if frame.1.contains("perf-")
            || frame.0.starts_with("wasm[")
            || frame.0.starts_with("wasm::")
        {
            "guest"
        } else if frame.1 == "[unknown]" {
            "unknown"
        } else if frame.1.contains("kernel") {
            "kernel"
        } else {
            "host"
        }
    };
    let mut self_time: BTreeMap<Frame, u64> = BTreeMap::new();
    let mut inclusive: BTreeMap<Frame, u64> = BTreeMap::new();
    let mut callers: BTreeMap<Frame, u64> = BTreeMap::new();
    let mut split: BTreeMap<&str, u64> = BTreeMap::new();
    let mut samples = 0;
    let mut total = 0;
    let mut shallow = 0;
    let mut unknown = 0;
    let mut frames = Vec::new();
    let mut period = 0;
    let text = std::fs::read_to_string(source)?;
    for line in text.lines().chain(std::iter::once("")) {
        if let Some(row) = header.captures(line) {
            period = row[1].parse::<u64>()?;
        }
        if let Some(row) = frame.captures(line) {
            frames.push((offset.replace(&row[1], "").into_owned(), row[2].to_owned()));
        } else if line.trim().is_empty() && !frames.is_empty() {
            ensure!(period > 0, "decode cpu-clock:u with perf script -F +period");
            samples += 1;
            total += period;
            shallow += usize::from(frames.len() < 3);
            let leaf = &frames[0];
            *self_time.entry(leaf.clone()).or_default() += period;
            *split.entry(category(leaf)).or_default() += period;
            unknown += usize::from(
                frames
                    .iter()
                    .any(|f| f.0 == "[unknown]" || category(f) == "unknown"),
            );
            for frame in frames.iter().collect::<BTreeSet<_>>() {
                *inclusive.entry(frame.clone()).or_default() += period;
            }
            if is_allocator(&leaf.0) {
                let caller = frames
                    .iter()
                    .skip(1)
                    .find(|f| !is_allocator(&f.0))
                    .cloned()
                    .unwrap_or_else(|| ("[unresolved caller]".into(), "[unknown]".into()));
                *callers.entry(caller).or_default() += period;
            }
            frames.clear();
        }
    }
    ensure!(samples > 0, "no perf call stacks decoded");
    let total_float = number(&json!(total))?;
    let rows = |counter: &BTreeMap<Frame, u64>, group: Option<&str>| -> anyhow::Result<Value> {
        let mut entries = counter
            .iter()
            .filter(|(frame, _)| group.is_none_or(|group| category(frame) == group))
            .collect::<Vec<_>>();
        entries.sort_by_key(|(_, count)| std::cmp::Reverse(**count));
        let mut result = Vec::new();
        for ((symbol, dso), count) in entries.into_iter().take(30) {
            let count = number(&json!(count))?;
            result.push(json!({"function":symbol,"dso":dso,"sampled_cpu_ms":count/1e6,"percent":100.0*count/total_float}));
        }
        Ok(result.into())
    };
    let unresolved = self_time
        .iter()
        .filter(|((symbol, _), _)| symbol == "[unknown]")
        .map(|(_, count)| *count)
        .sum::<u64>();
    let mut result = json!({"samples":samples,"sampled_cpu_ns":total,"shallow_percent":100.0*f64_count(shallow)/f64_count(samples),"stacks_with_unknown_percent":100.0*f64_count(unknown)/f64_count(samples),"unresolved_self_percent":100.0*number(&json!(unresolved))?/total_float,"allocation_method":"CPU samples inside allocation/free functions, attributed to first non-allocator caller; not allocation counts or bytes"});
    for (name, counter, group) in [
        ("self_top30", &self_time, None),
        ("inclusive_top30", &inclusive, None),
        ("host_self_top30", &self_time, Some("host")),
        ("guest_self_top30", &self_time, Some("guest")),
        ("host_inclusive_top30", &inclusive, Some("host")),
        ("guest_inclusive_top30", &inclusive, Some("guest")),
        ("allocation_callers", &callers, None),
    ] {
        result[name] = rows(counter, group)?;
    }
    for (name, count) in split {
        result["split_percent"][name] = json!(100.0 * number(&json!(count))? / total_float);
    }
    write(output, &result)
}
