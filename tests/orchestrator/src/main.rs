//! Orchestration facade for retained test inputs and repository fixtures.
//!
//! MVP outcome: test coverage.
//!
//! `wamn-gates` retains the routing needed by the MVP test inputs while their
//! implementations live in explicit conformance, integration, system, and
//! test-support homes. Tests that import service clients remain integration
//! evidence even when they also exercise a deployed endpoint.

// Each test implementation is owned and compiled by its tier package. This
// binary is only the stable deploy-facing command router.
use wamn_conformance_tests::socketguard;
use wamn_integration_tests::agent_pilot;
use wamn_integration_tests::{
    dashboard_test, host_session_test, identity_keys_test, identity_session_test, membership_test,
    readerbench, rc, retention,
};
use wamn_system_tests::trace_test;

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
    AgentPilotRun(agent_pilot::RunArgs),
    AgentPilotGrade(agent_pilot::GradeArgs),
    /// Prove session acceptance and removed-key refusal on two deployed hosts.
    #[command(name = "host-session-proof")]
    HostSessionTest(host_session_test::HostSessionTestArgs),
    /// Prove public JWKS and cache evidence through deployed identity HTTPS.
    #[command(name = "identity-jwks")]
    IdentityKeysTest(identity_keys_test::IdentityKeysTestArgs),
    /// Prove fresh PAT exchange and signed environment claims through HTTPS.
    #[command(name = "identity-session")]
    IdentitySessionTest(identity_session_test::IdentitySessionTestArgs),
    /// Mint private credentials only inside an explicitly armed disposable fixture.
    #[command(name = "identity-session-fixture")]
    IdentitySessionFixture(identity_session_test::IdentitySessionFixtureArgs),
    /// Prove fresh human membership through a deployed Receiving HTTP route.
    #[command(name = "membershipproof")]
    MembershipTest(membership_test::MembershipTestArgs),
    /// Prove the real prune-run-history verb removes only old TERMINAL runs, keeping recent and non-terminal history.
    Retention(retention::RetentionArgs),
    /// Assert an EVT_ stream holds a CDC reader's exact write program (order / dedupe / envelope shape) — the l5i9.10 gate's stream-side step
    Readerbench(readerbench::ReaderBenchArgs),
    /// Serve the 9.2 reflecting upstream (echoes received trace headers as JSON)
    ServeEcho(trace_test::ServeEchoArgs),
    /// Run the E13a publish-time egress-guard refusal gate (a wasi:sockets importer is refused; a standard component publishes)
    Socketguard(socketguard::SocketGuardArgs),
    /// Run the 9.2 trace-inject gate: prove the host stamps `traceparent` on both the P2 and P3 outbound surfaces, read back from serve-echo
    #[command(name = "traceproof")]
    TraceTest(trace_test::TraceTestArgs),
    /// Run the 9.9 dashboards gate: assert a deployed Grafana's health, its datasources, and the static plus per-tenant folders and dashboards
    #[command(name = "dashproof")]
    DashboardTest(dashboard_test::DashboardTestArgs),
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

    let mut exit_code = 0u8;
    let result = match cli.command {
        Command::Rc(args) => rc::run(args).await,
        Command::AgentPilotRun(args) => {
            exit_code = agent_pilot::run(args).await;
            Ok(())
        }
        Command::AgentPilotGrade(args) => {
            exit_code = agent_pilot::grade(args).await;
            Ok(())
        }
        Command::HostSessionTest(args) => host_session_test::run(args).await,
        Command::IdentityKeysTest(args) => identity_keys_test::run(args).await,
        Command::IdentitySessionTest(args) => identity_session_test::run(args).await,
        Command::IdentitySessionFixture(args) => identity_session_test::fixture(args).await,
        Command::MembershipTest(args) => membership_test::run(args).await,
        Command::Retention(args) => retention::run(args).await,
        Command::Readerbench(args) => readerbench::run(args).await,
        Command::ServeEcho(args) => trace_test::serve_echo(args).await,
        Command::Socketguard(args) => socketguard::run(args).await,
        Command::TraceTest(args) => trace_test::run(args).await,
        Command::DashboardTest(args) => dashboard_test::run(args).await,
    };

    shutdown_observability();
    if exit_code != 0 {
        std::process::exit(i32::from(exit_code));
    }
    result
}
