//! Orchestration facade for retained proof inputs and repository fixtures.
//!
//! `wamn-gates` retains the routing needed by the MVP proof inputs while their
//! implementations live in explicit conformance, integration, system, and
//! test-support homes. Proofs that import service clients remain integration
//! evidence even when they also exercise a deployed endpoint.

// Each proof implementation is owned and compiled by its tier package. This
// binary is only the stable deploy-facing command router.
use wamn_proof_conformance::{credprobe, socketguard};
use wamn_proof_integration::{
    capturebench, causation_e2e, credproof, impactproof, invocationproof, nodebench, nodeinvoke,
    readerbench, runnerbench, suiteproof, testkitbench, wakeproof,
};
use wamn_proof_system::traceproof;

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
    /// Prove admitted full/off node I/O capture, oversized-output metadata, redaction, and retention.
    Capturebench(capturebench::CaptureBenchArgs),
    /// Run the fqg.8 production runner gate (ExecutionHost drains run_queue to completion; drive+reuse+empty)
    Runnerbench(runnerbench::RunnerBenchArgs),
    /// Assert an EVT_ stream holds a CDC reader's exact write program (order / dedupe / envelope shape) — the l5i9.10 gate's stream-side step
    Readerbench(readerbench::ReaderBenchArgs),
    /// Prove one admitted invocation through the deployed runner, WAL reader, and R3 stream.
    CausationE2e(causation_e2e::CausationE2eArgs),
    /// Run the 5.9 credential-vault proof (delivery to serve-echo + no-leak containment)
    Credprobe(credprobe::CredProbeArgs),
    Credproof(credproof::CredProofArgs),
    /// Serve a wamn:node component over HTTP (S4 hop node host)
    ServeNode(nodebench::ServeNodeArgs),
    /// Run the 5.6/wamn-bd5 production custom-node invocation gate (real runner -> HTTP hop -> serve-node; grant + not-granted + config memoization)
    Nodeinvoke(nodeinvoke::NodeInvokeArgs),
    /// Serve the 9.2 reflecting upstream (echoes received trace headers as JSON)
    ServeEcho(traceproof::ServeEchoArgs),
    /// Route the temporarily retained persisted selector protocol to the product stored-test worker.
    Testkitbench(testkitbench::TestKitBenchArgs),
    /// Run the E13a publish-time egress-guard refusal gate (a wasi:sockets importer is refused; a standard component publishes)
    Socketguard(socketguard::SocketGuardArgs),
    /// Run the POC-F3 scale-to-zero wake proof (park the runner at 0; a LIVE dispatcher cron fire wakes it via the waker and it completes)
    Wakeproof(wakeproof::WakeProofArgs),
    /// Run the 11.2 flow test-suite gate (test cases as catalog data: envelope round-trip + version binding + RLS + FK cascade in an ephemeral schema)
    Suiteproof(suiteproof::SuiteProofArgs),
    /// Run the 11.8 schema-change impact-analysis gate (wamn-wvb): seed a name-keyed node-config flow + suite in an ephemeral schema, then assert `wamn-ctl impact-report` names the affected flow/suite/api resource and gates a destructive change with dependents behind acknowledgement
    Impactproof(impactproof::ImpactProofArgs),
    /// Prove exact claimed-run execution through the production host and baked flowrunner image.
    Invocationproof(invocationproof::InvocationProofArgs),
}

fn main() -> anyhow::Result<()> {
    // The bench harnesses create stores through the same fork limiter the prod
    // host does; advertise the ceiling exactly like the prod binary.
    wamn_runtime::advertise_memory_ceiling();
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
        Command::Capturebench(args) => capturebench::run(args).await,
        Command::Runnerbench(args) => runnerbench::run(args).await,
        Command::Readerbench(args) => readerbench::run(args).await,
        Command::CausationE2e(args) => causation_e2e::run(args).await,
        Command::Credprobe(args) => credprobe::run(args).await,
        Command::Credproof(args) => credproof::run(args).await,
        Command::ServeNode(args) => nodebench::serve(args).await,
        Command::Nodeinvoke(args) => nodeinvoke::run(args).await,
        Command::ServeEcho(args) => traceproof::serve_echo(args).await,
        Command::Testkitbench(args) => testkitbench::run(args).await,
        Command::Socketguard(args) => socketguard::run(args).await,
        Command::Wakeproof(args) => wakeproof::run(args).await,
        Command::Suiteproof(args) => suiteproof::run(args).await,
        Command::Impactproof(args) => impactproof::run(args).await,
        Command::Invocationproof(args) => invocationproof::run(args).await,
    };

    shutdown_observability();
    result
}
