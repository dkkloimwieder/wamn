use clap::{CommandFactory as _, Parser};
use wamn_environment_control::provision_project_env::ensure_distinct_secret_paths;

use super::*;

#[derive(Debug, Parser)]
struct TestCli {
    #[command(flatten)]
    args: ProvisionProjectEnvArgs,
}

fn parse_args(extra: &[&str]) -> Result<ProvisionProjectEnvArgs, clap::Error> {
    let mut argv = vec![
        "test",
        "--org",
        "acme",
        "--project",
        "billing",
        "--env",
        "dev",
    ];
    argv.extend_from_slice(extra);
    TestCli::try_parse_from(argv).map(|cli| cli.args)
}

/// The non-revoke path may treat the identity triple as an infallible parser
/// invariant: Clap rejects every provisioning invocation missing one member,
/// and [`run`] returns before the infallible accesses in the sole exempt mode.
#[test]
fn clap_guards_the_three_infallible_provisioning_identity_accesses() {
    for omitted in ["--org", "--project", "--env"] {
        let mut argv = vec![
            "test",
            "--org",
            "acme",
            "--project",
            "billing",
            "--env",
            "dev",
            "--cluster",
            "acme-dev",
            "--emit-secret",
            "/tmp/db.json",
        ];
        let at = argv
            .iter()
            .position(|arg| *arg == omitted)
            .expect("the omitted flag is in the complete invocation");
        argv.drain(at..=at + 1);

        let error = TestCli::try_parse_from(argv)
            .expect_err("provisioning accepted a missing identity member");
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument,
            "missing {omitted} failed for the wrong reason: {error}"
        );
    }

    // wamn-hopk R5: a source-text scan counting `.expect(` calls between two
    // function-name markers stood here. Deleted; the clap arms above test
    // the parser contract by invoking the parser.
}

#[test]
fn pat_issue_flags_select_independently_and_revoke_conflicts() {
    let management = parse_args(&[
        "--emit-secret",
        "/tmp/db.json",
        "--emit-management-author-pat-secret",
        "/tmp/management.json",
    ])
    .unwrap();
    assert!(management.emit_management_author_pat_secret.is_some());
    assert!(management.emit_operator_pat_secret.is_none());

    let route = parse_args(&[
        "--emit-secret",
        "/tmp/db.json",
        "--emit-operator-pat-secret",
        "/tmp/route.json",
    ])
    .unwrap();
    assert!(route.emit_management_author_pat_secret.is_none());
    assert!(route.emit_operator_pat_secret.is_some());

    let both = parse_args(&[
        "--emit-secret",
        "/tmp/db.json",
        "--emit-management-author-pat-secret",
        "/tmp/management.json",
        "--emit-operator-pat-secret",
        "/tmp/route.json",
    ])
    .unwrap();
    assert!(both.emit_management_author_pat_secret.is_some());
    assert!(both.emit_operator_pat_secret.is_some());

    let transport = parse_args(&[
        "--emit-secret",
        "/tmp/db.json",
        "--emit-operator-pat-secret",
        "/tmp/route.json",
        "--pat-issuer",
        "https://identity.example/authority",
        "--pat-client-cert",
        "/tmp/operator.pem",
        "--pat-client-key",
        "/tmp/operator.key",
        "--pat-server-ca",
        "/tmp/server-ca.pem",
    ])
    .unwrap();
    assert_eq!(
        transport.pat_issuer.endpoint.as_deref(),
        Some("https://identity.example/authority")
    );
    assert_eq!(
        transport.pat_issuer.client_cert.as_deref(),
        Some(Path::new("/tmp/operator.pem"))
    );
    assert_eq!(
        transport.pat_issuer.client_key.as_deref(),
        Some(Path::new("/tmp/operator.key"))
    );
    assert_eq!(
        transport.pat_issuer.server_ca.as_deref(),
        Some(Path::new("/tmp/server-ca.pem"))
    );

    let revoke = TestCli::try_parse_from([
        "test",
        "--system-database-url",
        "postgresql://postgres@localhost/postgres",
        "--revoke-pat-prefix",
        "0123456789abcdef",
    ])
    .unwrap()
    .args;
    assert_eq!(
        revoke.revoke_pat_prefix.as_deref(),
        Some("0123456789abcdef")
    );
    assert!(revoke.emit_secret.is_none());
    assert!(revoke.org.is_none());
    assert!(revoke.project.is_none());
    assert!(revoke.env.is_none());

    for issue_flag in [
        "--emit-management-author-pat-secret",
        "--emit-operator-pat-secret",
    ] {
        assert!(
            parse_args(&[
                "--revoke-pat-prefix",
                "0123456789abcdef",
                issue_flag,
                "/tmp/pat.json",
            ])
            .is_err(),
            "revoke accepted conflicting {issue_flag}"
        );
    }
}

#[tokio::test]
async fn pat_transport_configuration_refuses_before_provisioning_effects() {
    for (issuer, expected_error) in [
        (PatIssuerArgs::default(), "requires --pat-issuer"),
        (
            PatIssuerArgs {
                endpoint: Some("http://identity.example".to_owned()),
                ..Default::default()
            },
            "must be an HTTPS URL",
        ),
        (
            PatIssuerArgs {
                endpoint: Some("https://identity.example".to_owned()),
                client_cert: Some(PathBuf::from("/private-marker/operator.pem")),
                ..Default::default()
            },
            "requires --pat-client-key",
        ),
    ] {
        let mut args = parse_args(&[
            "--emit-secret",
            "/tmp/pat-config-refusal-db.json",
            "--emit-operator-pat-secret",
            "/tmp/pat-config-refusal-route.json",
        ])
        .unwrap();
        // These values fail later guards or the DB connection. TLS refusal
        // must occur first, without reaching either provisioning operation.
        args.project = Some("wamn-reserved".to_owned());
        args.system_database_url = Some("invalid-database-url".to_owned());
        args.target = Some("cloud".to_owned());
        args.pat_issuer = issuer;
        let error = provision(args).await.unwrap_err();
        assert!(error.to_string().contains(expected_error), "{error}");
        assert!(!format!("{error:#} {error:?}").contains("private-marker"));
    }
}

#[test]
fn pat_transport_flags_hide_environment_values() {
    let command = TestCli::command();
    for name in ["endpoint", "client_cert", "client_key", "server_ca"] {
        let argument = command
            .get_arguments()
            .find(|arg| arg.get_id() == name)
            .unwrap();
        assert!(argument.is_hide_env_values_set(), "{name}");
    }
}

#[test]
fn every_secret_output_rejects_stdout_and_prefix_is_strict() {
    assert!(
        parse_args(&[]).is_err(),
        "database Secret path became optional"
    );
    assert!(parse_args(&["--emit-secret", "-"]).is_err());
    // A PAT-only rerun writes no database Secret (wamn-6b4g).
    for issue_flag in [
        "--emit-management-author-pat-secret",
        "--emit-operator-pat-secret",
    ] {
        let pat_only = parse_args(&[issue_flag, "/tmp/pat.json"]).unwrap();
        assert!(pat_only.emit_secret.is_none(), "{issue_flag}");
    }
    for issue_flag in [
        "--emit-management-author-pat-secret",
        "--emit-operator-pat-secret",
    ] {
        assert!(
            parse_args(&["--emit-secret", "/tmp/db.json", issue_flag, "-"]).is_err(),
            "{issue_flag} accepted stdout"
        );
    }
    for invalid in ["0123456789abcde", "0123456789ABCDEF", "0123456789abcdeg"] {
        assert!(
            parse_args(&["--revoke-pat-prefix", invalid]).is_err(),
            "accepted invalid PAT prefix {invalid:?}"
        );
    }
}

#[test]
fn parsed_credential_outputs_must_name_distinct_files() {
    for argv in [
        vec![
            "--emit-secret",
            "/tmp/shared-credential.json",
            "--emit-management-author-pat-secret",
            "/tmp/shared-credential.json",
        ],
        vec![
            "--emit-secret",
            "/tmp/shared-credential.json",
            "--emit-operator-pat-secret",
            "/tmp/shared-credential.json",
        ],
        vec![
            "--emit-secret",
            "/tmp/shared-credential.json",
            "--emit-management-author-pat-secret",
            "/tmp/./shared-credential.json",
        ],
        vec![
            "--emit-secret",
            "/tmp/db.json",
            "--emit-management-author-pat-secret",
            "/tmp/shared-credential.json",
            "--emit-operator-pat-secret",
            "/tmp/shared-credential.json",
        ],
    ] {
        let args = parse_args(&argv).unwrap();
        let error = ensure_distinct_secret_paths([
            ("--emit-secret", args.emit_secret.as_deref()),
            (
                "--emit-management-author-pat-secret",
                args.emit_management_author_pat_secret.as_deref(),
            ),
            (
                "--emit-operator-pat-secret",
                args.emit_operator_pat_secret.as_deref(),
            ),
        ])
        .expect_err("duplicate credential output was accepted");
        assert!(error.to_string().contains("must name distinct"));
    }

    let args = parse_args(&[
        "--emit-secret",
        "/tmp/db.json",
        "--emit-management-author-pat-secret",
        "/tmp/management.json",
        "--emit-operator-pat-secret",
        "/tmp/route.json",
    ])
    .unwrap();
    ensure_distinct_secret_paths([
        ("--emit-secret", args.emit_secret.as_deref()),
        (
            "--emit-management-author-pat-secret",
            args.emit_management_author_pat_secret.as_deref(),
        ),
        (
            "--emit-operator-pat-secret",
            args.emit_operator_pat_secret.as_deref(),
        ),
    ])
    .unwrap();
}

#[test]
fn provisioning_summary_contains_no_database_credentials() {
    let triple = Triple::new("acme", "dev", "billing");
    let summary = provision_summary(&triple, "wamn-db-acme--billing--dev--k3m9x2p7", "acme-dev");
    assert!(!summary.contains("postgres://"));
    assert!(!summary.contains("password"));
    assert!(!summary.contains("app url"));
}
/// The exempt-mode half of `wamn-0h0g.12.141`, kept after `wamn-xv69`
/// deleted the `--app-password` half it was written beside.
///
/// The modes that provision nothing reach neither [`compose_url`] nor
/// [`role_sql`], so the parser must not demand a database `Secret` they would
/// immediately discard.
///
/// Deliberately built without [`parse_args`]'s extras, which would supply the
/// Secret and leave every assertion here vacuous.
#[test]
fn the_credential_free_modes_parse_without_a_database_secret() {
    let revoke = TestCli::try_parse_from([
        "test",
        "--system-database-url",
        "postgresql://postgres@localhost/postgres",
        "--revoke-pat-prefix",
        "0123456789abcdef",
    ])
    .expect("revoke provisions nothing and needs no database Secret")
    .args;
    assert!(revoke.emit_secret.is_none());
}

mod enable_cdc {
    use std::time::Duration;

    use clap::Parser;

    use super::super::*;

    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(flatten)]
        args: EnableCdcProjectEnvArgs,
    }

    /// No `default_value` on `--replication-password` (wamn-0h0g.12.134). A
    /// default minted a `LOGIN REPLICATION` role with a publicly known
    /// password, and `REPLICATION` authority is cluster-wide — a stolen
    /// credential can open a replication session against any database on the
    /// cluster and decode co-project WAL, which slot/publication naming does not
    /// constrain. This test exists so the argument cannot quietly re-acquire
    /// one.
    #[test]
    fn the_replication_password_has_no_default() {
        let base = [
            "test",
            "--org",
            "acme",
            "--project",
            "billing",
            "--env",
            "dev",
            "--stream-replicas",
            "1",
            "--dup-window-secs",
            "120",
        ];
        assert!(
            TestCli::try_parse_from(base).is_err(),
            "CDC enablement accepted a missing --replication-password"
        );
        let mut with = base.to_vec();
        with.extend_from_slice(&["--replication-password", "probe"]);
        assert_eq!(
            TestCli::try_parse_from(with)
                .unwrap()
                .args
                .replication_password,
            "probe"
        );
    }

    #[test]
    fn cdc_command_accepts_native_consumer_declarations() {
        let consumer = wamn_broker::events::materializer_consumer_config(
            "mat_t_pkg_r1",
            "evt.acme.dev.billing.invoice.>",
            Duration::from_secs(30),
            5,
        );
        let json = serde_json::to_string(&consumer).unwrap();
        let parsed = TestCli::try_parse_from([
            "test",
            "--org",
            "acme",
            "--project",
            "billing",
            "--env",
            "dev",
            "--replication-password",
            "test-password",
            "--stream-replicas",
            "1",
            "--dup-window-secs",
            "60",
            "--consumer-config",
            &json,
        ])
        .unwrap()
        .args;
        assert_eq!(parsed.stream_replicas, 1);
        assert_eq!(parsed.dup_window_secs, 60);
        assert_eq!(
            serde_json::from_str::<async_nats::jetstream::consumer::pull::Config>(
                &parsed.consumer_config[0]
            )
            .unwrap(),
            consumer
        );
    }
}

/// The CLI's template values name the shipped presets the library stamps from.
#[test]
fn every_template_arg_names_a_shipped_template() {
    for (arg, name) in [
        (TemplateArg::Trials, "trials"),
        (TemplateArg::Standard, "standard"),
        (TemplateArg::Dedicated, "dedicated"),
    ] {
        assert_eq!(arg.template().name, name);
    }
}
