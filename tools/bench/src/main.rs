//! A benchmark of a deployed host over HTTPS (docs/history/gcp-deployment.md
//! section 6).
//!
//! The tool drives one Receiving purchase-order call against the route
//! interface of a host: `get` (a read), `update` as a supplier change (a
//! write), or `query` (a list). Workers run on `tokio` for the stated
//! duration and time every call. The tool writes one JSON file with p50, p95
//! and p99 latency, throughput and the error count, tagged with the tier.
//!
//! Before the timed part, the tool lists every purchase order and the
//! suppliers they name. Each worker takes its own slice of the orders, so no
//! two workers write one row. A write changes the supplier of the order to the
//! next supplier in the cycle. Any refusal counts as an error, a concurrency
//! conflict included. After a failed write, the worker reads the row again,
//! outside the timing, and goes on.
//!
//! `acme_record_receipt` posts Acme's `acme.record_receipt` in the approved
//! case: a purchase order that needs inspection and has an approved quality
//! status, so the call writes a receipt and an inspection row. A purchase
//! order line takes a receipt once, so each request uses its own purchase
//! order. Before the timed part, the tool seeds a fresh set of approved
//! orders with one line each through `--db-url`, which the call requires, and
//! gives each worker its own slice. Each request has its own request id,
//! idempotency key and receipt reference. A worker that used its whole slice
//! stops early.
//!
//! With `--db-url`, the URL of the project database, the tool samples
//! `pg_stat_database` and the ten statements with the most total time in
//! `pg_stat_statements` before and after the timed part, into the result. If
//! `pg_stat_statements` cannot be read, the sample says why and the run goes on.
//!
//! wamn-bench --host receiving.wamn.dev --pat-file <Secret JSON> --call update \
//!   --concurrency 16 --duration-secs 60 --tier tier-1 --output tests/bench/<file>.json \
//!   [--db-url postgres://...]

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
    /// The PAT Secret JSON that `provision-project-env` wrote, with the token
    /// at `.stringData.token`.
    #[arg(long)]
    pat_file: PathBuf,
    #[arg(long, value_enum)]
    call: Call,
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

#[derive(Debug, Clone, Copy, ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
enum Call {
    Get,
    Update,
    Query,
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
    errors: u64,
}

#[derive(Debug, Serialize)]
struct Summary {
    host: String,
    call: Call,
    tier: String,
    concurrency: usize,
    duration_secs: f64,
    started_at_unix: u64,
    orders: usize,
    calls: usize,
    errors: u64,
    error_rate: f64,
    throughput_per_sec: f64,
    p50_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    postgres: Option<Samples>,
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
    let target = Arc::new(Target {
        client: reqwest::Client::builder()
            .pool_max_idle_per_host(args.concurrency)
            .timeout(Duration::from_secs(30))
            .build()?,
        base: format!("https://{}/api", args.host),
        authorization: format!("Bearer {token}"),
    });

    let database = match &args.db_url {
        Some(url) => Some(connect(url).await?),
        None => None,
    };
    let started_at_unix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let (orders, suppliers, receipts) = if matches!(args.call, Call::AcmeRecordReceipt) {
        let database = database
            .as_ref()
            .context("--call acme_record_receipt needs --db-url to seed its orders")?;
        let count =
            u64::try_from(args.concurrency)? * args.duration_secs * RECEIPTS_PER_WORKER_SECOND;
        let receipts = seed_receipts(database, &format!("BENCH-{started_at_unix}-"), count).await?;
        (Vec::new(), Vec::new(), receipts)
    } else {
        let (orders, suppliers) = list_orders(&target).await?;
        (orders, suppliers, Vec::new())
    };
    let units = orders.len().max(receipts.len());
    ensure!(
        units >= args.concurrency,
        "{units} orders cannot give {} workers one each",
        args.concurrency
    );
    let before = match &database {
        Some(database) => Some(sample(database).await?),
        None => None,
    };
    let suppliers = Arc::new(suppliers);
    let slice = units / args.concurrency;
    let started = Instant::now();
    let deadline = started + Duration::from_secs(args.duration_secs);
    let mut workers = Vec::with_capacity(args.concurrency);
    for worker in 0..args.concurrency {
        let target = Arc::clone(&target);
        let call = args.call;
        if matches!(call, Call::AcmeRecordReceipt) {
            let own = receipts[worker * slice..(worker + 1) * slice].to_vec();
            let run = started_at_unix;
            workers.push(tokio::spawn(async move {
                run_receipts(&target, run, worker, own, deadline).await
            }));
        } else {
            let own = orders[worker * slice..(worker + 1) * slice].to_vec();
            let suppliers = Arc::clone(&suppliers);
            workers.push(tokio::spawn(async move {
                run_worker(&target, call, worker, own, &suppliers, deadline).await
            }));
        }
    }
    let mut latencies = Vec::new();
    let mut errors = 0;
    for worker in workers {
        let measured = worker.await?;
        latencies.extend(measured.latencies_micros);
        errors += measured.errors;
    }
    let elapsed = started.elapsed().as_secs_f64();
    let postgres = match (&database, before) {
        (Some(database), Some(before)) => Some(Samples {
            before,
            after: sample(database).await?,
        }),
        _ => None,
    };
    latencies.sort_unstable();
    let calls = latencies.len();
    let result = Summary {
        host: args.host,
        call: args.call,
        tier: args.tier,
        concurrency: args.concurrency,
        duration_secs: elapsed,
        started_at_unix,
        orders: units,
        calls,
        errors,
        error_rate: ratio(errors, calls),
        throughput_per_sec: to_f64(calls) / elapsed,
        p50_ms: millis(percentile(&latencies, 50)),
        p95_ms: millis(percentile(&latencies, 95)),
        p99_ms: millis(percentile(&latencies, 99)),
        postgres,
    };
    let text = serde_json::to_string_pretty(&result)?;
    tokio::fs::write(&args.output, format!("{text}\n"))
        .await
        .with_context(|| format!("write {}", args.output.display()))?;
    println!("{text}");
    Ok(())
}

async fn run_worker(
    target: &Target,
    call: Call,
    worker: usize,
    mut own: Vec<Order>,
    suppliers: &[String],
    deadline: Instant,
) -> Measured {
    let mut measured = Measured::default();
    let mut next = 0;
    let mut sequence = 0_u64;
    while Instant::now() < deadline {
        let index = next % own.len();
        next += 1;
        let begun = Instant::now();
        let outcome = match call {
            Call::Get => read(target, "get", &json!({ "id": own[index].id })).await,
            Call::Query | Call::AcmeRecordReceipt => read(target, "query", &json!({})).await,
            Call::Update => {
                sequence += 1;
                let order = &own[index];
                let supplier = (order.supplier + 1) % suppliers.len();
                let item = json!([{
                    "request_id": format!("bench-{worker}-{sequence}"),
                    "id": order.id,
                    "expected_row_version": order.row_version,
                    "change": { "supplier_id": suppliers[supplier] },
                }]);
                write(target, &item).await.inspect(|value| {
                    let order = &mut own[index];
                    order.supplier = supplier;
                    order.row_version = value
                        .get("row_version")
                        .and_then(Value::as_i64)
                        .unwrap_or(order.row_version + 1);
                })
            }
        };
        let micros = u64::try_from(begun.elapsed().as_micros()).unwrap_or(u64::MAX);
        measured.latencies_micros.push(micros);
        if outcome.is_err() {
            measured.errors += 1;
            if matches!(call, Call::Update)
                && let Ok(value) = read(target, "get", &json!({ "id": own[index].id })).await
            {
                own[index].row_version = value
                    .get("row_version")
                    .and_then(Value::as_i64)
                    .unwrap_or(own[index].row_version);
            }
        }
    }
    measured
}

/// One worker of `acme_record_receipt`: one receipt per seeded order of its
/// slice, until the deadline or the end of the slice.
async fn run_receipts(
    target: &Target,
    run: u64,
    worker: usize,
    own: Vec<Receipt>,
    deadline: Instant,
) -> Measured {
    let mut measured = Measured::default();
    for (sequence, receipt) in own.iter().enumerate() {
        if Instant::now() >= deadline {
            break;
        }
        let key = format!("bench-{run}-{worker}-{sequence}");
        let item = json!([{
            "request_id": key,
            "value": {
                "idempotency_key": key,
                "purchase_order_id": receipt.purchase_order_id,
                "receipt_reference": key,
                "occurred_at": "2026-10-15T00:00:00Z",
                "line": [{
                    "purchase_order_line_id": receipt.line_id,
                    "quantity": "1",
                    "location_id": SEED_LOCATION,
                }],
            },
        }]);
        let begun = Instant::now();
        let outcome = post(target, "acme/acme/record_receipt", &item).await;
        let micros = u64::try_from(begun.elapsed().as_micros()).unwrap_or(u64::MAX);
        measured.latencies_micros.push(micros);
        if outcome.is_err() {
            measured.errors += 1;
        }
    }
    measured
}

/// One read route, `/purchase_order/<name>`, with its item in the query string.
async fn read(target: &Target, name: &str, item: &Value) -> anyhow::Result<Value> {
    let item: &Map<String, Value> = item.as_object().context("a read item is an object")?;
    let query = encode_read_query(item);
    let url = if query.is_empty() {
        format!("{}/purchase_order/{name}", target.base)
    } else {
        format!("{}/purchase_order/{name}?{query}", target.base)
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
    ensure!(status.is_success(), "the route answered {status}");
    match body.as_array().map(Vec::as_slice) {
        Some([item]) => item
            .get("value")
            .cloned()
            .with_context(|| format!("the outcome is a refusal: {item}")),
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
        let page = read(target, "query", &item)
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
    ensure!(suppliers.len() > 1, "a supplier change needs two suppliers");
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

/// Seed `count` open purchase orders that need inspection and are approved,
/// with one line of quantity 1 each, numbered `<prefix><n>`.
async fn seed_receipts(
    database: &tokio_postgres::Client,
    prefix: &str,
    count: u64,
) -> anyhow::Result<Vec<Receipt>> {
    database
        .batch_execute(&format!(
            "INSERT INTO receiving.item (id, item_number) VALUES ('{SEED_ITEM}', 'ITEM-BENCH') \
               ON CONFLICT DO NOTHING; \
             INSERT INTO receiving.location (id, location_code) VALUES ('{SEED_LOCATION}', 'BENCH') \
               ON CONFLICT DO NOTHING; \
             INSERT INTO receiving.supplier (id, name) VALUES ('{SEED_SUPPLIER}', 'SUPPLIER-BENCH') \
               ON CONFLICT DO NOTHING;"
        ))
        .await
        .context("seed the item, location and supplier")?;
    let count = i32::try_from(count).context("the seed count fits an int4")?;
    let rows = database
        .query(
            &format!(
                "WITH orders AS ( \
                   INSERT INTO receiving.purchase_order (id, purchase_order_number, supplier_id, \
                     status, row_version, created_at, created_by, updated_at, updated_by, \
                     acme_inspection_required, acme_quality_status) \
                   SELECT gen_random_uuid(), $1::text || n::text, '{SEED_SUPPLIER}', 'open', 1, \
                     now(), '{SEED_PRINCIPAL}', now(), '{SEED_PRINCIPAL}', true, 'approved' \
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
        .context("seed the approved purchase orders")?;
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
        Err(error) => (None, Some(error.to_string())),
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

    use super::{DatabaseStats, Sample, Samples, StatementStats, percentile};

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
