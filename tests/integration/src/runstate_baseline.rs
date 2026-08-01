//! PLAN-3 baseline for the pre-checkpoint F1 run-state persistence model.
//!
//! This is a measurement campaign, not a regression gate. It applies the
//! production run-state DDL in an ephemeral schema and records the successful
//! F1 capture-on path at several schema-valid line counts. Only provenance and
//! exact-row sanity assertions gate; the CSV curves are the durable result.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use clap::Args;
use serde_json::{Value, json};
use tokio_postgres::{Client, NoTls};
use wamn_gate_harness::{check, emit_csv, percentile};

const SCHEMA: &str = "wamn_plan3_baseline";
const TENANT: &str = "plan3-baseline";
const RUN_STATE_SQL: &str = include_str!("../../../deploy/sql/run-state.sql");
const F1_FLOW_JSON: &str = include_str!("../../../deploy/poc/f1-flow.json");
const SUCCESS_NODES: usize = 8;

#[derive(Debug, Args)]
pub struct RunstateBaselineArgs {
    /// App Postgres URL for the NOSUPERUSER wamn_app role.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Superuser URL used to provision, normalize, and inspect the schema.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// Schema-valid F1 receipt line counts, comma-separated (maximum 100).
    #[arg(long, default_value = "1,10,100")]
    pub line_counts: String,

    /// Completed F1 runs recorded at each line count.
    #[arg(long, default_value_t = 500)]
    pub runs_per_size: usize,

    /// Concurrent run writers.
    #[arg(long, default_value_t = 4)]
    pub concurrency: usize,

    /// Also write CSV output to this directory.
    #[arg(long)]
    pub out: Option<PathBuf>,
}

#[derive(Clone)]
struct Stage {
    node_id: &'static str,
    input: Value,
    output: Value,
    updates_context: bool,
}

struct RelationStats {
    heap: i64,
    total: i64,
    toast: i64,
    inserted: i64,
    updated: i64,
    dead: i64,
    autovacuums: i64,
}

fn parse_line_counts(value: &str) -> anyhow::Result<Vec<usize>> {
    let counts: Vec<usize> = value
        .split(',')
        .map(|part| part.trim().parse::<usize>())
        .collect::<Result<_, _>>()
        .with_context(|| format!("bad --line-counts {value:?}"))?;
    if counts.is_empty() || counts.iter().any(|count| !(1..=100).contains(count)) {
        bail!("--line-counts must contain values from 1 through 100");
    }
    Ok(counts)
}

fn f1_stages(run_id: &str, line_count: usize) -> Vec<Stage> {
    let lines: Vec<Value> = (0..line_count)
        .map(|index| {
            json!({
                "material": "resin-a",
                "quantity": format!("{}.{:03}", 100 + index, index % 1000),
                "moisture_pct": "11.20",
                "weight_kg": format!("{}.{:03}", 99 + index, 980),
            })
        })
        .collect();
    let receipt = json!({
        "receipt_no": run_id,
        "supplier": "acme",
        "site": "hq",
        "received_at": "2026-07-12T08:00:00Z",
        "lines": lines,
    });
    let line_ids: Vec<String> = (1..=line_count)
        .map(|line| format!("{run_id}-line-{line}"))
        .collect();
    let specs: Vec<Value> = (0..line_count)
        .map(|_| {
            json!({
                "material_id": "material-resin-a",
                "moisture_max_pct": "12.50",
                "weight_tolerance_kg": "0.050",
            })
        })
        .collect();
    let resolved = json!({
        "rows": [{
            "receipt": receipt,
            "references_valid": true,
            "receipt_id": format!("receipt-{run_id}"),
            "site_id": "site-hq",
            "line_specs": specs,
            "line_ids": line_ids,
        }]
    });
    let evaluated = json!({
        "receipt_id": format!("receipt-{run_id}"),
        "site_id": "site-hq",
        "out_of_spec": [],
    });
    let held = json!({"rows": [{"receipt_id": format!("receipt-{run_id}"), "holds": []}]});
    let response = held["rows"][0].clone();

    vec![
        Stage {
            node_id: "request",
            input: receipt.clone(),
            output: receipt.clone(),
            updates_context: false,
        },
        Stage {
            node_id: "normalize-receipt",
            input: receipt.clone(),
            output: receipt,
            updates_context: true,
        },
        Stage {
            node_id: "resolve-and-persist",
            input: resolved["rows"][0]["receipt"].clone(),
            output: resolved.clone(),
            updates_context: true,
        },
        Stage {
            node_id: "references-valid",
            input: resolved.clone(),
            output: resolved,
            updates_context: false,
        },
        Stage {
            node_id: "evaluate-specs",
            input: json!({"references_valid": true}),
            output: evaluated.clone(),
            updates_context: true,
        },
        Stage {
            node_id: "create-holds",
            input: evaluated,
            output: held.clone(),
            updates_context: true,
        },
        Stage {
            node_id: "shape-response",
            input: held,
            output: response.clone(),
            updates_context: true,
        },
        Stage {
            node_id: "respond",
            input: response.clone(),
            output: response,
            updates_context: false,
        },
    ]
}

async fn connect(url: &str) -> anyhow::Result<(Client, tokio::task::JoinHandle<()>)> {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
    let handle = tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok((client, handle))
}

async fn connect_app(url: &str) -> anyhow::Result<(Client, tokio::task::JoinHandle<()>)> {
    let (client, handle) = connect(url).await?;
    client
        .batch_execute(&format!(
            "SET search_path TO {SCHEMA}; SET app.tenant TO '{TENANT}';"
        ))
        .await?;
    Ok((client, handle))
}

async fn provision(admin_url: &str) -> anyhow::Result<()> {
    let (client, _handle) = connect(admin_url).await?;
    client
        .batch_execute(&format!("DROP SCHEMA IF EXISTS {SCHEMA} CASCADE;"))
        .await?;
    client
        .batch_execute(&RUN_STATE_SQL.replace("wamn_run", SCHEMA))
        .await
        .context("apply production run-state DDL in the baseline schema")?;
    Ok(())
}

async fn reset(admin: &Client) -> anyhow::Result<()> {
    admin
        .batch_execute(&format!(
            "TRUNCATE {SCHEMA}.runs CASCADE; \
             SELECT pg_stat_reset_single_table_counters('{SCHEMA}.runs'::regclass); \
             SELECT pg_stat_reset_single_table_counters('{SCHEMA}.node_runs'::regclass);"
        ))
        .await?;
    admin
        .batch_execute(&format!("VACUUM (ANALYZE) {SCHEMA}.runs"))
        .await?;
    admin
        .batch_execute(&format!("VACUUM (ANALYZE) {SCHEMA}.node_runs"))
        .await?;
    admin.batch_execute("CHECKPOINT").await?;
    Ok(())
}

async fn wal_lsn(admin: &Client) -> anyhow::Result<String> {
    Ok(admin
        .query_one("SELECT pg_current_wal_insert_lsn()::text", &[])
        .await?
        .get(0))
}

async fn wal_since(admin: &Client, before: &str) -> anyhow::Result<i64> {
    Ok(admin
        .query_one(
            "SELECT pg_wal_lsn_diff(pg_current_wal_insert_lsn(), $1::text::pg_lsn)::bigint",
            &[&before],
        )
        .await?
        .get(0))
}

async fn relation_stats(admin: &Client, table: &str) -> anyhow::Result<RelationStats> {
    let row = admin
        .query_one(
            &format!(
                "SELECT pg_relation_size('{SCHEMA}.{table}')::bigint, \
                        pg_total_relation_size('{SCHEMA}.{table}')::bigint, \
                        COALESCE(pg_relation_size(c.reltoastrelid), 0)::bigint, \
                        COALESCE(s.n_tup_ins, 0)::bigint, COALESCE(s.n_tup_upd, 0)::bigint, \
                        COALESCE(s.n_dead_tup, 0)::bigint, COALESCE(s.autovacuum_count, 0)::bigint \
                   FROM pg_class AS c \
                   LEFT JOIN pg_stat_all_tables AS s ON s.relid = c.oid \
                  WHERE c.oid = '{SCHEMA}.{table}'::regclass"
            ),
            &[],
        )
        .await?;
    Ok(RelationStats {
        heap: row.get(0),
        total: row.get(1),
        toast: row.get(2),
        inserted: row.get(3),
        updated: row.get(4),
        dead: row.get(5),
        autovacuums: row.get(6),
    })
}

async fn write_run(
    app_url: &str,
    run_id: &str,
    line_count: usize,
    latencies: &Mutex<Vec<Duration>>,
) -> anyhow::Result<()> {
    let (mut client, _handle) = connect_app(app_url).await?;
    let stages = f1_stages(run_id, line_count);
    let input = stages[0].input.to_string();
    client
        .execute(
            "INSERT INTO runs (tenant_id, run_id, flow_id, flow_version, status, trigger_source, input_json) \
             VALUES (current_setting('app.tenant', true), $1, 'receipt-received', 1, 'running', 'http', $2::text::jsonb)",
            &[&run_id, &input],
        )
        .await?;

    for (sequence, stage) in stages.iter().enumerate() {
        let input = stage.input.to_string();
        let output = stage.output.to_string();
        let payload_size = i64::try_from(output.len()).context("payload size exceeds i64")?;
        let started = Instant::now();
        let transaction = client.transaction().await?;
        transaction
            .execute(
                "INSERT INTO node_runs \
                   (tenant_id, run_id, node_id, occurrence, seq, status, output_port, \
                    output_json, input_json, payload_size, payload_hash, capture_mode, redacted, ended_at) \
                 VALUES (current_setting('app.tenant', true), $1, $2, 0, $3, 'success', 'main', \
                         $4::text::jsonb, $5::text::jsonb, $6, 'baseline', 'full', false, now())",
                &[&run_id, &stage.node_id, &(sequence as i32), &output, &input, &payload_size],
            )
            .await?;
        if stage.updates_context {
            transaction
                .execute(
                    "UPDATE runs SET state_json = jsonb_build_object('context', $2::text::jsonb), updated_at = now() \
                     WHERE tenant_id = current_setting('app.tenant', true) AND run_id = $1",
                    &[&run_id, &output],
                )
                .await?;
        }
        transaction.commit().await?;
        latencies
            .lock()
            .expect("latency mutex must not be poisoned")
            .push(started.elapsed());
    }
    let result = stages
        .last()
        .expect("F1 path is non-empty")
        .output
        .to_string();
    client
        .execute(
            "UPDATE runs SET status = 'completed', result_json = $2::text::jsonb, updated_at = now() \
             WHERE tenant_id = current_setting('app.tenant', true) AND run_id = $1",
            &[&run_id, &result],
        )
        .await?;
    Ok(())
}

async fn measure_size(
    app_url: &str,
    admin: &Client,
    line_count: usize,
    args: &RunstateBaselineArgs,
) -> anyhow::Result<(String, bool)> {
    reset(admin).await?;
    let runs_before = relation_stats(admin, "runs").await?;
    let nodes_before = relation_stats(admin, "node_runs").await?;
    let before_lsn = wal_lsn(admin).await?;
    let latencies = Arc::new(Mutex::new(Vec::with_capacity(
        args.runs_per_size * SUCCESS_NODES,
    )));
    let started = Instant::now();
    let mut writers = tokio::task::JoinSet::new();
    let concurrency = args.concurrency;
    for worker in 0..args.concurrency {
        let app_url = app_url.to_string();
        let latencies = latencies.clone();
        let total = args.runs_per_size;
        writers.spawn(async move {
            for index in (worker..total).step_by(concurrency) {
                write_run(
                    &app_url,
                    &format!("f1-{line_count}-{index}"),
                    line_count,
                    &latencies,
                )
                .await?;
            }
            anyhow::Ok(())
        });
    }
    while let Some(result) = writers.join_next().await {
        result??;
    }
    let elapsed = started.elapsed();
    let wal = wal_since(admin, &before_lsn).await?;
    admin
        .batch_execute(&format!(
            "ANALYZE {SCHEMA}.runs; ANALYZE {SCHEMA}.node_runs;"
        ))
        .await?;
    let runs_after = relation_stats(admin, "runs").await?;
    let nodes_after = relation_stats(admin, "node_runs").await?;
    let row = admin
        .query_one(
            &format!(
                "SELECT (SELECT count(*) FROM {SCHEMA}.runs), \
                        (SELECT count(*) FROM {SCHEMA}.node_runs), \
                        (SELECT count(*) FROM {SCHEMA}.node_runs \
                          WHERE capture_mode = 'full' AND input_json IS NOT NULL AND output_json IS NOT NULL)"
            ),
            &[],
        )
        .await?;
    let run_rows: i64 = row.get(0);
    let node_rows: i64 = row.get(1);
    let full_rows: i64 = row.get(2);
    let expected_runs = args.runs_per_size as i64;
    let expected_nodes = expected_runs * SUCCESS_NODES as i64;
    let mut samples = latencies
        .lock()
        .expect("latency mutex must not be poisoned")
        .clone();
    samples.sort();
    let p50_ms = percentile(&samples, 0.50).as_secs_f64() * 1e3;
    let p99_ms = percentile(&samples, 0.99).as_secs_f64() * 1e3;
    let run_growth = runs_after.total - runs_before.total;
    let node_growth = nodes_after.total - nodes_before.total;
    let mut pass = true;
    check(
        &mut pass,
        "exact completed F1 run count",
        run_rows == expected_runs,
    );
    check(
        &mut pass,
        "exact successful F1 node count",
        node_rows == expected_nodes,
    );
    check(
        &mut pass,
        "every node row is full capture",
        full_rows == expected_nodes,
    );
    check(&mut pass, "measurement generated WAL", wal > 0);
    println!(
        "  {line_count:>3} lines | {expected_runs} runs in {:.2}s ({:.1}/s) | boundary p50 {:.3}ms p99 {:.3}ms | WAL {:.0} B/run",
        elapsed.as_secs_f64(),
        expected_runs as f64 / elapsed.as_secs_f64(),
        p50_ms,
        p99_ms,
        wal as f64 / expected_runs as f64,
    );
    println!(
        "      runs total +{run_growth} B (heap {} toast {}), node_runs total +{node_growth} B (heap {} toast {}); updates {} dead {} autovacuums {}",
        runs_after.heap,
        runs_after.toast,
        nodes_after.heap,
        nodes_after.toast,
        runs_after.updated + nodes_after.updated,
        runs_after.dead + nodes_after.dead,
        runs_after.autovacuums + nodes_after.autovacuums,
    );
    let csv = format!(
        "line_count,runs,nodes,elapsed_secs,runs_per_sec,boundary_p50_ms,boundary_p99_ms,wal_bytes,wal_bytes_per_run,runs_total_growth,runs_heap,runs_toast,node_runs_total_growth,node_runs_heap,node_runs_toast,tuples_inserted,tuples_updated,dead_tuples,autovacuum_count\n\
         {line_count},{expected_runs},{expected_nodes},{:.3},{:.3},{p50_ms:.3},{p99_ms:.3},{wal},{:.3},{run_growth},{},{},{node_growth},{},{},{},{},{},{}\n",
        elapsed.as_secs_f64(),
        expected_runs as f64 / elapsed.as_secs_f64(),
        wal as f64 / expected_runs as f64,
        runs_after.heap,
        runs_after.toast,
        nodes_after.heap,
        nodes_after.toast,
        runs_after.inserted + nodes_after.inserted,
        runs_after.updated + nodes_after.updated,
        runs_after.dead + nodes_after.dead,
        runs_after.autovacuums + nodes_after.autovacuums,
    );
    Ok((csv, pass))
}

pub async fn run(args: RunstateBaselineArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();
    let app_url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no app database URL")?;
    let admin_url = args
        .admin_database_url
        .clone()
        .context("runstate-baseline needs WAMN_PG_ADMIN_URL")?;
    let line_counts = parse_line_counts(&args.line_counts)?;
    if args.runs_per_size == 0 || args.concurrency == 0 {
        bail!("--runs-per-size and --concurrency must be positive");
    }
    let flow: Value = serde_json::from_str(F1_FLOW_JSON).context("parse canonical F1 flow")?;
    if flow["flow-id"] != "receipt-received" || flow["version"] != 1 {
        bail!("canonical F1 identity drifted");
    }

    println!(
        "# PLAN-3 F1 capture-on baseline (schema {SCHEMA}, tenant {TENANT})\n\
         production run-state DDL; {} runs/size; {} writers; line counts {:?}",
        args.runs_per_size, args.concurrency, line_counts
    );
    provision(&admin_url).await?;
    let outcome = async {
        let (admin, _handle) = connect(&admin_url).await?;
        let fsync: String = admin.query_one("SHOW fsync", &[]).await?.get(0);
        let synchronous_commit: String = admin
            .query_one("SHOW synchronous_commit", &[])
            .await?
            .get(0);
        println!("provenance: fsync={fsync}, synchronous_commit={synchronous_commit}");
        let mut pass = fsync == "on" && synchronous_commit == "on";
        check(
            &mut pass,
            "durable commit settings are enabled",
            fsync == "on" && synchronous_commit == "on",
        );
        let mut combined = String::new();
        for line_count in line_counts {
            let (csv, ok) = measure_size(&app_url, &admin, line_count, &args).await?;
            if combined.is_empty() {
                combined.push_str(&csv);
            } else if let Some((_, row)) = csv.split_once('\n') {
                combined.push_str(row);
            }
            pass &= ok;
        }
        emit_csv("plan3-f1-capture-baseline", &combined, &args.out);
        anyhow::Ok(pass)
    }
    .await;
    let (admin, _handle) = connect(&admin_url).await?;
    let _ = admin
        .batch_execute(&format!("DROP SCHEMA IF EXISTS {SCHEMA} CASCADE;"))
        .await;
    let pass = outcome?;
    println!("\nrunstate-baseline complete — overall PASS: {pass}");
    if !pass {
        bail!("a PLAN-3 baseline sanity assertion failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_counts_reject_outside_the_f1_schema() {
        assert_eq!(parse_line_counts("1, 10,100").unwrap(), [1, 10, 100]);
        assert!(parse_line_counts("0").is_err());
        assert!(parse_line_counts("101").is_err());
    }

    #[test]
    fn successful_path_tracks_the_canonical_f1_nodes() {
        let flow: Value = serde_json::from_str(F1_FLOW_JSON).unwrap();
        let stages = f1_stages("run-1", 100);
        assert_eq!(stages.len(), SUCCESS_NODES);
        assert_eq!(stages[0].node_id, "request");
        assert_eq!(stages.last().unwrap().node_id, "respond");
        assert_eq!(stages[0].input["lines"].as_array().unwrap().len(), 100);
        for stage in &stages {
            assert!(
                flow["nodes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|node| node["id"] == stage.node_id)
            );
        }
    }
}
