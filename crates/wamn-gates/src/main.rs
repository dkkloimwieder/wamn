//! wamn-gates: the gate suite binary (docs/archive/structure-review.md SR1).
//!
//! Every bench/fixture/proof subcommand that used to ride the wamn-host
//! binary, with an identical subcommand surface (`wamn-gates pgbench …`), so
//! the deploy Jobs swap only their image. Depends on the `wamn-host` library:
//! gates exercise the identical host code the prod artifact runs.

mod apibench;
mod apifixture;
mod apiproof;
mod bench;
mod credprobe;
mod credproof;
mod dispatchbench;
mod egressbench;
mod f1bench;
mod f1fixture;
mod f1proof;
mod failoverbench;
mod flowbench;
mod ladderproof;
mod logbench;
mod matbench;
mod nodebench;
mod nodeinvoke;
mod pgbench;
mod provisionbench;
mod publish_catalog_demo;
mod queuebench;
mod readerbench;
mod rie2ebench;
mod runnerbench;
mod samplebench;
#[cfg(test)]
mod schema_drift;
mod socketguard;
mod streambench;
mod testhostbench;
mod tracebench;
mod traceproof;
mod walbench;

use std::str::FromStr as _;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "wamn-gates", version, about)]
struct Cli {
    /// Log level (the Jobs pass this before the subcommand)
    #[arg(long = "log-level", global = true, default_value = "info")]
    log_level: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the S1 benchmark suite
    Bench(bench::BenchArgs),
    /// Run the S2 wamn:postgres benchmark + security gates
    Pgbench(pgbench::PgBenchArgs),
    /// Run the 2.3 provisioning gate (per-project DB provisioning / credential resolution / isolation)
    Provisionbench(provisionbench::ProvisionBenchArgs),
    /// Run the S3 flow-runner gates (dispatch / hot-reload / resume)
    Flowbench(flowbench::FlowBenchArgs),
    /// Run the 5.14 durable-run-queue gates (dispatch SLOs / throughput / reclaim / janitor / doorbell)
    Queuebench(queuebench::QueueBenchArgs),
    /// Run the 5.14 failover gates (checkpoint/resume on replica loss / janitor completion-race guard)
    Failoverbench(failoverbench::FailoverBenchArgs),
    /// Run the fqg.8 production runner gate (RunWorker drains run_queue to completion; drive+reuse+empty)
    Runnerbench(runnerbench::RunnerBenchArgs),
    /// Run the 5.14 dispatcher gates (cron / ordering / race / fairness / wake / live)
    Dispatchbench(dispatchbench::DispatchBenchArgs),
    /// Run the EVT-NATS data-plane JetStream gate (publish / consume / Nats-Msg-Id dedupe / R3 node-loss heal)
    Streambench(streambench::StreamBenchArgs),
    /// Assert an EVT_ stream holds a CDC reader's exact write program (order / dedupe / envelope shape) — the l5i9.10 gate's stream-side step
    Readerbench(readerbench::ReaderBenchArgs),
    /// Run the EVT-C-WAL-0 pre-CDC WAL-volume baseline (per-op WAL/op + representative-load bytes/s)
    Walbench(walbench::WalBenchArgs),
    /// Run the 5.9 credential-vault proof (delivery to serve-echo + no-leak containment)
    Credprobe(credprobe::CredProbeArgs),
    Credproof(credproof::CredProofArgs),
    /// Run the S4 custom-node gates (HTTP hop / interpreted-vs-composed / config parse)
    Nodebench(nodebench::NodeBenchArgs),
    /// Serve a wamn:node component over HTTP (S4 hop node host)
    ServeNode(nodebench::ServeNodeArgs),
    /// Run the 5.6/wamn-bd5 production custom-node invocation gate (real runner -> HTTP hop -> serve-node; grant + not-granted + config memoization)
    Nodeinvoke(nodeinvoke::NodeInvokeArgs),
    /// Run the S5 logging-capture gates (overhead / loss / drops / enrichment)
    Logbench(logbench::LogBenchArgs),
    /// Run the 9.1 OTel trace-pipeline gate (host spans → Tempo; enriched single trace)
    Tracebench(tracebench::TracebenchArgs),
    /// Run the 9.2 deployed cross-pod traceparent-propagation proof (relay → serve-echo)
    Traceproof(traceproof::TraceproofArgs),
    /// Serve the 9.2 reflecting upstream (echoes received trace headers as JSON)
    ServeEcho(traceproof::ServeEchoArgs),
    /// Run the S6 test-host plugin-swap gates (sameness / delay / egress / regression)
    Testhostbench(testhostbench::TestHostBenchArgs),
    /// Run the 2.6 DB-path egress review gate (no shipped workload imports wasi:sockets)
    Egressbench(egressbench::EgressBenchArgs),
    /// Run the E13a publish-time egress-guard refusal gate (a wasi:sockets importer is refused; a standard component publishes)
    Socketguard(socketguard::SocketGuardArgs),
    /// Run the l5i9.17 materializer gate (decide/refuse/enqueue/doorbell + C-MAT numbers)
    Matbench(matbench::MatBenchArgs),
    /// Run the wamn-3glr reader-inclusive RI-flip e2e gate (real reader → materializer: pre-flip refusal, live flip, post-flip scoped delete run, non-retroactive)
    Rie2ebench(rie2ebench::Rie2eBenchArgs),
    /// Run the l5i9.57 E10-e2e wamn:jetstream sample gate (bind/fetch/ack/publish/dedupe/reject via the js-sample guest)
    Samplebench(samplebench::SampleBenchArgs),
    /// Run the 4.1 generated-REST-API-gateway gates (CRUD / expand / RLS / injection)
    Apibench(apibench::ApiBenchArgs),
    /// Publish a catalog snapshot with the bundled 4.1b demo seed (wraps the
    /// prod publish-catalog and re-adds the gates-only --seed)
    PublishCatalog(publish_catalog_demo::PublishCatalogDemoArgs),
    /// Run the 4.1b in-cluster proof against a deployed api-gateway over HTTP
    Apiproof(apiproof::ApiProofArgs),
    /// Run the POC-F1 receipt-received gates (happy / holds / invalid / burst / rest)
    F1bench(f1bench::F1BenchArgs),
    /// Run the POC-F1 proof against the deployed poc-webhook-f1 + api-gateway over HTTP
    F1proof(f1proof::F1ProofArgs),
    /// Run the exec-ladder rung-1 conformance proof against the deployed runner (seed one manual run, assert it executes correctly)
    Ladderproof(ladderproof::LadderProofArgs),
}

fn main() -> anyhow::Result<()> {
    // The bench harnesses create stores through the same fork limiter the prod
    // host does; advertise the ceiling exactly like the prod binary.
    wamn_host::advertise_memory_ceiling();
    async_main()
}

#[tokio::main]
async fn async_main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let level = tracing::Level::from_str(&cli.log_level)
        .map_err(|_| anyhow::anyhow!("invalid log level: {}", cli.log_level))?;
    // OTel exporters activate when OTEL_* env vars are present.
    let shutdown_observability =
        wash_runtime::observability::initialize_observability(level, false, false)?;

    let result = match cli.command {
        Command::Bench(args) => bench::run(args).await,
        Command::Pgbench(args) => pgbench::run(args).await,
        Command::Provisionbench(args) => provisionbench::run(args).await,
        Command::Flowbench(args) => flowbench::run(args).await,
        Command::Queuebench(args) => queuebench::run(args).await,
        Command::Failoverbench(args) => failoverbench::run(args).await,
        Command::Runnerbench(args) => runnerbench::run(args).await,
        Command::Dispatchbench(args) => dispatchbench::run(args).await,
        Command::Streambench(args) => streambench::run(args).await,
        Command::Readerbench(args) => readerbench::run(args).await,
        Command::Walbench(args) => walbench::run(args).await,
        Command::Credprobe(args) => credprobe::run(args).await,
        Command::Credproof(args) => credproof::run(args).await,
        Command::Nodebench(args) => nodebench::run(args).await,
        Command::ServeNode(args) => nodebench::serve(args).await,
        Command::Nodeinvoke(args) => nodeinvoke::run(args).await,
        Command::Logbench(args) => logbench::run(args).await,
        Command::Tracebench(args) => tracebench::run(args).await,
        Command::Traceproof(args) => traceproof::run(args).await,
        Command::ServeEcho(args) => traceproof::serve_echo(args).await,
        Command::Testhostbench(args) => testhostbench::run(args).await,
        Command::Egressbench(args) => egressbench::run(args).await,
        Command::Socketguard(args) => socketguard::run(args).await,
        Command::Matbench(args) => matbench::run(args).await,
        Command::Rie2ebench(args) => rie2ebench::run(args).await,
        Command::Samplebench(args) => samplebench::run(args).await,
        Command::Apibench(args) => apibench::run(args).await,
        Command::PublishCatalog(args) => publish_catalog_demo::run(args).await,
        Command::Apiproof(args) => apiproof::run(args).await,
        Command::F1bench(args) => f1bench::run(args).await,
        Command::F1proof(args) => f1proof::run(args).await,
        Command::Ladderproof(args) => ladderproof::run(args).await,
    };

    shutdown_observability();
    result
}
