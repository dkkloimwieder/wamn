//! Timed Receiving calls, mixed traffic, and shared-order contention.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, bail, ensure};
use clap::{Parser, ValueEnum};
use serde::Serialize;
use serde_json::{Map, Value, json};
use wamn_execution_contract::encode_read_query;

/// The most rows one query returns (`limit.maximum` in the Receiving manifest).
const PAGE: i64 = 100;

/// The most receipts one worker can make per second, which sizes the seed.
const RECEIPTS_PER_WORKER_SECOND: u64 = 50;

/// The item, location and supplier of the seeded orders.
const SEED_ITEM: &str = "00000000-0000-0000-0000-00000000b710";
const SEED_LOCATION: &str = "00000000-0000-0000-0000-00000000b711";
const SEED_SUPPLIER: &str = "00000000-0000-0000-0000-00000000b730";
/// The audit principal of the seeded rows.
const SEED_PRINCIPAL: &str = "00000000-0000-0000-0000-000000000000";

#[derive(Debug, Parser)]
struct Args {
    /// The host name of the application, such as `receiving.wamn.dev`.
    #[arg(long)]
    host: String,
    /// Override the API base URL and send `--host` as the Host header.
    #[arg(long)]
    base_url: Option<String>,
    /// The PAT Secret JSON that `provision-project-env` wrote, with the token
    /// at `.stringData.token`.
    #[arg(long)]
    pat_file: PathBuf,
    #[arg(long, value_enum)]
    call: Call,
    /// Share four orders between all update workers.
    #[arg(long)]
    contention: bool,
    /// Interleave 70% reads, 20% updates, and 10% creates and commands.
    #[arg(long)]
    mix: bool,
    #[arg(long)]
    concurrency: usize,
    #[arg(long)]
    duration_secs: u64,
    /// The tier name, written into the result.
    #[arg(long)]
    tier: String,
    /// The result file.
    #[arg(long)]
    output: PathBuf,
    /// The URL of the project database: Postgres samples before and after
    /// the run, and the seed of `acme_record_receipt`.
    #[arg(long)]
    db_url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
enum Call {
    Get,
    Update,
    Query,
    ReceiptQuery,
    Create,
    #[value(name = "record_receipt")]
    #[serde(rename = "record_receipt")]
    RecordReceipt,
    /// Read one warm receipt through the real host.
    #[serde(rename = "app-path")]
    AppPath,
    #[value(name = "acme_record_receipt")]
    #[serde(rename = "acme_record_receipt")]
    AcmeRecordReceipt,
}

/// One seeded purchase order and its one line, for one receipt.
#[derive(Debug, Clone)]
struct Receipt {
    purchase_order_id: String,
    line_id: String,
}

/// One purchase order as a worker tracks it.
#[derive(Debug, Clone)]
struct Order {
    id: String,
    row_version: i64,
    supplier: usize,
}

/// The shared parts of every call.
#[derive(Debug)]
struct Target {
    client: reqwest::Client,
    base: String,
    authorization: String,
}

/// What one worker measured.
#[derive(Debug, Default)]
struct Measured {
    latencies_micros: Vec<u64>,
    slow: Vec<SlowRequest>,
    errors: u64,
    conflicts: u64,
    /// The errors counted by kind: the refusal code or the status.
    error_kinds: BTreeMap<String, u64>,
}

#[derive(Debug, Serialize)]
struct SlowRequest {
    start_ms: f64,
    latency_ms: f64,
}

impl Measured {
    fn record(&mut self, started: Instant, begun: Instant) {
        let latency = begun.elapsed();
        let micros = u64::try_from(latency.as_micros()).unwrap_or(u64::MAX);
        self.latencies_micros.push(micros);
        if latency > Duration::from_secs(1) && self.slow.len() < 100 {
            self.slow.push(SlowRequest {
                start_ms: begun.duration_since(started).as_secs_f64() * 1000.0,
                latency_ms: millis(micros),
            });
        }
    }

    /// Count one error under its kind.
    fn error(&mut self, error: &anyhow::Error) {
        if error.to_string().contains("concurrency_conflict") {
            self.conflicts += 1;
        } else {
            self.errors += 1;
        }
        *self.error_kinds.entry(error.to_string()).or_default() += 1;
    }
}

#[derive(Debug, Serialize)]
struct Summary {
    host: String,
    call: Call,
    tier: String,
    concurrency: usize,
    duration_secs: f64,
    started_at_unix: u64,
    started_at_unix_nanos: u128,
    ended_at_unix_nanos: u128,
    orders: usize,
    calls: usize,
    errors: u64,
    conflicts: u64,
    contention_orders: usize,
    mix: bool,
    per_call: BTreeMap<Call, CallSummary>,
    statements_per_request: Option<f64>,
    error_rate: f64,
    error_kinds: BTreeMap<String, u64>,
    throughput_per_sec: f64,
    p50_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
    max_ms: f64,
    slow: Vec<SlowRequest>,
    #[serde(skip_serializing_if = "Option::is_none")]
    postgres: Option<Samples>,
}

#[derive(Debug, Serialize)]
struct CallSummary {
    calls: usize,
    throughput_per_sec: f64,
    p50_ms: f64,
    p99_ms: f64,
    max_ms: f64,
    errors: u64,
    conflicts: u64,
    statements_per_request: Option<f64>,
}

fn summarize(measured: &mut Measured, elapsed: f64, statements: Option<f64>) -> CallSummary {
    measured.latencies_micros.sort_unstable();
    CallSummary {
        calls: measured.latencies_micros.len(),
        throughput_per_sec: to_f64(measured.latencies_micros.len()) / elapsed,
        p50_ms: millis(percentile(&measured.latencies_micros, 50)),
        p99_ms: millis(percentile(&measured.latencies_micros, 99)),
        max_ms: millis(
            measured
                .latencies_micros
                .last()
                .copied()
                .unwrap_or_default(),
        ),
        errors: measured.errors,
        conflicts: measured.conflicts,
        statements_per_request: statements,
    }
}

/// The Postgres samples of one run.
#[derive(Debug, Serialize)]
struct Samples {
    before: Sample,
    after: Sample,
}

/// One sample of the project database.
#[derive(Debug, Serialize)]
struct Sample {
    database: DatabaseStats,
    /// The ten statements with the most total time, or `None` when
    /// `pg_stat_statements` cannot be read.
    statements: Option<Vec<StatementStats>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    statements_error: Option<String>,
}

/// The row of `pg_stat_database` of the connected database.
#[derive(Debug, Serialize)]
struct DatabaseStats {
    xact_commit: i64,
    blks_hit: i64,
    blks_read: i64,
    numbackends: i32,
}

/// One row of `pg_stat_statements`.
#[derive(Debug, Serialize)]
struct StatementStats {
    query: String,
    calls: i64,
    total_exec_time_ms: f64,
    mean_exec_time_ms: f64,
    rows: i64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    ensure!(args.concurrency > 0, "--concurrency must be at least 1");
    ensure!(args.duration_secs > 0, "--duration-secs must be at least 1");
    let secret: Value = serde_json::from_slice(
        &std::fs::read(&args.pat_file)
            .with_context(|| format!("read {}", args.pat_file.display()))?,
    )
    .context("the PAT file is JSON")?;
    let token = secret
        .pointer("/stringData/token")
        .and_then(Value::as_str)
        .context("the PAT file holds .stringData.token")?;
    let mut headers = reqwest::header::HeaderMap::new();
    if args.base_url.is_some() {
        headers.insert(reqwest::header::HOST, args.host.parse()?);
    }
    let target = Arc::new(Target {
        client: reqwest::Client::builder()
            .default_headers(headers)
            .pool_max_idle_per_host(args.concurrency)
            .timeout(Duration::from_secs(30))
            .build()?,
        base: args
            .base_url
            .unwrap_or_else(|| format!("https://{}/api", args.host)),
        authorization: format!("Bearer {token}"),
    });

    let database = match &args.db_url {
        Some(url) => Some(connect(url).await?),
        None => None,
    };
    let run_time = SystemTime::now().duration_since(UNIX_EPOCH)?;
    let started_at_unix = run_time.as_secs();
    let run = run_time.as_nanos();

    let (mut orders, suppliers) = list_orders(&target).await?;
    ensure!(
        !(args.mix || args.call == Call::Update) || suppliers.len() > 1,
        "a supplier change needs two suppliers"
    );
    if args.contention {
        ensure!(orders.len() >= 4, "contention needs four orders");
        orders.truncate(4);
    }
    let receipt_id = if args.mix || matches!(args.call, Call::AppPath | Call::ReceiptQuery) {
        seed_app_path(database.as_ref().context("receipt reads need --db-url")?).await?
    } else {
        String::new()
    };
    let receipts = if args.mix || matches!(args.call, Call::RecordReceipt | Call::AcmeRecordReceipt)
    {
        let count =
            u64::try_from(args.concurrency)? * args.duration_secs * RECEIPTS_PER_WORKER_SECOND;
        seed_receipts(
            database.as_ref().context("receipt seed needs --db-url")?,
            &format!("BENCH-{started_at_unix}-"),
            count,
            args.call == Call::AcmeRecordReceipt,
        )
        .await?
    } else {
        Vec::new()
    };
    let units = orders.len();
    ensure!(
        args.contention || (!args.mix && args.call == Call::AppPath) || units >= args.concurrency,
        "not enough orders for workers"
    );
    let slice = units / args.concurrency;
    let receipt_slice = receipts.len() / args.concurrency;
    let make_worker = |worker: usize, orders: &[Order]| Workload {
        target: Arc::clone(&target),
        run,
        worker,
        own: if args.contention || (!args.mix && args.call == Call::AppPath) {
            orders.to_vec()
        } else {
            orders[worker * slice..(worker + 1) * slice].to_vec()
        },
        suppliers: suppliers.clone(),
        receipts: receipts[worker * receipt_slice..(worker + 1) * receipt_slice].to_vec(),
        receipt_id: receipt_id.clone(),
        sequence: 0,
        receipt_next: 0,
        cursors: BTreeMap::new(),
    };
    // Isolated warm calls establish statement counts without attributing shared
    // SQL in a concurrent mix to an invented route. Raw deltas remain in postgres.
    let kinds = if args.mix {
        MIX_CALLS.to_vec()
    } else if args.contention {
        vec![Call::Get, args.call]
    } else {
        vec![args.call]
    };
    let mut statement_counts = BTreeMap::new();
    let mut conflict_statements = None;
    let mut warm = make_worker(0, &orders);
    warm.run = run.saturating_sub(1);
    for call in kinds {
        warm.invoke(call).await?;
        if let Some(database) = &database {
            let before = statement_calls(database).await?;
            for _ in 0..5 {
                warm.invoke(call).await?;
            }
            let after = statement_calls(database).await?;
            statement_counts.insert(
                call,
                to_f64(usize::try_from(after - before).context("statement count increased")?) / 5.0,
            );
        }
    }
    if args.contention
        && let Some(database) = &database
    {
        // Measure the refused update path too: it rolls back instead of
        // finishing the claim and committing. Recovery reads are separate calls.
        let index = warm.sequence % warm.own.len();
        let version = warm.own[index].row_version;
        warm.own[index].row_version = version + 1;
        let before = statement_calls(database).await?;
        let refusal = warm
            .invoke(Call::Update)
            .await
            .expect_err("a future version conflicts");
        ensure!(
            refusal.to_string().contains("concurrency_conflict"),
            "unexpected calibration refusal: {refusal}"
        );
        conflict_statements = Some(to_f64(usize::try_from(
            statement_calls(database).await? - before,
        )?));
        warm.own[index].row_version = version;
    }
    // Carry the calibration worker's updated versions into the timed run.
    for (order, warmed) in orders.iter_mut().zip(&warm.own) {
        *order = warmed.clone();
    }
    let before = match &database {
        Some(database) => Some(sample(database).await?),
        None => None,
    };
    let started_at_unix_nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    eprintln!("benchmark-start {started_at_unix_nanos}");
    let started = Instant::now();
    let deadline = started + Duration::from_secs(args.duration_secs);
    let mut workers = Vec::with_capacity(args.concurrency);
    for worker in 0..args.concurrency {
        let mut workload = make_worker(worker, &orders);
        // Worker zero's calibration consumed receipts before the timed run.
        if worker == 0 {
            workload.receipt_next = warm.receipt_next;
        }
        workers.push(tokio::spawn(run_worker(
            workload,
            args.call,
            args.mix,
            started..deadline,
        )));
    }
    let mut combined: BTreeMap<Call, Measured> = BTreeMap::new();
    for worker in workers {
        for (call, measured) in worker.await?? {
            let total = combined.entry(call).or_default();
            total.latencies_micros.extend(measured.latencies_micros);
            total.slow.extend(measured.slow);
            total.errors += measured.errors;
            total.conflicts += measured.conflicts;
            for (kind, count) in measured.error_kinds {
                *total.error_kinds.entry(kind).or_default() += count;
            }
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    let mut latencies = Vec::new();
    let mut slow = Vec::new();
    let mut errors = 0;
    let mut conflicts = 0;
    let mut error_kinds = BTreeMap::new();
    let mut per_call = BTreeMap::new();
    let mut statements = 0.0;
    for (call, mut measured) in combined {
        let count = statement_counts.get(&call).map(|success| {
            let conflict = if call == Call::Update {
                conflict_statements.unwrap_or(*success)
            } else {
                *success
            };
            success
                + ratio(measured.conflicts, measured.latencies_micros.len()) * (conflict - success)
        });
        let summary = summarize(&mut measured, elapsed, count);
        statements += summary.statements_per_request.unwrap_or_default() * to_f64(summary.calls);
        per_call.insert(call, summary);
        latencies.extend(measured.latencies_micros);
        slow.extend(measured.slow);
        errors += measured.errors;
        conflicts += measured.conflicts;
        for (kind, count) in measured.error_kinds {
            *error_kinds.entry(kind).or_default() += count;
        }
    }
    let ended_at_unix_nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let postgres = match (&database, before) {
        (Some(database), Some(before)) => Some(Samples {
            before,
            after: sample(database).await?,
        }),
        _ => None,
    };
    latencies.sort_unstable();
    slow.sort_unstable_by(|left, right| left.start_ms.total_cmp(&right.start_ms));
    slow.truncate(100);
    let calls = latencies.len();
    let result = Summary {
        host: args.host,
        call: args.call,
        tier: args.tier,
        concurrency: args.concurrency,
        duration_secs: elapsed,
        started_at_unix,
        started_at_unix_nanos,
        ended_at_unix_nanos,
        orders: units,
        calls,
        errors,
        conflicts,
        contention_orders: if args.contention { 4 } else { 0 },
        mix: args.mix,
        per_call,
        statements_per_request: database.as_ref().map(|_| statements / to_f64(calls)),
        error_rate: ratio(errors, calls),
        error_kinds,
        throughput_per_sec: to_f64(calls) / elapsed,
        p50_ms: millis(percentile(&latencies, 50)),
        p95_ms: millis(percentile(&latencies, 95)),
        p99_ms: millis(percentile(&latencies, 99)),
        max_ms: millis(latencies.last().copied().unwrap_or_default()),
        slow,
        postgres,
    };
    let text = serde_json::to_string_pretty(&result)?;
    tokio::fs::write(&args.output, format!("{text}\n"))
        .await
        .with_context(|| format!("write {}", args.output.display()))?;
    println!("{text}");
    Ok(())
}

/// Seed enough receipts for multiple list pages, and return one get target.
async fn seed_app_path(database: &tokio_postgres::Client) -> anyhow::Result<String> {
    let row = database
        .query_one(
            "WITH claims AS MATERIALIZED (SELECT set_config('app.user_id', $1, true), \
             set_config('app.operation', 'admin:benchmark-app-path', true)), inserted AS ( \
             INSERT INTO receiving.receipt (purchase_order_id, receipt_reference, occurred_at) \
             SELECT purchase_order.id, gen_random_uuid()::text, now() \
             FROM receiving.purchase_order CROSS JOIN claims ORDER BY purchase_order.id LIMIT 100 \
             RETURNING id::text) SELECT id FROM inserted LIMIT 1",
            &[&SEED_PRINCIPAL],
        )
        .await
        .context("seed the application-path receipt")?;
    Ok(row.get(0))
}

const MIX_CALLS: [Call; 7] = [
    Call::Get,
    Call::AppPath,
    Call::Query,
    Call::ReceiptQuery,
    Call::Update,
    Call::Create,
    Call::RecordReceipt,
];

// Twenty requests give exactly 70/20/10 weights. Offset each worker to avoid
// synchronizing writes. Each query follows its returned cursor until the end.
fn mix_call(sequence: usize, worker: usize) -> Call {
    match (sequence + worker * 7) % 20 {
        0 | 4 | 8 | 12 => Call::Get,
        1 | 5 | 9 | 13 => Call::AppPath,
        2 | 6 | 10 => Call::Query,
        3 | 7 | 11 => Call::ReceiptQuery,
        14..=17 => Call::Update,
        18 => Call::Create,
        _ => Call::RecordReceipt,
    }
}

struct Workload {
    target: Arc<Target>,
    run: u128,
    worker: usize,
    own: Vec<Order>,
    suppliers: Vec<String>,
    receipts: Vec<Receipt>,
    receipt_id: String,
    sequence: usize,
    receipt_next: usize,
    cursors: BTreeMap<Call, String>,
}

impl Workload {
    async fn invoke(&mut self, call: Call) -> anyhow::Result<Value> {
        let index = self.sequence % self.own.len();
        self.sequence += 1;
        let key = format!("bench-{}-{}-{}", self.run, self.worker, self.sequence);
        match call {
            Call::Get => {
                read(
                    &self.target,
                    "purchase_order/get",
                    &json!({"id": self.own[index].id}),
                )
                .await
            }
            Call::AppPath => {
                read(&self.target, "receipt/get", &json!({"id": self.receipt_id})).await
            }
            Call::Query | Call::ReceiptQuery => {
                let mut item = json!({"limit": 20});
                if call == Call::Query {
                    item["filter"] = json!({"status": ["open"]});
                }
                if let Some(cursor) = self.cursors.get(&call) {
                    item["cursor"] = json!(cursor);
                }
                let route = if call == Call::Query {
                    "purchase_order/query"
                } else {
                    "receipt/query"
                };
                let value = read(&self.target, route, &item).await?;
                if let Some(cursor) = value["next_cursor"].as_str() {
                    self.cursors.insert(call, cursor.to_owned());
                } else {
                    self.cursors.remove(&call);
                }
                Ok(value)
            }
            Call::Create => {
                post(
                    &self.target,
                    "supplier/create",
                    &json!([{
                        "request_id": key, "idempotency_key": key, "name": key,
                    }]),
                )
                .await
            }
            Call::Update => {
                let order = &self.own[index];
                let supplier = (order.supplier + 1) % self.suppliers.len();
                let result = write(
                    &self.target,
                    &json!([{
                        "request_id": key, "id": order.id,
                        "expected_row_version": order.row_version,
                        "change": {"supplier_id": self.suppliers[supplier]},
                    }]),
                )
                .await;
                if let Ok(value) = &result {
                    self.own[index].supplier = supplier;
                    self.own[index].row_version = value["row_version"]
                        .as_i64()
                        .context("updated row version")?;
                }
                result
            }
            Call::RecordReceipt | Call::AcmeRecordReceipt => {
                let receipt = self
                    .receipts
                    .get(self.receipt_next)
                    .context("receipt seed exhausted before deadline")?;
                self.receipt_next += 1;
                let route = if call == Call::RecordReceipt {
                    "receiving/record_receipt"
                } else {
                    "acme/acme/record_receipt"
                };
                post(&self.target, route, &json!([{
                    "request_id": key,
                    "value": {"idempotency_key": key, "purchase_order_id": receipt.purchase_order_id,
                        "receipt_reference": key, "occurred_at": "2026-10-17T00:00:00Z",
                        "line": [{"purchase_order_line_id": receipt.line_id, "quantity": "1", "location_id": SEED_LOCATION}]},
                }])).await
            }
        }
    }
}

async fn run_worker(
    mut work: Workload,
    call: Call,
    mix: bool,
    window: std::ops::Range<Instant>,
) -> anyhow::Result<BTreeMap<Call, Measured>> {
    let mut measured: BTreeMap<Call, Measured> = BTreeMap::new();
    while Instant::now() < window.end {
        let call = if mix {
            mix_call(work.sequence, work.worker)
        } else {
            call
        };
        let index = work.sequence % work.own.len();
        if matches!(call, Call::RecordReceipt | Call::AcmeRecordReceipt) {
            ensure!(
                work.receipt_next < work.receipts.len(),
                "receipt seed exhausted before deadline"
            );
        }
        let begun = Instant::now();
        let outcome = work.invoke(call).await;
        let stats = measured.entry(call).or_default();
        stats.record(window.start, begun);
        if let Err(error) = outcome {
            stats.error(&error);
            if call == Call::Update {
                // Report recovery reads as actual calls, including their latency.
                let begun = Instant::now();
                let recovery = read(
                    &work.target,
                    "purchase_order/get",
                    &json!({"id": work.own[index].id}),
                )
                .await;
                let stats = measured.entry(Call::Get).or_default();
                stats.record(window.start, begun);
                match recovery {
                    Ok(value) => {
                        work.own[index].row_version = value["row_version"]
                            .as_i64()
                            .context("recovered row version")?;
                        work.own[index].supplier = work
                            .suppliers
                            .iter()
                            .position(|supplier| {
                                value["supplier_id"].as_str() == Some(supplier.as_str())
                            })
                            .context("recovered supplier")?;
                    }
                    Err(error) => stats.error(&error),
                }
            }
        }
    }
    Ok(measured)
}

async fn statement_calls(database: &tokio_postgres::Client) -> anyhow::Result<i64> {
    Ok(database.query_one(
        "SELECT coalesce(sum(s.calls), 0)::bigint FROM pg_stat_statements s JOIN pg_roles r ON r.oid = s.userid WHERE s.dbid = (SELECT oid FROM pg_database WHERE datname = current_database()) AND s.toplevel AND NOT r.rolsuper AND pg_has_role(r.oid, 'wamn_app', 'MEMBER')",
        &[],
    ).await?.get(0))
}

/// One read route, with its item in the query string.
async fn read(target: &Target, name: &str, item: &Value) -> anyhow::Result<Value> {
    let item: &Map<String, Value> = item.as_object().context("a read item is an object")?;
    let query = encode_read_query(item);
    let url = if query.is_empty() {
        format!("{}/{name}", target.base)
    } else {
        format!("{}/{name}?{query}", target.base)
    };
    let response = target
        .client
        .get(url)
        .header("authorization", &target.authorization)
        .send()
        .await?;
    outcome(response).await
}

/// The write route `/purchase_order/update`.
async fn write(target: &Target, items: &Value) -> anyhow::Result<Value> {
    post(target, "purchase_order/update", items).await
}

/// One write route, `/<path>`.
async fn post(target: &Target, path: &str, items: &Value) -> anyhow::Result<Value> {
    let response = target
        .client
        .post(format!("{}/{path}", target.base))
        .header("authorization", &target.authorization)
        .json(items)
        .send()
        .await?;
    outcome(response).await
}

/// The value of the one outcome, or an error for any status, transport or
/// refusal.
async fn outcome(response: reqwest::Response) -> anyhow::Result<Value> {
    let status = response.status();
    let body: Value = response.json().await?;
    ensure!(
        status.is_success(),
        "the route answered {status} {}",
        body.pointer("/error/code").unwrap_or(&Value::Null)
    );
    match body.as_array().map(Vec::as_slice) {
        Some([item]) => item.get("value").cloned().with_context(|| {
            format!(
                "refusal {}",
                item.pointer("/error/code").unwrap_or(&Value::Null)
            )
        }),
        _ => bail!("the reply is not one outcome"),
    }
}

/// Every purchase order, in id order, and the suppliers they name.
async fn list_orders(target: &Target) -> anyhow::Result<(Vec<Order>, Vec<String>)> {
    let mut rows = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut item = json!({ "limit": PAGE });
        if let Some(cursor) = &cursor {
            item["cursor"] = json!(cursor);
        }
        let page = read(target, "purchase_order/query", &item)
            .await
            .context("list the orders")?;
        rows.extend(
            page.get("item")
                .and_then(Value::as_array)
                .context("a query page holds item")?
                .iter()
                .cloned(),
        );
        match page.get("next_cursor").and_then(Value::as_str) {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
    }
    let mut suppliers: Vec<String> = rows
        .iter()
        .filter_map(|row| {
            row.get("supplier_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect();
    suppliers.sort();
    suppliers.dedup();
    let mut orders = rows
        .iter()
        .map(|row| {
            let text = |name: &str| {
                row.get(name)
                    .and_then(Value::as_str)
                    .with_context(|| format!("an order row holds {name}"))
            };
            let supplier = text("supplier_id")?;
            Ok(Order {
                id: text("id")?.to_owned(),
                row_version: row
                    .get("row_version")
                    .and_then(Value::as_i64)
                    .context("an order row holds row_version")?,
                supplier: suppliers
                    .iter()
                    .position(|known| known == supplier)
                    .context("the supplier is listed")?,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    orders.sort_by(|left, right| left.id.cmp(&right.id));
    Ok((orders, suppliers))
}

/// Connect to the project database.
async fn connect(url: &str) -> anyhow::Result<tokio_postgres::Client> {
    let (client, connection) = tokio_postgres::connect(url, tokio_postgres::NoTls)
        .await
        .context("connect to --db-url")?;
    tokio::spawn(connection);
    Ok(client)
}

/// Seed open orders with one line each, optionally with Acme approval.
async fn seed_receipts(
    database: &tokio_postgres::Client,
    prefix: &str,
    count: u64,
    acme: bool,
) -> anyhow::Result<Vec<Receipt>> {
    database
        .batch_execute(&format!(
            "BEGIN; \
             SELECT set_config('app.user_id', '{SEED_PRINCIPAL}', true), \
                    set_config('app.operation', 'admin:benchmark-seed', true); \
             INSERT INTO receiving.item (id, item_number) VALUES ('{SEED_ITEM}', 'ITEM-BENCH') \
               ON CONFLICT DO NOTHING; \
             INSERT INTO receiving.location (id, location_code) VALUES ('{SEED_LOCATION}', 'BENCH') \
               ON CONFLICT DO NOTHING; \
             INSERT INTO receiving.supplier (id, name) VALUES ('{SEED_SUPPLIER}', 'SUPPLIER-BENCH') \
               ON CONFLICT DO NOTHING;"
        ))
        .await
        .context("seed the item, location and supplier")?;
    let count = i32::try_from(count).context("the seed count fits an int4")?;
    let extra_columns = if acme {
        ", acme_inspection_required, acme_quality_status"
    } else {
        ""
    };
    let extra_values = if acme { ", true, 'approved'" } else { "" };
    let rows = database
        .query(
            &format!(
                "WITH orders AS ( \
                   INSERT INTO receiving.purchase_order (id, purchase_order_number, supplier_id, \
                     status, row_version{extra_columns}) \
                   SELECT gen_random_uuid(), $1::text || n::text, '{SEED_SUPPLIER}', 'open', 1{extra_values} \
                   FROM generate_series(1, $2::int4) AS n \
                   RETURNING id) \
                 INSERT INTO receiving.purchase_order_line (id, purchase_order_id, line_number, \
                   item_id, ordered_quantity, received_quantity) \
                 SELECT gen_random_uuid(), id, 1, '{SEED_ITEM}', 1, 0 FROM orders \
                 RETURNING purchase_order_id::text, id::text"
            ),
            &[&prefix, &count],
        )
        .await
        .context("seed the purchase orders")?;
    database
        .batch_execute("COMMIT")
        .await
        .context("commit the receipt seed")?;
    Ok(rows
        .iter()
        .map(|row| Receipt {
            purchase_order_id: row.get(0),
            line_id: row.get(1),
        })
        .collect())
}

/// One sample of `pg_stat_database` and `pg_stat_statements`.
async fn sample(database: &tokio_postgres::Client) -> anyhow::Result<Sample> {
    let row = database
        .query_one(
            "SELECT xact_commit, blks_hit, blks_read, numbackends FROM pg_stat_database \
             WHERE datname = current_database()",
            &[],
        )
        .await
        .context("read pg_stat_database")?;
    let statements = database
        .query(
            "SELECT query, calls, total_exec_time, mean_exec_time, rows FROM pg_stat_statements \
             WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database()) \
             ORDER BY total_exec_time DESC LIMIT 10",
            &[],
        )
        .await;
    let (statements, statements_error) = match statements {
        Ok(rows) => (
            Some(
                rows.iter()
                    .map(|row| StatementStats {
                        query: row.get(0),
                        calls: row.get(1),
                        total_exec_time_ms: row.get(2),
                        mean_exec_time_ms: row.get(3),
                        rows: row.get(4),
                    })
                    .collect(),
            ),
            None,
        ),
        Err(error) => (
            None,
            Some(
                error
                    .as_db_error()
                    .map_or_else(|| error.to_string(), |db| db.message().to_owned()),
            ),
        ),
    };
    Ok(Sample {
        database: DatabaseStats {
            xact_commit: row.get(0),
            blks_hit: row.get(1),
            blks_read: row.get(2),
            numbackends: row.get(3),
        },
        statements,
        statements_error,
    })
}

/// The nearest-rank percentile of sorted values: the smallest value with at
/// least `percent` of the values at or below it. Empty input gives 0.
fn percentile(sorted: &[u64], percent: u64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let count = sorted.len() as u64;
    let rank = (percent * count).div_ceil(100).max(1);
    sorted[usize::try_from(rank - 1)
        .unwrap_or(usize::MAX)
        .min(sorted.len() - 1)]
}

#[expect(
    clippy::cast_precision_loss,
    reason = "a latency in microseconds fits an f64 exactly"
)]
fn millis(micros: u64) -> f64 {
    micros as f64 / 1000.0
}

#[expect(
    clippy::cast_precision_loss,
    reason = "a call count fits an f64 exactly"
)]
fn to_f64(count: usize) -> f64 {
    count as f64
}

#[expect(
    clippy::cast_precision_loss,
    reason = "a call count fits an f64 exactly"
)]
fn ratio(errors: u64, calls: usize) -> f64 {
    if calls == 0 {
        0.0
    } else {
        errors as f64 / calls as f64
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        Call, DatabaseStats, Measured, Sample, Samples, StatementStats, mix_call, percentile,
    };

    #[test]
    fn mix_weights_hold_for_every_worker_offset() {
        for worker in 0..32 {
            let calls: Vec<_> = (0..20).map(|sequence| mix_call(sequence, worker)).collect();
            assert_eq!(
                calls
                    .iter()
                    .filter(|call| matches!(
                        call,
                        Call::Get | Call::AppPath | Call::Query | Call::ReceiptQuery
                    ))
                    .count(),
                14
            );
            assert_eq!(
                calls.iter().filter(|call| **call == Call::Update).count(),
                4
            );
            assert_eq!(
                calls.iter().filter(|call| **call == Call::Create).count(),
                1
            );
            assert_eq!(
                calls
                    .iter()
                    .filter(|call| **call == Call::RecordReceipt)
                    .count(),
                1
            );
        }
    }

    #[test]
    fn version_conflicts_are_separate_from_errors() {
        let mut measured = Measured::default();
        measured.error(&anyhow::anyhow!("refusal \"concurrency_conflict\""));
        measured.error(&anyhow::anyhow!("refusal \"idempotency_conflict\""));
        assert_eq!(measured.conflicts, 1);
        assert_eq!(measured.errors, 1);
    }

    /// The Postgres samples keep their field names in the result file, and a
    /// sample without `pg_stat_statements` says why.
    #[test]
    fn postgres_samples_serialize_before_and_after() {
        let database = || DatabaseStats {
            xact_commit: 10,
            blks_hit: 20,
            blks_read: 3,
            numbackends: 4,
        };
        let samples = Samples {
            before: Sample {
                database: database(),
                statements: Some(vec![StatementStats {
                    query: "SELECT 1".to_owned(),
                    calls: 2,
                    total_exec_time_ms: 1.5,
                    mean_exec_time_ms: 0.75,
                    rows: 2,
                }]),
                statements_error: None,
            },
            after: Sample {
                database: database(),
                statements: None,
                statements_error: Some("relation does not exist".to_owned()),
            },
        };
        assert_eq!(
            serde_json::to_value(&samples).unwrap(),
            json!({
                "before": {
                    "database": {"xact_commit": 10, "blks_hit": 20, "blks_read": 3, "numbackends": 4},
                    "statements": [{"query": "SELECT 1", "calls": 2, "total_exec_time_ms": 1.5,
                                    "mean_exec_time_ms": 0.75, "rows": 2}],
                },
                "after": {
                    "database": {"xact_commit": 10, "blks_hit": 20, "blks_read": 3, "numbackends": 4},
                    "statements": null,
                    "statements_error": "relation does not exist",
                },
            })
        );
    }

    #[test]
    fn percentiles_take_the_nearest_rank() {
        let values: Vec<u64> = (1..=200).collect();
        assert_eq!(percentile(&values, 50), 100);
        assert_eq!(percentile(&values, 95), 190);
        assert_eq!(percentile(&values, 99), 198);
        assert_eq!(percentile(&[7], 99), 7);
        assert_eq!(percentile(&[1, 2, 3], 50), 2);
        assert_eq!(percentile(&[], 50), 0);
    }
}
