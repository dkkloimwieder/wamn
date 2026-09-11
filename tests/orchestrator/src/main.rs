//! Orchestration facade for retained proof inputs and repository fixtures.
//!
//! MVP outcome: proof floor.
//!
//! `wamn-gates` retains the routing needed by the MVP proof inputs while their
//! implementations live in explicit conformance, integration, system, and
//! test-support homes. Proofs that import service clients remain integration
//! evidence even when they also exercise a deployed endpoint.

// Each proof implementation is owned and compiled by its tier package. This
// binary is only the stable deploy-facing command router.
use wamn_proof_conformance::socketguard;
use wamn_proof_integration::{
    dashproof, host_session_proof, identity_keys_proof, identity_session_proof, membershipproof,
    readerbench, rc, retention,
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
    /// Run the RC bootstrap and native socket and trace tests on an owned cluster.
    Rc(rc::RcArgs),
    /// Prove session acceptance and removed-key refusal on two deployed hosts.
    #[command(name = "host-session-proof")]
    HostSessionProof(host_session_proof::HostSessionProofArgs),
    /// Prove public JWKS and cache evidence through deployed identity HTTPS.
    #[command(name = "identity-jwks")]
    IdentityJwks(identity_keys_proof::IdentityKeysProofArgs),
    /// Prove fresh PAT exchange and signed environment claims through HTTPS.
    #[command(name = "identity-session")]
    IdentitySession(identity_session_proof::IdentitySessionProofArgs),
    /// Mint private credentials only inside an explicitly armed disposable fixture.
    #[command(name = "identity-session-fixture")]
    IdentitySessionFixture(identity_session_proof::IdentitySessionFixtureArgs),
    /// Prove fresh human membership through a deployed Receiving HTTP route.
    Membershipproof(membershipproof::MembershipProofArgs),
    /// Prove the real prune-run-history verb removes only old TERMINAL runs, keeping recent and non-terminal history.
    Retention(retention::RetentionArgs),
    /// Assert an EVT_ stream holds a CDC reader's exact write program (order / dedupe / envelope shape) — the l5i9.10 gate's stream-side step
    Readerbench(readerbench::ReaderBenchArgs),
    /// Serve the 9.2 reflecting upstream (echoes received trace headers as JSON)
    ServeEcho(traceproof::ServeEchoArgs),
    /// Run the E13a publish-time egress-guard refusal gate (a wasi:sockets importer is refused; a standard component publishes)
    Socketguard(socketguard::SocketGuardArgs),
    /// Run the 9.2 trace-inject gate: prove the host stamps `traceparent` on both the P2 and P3 outbound surfaces, read back from serve-echo
    Traceproof(traceproof::TraceproofArgs),
    /// Run the 9.9 dashboards gate: assert a deployed Grafana's health, its datasources, and the static plus per-tenant folders and dashboards
    Dashproof(dashproof::DashproofArgs),
}

fn main() -> anyhow::Result<()> {
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
        Command::Rc(args) => rc::run(args).await,
        Command::HostSessionProof(args) => host_session_proof::run(args).await,
        Command::IdentityJwks(args) => identity_keys_proof::run(args).await,
        Command::IdentitySession(args) => identity_session_proof::run(args).await,
        Command::IdentitySessionFixture(args) => identity_session_proof::fixture(args).await,
        Command::Membershipproof(args) => membershipproof::run(args).await,
        Command::Retention(args) => retention::run(args).await,
        Command::Readerbench(args) => readerbench::run(args).await,
        Command::ServeEcho(args) => traceproof::serve_echo(args).await,
        Command::Socketguard(args) => socketguard::run(args).await,
        Command::Traceproof(args) => traceproof::run(args).await,
        Command::Dashproof(args) => dashproof::run(args).await,
    };

    shutdown_observability();
    result
}
