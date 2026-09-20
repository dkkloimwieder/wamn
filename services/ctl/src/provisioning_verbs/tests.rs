use clap::{CommandFactory as _, Parser};
use wamn_control::provision_project_env::{ensure_distinct_secret_paths, role_sql};

use super::*;

/// The boundary the source-scanning guards below split this file on, so they
/// read the IMPLEMENTATION half only (wamn-3o3a).
///
/// Every signature those guards search for is also spelled in this module, so
/// a scan that reaches the test half lets a DELETED subject match the test's
/// own search string: the `expect` never fires and the span silently
/// collapses onto test source. Splitting on the bare attribute is not enough
/// — that string is spelled here too, so it holds only while the real
/// attribute happens to come first, and a boundary that stops matching (an
/// inner `cfg`, a reshaped attribute) reopens the collapse with no failure.
///
/// This literal is immune because in source it is written with an ESCAPED
/// `\n`, so it cannot match its own spelling — only the real module header.
/// Locating it with `find` also makes the boundary's absence LOUD, where
/// `split(..).next()` is infallible and can never report it.
const TEST_MODULE_BOUNDARY: &str = "\n#[cfg(test)]\nmod tests;";

/// This file's IMPLEMENTATION half — everything before the test module.
fn implementation_source() -> &'static str {
    const SOURCE: &str = include_str!("../provisioning_verbs.rs");
    let boundary = SOURCE
        .find(TEST_MODULE_BOUNDARY)
        .expect("the test module header is where a source scan must stop");
    &SOURCE[..boundary]
}

#[derive(Debug, Parser)]
struct TestCli {
    #[command(flatten)]
    args: ProvisionProjectEnvArgs,
}

/// The parser over a DERIVED command line, which a fixed-size array cannot
/// express.
fn parse_argv(argv: Vec<String>) -> Result<ProvisionProjectEnvArgs, clap::Error> {
    TestCli::try_parse_from(argv).map(|cli| cli.args)
}

/// `["test", "--org", .., "--env", "dev"]` plus whatever the caller adds.
fn action_argv(extra: &[&str]) -> Vec<String> {
    let mut argv: Vec<String> = [
        "test",
        "--org",
        "acme",
        "--project",
        "billing",
        "--env",
        "dev",
    ]
    .iter()
    .map(|arg| (*arg).to_string())
    .collect();
    argv.extend(extra.iter().map(|arg| (*arg).to_string()));
    argv
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
    // function-name markers stood here. Deleted; the clap arms above check
    // the parser contract by invoking the parser.
}

/// Every workload action is a non-revoke invocation, so the same Clap
/// contract makes its identity accesses infallible in every action mode.
#[test]
fn clap_guards_the_workload_identity_accesses_in_every_action_mode() {
    for action in every_action_flag() {
        for omitted in ["--org", "--project", "--env"] {
            let mut argv = action_argv(&[&action, "a"]);
            let at = argv
                .iter()
                .position(|arg| arg == omitted)
                .expect("the omitted flag is in the complete action invocation");
            argv.drain(at..=at + 1);

            let error =
                parse_argv(argv).expect_err("a workload action accepted a missing identity member");
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::MissingRequiredArgument,
                "{action} missing {omitted} failed for the wrong reason: {error}"
            );
        }
    }

    // wamn-hopk R5: a source-text scan counting `.expect(` calls between two
    // function-name markers stood here. Deleted; the clap arms above check
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
    assert!(management.emit_route_caller_pat_secret.is_none());

    let route = parse_args(&[
        "--emit-secret",
        "/tmp/db.json",
        "--emit-route-caller-pat-secret",
        "/tmp/route.json",
    ])
    .unwrap();
    assert!(route.emit_management_author_pat_secret.is_none());
    assert!(route.emit_route_caller_pat_secret.is_some());

    let both = parse_args(&[
        "--emit-secret",
        "/tmp/db.json",
        "--emit-management-author-pat-secret",
        "/tmp/management.json",
        "--emit-route-caller-pat-secret",
        "/tmp/route.json",
    ])
    .unwrap();
    assert!(both.emit_management_author_pat_secret.is_some());
    assert!(both.emit_route_caller_pat_secret.is_some());

    let transport = parse_args(&[
        "--emit-secret",
        "/tmp/db.json",
        "--emit-route-caller-pat-secret",
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

    let abort = parse_args(&[
        "--target-admin-database-url",
        "postgresql://postgres@localhost/wamn-db-acme--billing--dev",
        "--abort-management-admitter-generation",
        "a",
    ])
    .unwrap();
    assert_eq!(
        abort.workload.action,
        Some(WorkloadGenerationAction {
            family: WorkloadRoleFamily::ManagementAdmitter,
            verb: WorkloadActionVerb::Abort,
            generation: CredentialGeneration::A,
        })
    );
    assert!(abort.emit_secret.is_none());
    assert!(abort.workload.secret.is_none());

    for issue_flag in [
        "--emit-management-author-pat-secret",
        "--emit-route-caller-pat-secret",
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
            "--emit-route-caller-pat-secret",
            "/tmp/pat-config-refusal-route.json",
        ])
        .unwrap();
        // These values fail later guards or the DB connection. TLS refusal
        // must occur first, without reaching either provisioning operation.
        args.project = Some("wamn-reserved".to_owned());
        args.system_database_url = Some("invalid-database-url".to_owned());
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
    for issue_flag in [
        "--emit-management-author-pat-secret",
        "--emit-route-caller-pat-secret",
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
            "--emit-route-caller-pat-secret",
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
            "--emit-route-caller-pat-secret",
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
                "--emit-route-caller-pat-secret",
                args.emit_route_caller_pat_secret.as_deref(),
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
        "--emit-route-caller-pat-secret",
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
            "--emit-route-caller-pat-secret",
            args.emit_route_caller_pat_secret.as_deref(),
        ),
    ])
    .unwrap();
}

#[test]
fn provisioning_summary_contains_no_database_credentials() {
    let triple = Triple::new("acme", "billing", "dev");
    let summary = provision_summary(&triple, "wamn-db-acme--billing--dev--k3m9x2p7", "acme-dev");
    assert_eq!(
        summary,
        "project-env acme/billing/dev: database \"wamn-db-acme--billing--dev--k3m9x2p7\" on cluster \"acme-dev\" (owner wamn_db_owner)"
    );
    assert!(!summary.contains("postgres://"));
    assert!(!summary.contains("password"));
    assert!(!summary.contains("app url"));
}
/// The exempt-mode half of `wamn-0h0g.12.141`, kept after `wamn-xv69`
/// deleted the `--app-password` half it was written beside.
///
/// The modes that provision nothing reach neither [`compose_url`] nor
/// [`role_sql`], so the parser must not demand a database `Secret` they would
/// immediately discard — which is what forced `deploy/mvp/bootstrap.sh`'s
/// generation and revoke call sites to invent one.
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

    // EVERY family's action, derived — not the six that were remembered.
    // `wamn-0h0g.22.16` measured that the guest family's three flags were
    // MISSING from all three exempt lists, so `--prepare-guest-generation`
    // demanded `--emit-secret` while `run_guest_action` refused it: an
    // unrunnable mode. Deriving the exemption from the family set closes
    // that by construction rather than by remembering.
    for action in every_action_flag() {
        let parsed = parse_argv(action_argv(&[
            "--target-admin-database-url",
            "postgresql://postgres@localhost/wamn-db-acme--billing--dev",
            &action,
            "a",
        ]))
        .unwrap_or_else(|e| panic!("{action} demanded a database Secret: {e}"));
        assert!(
            parsed.emit_secret.is_none(),
            "{action} was made to name a database Secret it would discard"
        );
    }
}

/// Every derived action flag, in family order.
fn every_action_flag() -> Vec<String> {
    WorkloadRoleFamily::ALL
        .into_iter()
        .flat_map(|family| {
            WorkloadActionVerb::ALL
                .into_iter()
                .map(move |verb| format!("--{}", workload_action_flag(family, verb)))
        })
        .collect()
}

/// *** THE DELETION — `wamn-0h0g.22.16`'s load-bearing half. ***
///
/// SIXTEEN hand-written clap exclusion arrays named every family's three
/// flag ids, so admitting a family meant remembering to append to all
/// sixteen. A closed enum that must be appended to by hand in sixteen
/// places is not closed; it is a checklist.
///
/// The count is now ZERO, and this checks the stronger property the
/// acceptance actually asks for: NO family's flag or id is SPELLED anywhere
/// in the implementation. A list cannot name a family it never mentions, so
/// admitting a family cannot require an edit to any list.
#[test]
fn no_flag_exclusion_list_names_a_family_and_none_can() {
    let implementation = implementation_source();
    assert_eq!(
        implementation.matches("conflicts_with_all = [").count(),
        0,
        "a hand-written clap exclusion array is back"
    );
    // The only surviving requirement list names the single derived GROUP.
    // Sixteen arrays collapse to this one two-element expression, repeated
    // once per argument that owes it — never per family.
    let survivors: Vec<&str> = implementation
        .match_indices("required_unless_present_any = ")
        .map(|(at, _)| {
            implementation[at..]
                .lines()
                .next()
                .expect("the attribute occupies one line")
        })
        .collect();
    for line in &survivors {
        assert_eq!(
            *line, "required_unless_present_any = [\"revoke_pat_prefix\", WORKLOAD_ACTION_GROUP]",
            "an exclusion list grew members again"
        );
    }
    assert_eq!(
        survivors.len(),
        1,
        "the database Secret is the only argument a provisioning-only invocation still owes"
    );

    for family in WorkloadRoleFamily::ALL {
        let mut spellings = vec![workload_secret_flag(family), workload_secret_id(family)];
        for verb in WorkloadActionVerb::ALL {
            spellings.push(workload_action_flag(family, verb));
            spellings.push(workload_action_id(family, verb));
        }
        for spelling in spellings {
            assert!(
                !implementation.contains(&spelling),
                "{spelling:?} is spelled in the implementation; whatever names it \
                     would have to be edited to admit a family"
            );
        }
    }
}

/// The flag SET is a function of the family set, measured on the built
/// parser rather than asserted about the source.
///
/// An eleventh family reaches all four of its flags, both groups and every
/// exemption through this derivation, with no edit outside its own
/// declaration and its grant set.
#[test]
fn every_family_gets_its_flags_from_the_one_derivation() {
    let command = TestCli::command();
    let group = |id: &str| {
        command
            .get_groups()
            .find(|group| group.get_id().as_str() == id)
            .unwrap_or_else(|| panic!("the derived group {id} exists"))
    };
    let actions = group(WORKLOAD_ACTION_GROUP);
    assert_eq!(
        actions.get_args().count(),
        3 * WorkloadRoleFamily::ALL.len(),
        "the action group is three verbs per family and nothing else"
    );
    // `multiple(false)` is not readable off a `&ArgGroup`, so the
    // one-action rule is checked where it bites, by parsing: see
    // `one_action_group_excludes_every_pair_across_every_family`.
    let secrets = group(WORKLOAD_SECRET_GROUP);
    assert_eq!(secrets.get_args().count(), WorkloadRoleFamily::ALL.len());

    for family in WorkloadRoleFamily::ALL {
        for verb in WorkloadActionVerb::ALL {
            let id = workload_action_id(family, verb);
            assert!(
                command
                    .get_arguments()
                    .any(|arg| arg.get_id().as_str() == id),
                "{id} was not derived into the parser"
            );
            assert!(
                actions.get_args().any(|arg| arg.as_str() == id),
                "{id} is outside the one exclusion group"
            );
        }
        let secret = workload_secret_id(family);
        assert!(
            secrets.get_args().any(|arg| arg.as_str() == secret),
            "{secret} is outside the one Secret group"
        );
    }
}

/// One action per invocation, for EVERY pair across EVERY family — the
/// generalization of a nine-flag hand-written check that could only ever
/// cover the families someone remembered to list.
#[test]
fn one_action_group_excludes_every_pair_across_every_family() {
    let flags = every_action_flag();
    assert_eq!(flags.len(), 3 * WorkloadRoleFamily::ALL.len());
    for (index, first) in flags.iter().enumerate() {
        for second in &flags[index + 1..] {
            assert!(
                parse_argv(action_argv(&[first, "a", second, "b"])).is_err(),
                "{first} and {second} were accepted together"
            );
        }
    }
}

/// A credential Secret is bound to its OWN family's prepare, never to
/// another family's action, never to a retire or abort, and never to stdout.
#[test]
fn every_family_secret_is_bound_to_its_own_prepare() {
    for family in WorkloadRoleFamily::ALL {
        let secret = format!("--{}", workload_secret_flag(family));
        let prepare = format!(
            "--{}",
            workload_action_flag(family, WorkloadActionVerb::Prepare)
        );
        assert!(
            parse_argv(action_argv(&[&secret, "/tmp/workload.json"])).is_err(),
            "{secret} escaped its prepare requirement"
        );
        assert!(
            parse_argv(action_argv(&[&prepare, "a", &secret, "-"])).is_err(),
            "{secret} accepted stdout"
        );
        let parsed = parse_argv(action_argv(&[&prepare, "a", &secret, "/tmp/workload.json"]))
            .unwrap_or_else(|e| panic!("{prepare} with {secret} must parse: {e}"));
        assert_eq!(
            parsed.workload_secret_path(family),
            Some(Path::new("/tmp/workload.json"))
        );
        for verb in [WorkloadActionVerb::Retire, WorkloadActionVerb::Abort] {
            let other_verb = format!("--{}", workload_action_flag(family, verb));
            assert!(
                parse_argv(action_argv(&[
                    &other_verb,
                    "a",
                    &secret,
                    "/tmp/workload.json"
                ]))
                .is_err(),
                "{secret} accompanied {other_verb}"
            );
        }
        for other in WorkloadRoleFamily::ALL {
            if other == family {
                continue;
            }
            let foreign = format!("--{}", workload_secret_flag(other));
            assert!(
                parse_argv(action_argv(&[
                    &prepare,
                    "a",
                    &foreign,
                    "/tmp/workload.json"
                ]))
                .is_err(),
                "{prepare} accepted {foreign}, another family's Secret"
            );
        }
    }
}

/// The management-admitter family's own parse, split from
/// `the_management_admitter_action_is_one_more_stamp_of_the_workload_lifecycle`,
/// whose lifecycle half stays beside the implementation in `wamn-control`.
#[test]
fn the_management_admitter_action_parses_its_own_prepare_and_secret() {
    // One action per invocation and one Secret bound to its own prepare are
    // now group-derived properties, checked for EVERY pair and EVERY family
    // by `one_action_group_excludes_every_pair_across_every_family` and
    // `every_family_secret_is_bound_to_its_own_prepare` below. What stays
    // here is this family's own parse.
    let prepared = parse_argv(action_argv(&[
        "--prepare-management-admitter-generation",
        "a",
        "--emit-management-admitter-secret",
        "/tmp/management-admitter.json",
    ]))
    .expect("prepare with its Secret path parses");
    assert_eq!(
        prepared.workload.action,
        Some(WorkloadGenerationAction {
            family: WorkloadRoleFamily::ManagementAdmitter,
            verb: WorkloadActionVerb::Prepare,
            generation: CredentialGeneration::A,
        })
    );
    assert!(prepared.emit_secret.is_none());
    assert_eq!(
        prepared.workload_secret_path(WorkloadRoleFamily::ManagementAdmitter),
        Some(Path::new("/tmp/management-admitter.json"))
    );
    assert!(
        prepared
            .workload_secret_path(WorkloadRoleFamily::ControlAuthor)
            .is_none()
    );
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
    /// cluster and decode co-tenant WAL, which slot/publication naming does not
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
            "--nats-url",
            "nats://127.0.0.1:4222",
            "--nats-username",
            "test_provision",
            "--nats-password-file",
            "/test/private/event-password",
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
    fn cdc_command_accepts_explicit_broker_and_native_consumer_declarations() {
        let consumer = wamn_control_provision::events::materializer_consumer_config(
            "mat_t_pkg_r1",
            "evt.acme.billing.dev.invoice.>",
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
            "--nats-url",
            "nats://127.0.0.1:4222",
            "--nats-username",
            "test_provision",
            "--nats-password-file",
            "/test/private/event-password",
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

/// The render path emits the Cluster + WAL/PITR CRs wrapped in `List`s (wamn-e1g).
#[test]
#[cfg(feature = "ops")]
fn render_path_emits_lists() {
    let (org, _) = Template::standard().stamp("acme", "wamn-pg");
    let set =
        wamn_control_provision::org::render_org_cluster_set(&org, &Template::standard().policies)
            .unwrap();
    let clusters = k8s_list(&set.clusters);
    assert_eq!(clusters["kind"], "List");
    assert_eq!(clusters["items"][0]["kind"], "Cluster");
    assert_eq!(set.object_stores.len(), 1, "prod is backed");
    let stores = k8s_list(&set.object_stores);
    assert_eq!(stores["items"][0]["kind"], "ObjectStore");
    // An empty List (a pooled org has no clusters) is a harmless no-op apply.
    assert_eq!(k8s_list(&[])["items"].as_array().unwrap().len(), 0);
}
