//! The throughput bench's Rust half, as a command the journey calls.
//!
//!   wamn-throughput sample --pg-url URL --database NAME... [--nats-varz URL] --out FILE
//!   wamn-throughput report --evidence-dir DIR --out-md FILE --out-json FILE
//!   wamn-throughput schema
//!
//! The load itself is oha and pgbench in pods; see
//! `wamn_proof_integration::throughput_bench` and
//! `tools/receiving-cluster-journey-run --throughput`.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use wamn_proof_integration::throughput_bench;

#[derive(Parser)]
#[command(about = "PostgreSQL counter samples and the report for the throughput bench")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Read pg_stat_database and pg_stat_activity (and NATS /varz) once.
    Sample {
        #[arg(long)]
        pg_url: String,
        /// Databases to read counters for; repeatable.
        #[arg(long = "database", required = true)]
        databases: Vec<String>,
        #[arg(long)]
        nats_varz: Option<String>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Reduce a sweep's evidence directory to tables, a knee and a peak per layer.
    Report {
        #[arg(long)]
        evidence_dir: PathBuf,
        #[arg(long)]
        out_md: PathBuf,
        #[arg(long)]
        out_json: PathBuf,
    },
    /// Print the index document's JSON Schema (what schema/wamn-throughput.schema.json holds).
    Schema,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Sample {
            pg_url,
            databases,
            nats_varz,
            out,
        } => {
            let sample =
                throughput_bench::sample(&pg_url, &databases, nats_varz.as_deref()).await?;
            std::fs::write(&out, serde_json::to_vec_pretty(&sample)?)
                .with_context(|| format!("write {}", out.display()))?;
        }
        Command::Report {
            evidence_dir,
            out_md,
            out_json,
        } => {
            let report = throughput_bench::build_report(&evidence_dir)?;
            std::fs::write(&out_md, report.render_markdown())
                .with_context(|| format!("write {}", out_md.display()))?;
            std::fs::write(&out_json, serde_json::to_vec_pretty(&report)?)
                .with_context(|| format!("write {}", out_json.display()))?;
            for verdict in &report.verdicts {
                match &verdict.knee {
                    Some(knee) => println!(
                        "layer={} knee={} turns_at={} efficiency={:.2} peak_rps={:.0} peak_c={}",
                        verdict.layer,
                        knee.concurrency,
                        knee.turns_at,
                        knee.efficiency,
                        verdict.peak.requests_per_second,
                        verdict.peak.concurrency
                    ),
                    None => println!(
                        "layer={} knee=none peak_rps={:.0} peak_c={}",
                        verdict.layer, verdict.peak.requests_per_second, verdict.peak.concurrency
                    ),
                }
            }
        }
        Command::Schema => {
            use std::io::Write as _;
            std::io::stdout().write_all(&throughput_bench::index_schema_bytes())?;
        }
    }
    Ok(())
}
