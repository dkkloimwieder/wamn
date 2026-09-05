//! wamn-0h0g.17.27: the throughput bench's Rust half.
//!
//! THE LOAD IS NOT GENERATED HERE. A stock generator runs in a pod, pinned by
//! digest in the disposable cluster -- `oha` for the two HTTP layers, `pgbench`
//! from the pinned `postgres` image for the direct-PostgreSQL layer -- and
//! `tools/receiving-cluster-journey-run --throughput` drives the sweep. Owner
//! ruling on the bead: a standard tool gives comparable numbers and no
//! generator code to own. This module owns the two things Rust does own: the
//! PostgreSQL counter sample taken before and after every step, and the report
//! that turns the generators' own output and those samples into one row per
//! step, then a knee and a peak per layer.
//!
//! THE KNEE IS THE RESULT. For each layer the concurrency sweep doubles (and
//! quadruples once) the client count; while the server has headroom,
//! throughput grows with it and p99 barely moves; once something saturates,
//! throughput flattens and p99 grows with the queue. The knee is the last step
//! that still scaled -- the first step that gains less than [`KNEE_MIN_GAIN`]
//! in throughput over the step before it names where p99 turned. No absolute
//! number is asserted; the report records knee and peak so a later run can be
//! compared to this one.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context as _;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The document the journey writes at the end of its sweep, relative to the
/// evidence directory it hands the report.
pub const INDEX_FILE: &str = "index.json";
/// Checked-in schema generated from [`ThroughputIndex`], relative to this
/// crate's manifest. Regenerate with `wamn-throughput schema`.
pub const SCHEMA_PATH: &str = "schema/wamn-throughput.schema.json";
/// The schema tag the journey stamps on the document it writes.
pub const INDEX_SCHEMA: &str = "wamn-throughput/v0.1";
/// A step whose throughput is less than this multiple of the previous step's is
/// where the layer stopped scaling.
pub const KNEE_MIN_GAIN: f64 = 1.2;
/// The marker the pgbench Job prints between its summary and its per-transaction
/// log lines, so one log stream carries both.
pub const PGBENCH_LOG_MARKER: &str = "===LOGS===";

/// One step per layer per concurrency, naming the files the step produced.
/// Strict on both sides: a key the shell invents fails here, naming it.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ThroughputIndex {
    pub schema: String,
    /// The git head the measured host was built from.
    pub source: String,
    /// Fixed duration of every step, in seconds.
    pub duration_seconds: u64,
    /// The concurrency sweep, in the order it ran.
    pub concurrency: Vec<u32>,
    pub layers: Vec<LayerSpec>,
    pub steps: Vec<StepSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayerSpec {
    pub layer: String,
    /// `oha` or `pgbench`.
    pub driver: String,
    /// What the generator was pointed at, for the report.
    pub target: String,
    /// The HTTP status every response is expected to carry; absent for pgbench.
    pub expected_status: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StepSpec {
    pub layer: String,
    pub concurrency: u32,
    /// The generator's own output: oha's JSON, or pgbench's summary followed by
    /// [`PGBENCH_LOG_MARKER`] and its sampled per-transaction log.
    pub result: String,
    /// [`Sample`] taken just before and just after the step.
    pub before: String,
    pub after: String,
    /// `cpu.stat` of the host pod's container, before and after.
    pub host_cpu_before: String,
    pub host_cpu_after: String,
    /// `cpu.stat` of the PostgreSQL container, before and after.
    pub pg_cpu_before: String,
    pub pg_cpu_after: String,
}

/// PostgreSQL's own view, read from the server rather than inferred.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sample {
    pub taken_at_unix_ms: u128,
    pub databases: Vec<DatabaseCounters>,
    pub activity: Vec<ActivityCount>,
    /// NATS `/varz`, when a monitoring endpoint was given and answered.
    pub nats: Option<serde_json::Value>,
    pub nats_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseCounters {
    pub datname: String,
    pub numbackends: i32,
    pub xact_commit: i64,
    pub xact_rollback: i64,
    pub blks_read: i64,
    pub blks_hit: i64,
    pub tup_returned: i64,
    pub tup_fetched: i64,
    pub sessions: i64,
    pub active_time_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityCount {
    pub datname: Option<String>,
    pub state: Option<String>,
    pub count: i64,
}

/// Read `pg_stat_database` for the named databases and the client-backend
/// population from `pg_stat_activity`; fetch NATS `/varz` if asked.
pub async fn sample(
    pg_url: &str,
    databases: &[String],
    nats_varz: Option<&str>,
) -> anyhow::Result<Sample> {
    let (client, connection) = tokio_postgres::connect(pg_url, tokio_postgres::NoTls)
        .await
        .context("connect to PostgreSQL for a counter sample")?;
    let connection = tokio::spawn(async move {
        if let Err(error) = connection.await {
            tracing::warn!(%error, "counter-sample connection ended with an error");
        }
    });
    let names = databases.to_vec();
    let rows = client
        .query(
            "SELECT datname, numbackends, xact_commit, xact_rollback, blks_read, blks_hit, \
             tup_returned, tup_fetched, sessions, active_time \
             FROM pg_stat_database WHERE datname = ANY($1) ORDER BY datname",
            &[&names],
        )
        .await
        .context("read pg_stat_database")?;
    let databases = rows
        .iter()
        .map(|row| DatabaseCounters {
            datname: row.get(0),
            numbackends: row.get(1),
            xact_commit: row.get(2),
            xact_rollback: row.get(3),
            blks_read: row.get(4),
            blks_hit: row.get(5),
            tup_returned: row.get(6),
            tup_fetched: row.get(7),
            sessions: row.get(8),
            active_time_ms: row.get(9),
        })
        .collect();
    let rows = client
        .query(
            "SELECT datname, state, count(*) FROM pg_stat_activity \
             WHERE backend_type = 'client backend' GROUP BY 1, 2 ORDER BY 1, 2",
            &[],
        )
        .await
        .context("read pg_stat_activity")?;
    let activity = rows
        .iter()
        .map(|row| ActivityCount {
            datname: row.get(0),
            state: row.get(1),
            count: row.get(2),
        })
        .collect();
    drop(client);
    let _ = connection.await;

    let (nats, nats_error) = match nats_varz {
        None => (None, None),
        Some(url) => match fetch_json(url).await {
            Ok(value) => (Some(value), None),
            Err(error) => (None, Some(format!("{error:#}"))),
        },
    };
    Ok(Sample {
        taken_at_unix_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis()),
        databases,
        activity,
        nats,
        nats_error,
    })
}

async fn fetch_json(url: &str) -> anyhow::Result<serde_json::Value> {
    let response = reqwest::get(url)
        .await
        .with_context(|| format!("GET {url}"))?;
    anyhow::ensure!(
        response.status().is_success(),
        "{url} answered {}",
        response.status()
    );
    response
        .json()
        .await
        .with_context(|| format!("decode {url} as JSON"))
}

/// What a generator reported for one step, in one shape for both drivers.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GeneratorResult {
    pub requests_per_second: f64,
    pub p50_ms: f64,
    pub p99_ms: f64,
    pub total_requests: u64,
    /// Transport errors plus responses carrying a status other than the
    /// expected one (HTTP), or failed transactions (pgbench).
    pub errors: u64,
    /// Requests still in flight when the step's fixed duration ended -- one per
    /// client in a closed loop. Not errors: the generator abandoned them.
    pub cut_off: u64,
    pub status_distribution: BTreeMap<String, u64>,
    pub duration_seconds: f64,
}

/// oha's reason for a request its deadline abandoned.
const OHA_CUT_OFF: &str = "aborted due to deadline";

/// oha `--output-format json`.
pub fn parse_oha(json: &str, expected_status: Option<u16>) -> anyhow::Result<GeneratorResult> {
    let value: serde_json::Value = serde_json::from_str(json).context("oha output is not JSON")?;
    let number = |path: &[&str]| -> anyhow::Result<f64> {
        let mut cursor = &value;
        for key in path {
            cursor = cursor
                .get(key)
                .with_context(|| format!("oha output has no {}", path.join(".")))?;
        }
        cursor
            .as_f64()
            .with_context(|| format!("oha {} is not a number", path.join(".")))
    };
    let counts = |key: &str| -> BTreeMap<String, u64> {
        value
            .get(key)
            .and_then(|v| v.as_object())
            .map(|object| {
                object
                    .iter()
                    .map(|(k, v)| (k.clone(), v.as_u64().unwrap_or(0)))
                    .collect()
            })
            .unwrap_or_default()
    };
    let statuses = counts("statusCodeDistribution");
    let mut transport = counts("errorDistribution");
    let cut_off = transport.remove(OHA_CUT_OFF).unwrap_or(0);
    let answered: u64 = statuses.values().sum();
    let failed: u64 = transport.values().sum();
    let unexpected: u64 = match expected_status {
        Some(expected) => statuses
            .iter()
            .filter(|(status, _)| status.as_str() != expected.to_string())
            .map(|(_, n)| *n)
            .sum(),
        None => 0,
    };
    let mut status_distribution = statuses;
    for (reason, n) in transport {
        status_distribution.insert(format!("error: {reason}"), n);
    }
    if cut_off > 0 {
        status_distribution.insert("cut off at the deadline".to_owned(), cut_off);
    }
    Ok(GeneratorResult {
        requests_per_second: number(&["summary", "requestsPerSec"])?,
        p50_ms: number(&["latencyPercentiles", "p50"])? * 1000.0,
        p99_ms: number(&["latencyPercentiles", "p99"])? * 1000.0,
        total_requests: answered + failed,
        errors: failed + unexpected,
        cut_off,
        status_distribution,
        duration_seconds: number(&["summary", "total"])?,
    })
}

/// pgbench's summary, then [`PGBENCH_LOG_MARKER`], then its `-l` log lines
/// (`client_id transaction_no time_us script_no time_epoch time_us ...`), which
/// carry the per-transaction latency the summary does not.
pub fn parse_pgbench(text: &str) -> anyhow::Result<GeneratorResult> {
    let (summary, logs) = text
        .split_once(PGBENCH_LOG_MARKER)
        .with_context(|| format!("pgbench output carries no {PGBENCH_LOG_MARKER} marker"))?;
    let field = |prefix: &str| -> anyhow::Result<&str> {
        summary
            .lines()
            .find_map(|line| line.trim().strip_prefix(prefix))
            .map(str::trim)
            .with_context(|| format!("pgbench summary has no line starting {prefix:?}"))
    };
    let first_number = |text: &str| -> anyhow::Result<f64> {
        text.split(|c: char| !(c.is_ascii_digit() || c == '.'))
            .find(|token| !token.is_empty())
            .and_then(|token| token.parse().ok())
            .with_context(|| format!("no number in {text:?}"))
    };
    let first_integer = |text: &str| -> anyhow::Result<u64> {
        text.split(|c: char| !c.is_ascii_digit())
            .find(|token| !token.is_empty())
            .and_then(|token| token.parse().ok())
            .with_context(|| format!("no integer in {text:?}"))
    };
    let tps = first_number(field("tps = ")?)?;
    let total = first_integer(field("number of transactions actually processed:")?)?;
    let failed = first_integer(field("number of failed transactions:")?)?;
    let duration = first_number(field("duration:")?)?;
    let mut latencies_us: Vec<u64> = logs
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            (fields.len() >= 6)
                .then(|| fields[2].parse::<u64>().ok())
                .flatten()
        })
        .collect();
    anyhow::ensure!(
        !latencies_us.is_empty(),
        "pgbench log carries no per-transaction lines after the marker"
    );
    latencies_us.sort_unstable();
    // Nearest rank: the ceil(p% of n)-th smallest sample.
    let percentile = |p: usize| -> f64 {
        let n = latencies_us.len();
        let rank = (p * n).div_ceil(100).max(1);
        micros_to_millis(latencies_us[rank.min(n) - 1])
    };
    let mut status_distribution = BTreeMap::new();
    status_distribution.insert("committed".to_owned(), total.saturating_sub(failed));
    if failed > 0 {
        status_distribution.insert("failed".to_owned(), failed);
    }
    status_distribution.insert("sampled_latencies".to_owned(), latencies_us.len() as u64);
    Ok(GeneratorResult {
        requests_per_second: tps,
        p50_ms: percentile(50),
        p99_ms: percentile(99),
        total_requests: total,
        errors: failed,
        cut_off: 0,
        status_distribution,
        duration_seconds: duration,
    })
}

/// One counter out of a cgroup v2 `cpu.stat`.
pub fn cpu_stat_counter(cpu_stat: &str, key: &str) -> Option<u64> {
    cpu_stat
        .lines()
        .find_map(|line| {
            line.strip_prefix(key)
                .and_then(|rest| rest.strip_prefix(' '))
        })
        .and_then(|value| value.trim().parse::<u64>().ok())
}

/// The share of CPU periods across a step in which the cgroup hit its quota;
/// `None` when the container carries no quota (no `nr_periods` line moved).
pub fn throttled_share(before: &str, after: &str) -> Option<f64> {
    let periods = cpu_stat_counter(after, "nr_periods")? - cpu_stat_counter(before, "nr_periods")?;
    let throttled =
        cpu_stat_counter(after, "nr_throttled")? - cpu_stat_counter(before, "nr_throttled")?;
    (periods > 0).then(|| ratio(throttled, periods))
}

#[expect(
    clippy::cast_precision_loss,
    reason = "cgroup period counts across a ten-second step sit far below 2^53"
)]
fn ratio(part: u64, whole: u64) -> f64 {
    part as f64 / whole as f64
}

#[expect(
    clippy::cast_precision_loss,
    reason = "latencies and CPU time in microseconds sit far below 2^53"
)]
fn micros_to_millis(micros: u64) -> f64 {
    micros as f64 / 1000.0
}

#[expect(
    clippy::cast_precision_loss,
    reason = "transaction counts across a ten-second step sit far below 2^53"
)]
fn per_second(count: i64, seconds: f64) -> f64 {
    count as f64 / seconds
}

#[expect(
    clippy::cast_precision_loss,
    reason = "request counts across a ten-second step sit far below 2^53"
)]
fn requests_as_f64(requests: u64) -> f64 {
    requests as f64
}

#[expect(
    clippy::cast_precision_loss,
    reason = "a sample window in milliseconds sits far below 2^53"
)]
fn millis_to_seconds(millis: u128) -> f64 {
    millis as f64 / 1000.0
}

/// `usage_usec` out of a cgroup v2 `cpu.stat`.
pub fn cpu_usage_seconds(cpu_stat: &str) -> anyhow::Result<f64> {
    cpu_stat
        .lines()
        .find_map(|line| line.strip_prefix("usage_usec "))
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|usec| micros_to_millis(usec) / 1000.0)
        .context("cpu.stat carries no usage_usec line")
}

/// The server's side of one step: counter deltas across it, and CPU.
#[derive(Debug, Clone, Serialize)]
pub struct ServerDelta {
    pub xact_commit: i64,
    pub xact_rollback: i64,
    pub sessions: i64,
    /// Commits per second over the sample window, from the server's counters.
    pub server_tps: f64,
    /// `numbackends` per database at the end of the step.
    pub numbackends_after: BTreeMap<String, i32>,
    /// Client backends by state at the end of the step, `datname/state`.
    pub activity_after: BTreeMap<String, i64>,
    /// Wall seconds between the before and after samples: the step's fixed
    /// duration plus the Job's scheduling and image pull around it.
    pub sample_window_seconds: f64,
    /// Average cores the host pod and the PostgreSQL container burned across
    /// the sample window.
    pub host_cpu_cores: f64,
    pub pg_cpu_cores: f64,
    /// Host CPU per request: the pod's CPU seconds across the window over
    /// every request the generator sent.
    pub host_cpu_ms_per_request: f64,
    /// Share of the host pod's CPU periods throttled at its quota across the
    /// window; `None` when it carries no quota.
    pub host_throttled_share: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StepResult {
    pub layer: String,
    pub driver: String,
    pub concurrency: u32,
    #[serde(flatten)]
    pub generator: GeneratorResult,
    pub server: ServerDelta,
}

/// Where a layer stopped scaling.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Knee {
    /// The last concurrency that still scaled.
    pub concurrency: u32,
    /// The step whose throughput gain fell under [`KNEE_MIN_GAIN`]: where p99 turned.
    pub turns_at: u32,
    /// Throughput at `turns_at` over throughput at the knee.
    pub gain: f64,
    /// Throughput gain per unit of concurrency gain at `turns_at`, for the record.
    pub efficiency: f64,
    pub p99_ms_before: f64,
    pub p99_ms_at: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Peak {
    pub concurrency: u32,
    pub requests_per_second: f64,
    pub p99_ms: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LayerVerdict {
    pub layer: String,
    pub knee: Option<Knee>,
    pub peak: Peak,
}

/// Throughput gain step over step; the first step that gains less than
/// [`KNEE_MIN_GAIN`] is where the layer turned, and the step before it is the
/// knee. Measured on the first sweep: the sub-linear 1→4 step of a layer still
/// doubling its throughput is not a knee, and the step where throughput goes
/// flat or falls is exactly the step where p99 jumps.
pub fn knee(steps: &[(u32, f64, f64)]) -> Option<Knee> {
    steps.windows(2).find_map(|pair| {
        let (c0, rps0, p99_0) = pair[0];
        let (c1, rps1, p99_1) = pair[1];
        if c1 <= c0 || rps0 <= 0.0 {
            return None;
        }
        let gain = rps1 / rps0;
        let efficiency = (gain - 1.0) / (f64::from(c1) / f64::from(c0) - 1.0);
        (gain < KNEE_MIN_GAIN).then_some(Knee {
            concurrency: c0,
            turns_at: c1,
            gain,
            efficiency,
            p99_ms_before: p99_0,
            p99_ms_at: p99_1,
        })
    })
}

pub fn peak(steps: &[(u32, f64, f64)]) -> Option<Peak> {
    steps.iter().max_by(|a, b| a.1.total_cmp(&b.1)).map(
        |&(concurrency, requests_per_second, p99_ms)| Peak {
            concurrency,
            requests_per_second,
            p99_ms,
        },
    )
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub index: ThroughputIndex,
    pub results: Vec<StepResult>,
    pub verdicts: Vec<LayerVerdict>,
}

/// Read the index and every file it names, and reduce them.
pub fn build_report(evidence_dir: &Path) -> anyhow::Result<Report> {
    let read = |name: &str| -> anyhow::Result<String> {
        std::fs::read_to_string(evidence_dir.join(name))
            .with_context(|| format!("read {}", evidence_dir.join(name).display()))
    };
    let index: ThroughputIndex =
        serde_json::from_str(&read(INDEX_FILE)?).context("parse the throughput index")?;
    anyhow::ensure!(
        index.schema == INDEX_SCHEMA,
        "index schema is {:?}, expected {INDEX_SCHEMA:?}",
        index.schema
    );
    let mut results = Vec::with_capacity(index.steps.len());
    for step in &index.steps {
        let layer = index
            .layers
            .iter()
            .find(|l| l.layer == step.layer)
            .with_context(|| {
                format!(
                    "step names layer {:?} the index does not declare",
                    step.layer
                )
            })?;
        let generator = match layer.driver.as_str() {
            "oha" => parse_oha(&read(&step.result)?, layer.expected_status),
            "pgbench" => parse_pgbench(&read(&step.result)?),
            other => anyhow::bail!("layer {} has unknown driver {other:?}", layer.layer),
        }
        .with_context(|| format!("{} c={}", step.layer, step.concurrency))?;
        let before: Sample = serde_json::from_str(&read(&step.before)?)?;
        let after: Sample = serde_json::from_str(&read(&step.after)?)?;
        let window = millis_to_seconds(
            after
                .taken_at_unix_ms
                .saturating_sub(before.taken_at_unix_ms),
        )
        .max(f64::EPSILON);
        let delta = |pick: fn(&DatabaseCounters) -> i64| -> i64 {
            after.databases.iter().map(pick).sum::<i64>()
                - before.databases.iter().map(pick).sum::<i64>()
        };
        let xact_commit = delta(|d| d.xact_commit);
        let host_cpu_before = read(&step.host_cpu_before)?;
        let host_cpu_after = read(&step.host_cpu_after)?;
        let host_cpu_seconds =
            cpu_usage_seconds(&host_cpu_after)? - cpu_usage_seconds(&host_cpu_before)?;
        let server = ServerDelta {
            xact_commit,
            xact_rollback: delta(|d| d.xact_rollback),
            sessions: delta(|d| d.sessions),
            server_tps: per_second(xact_commit, window),
            numbackends_after: after
                .databases
                .iter()
                .map(|d| (d.datname.clone(), d.numbackends))
                .collect(),
            activity_after: after
                .activity
                .iter()
                .map(|a| {
                    (
                        format!(
                            "{}/{}",
                            a.datname.as_deref().unwrap_or("-"),
                            a.state.as_deref().unwrap_or("-")
                        ),
                        a.count,
                    )
                })
                .collect(),
            sample_window_seconds: window,
            host_cpu_cores: host_cpu_seconds / window,
            pg_cpu_cores: (cpu_usage_seconds(&read(&step.pg_cpu_after)?)?
                - cpu_usage_seconds(&read(&step.pg_cpu_before)?)?)
                / window,
            host_cpu_ms_per_request: if generator.total_requests > 0 {
                host_cpu_seconds * 1000.0 / requests_as_f64(generator.total_requests)
            } else {
                0.0
            },
            host_throttled_share: throttled_share(&host_cpu_before, &host_cpu_after),
        };
        results.push(StepResult {
            layer: step.layer.clone(),
            driver: layer.driver.clone(),
            concurrency: step.concurrency,
            generator,
            server,
        });
    }
    let mut verdicts = Vec::new();
    for layer in &index.layers {
        let series: Vec<(u32, f64, f64)> = results
            .iter()
            .filter(|r| r.layer == layer.layer)
            .map(|r| {
                (
                    r.concurrency,
                    r.generator.requests_per_second,
                    r.generator.p99_ms,
                )
            })
            .collect();
        let Some(peak) = peak(&series) else {
            continue;
        };
        verdicts.push(LayerVerdict {
            layer: layer.layer.clone(),
            knee: knee(&series),
            peak,
        });
    }
    Ok(Report {
        index,
        results,
        verdicts,
    })
}

impl Report {
    /// The report's tables, in the `docs/perf/2026.09` format.
    pub fn render_markdown(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(out, "## Knee and peak per layer\n");
        let _ = writeln!(
            out,
            "| layer | knee (last step that scaled) | p99 turns at | throughput gain there | peak req/s | at c | p99 at peak |"
        );
        let _ = writeln!(out, "|---|---:|---:|---:|---:|---:|---:|");
        for v in &self.verdicts {
            match &v.knee {
                Some(k) => {
                    let _ = writeln!(
                        out,
                        "| `{}` | **{}** | {} ({:.2} → {:.2} ms) | ×{:.2} | {:.0} | {} | {:.2} ms |",
                        v.layer,
                        k.concurrency,
                        k.turns_at,
                        k.p99_ms_before,
                        k.p99_ms_at,
                        k.gain,
                        v.peak.requests_per_second,
                        v.peak.concurrency,
                        v.peak.p99_ms
                    );
                }
                None => {
                    let _ = writeln!(
                        out,
                        "| `{}` | none in the sweep | — | — | {:.0} | {} | {:.2} ms |",
                        v.layer, v.peak.requests_per_second, v.peak.concurrency, v.peak.p99_ms
                    );
                }
            }
        }
        for layer in &self.index.layers {
            let _ = writeln!(
                out,
                "\n## `{}` — {} against `{}`\n",
                layer.layer, layer.driver, layer.target
            );
            let _ = writeln!(
                out,
                "| c | req/s | p50 ms | p99 ms | errors | cut off | requests | server commits/s | backends | host cores | host CPU ms/req | host throttled | pg cores |"
            );
            let _ = writeln!(
                out,
                "|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"
            );
            for r in self.results.iter().filter(|r| r.layer == layer.layer) {
                let backends: Vec<String> = r
                    .server
                    .numbackends_after
                    .iter()
                    .map(|(db, n)| format!("{}={n}", short_db(db)))
                    .collect();
                let throttled = r
                    .server
                    .host_throttled_share
                    .map_or("no quota".to_owned(), |share| {
                        format!("{:.0} %", share * 100.0)
                    });
                let _ = writeln!(
                    out,
                    "| {} | {:.0} | {:.2} | {:.2} | {} | {} | {} | {:.0} | {} | {:.2} | {:.1} | {} | {:.2} |",
                    r.concurrency,
                    r.generator.requests_per_second,
                    r.generator.p50_ms,
                    r.generator.p99_ms,
                    r.generator.errors,
                    r.generator.cut_off,
                    r.generator.total_requests,
                    r.server.server_tps,
                    backends.join(" "),
                    r.server.host_cpu_cores,
                    r.server.host_cpu_ms_per_request,
                    throttled,
                    r.server.pg_cpu_cores,
                );
            }
        }
        out
    }
}

fn short_db(name: &str) -> &str {
    if name.starts_with("wamn-db-") {
        "project"
    } else {
        name
    }
}

/// Byte-stable pretty JSON Schema generated from the strict index type.
pub fn index_schema_bytes() -> Vec<u8> {
    let schema = serde_json::to_value(schemars::schema_for!(ThroughputIndex))
        .expect("throughput index schema serializes");
    let mut bytes = serde_json::to_vec_pretty(&schema).expect("throughput index schema serializes");
    bytes.push(b'\n');
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    const OHA: &str = r#"{"summary":{"successRate":1.0,"total":2.001366311,"slowest":0.005535099,"fastest":0.000266366,"average":0.0009765679621509106,"requestsPerSec":4081.211897645458,"totalData":3935048,"sizePerRequest":482,"sizePerSec":1966180.7927774198},"latencyPercentiles":{"p10":0.000717547,"p25":0.000818105,"p50":0.000931111,"p75":0.001080905,"p90":0.001284732,"p95":0.001438893,"p99":0.001877135,"p99.9":0.002746559,"p99.99":0.005535099},"statusCodeDistribution":{"501":8164},"errorDistribution":{"aborted due to deadline":4}}"#;

    const PGBENCH: &str = "pgbench (18.6 (Debian 18.6-1.pgdg13+2))\nprogress: 1.0 s, 32452.0 tps, lat 0.059 ms stddev 0.031, 0 failed\ntransaction type: /tmp/q.sql\nscaling factor: 1\nquery mode: prepared\nnumber of clients: 2\nnumber of threads: 2\nmaximum number of tries: 1\nduration: 2 s\nnumber of transactions actually processed: 69457\nnumber of failed transactions: 0 (0.000%)\nlatency average = 0.056 ms\nlatency stddev = 0.033 ms\ninitial connection time = 8.770 ms\ntps = 34879.230595 (without initial connection time)\n===LOGS===\n0 1 471 0 1788644700 907135\n0 2 103 0 1788644700 907251\n0 3 73 0 1788644700 907327\n1 1 90 0 1788644700 907400\n";

    #[test]
    fn oha_json_yields_rate_percentiles_and_errors_against_the_expected_status() {
        let got = parse_oha(OHA, Some(200)).expect("parse");
        assert!((got.requests_per_second - 4081.211897645458).abs() < 1e-9);
        assert!((got.p50_ms - 0.931111).abs() < 1e-9);
        assert!((got.p99_ms - 1.877135).abs() < 1e-9);
        // 8164 requests were answered; the four the deadline cut off are the
        // four clients' last requests, neither answered nor errors.
        assert_eq!(got.total_requests, 8164);
        // 8164 answers carried 501, not 200.
        assert_eq!(got.errors, 8164);
        assert_eq!(got.cut_off, 4);
        assert_eq!(got.status_distribution["501"], 8164);
        assert_eq!(got.status_distribution["cut off at the deadline"], 4);
        assert!(
            !got.status_distribution
                .contains_key("error: aborted due to deadline")
        );
        // The same answers against the status they actually carried: nothing
        // is an error.
        let expected = parse_oha(OHA, Some(501)).expect("parse");
        assert_eq!((expected.errors, expected.cut_off), (0, 4));
    }

    #[test]
    fn pgbench_summary_and_log_yield_tps_and_sampled_percentiles() {
        let got = parse_pgbench(PGBENCH).expect("parse");
        assert!((got.requests_per_second - 34879.230595).abs() < 1e-9);
        assert_eq!(got.total_requests, 69457);
        assert_eq!(got.errors, 0);
        assert!((got.duration_seconds - 2.0).abs() < 1e-9);
        // Latencies 73, 90, 103, 471 us: nearest-rank p50 is the 2nd, p99 the 4th.
        assert!((got.p50_ms - 0.090).abs() < 1e-9, "p50 {}", got.p50_ms);
        assert!((got.p99_ms - 0.471).abs() < 1e-9, "p99 {}", got.p99_ms);
        assert_eq!(got.status_distribution["sampled_latencies"], 4);
    }

    #[test]
    fn pgbench_output_without_the_marker_is_refused() {
        let error = parse_pgbench("tps = 1 (x)\n").expect_err("no marker");
        assert!(error.to_string().contains(PGBENCH_LOG_MARKER));
    }

    #[test]
    fn cpu_stat_usage_reads_in_seconds() {
        assert!(
            (cpu_usage_seconds("usage_usec 20558\nuser_usec 14390\n").unwrap() - 0.020558).abs()
                < 1e-12
        );
        assert!(cpu_usage_seconds("user_usec 1\n").is_err());
    }

    #[test]
    fn the_knee_is_the_last_step_that_still_scaled() {
        // 1→4 quadruples rate (efficiency 1.0), 4→8 adds 60 % (0.6), 8→16 adds 10 % (0.1).
        let series = [
            (1, 100.0, 1.0),
            (4, 400.0, 1.1),
            (8, 640.0, 1.4),
            (16, 704.0, 4.0),
            (32, 700.0, 9.0),
        ];
        let k = knee(&series).expect("a knee");
        assert_eq!((k.concurrency, k.turns_at), (8, 16));
        assert!((k.gain - 1.1).abs() < 1e-9);
        assert!((k.efficiency - 0.1).abs() < 1e-9);
        assert_eq!((k.p99_ms_before, k.p99_ms_at), (1.4, 4.0));
        assert_eq!(peak(&series).unwrap().concurrency, 16);
        // A layer that scales through the whole sweep has no knee in it.
        assert_eq!(
            knee(&[(1, 10.0, 1.0), (4, 40.0, 1.0), (8, 80.0, 1.0)]),
            None
        );
        // The first sweep's shape: 1→4 gains ×2.1 on ×4 the clients (efficiency
        // 0.37, sub-linear) and is still scaling; the knee is where throughput
        // goes flat, which is where p99 jumps.
        let measured = [
            (1, 68.0, 20.8),
            (4, 145.0, 44.0),
            (8, 131.0, 94.2),
            (16, 152.0, 148.9),
        ];
        let k = knee(&measured).expect("a knee");
        assert_eq!((k.concurrency, k.turns_at), (4, 8));
        assert!(k.gain < 1.0);
    }

    #[test]
    fn the_throttled_share_is_read_across_the_step_and_absent_without_a_quota() {
        let before = "usage_usec 1\nnr_periods 100\nnr_throttled 10\nthrottled_usec 5\n";
        let after = "usage_usec 2\nnr_periods 238\nnr_throttled 80\nthrottled_usec 9\n";
        let share = throttled_share(before, after).expect("a quota");
        assert!((share - 70.0 / 138.0).abs() < 1e-12);
        assert_eq!(throttled_share("usage_usec 1\n", "usage_usec 2\n"), None);
        assert_eq!(cpu_stat_counter(after, "nr_throttled"), Some(80));
        // A key that is a prefix of another must not match it.
        assert_eq!(
            cpu_stat_counter("nr_throttled_x 5\nnr_throttled 6\n", "nr_throttled"),
            Some(6)
        );
    }

    #[test]
    fn checked_in_throughput_schema_matches_generated_bytes() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(SCHEMA_PATH);
        let checked_in = std::fs::read(&path).expect("read the checked-in throughput schema");
        assert_eq!(checked_in, index_schema_bytes());
    }

    #[test]
    fn the_index_refuses_a_key_it_does_not_know() {
        let error = serde_json::from_str::<ThroughputIndex>(
            r#"{"schema":"wamn-throughput/v0.1","source":"x","duration_seconds":1,"concurrency":[1],"layers":[],"steps":[],"extra":1}"#,
        )
        .expect_err("unknown key");
        assert!(error.to_string().contains("extra"));
    }
}
