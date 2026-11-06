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
use wamn_conformance_tests::socket_test;
use wamn_integration_tests::{
    cdcbench, dashboard_test, host_session_test, identity_keys_test, identity_session_test,
    membership_test, readerbench, streambench, walbench,
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
    /// Test session acceptance and removed-key refusal on two deployed hosts.
    HostSessionTest(host_session_test::HostSessionTestArgs),
    /// Test public JWKS and cache evidence through deployed identity HTTPS.
    IdentityKeysTest(identity_keys_test::IdentityKeysTestArgs),
    /// Test fresh PAT exchange and signed environment claims through HTTPS.
    IdentitySessionTest(identity_session_test::IdentitySessionTestArgs),
    /// Seed credentials or change membership in an explicitly armed disposable fixture.
    #[command(name = "identity-session-fixture")]
    IdentitySessionFixture(identity_session_test::IdentitySessionFixtureArgs),
    /// Test fresh user membership through a deployed Receiving HTTP route.
    MembershipTest(membership_test::MembershipTestArgs),
    /// Assert an EVT_ stream holds a CDC reader's exact write program (order / dedupe / envelope shape) — the l5i9.10 gate's stream-side step
    Readerbench(readerbench::ReaderBenchArgs),
    /// Measure database change capture throughput, lag, and logging cost.
    Cdcbench(cdcbench::CdcBenchArgs),
    /// Measure PostgreSQL log volume before change capture.
    Walbench(walbench::WalBenchArgs),
    /// Test event stream storage, ordering, deduplication, and recovery.
    Streambench(streambench::StreamBenchArgs),
    /// Serve the 9.2 reflecting upstream (echoes received trace headers as JSON)
    ServeEcho(trace_test::ServeEchoArgs),
    /// Run the E13a publish-time egress-guard refusal gate (a wasi:sockets importer is refused; a standard component publishes)
    SocketTest(socket_test::SocketTestArgs),
    /// Run the 9.2 trace test: assert that the host stamps `traceparent` on both the P2 and P3 outbound surfaces, read back from serve-echo
    TraceTest(trace_test::TraceTestArgs),
    /// Run the 9.9 dashboards gate: assert a deployed Grafana's health, its datasources, and the static plus per-tenant folders and dashboards
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

    let result = match cli.command {
        Command::HostSessionTest(args) => host_session_test::run(args).await,
        Command::IdentityKeysTest(args) => identity_keys_test::run(args).await,
        Command::IdentitySessionTest(args) => identity_session_test::run(args).await,
        Command::IdentitySessionFixture(args) => identity_session_test::fixture(args).await,
        Command::MembershipTest(args) => membership_test::run(args).await,
        Command::Readerbench(args) => readerbench::run(args).await,
        Command::Cdcbench(args) => cdcbench::run(args).await,
        Command::Walbench(args) => walbench::run(args).await,
        Command::Streambench(args) => streambench::run(args).await,
        Command::ServeEcho(args) => trace_test::serve_echo(args).await,
        Command::SocketTest(args) => socket_test::run(args),
        Command::TraceTest(args) => trace_test::run(args).await,
        Command::DashboardTest(args) => dashboard_test::run(args).await,
    };

    shutdown_observability();
    result
}

#[cfg(test)]
mod tests {
    use super::{Cli, Command};
    use clap::Parser as _;
    use wamn_integration_tests::identity_session_test::IdentitySessionFixtureCommand;

    #[test]
    fn membership_fixture_steps_parse_their_scope_without_credentials_or_output() {
        for step in ["grant-membership", "revoke-membership"] {
            let cli = Cli::try_parse_from([
                "wamn-gates",
                "identity-session-fixture",
                step,
                "--org",
                "acme",
                "--project",
                "shared",
                "--env",
                "dev",
                "--principal-id",
                "00112233-4455-6677-8899-aabbccddeeff",
            ])
            .unwrap();
            let Command::IdentitySessionFixture(args) = cli.command else {
                panic!("fixture command")
            };
            assert!(args.output.is_none());
            let args = match args.command.unwrap() {
                IdentitySessionFixtureCommand::GrantMembership(args) => {
                    assert_eq!(step, "grant-membership");
                    args
                }
                IdentitySessionFixtureCommand::RevokeMembership(args) => {
                    assert_eq!(step, "revoke-membership");
                    args
                }
            };
            assert_eq!(args.org, "acme");
            assert_eq!(args.project, "shared");
            assert_eq!(args.env, "dev");
            assert_eq!(args.principal_id, "00112233-4455-6677-8899-aabbccddeeff");
        }
    }

    #[test]
    fn credential_fixture_requires_its_output_and_refuses_membership_steps_with_it() {
        assert!(Cli::try_parse_from(["wamn-gates", "identity-session-fixture"]).is_err());
        let cli = Cli::try_parse_from([
            "wamn-gates",
            "identity-session-fixture",
            "--output",
            "/tmp/identity.json",
        ])
        .unwrap();
        let Command::IdentitySessionFixture(args) = cli.command else {
            panic!("fixture command")
        };
        assert_eq!(
            args.output.unwrap(),
            std::path::Path::new("/tmp/identity.json")
        );
        assert!(args.command.is_none());
        assert!(
            Cli::try_parse_from([
                "wamn-gates",
                "identity-session-fixture",
                "--output",
                "/tmp/identity.json",
                "grant-membership",
                "--org",
                "acme",
                "--project",
                "shared",
                "--env",
                "dev",
                "--principal-id",
                "00112233-4455-6677-8899-aabbccddeeff",
            ])
            .is_err()
        );
    }
}
