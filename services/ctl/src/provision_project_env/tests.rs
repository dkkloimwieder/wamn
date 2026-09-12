use super::*;
use clap::{CommandFactory as _, FromArgMatches as _, Parser};
use wamn_control_provision::{EFFECT_WRITER_ROLE, MANAGEMENT_ADMITTER_ROLE};

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
    const SOURCE: &str = include_str!("../provision_project_env.rs");
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

fn parse_without_password_envs<const N: usize>(
    argv: [&str; N],
) -> Result<ProvisionProjectEnvArgs, clap::Error> {
    parse_argv(argv.iter().map(|arg| (*arg).to_string()).collect())
}

/// The same parser over a DERIVED command line, which a fixed-size array
/// cannot express.
fn parse_argv(argv: Vec<String>) -> Result<ProvisionProjectEnvArgs, clap::Error> {
    let matches = TestCli::command()
        .mut_arg("app_password", |arg| arg.env(None::<&str>))
        .try_get_matches_from(argv)?;
    TestCli::from_arg_matches(&matches).map(|cli| cli.args)
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
        // Required with no default on a PROVISIONING invocation
        // (wamn-0h0g.12.129), which is every invocation this helper builds.
        // The credential-free modes are exempt (wamn-0h0g.12.141) and
        // must therefore be parsed bare — see
        // `the_credential_free_modes_parse_without_a_password`.
        "--app-password",
        "app-probe",
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
            "--app-password",
            "app-probe",
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
    // function-name markers stood here. Deleted; the clap arms above prove
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

            let error = parse_argv(argv)
                .expect_err("a workload action accepted a missing identity member");
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::MissingRequiredArgument,
                "{action} missing {omitted} failed for the wrong reason: {error}"
            );
        }
    }

    // wamn-hopk R5: a source-text scan counting `.expect(` calls between two
    // function-name markers stood here. Deleted; the clap arms above prove
    // the parser contract by invoking the parser.
}

/// The mint is the whole non-reuse mechanism (wamn-0h0g.13.57): every draw
/// must satisfy the pure crate's rule, and the draws must actually differ.
/// A constant, a counter, or a triple-derived suffix collapses `seen`.
#[test]
fn the_minted_instance_suffix_is_a_fresh_valid_dns_label_tail() {
    // A duplicated symbol would bias the `% len()` fold; a symbol outside
    // `[a-z0-9]` would leave the namespace an illegal DNS-1123 label.
    assert_eq!(INSTANCE_SUFFIX_ALPHABET.len(), 36);
    assert_eq!(
        INSTANCE_SUFFIX_ALPHABET
            .iter()
            .collect::<BTreeSet<_>>()
            .len(),
        INSTANCE_SUFFIX_ALPHABET.len()
    );
    assert!(
        INSTANCE_SUFFIX_ALPHABET
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    );

    let mut seen = BTreeSet::new();
    for _ in 0..64 {
        let suffix = mint_instance_suffix().expect("mint an instance suffix");
        validate_instance_suffix(&suffix).expect("a minted suffix satisfies the pure rule");
        seen.insert(suffix);
    }
    assert_eq!(seen.len(), 64, "64 draws collapsed to {}", seen.len());
}

#[test]
fn a_reserved_or_bad_project_id_is_rejected_before_any_effect() {
    // The name validation runs first — a reserved / non-slug project id fails
    // without touching the registry or emitting a CR.
    assert!(validate_project_env("acme", "wamn-x", "dev").is_err());
    assert!(validate_project_env("acme", "Bad", "prod").is_err());
    assert!(validate_project_env("acme", "billing", "prod").is_ok());
}

/// The target cluster is DERIVED (D18 `cluster_of`) from the org's placement +
/// the env's policy: a dedicated org owns `<org>-<owner(env)>`, a pooled org
/// collapses every env onto its pool. (The live routing through the DB is
/// proven by the in-cluster gate; here we pin the pure derivation the
/// subcommand calls.)
#[test]
fn cluster_is_derived_by_placement_and_policy() {
    use wamn_control_registry::EnvPolicy;
    let ded = Org::dedicated("acme");
    assert_eq!(cluster_of(&ded, &EnvPolicy::dev()).name, "acme-dev");
    assert_eq!(cluster_of(&ded, &EnvPolicy::prod()).name, "acme-prod");
    let pooled = Org::pooled("try", "wamn-pg");
    assert_eq!(cluster_of(&pooled, &EnvPolicy::prod()).name, "wamn-pg");
}

#[test]
fn pat_literals_and_secret_documents_are_exact() {
    let triple = Triple::new("acme", "billing", "dev");
    assert_eq!(PAT_TTL, Duration::from_secs(2_592_000));
    assert_eq!(
        MANAGEMENT_AUTHOR.subject(&triple).unwrap(),
        "wamn-management-author-acme--billing--dev"
    );
    assert_eq!(
        MANAGEMENT_AUTHOR.display_name(&triple),
        "WAMN management author acme/billing/dev"
    );
    assert_eq!(
        ROUTE_CALLER.subject(&triple).unwrap(),
        "wamn-route-caller-acme--billing--dev"
    );
    assert_eq!(
        ROUTE_CALLER.display_name(&triple),
        "WAMN route caller acme/billing/dev"
    );

    let secret = render_pat_secret(
        &triple,
        "wamn-system",
        MANAGEMENT_AUTHOR,
        "6d3f2d1c-0000-4000-8000-00000000abcd",
        "wamn_pat_token-material",
        "0123456789abcdef",
        "2026-09-09T12:34:56Z",
    )
    .unwrap();
    assert_eq!(
        secret,
        json!({
            "apiVersion": "v1",
            "kind": "Secret",
            "metadata": {
                "name": "wamn-pat-management-author-acme--billing--dev",
                "namespace": "wamn-system",
                "labels": {
                    "app.kubernetes.io/managed-by": "wamn",
                    "app.kubernetes.io/component": "project-env-pat",
                    "wamn.org": "acme",
                    "wamn.project": "billing",
                    "wamn.env": "dev",
                },
                "annotations": {
                    "wamn.io/credential-purpose": "management-author",
                    "wamn.io/principal-id": "6d3f2d1c-0000-4000-8000-00000000abcd",
                    "wamn.io/principal-kind": "service",
                    "wamn.io/principal-subject": "wamn-management-author-acme--billing--dev",
                    "wamn.io/project-role": "project-author",
                    "wamn.io/pat-prefix": "0123456789abcdef",
                    "wamn.io/pat-expires-at": "2026-09-09T12:34:56Z",
                },
            },
            "type": "Opaque",
            "stringData": {
                "token": "wamn_pat_token-material",
            },
        })
    );
    assert_eq!(
        secret["stringData"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["token"]
    );

    let route_secret = render_pat_secret(
        &triple,
        "wamn-system",
        ROUTE_CALLER,
        "6d3f2d1c-0000-4000-8000-00000000abcd",
        "wamn_pat_other-material",
        "fedcba9876543210",
        "2026-09-09T12:34:56Z",
    )
    .unwrap();
    assert_eq!(
        route_secret["metadata"]["name"],
        "wamn-pat-route-caller-acme--billing--dev"
    );
    assert_eq!(
        route_secret["metadata"]["annotations"]["wamn.io/project-role"],
        "route-caller"
    );
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

    // `--app-password` (wamn-0h0g.12.129) is required with no default, but
    // only where it is consumed: wamn-0h0g.12.141 scoped it to the
    // provisioning modes, so revoke-only may carry it and need not. Passing
    // it here keeps this case about the PAT flags; the exemption itself is
    // proven by `the_credential_free_modes_parse_without_a_password`.
    let revoke = TestCli::try_parse_from([
        "test",
        "--system-database-url",
        "postgresql://postgres@localhost/postgres",
        "--revoke-pat-prefix",
        "0123456789abcdef",
        "--app-password",
        "app-probe",
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
        "--abort-effect-writer-generation",
        "a",
    ])
    .unwrap();
    assert_eq!(
        abort.workload.action,
        Some(WorkloadGenerationAction {
            family: WorkloadRoleFamily::EffectWriter,
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
        let error = run(args).await.unwrap_err();
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
fn secret_files_are_plain_json_and_mode_0600() {
    let path = std::env::temp_dir().join(format!(
        "wamn-ctl-pat-secret-mode-{}.json",
        std::process::id()
    ));
    std::fs::write(&path, b"old credential material").unwrap();
    std::fs::set_permissions(&path, Permissions::from_mode(0o644)).unwrap();

    let document = json!({"stringData": {"token": "test-token"}});
    write_secret_json(&path, &document).unwrap();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(stored, document);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn secret_writes_replace_links_without_following_or_sharing_them() {
    let sequence = SECRET_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "wamn-ctl-pat-secret-links-{}-{sequence}",
        std::process::id()
    ));
    std::fs::create_dir(&root).unwrap();

    let db_path = root.join("db.json");
    let management_path = root.join("management.json");
    std::os::unix::fs::symlink(&db_path, &management_path).unwrap();
    let db_document = json!({"stringData": {"url": "db-secret"}});
    let management_document = json!({"stringData": {"token": "management-secret"}});
    write_secret_json(&db_path, &db_document).unwrap();
    write_secret_json(&management_path, &management_document).unwrap();
    assert!(
        !std::fs::symlink_metadata(&management_path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    let stored_db: Value = serde_json::from_slice(&std::fs::read(&db_path).unwrap()).unwrap();
    let stored_management: Value =
        serde_json::from_slice(&std::fs::read(&management_path).unwrap()).unwrap();
    assert_eq!(stored_db, db_document);
    assert_eq!(stored_management, management_document);

    let route_path = root.join("route.json");
    std::fs::hard_link(&management_path, &route_path).unwrap();
    let route_document = json!({"stringData": {"token": "route-secret"}});
    write_secret_json(&route_path, &route_document).unwrap();
    let stored_management: Value =
        serde_json::from_slice(&std::fs::read(&management_path).unwrap()).unwrap();
    let stored_route: Value =
        serde_json::from_slice(&std::fs::read(&route_path).unwrap()).unwrap();
    assert_eq!(stored_management, management_document);
    assert_eq!(stored_route, route_document);

    std::fs::remove_file(db_path).unwrap();
    std::fs::remove_file(management_path).unwrap();
    std::fs::remove_file(route_path).unwrap();
    std::fs::remove_dir(root).unwrap();
}

#[test]
fn provisioning_summary_contains_no_database_credentials() {
    let triple = Triple::new("acme", "billing", "dev");
    let summary =
        provision_summary(&triple, "wamn-db-acme--billing--dev--k3m9x2p7", "acme-dev");
    assert_eq!(
        summary,
        "project-env acme/billing/dev: database \"wamn-db-acme--billing--dev--k3m9x2p7\" on cluster \"acme-dev\" (owner wamn_db_owner)"
    );
    assert!(!summary.contains("postgres://"));
    assert!(!summary.contains("password"));
    assert!(!summary.contains("app url"));
}

/// wamn-0h0g.12.122. The emitted privilege batch is PINNED whole: a runtime
/// gate that only asserts "the reader can connect" stays green when a
/// builder is swapped for a wider one, and stays green when a `CONNECT`
/// statement drifts back above the owner statement on a database that
/// happens to be owned by `wamn_db_owner` already. The frozen literal is the
/// guard.
///
/// wamn-0h0g.12.179 re-pinned it once, moving `wamn_app` from granted to
/// revoked. wamn-0h0g.22.24 re-pins it again for the LAST stable-LOGIN
/// family: `wamn_dispatch_reader` moves the same way, and the batch now
/// grants `CONNECT` to NOBODY. Every principal that reaches a project-env
/// database is a generation, and a generation is granted `CONNECT` directly
/// by its own prepare.
#[test]
fn the_privilege_batch_revokes_every_stable_role_connect_after_the_owner_statement() {
    let batch = privilege_sql("wamn-db-acme--billing--dev");
    assert_eq!(
        batch,
        "ALTER DATABASE \"wamn-db-acme--billing--dev\" OWNER TO \"wamn_db_owner\";\n\
             REVOKE CONNECT, TEMPORARY ON DATABASE \"wamn-db-acme--billing--dev\" FROM PUBLIC; \
             REVOKE CONNECT ON DATABASE \"wamn-db-acme--billing--dev\" FROM \"wamn_app\";\n\
             REVOKE CONNECT ON DATABASE \"wamn-db-acme--billing--dev\" \
             FROM \"wamn_dispatch_reader\";\n"
    );

    // The ordering assertion, stated independently of the frozen literal so
    // a deliberate re-pin cannot silently drop it. `ALTER DATABASE … OWNER
    // TO` rewrites the outgoing owner's ACL entry, so a revoke applied
    // before it can be undone by what the owner change carries over.
    let owner = batch
        .find("ALTER DATABASE")
        .expect("the owner statement is emitted");
    let reader_revoke = batch
        .find("REVOKE CONNECT ON DATABASE \"wamn-db-acme--billing--dev\" FROM \"wamn_dispatch_reader\"")
        .expect("the reader CONNECT revoke is emitted");
    assert!(
        owner < reader_revoke,
        "reader CONNECT revoke must follow ALTER DATABASE … OWNER TO: {batch}"
    );

    // NOBODY is granted CONNECT here, and the PUBLIC confinement stands.
    assert!(!batch.contains("GRANT CONNECT"));
    assert!(batch.contains("REVOKE CONNECT, TEMPORARY ON DATABASE"));
}

/// wamn-0h0g.12.179. The stable guest ACL role must never be handed
/// `CONNECT` by the batch an operator applies. It is cluster-global and
/// every per-tenant generation INHERITS it, so one grant here reaches every
/// project-env database on the cluster — and `--prepare-guest-generation`
/// refuses the result, which is how the defect surfaced.
#[test]
fn the_privilege_batch_never_grants_the_stable_guest_acl_role_connect() {
    let batch = privilege_sql("wamn-db-acme--billing--dev");
    assert!(
        !batch.contains(&format!("TO \"{APP_ROLE}\"")),
        "the batch must not grant the stable guest ACL role anything: {batch}"
    );
    assert!(
        batch.contains(&format!(
            "REVOKE CONNECT ON DATABASE \"wamn-db-acme--billing--dev\" FROM \"{APP_ROLE}\""
        )),
        "the batch must CONVERGE a pre-cutover CONNECT away: {batch}"
    );
    // The revoke follows the owner statement for the same reason the grants
    // did: ALTER DATABASE … OWNER TO rewrites the outgoing owner's entry.
    assert!(
        batch.find("ALTER DATABASE") < batch.find("REVOKE CONNECT ON DATABASE"),
        "ownership converges first: {batch}"
    );
}

/// The role batch must create both stable principals as NOLOGIN carriers.
///
/// Before wamn-0h0g.12.122 the example manifest named a dispatch role
/// production provisioning never created; wamn-0h0g.22.24 keeps it created
/// without a credential. `wamn-0h0g.12.140` gives `wamn_app` the same
/// posture while preserving the legacy password argument outside SQL.
#[test]
fn the_role_batch_creates_passwordless_nologin_acl_roles() {
    let batch = role_sql("app-secret");
    assert_eq!(
        role_posture_sql("app-secret"),
        format!(
            "{app}\n{owner}\n{reader}\n",
            app = sql::ensure_app_role_sql("app-secret"),
            owner = sql::ensure_db_owner_role_sql(),
            reader = sql::ensure_workload_acl_role_sql(WorkloadRoleFamily::DispatchReader),
        )
    );
    assert_eq!(
        batch,
        format!(
            "{posture}\n{drain}\n",
            posture = role_posture_sql("app-secret"),
            drain = sql::drain_app_role_sessions_sql(),
        )
    );
    for role in ["wamn_app", "wamn_dispatch_reader"] {
        assert!(batch.contains(&format!("'{role}'")));
    }
    assert!(batch.contains("CREATE ROLE \"wamn_app\" NOLOGIN"));
    assert!(batch.contains("ALTER ROLE \"wamn_app\" NOLOGIN PASSWORD NULL"));
    assert!(batch.contains("CREATE ROLE %I NOLOGIN"));
    assert!(batch.contains("ALTER ROLE %I NOLOGIN PASSWORD NULL"));
    assert!(
        !batch.contains("app-secret"),
        "the legacy app password reached role SQL: {batch}"
    );
}

/// `wamn-0h0g.22.24` RETIRED `--dispatch-reader-password`, and this is the
/// pin that keeps it retired.
///
/// The flag existed because the dispatcher authenticated as the stable,
/// cluster-global `wamn_dispatch_reader` LOGIN. That shape is the hazard
/// `wamn-0h0g.12.179` measured live for the guest — a cluster-global role
/// with a per-database `GRANT CONNECT` reaches every database on the
/// cluster, because its generations inherit `WITH INHERIT TRUE`. The family
/// is now on generations, so provisioning mints no dispatcher credential at
/// all and there is nothing to pass. A reintroduced flag would be a
/// reintroduced shared login.
#[test]
fn provisioning_mints_no_dispatch_reader_credential() {
    let parsed = parse_without_password_envs([
        "test",
        "--org",
        "acme",
        "--project",
        "billing",
        "--env",
        "dev",
        "--app-password",
        "app-probe",
        "--emit-secret",
        "/tmp/db.json",
    ])
    .expect("provisioning needs no dispatch-reader credential");
    assert!(parsed.emit_secret.is_some());
    // The flag is gone from the parser, not merely unused by this call.
    let rejected = parse_without_password_envs([
        "test",
        "--org",
        "acme",
        "--project",
        "billing",
        "--env",
        "dev",
        "--app-password",
        "app-probe",
        "--emit-secret",
        "/tmp/db.json",
        "--dispatch-reader-password",
        "reader-probe",
    ])
    .expect_err("the retired dispatch-reader credential flag still parses");
    assert_eq!(rejected.kind(), clap::error::ErrorKind::UnknownArgument);
    // And the role batch mints a connection-free NOLOGIN carrier, never a
    // login with a password.
    let batch = role_sql("app-secret");
    assert!(batch.contains("'wamn_dispatch_reader'"));
    assert!(!batch.contains("\"wamn_dispatch_reader\" LOGIN"));
    assert!(batch.contains("ALTER ROLE %I NOLOGIN PASSWORD NULL"));
}

/// The sibling guard for `--app-password` (wamn-0h0g.12.129).
///
/// The argument remains required for the legacy URL surface until
/// `wamn-0h0g.12.185`, but it must never regain a default or reach role SQL.
/// A 2026-08-19 verifier read measured the old default on every cluster the
/// shared LOGIN existed on.
#[test]
fn the_app_password_has_no_default() {
    let error = parse_without_password_envs([
        "test",
        "--org",
        "acme",
        "--project",
        "billing",
        "--env",
        "dev",
        "--emit-secret",
        "/tmp/db.json",
    ])
    .expect_err("provisioning accepted a missing --app-password");
    assert_eq!(
        error.kind(),
        clap::error::ErrorKind::MissingRequiredArgument
    );
    assert!(
        error.to_string().contains("--app-password"),
        "unexpected missing-argument error: {error}"
    );
}

/// The other half of the two guards above (wamn-0h0g.12.141). Refusing a
/// missing credential is only half the contract: the modes that
/// provision nothing reach neither [`compose_url`] nor [`role_sql`], so the
/// parser must not demand a secret they would immediately discard — which
/// is what forced `deploy/mvp/bootstrap.sh`'s generation and revoke call
/// sites to invent one. The exempt list is `--emit-secret`'s: the
/// credentials and the Secret are owed by the same invocations.
///
/// Deliberately built without [`parse_args`], which injects both credentials
/// and would leave every assertion here vacuous. The test parser also
/// ignores ambient credential variables so they cannot contaminate the
/// asserted command-line shape.
#[test]
fn the_credential_free_modes_parse_without_a_password() {
    let revoke = parse_without_password_envs([
        "test",
        "--system-database-url",
        "postgresql://postgres@localhost/postgres",
        "--revoke-pat-prefix",
        "0123456789abcdef",
    ])
    .expect("revoke provisions nothing and needs no database credential");
    assert!(revoke.app_password.is_none());

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
        .unwrap_or_else(|e| panic!("{action} demanded a database credential: {e}"));
        assert!(
            parsed.app_password.is_none(),
            "{action} acquired an --app-password"
        );
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
/// The count is now ZERO, and this proves the stronger property the
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
            *line,
            "required_unless_present_any = [\"revoke_pat_prefix\", WORKLOAD_ACTION_GROUP]",
            "an exclusion list grew members again"
        );
    }
    assert_eq!(
        survivors.len(),
        2,
        "the app password and the database Secret are the arguments a \
             provisioning-only invocation owes — wamn-0h0g.22.24 retired the \
             third, `--dispatch-reader-password`, with the stable-LOGIN shape \
             that needed it"
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
    // one-action rule is proven where it bites, by parsing: see
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

/// The dispatch, the lifecycle and the ACL expectation are one derivation
/// each, total over the family set.
///
/// Four copy-pasted `run_*_action` functions, four lifecycle constructors
/// and a two-arm `RoleAclExpectation` are gone. What stays per family is the
/// GRANT SET, and the wildcard arm of [`stable_grant_set`] means a family
/// admitted without authority needs no entry there either.
#[test]
fn every_family_derives_a_lifecycle_and_only_a_grant_set_stays_per_family() {
    let identity = WorkloadActionIdentity {
        org: "acme",
        project: "billing",
        environment: "dev",
        tenant: "tenant",
    };
    for family in WorkloadRoleFamily::ALL {
        let lifecycle = workload_lifecycle(family, identity, "wamn-db-acme--billing--dev");
        assert_eq!(lifecycle.family, family);
        assert_eq!(lifecycle.database(), "wamn-db-acme--billing--dev");
        // The scope grain is the family's own declaration, so a family can
        // never be paired with the wrong one here.
        assert_eq!(lifecycle.scope, {
            let probe = workload_lifecycle(family, identity, "wamn-db-acme--billing--dev");
            probe.scope
        });
        let a = lifecycle.role(CredentialGeneration::A);
        let b = lifecycle.role(CredentialGeneration::B);
        assert_ne!(a, b, "{family:?}");
        assert!(is_workload_generation_role(family, &a), "{family:?}: {a}");
        assert!(a.len() <= 63, "{family:?}: {a}");
        // Only a CONTROL-scoped family records a login-to-tenant mapping.
        assert_eq!(
            lifecycle.control_tenant.is_some(),
            family.scope_kind() == WorkloadRoleScopeKind::Control,
            "{family:?}"
        );
    }

    let with_grant_sets: Vec<WorkloadRoleFamily> = WorkloadRoleFamily::ALL
        .into_iter()
        .filter(|family| stable_grant_set(*family).is_some())
        .collect();
    assert_eq!(
        with_grant_sets,
        [
            WorkloadRoleFamily::EffectWriter,
            WorkloadRoleFamily::ManagementAdmitter,
            // `wamn-0h0g.22.24`: the dispatch reader acquired GENERATIONS,
            // so its long-standing grant set finally has an inheritor to
            // guard and acquires a denial matrix with them.
            WorkloadRoleFamily::DispatchReader,
            // `wamn-0h0g.12.69`: run retention acquired authority — DELETE
            // plus a three-column SELECT on `runs` — so it acquires a
            // denial matrix here at the same time. The two are the same
            // event, and a family with one and not the other is the bug
            // this assertion exists to catch.
            WorkloadRoleFamily::Retention,
            // `wamn-0h0g.22.37`: the executor-platform and callable-HTTP
            // families acquired the grant sets their MEASURED production
            // surfaces derive, so both acquire a denial matrix with them.
            WorkloadRoleFamily::ExecutorPlatform,
            WorkloadRoleFamily::HttpAdmitter,
            WorkloadRoleFamily::EventMaterializer,
            WorkloadRoleFamily::RegistryReader,
            WorkloadRoleFamily::IdentityReader,
            WorkloadRoleFamily::SessionRoleReader,
        ],
        "a family acquired a grant set without acquiring authority"
    );
    // The pre-prepare grant-set assertion fires for the families whose grant
    // set is converged ELSEWHERE, and not for the ones this batch applies.
    assert!(sql::stable_surface_sql(WorkloadRoleFamily::EffectWriter).is_none());
    assert!(sql::stable_surface_sql(WorkloadRoleFamily::Retention).is_none());
    assert!(sql::stable_surface_sql(WorkloadRoleFamily::DispatchReader).is_none());
    for family in [
        WorkloadRoleFamily::ManagementAdmitter,
        // `wamn-0h0g.22.37`: this batch applies both new surfaces, so the
        // pre-prepare assertion must NOT fire for them — on a first prepare
        // there is nothing converged yet to assert against.
        WorkloadRoleFamily::ExecutorPlatform,
        WorkloadRoleFamily::HttpAdmitter,
        WorkloadRoleFamily::EventMaterializer,
        WorkloadRoleFamily::RegistryReader,
        WorkloadRoleFamily::IdentityReader,
        WorkloadRoleFamily::SessionRoleReader,
    ] {
        assert!(sql::stable_surface_sql(family).is_some(), "{family:?}");
    }
    for family in WorkloadRoleFamily::ALL {
        if !matches!(
            family,
            WorkloadRoleFamily::EffectWriter
                | WorkloadRoleFamily::ManagementAdmitter
                | WorkloadRoleFamily::ExecutorPlatform
                | WorkloadRoleFamily::HttpAdmitter
                | WorkloadRoleFamily::EventMaterializer
                | WorkloadRoleFamily::RegistryReader
                | WorkloadRoleFamily::IdentityReader
                | WorkloadRoleFamily::SessionRoleReader
        ) {
            assert!(sql::stable_surface_sql(family).is_none(), "{family:?}");
        }
    }
    assert!(sql::stable_surface_sql(WorkloadRoleFamily::EventMaterializer).is_some());
    assert_eq!(
        stable_grant_set(WorkloadRoleFamily::EventMaterializer),
        Some(StableGrantSet::EventMaterializer)
    );
}

/// THE DISJOINTNESS MATRIX, exercised from both sides
/// (`wamn-0h0g.12.116`).
///
/// The exact set passes; the same set widened onto the identity plane, or
/// widened with a write privilege, does not. The empty grant set is required
/// in the control database and required to be empty everywhere else.
#[test]
fn the_registry_reader_grants_are_exact_and_never_reach_identity() {
    let exact = vec![
        role_acl("schema", "registry", "registry", "USAGE"),
        role_acl("relation", "registry", "event_readers", "SELECT"),
    ];
    let verify = |grants: &[RoleAcl], database: &str| {
        verify_system_reader_grants(
            SystemReader::Registry,
            "registry",
            &sql::REGISTRY_READER_RELATIONS,
            WorkloadRoleFamily::RegistryReader.acl_role(),
            database,
            "wamn_system",
            grants,
        )
    };
    verify(&exact, "wamn_system").unwrap();

    for widening in [
        role_acl("schema", "identity", "identity", "USAGE"),
        role_acl("relation", "identity", "pats", "SELECT"),
        role_acl("relation", "identity", "project_roles", "SELECT"),
        role_acl("relation", "registry", "event_readers", "INSERT"),
        role_acl("relation", "registry", "event_readers", "UPDATE"),
        role_acl("relation", "registry", "orgs", "SELECT"),
    ] {
        let mut widened = exact.clone();
        widened.push(widening.clone());
        let error = verify(&widened, "wamn_system")
            .expect_err("a widened registry reader passed its own matrix");
        assert!(
            error
                .to_string()
                .contains("are not the exact registry-reader grant set"),
            "refused for the wrong reason: {error}"
        );
    }

    // A missing grant set in the control database is a failure; nothing at
    // all in any OTHER database is the required state.
    verify(&[], "wamn_system").expect_err("an empty control-database grant set passed");
    verify(&[], "some_project_db").unwrap();
    let error = verify(&exact, "some_project_db")
        .expect_err("the reader holds its grant set in a database that is not the control one");
    assert!(
        error
            .to_string()
            .contains("which is not the control database"),
        "refused for the wrong reason: {error}"
    );
}

/// THE DISJOINTNESS MATRIX from the identity side, and THE THREE-TIMES-DRIFT
/// GUARD at the verification boundary (`wamn-0h0g.12.67`).
///
/// The live cluster role carries `SELECT, INSERT, UPDATE`. Every one of the
/// widenings below — the registry plane, `INSERT`, `UPDATE` — is measured
/// against the server's own `aclexplode` answer for EQUALITY, so a role that
/// drifts a fourth time fails provisioning instead of being converged around.
#[test]
fn the_identity_reader_grants_are_exact_and_never_allow_a_write() {
    let exact = vec![
        role_acl("schema", "identity", "identity", "USAGE"),
        role_acl("relation", "identity", "pats", "SELECT"),
        role_acl("relation", "identity", "principals", "SELECT"),
        role_acl("relation", "identity", "project_env_memberships", "SELECT"),
        role_acl("relation", "identity", "project_roles", "SELECT"),
    ];
    let verify = |grants: &[RoleAcl], database: &str| {
        verify_system_reader_grants(
            SystemReader::Identity,
            "identity",
            &sql::IDENTITY_READER_RELATIONS,
            WorkloadRoleFamily::IdentityReader.acl_role(),
            database,
            "wamn_system",
            grants,
        )
    };
    verify(&exact, "wamn_system").unwrap();

    for widening in [
        // The forgery primitives themselves.
        role_acl("relation", "identity", "pats", "INSERT"),
        role_acl("relation", "identity", "pats", "UPDATE"),
        role_acl("relation", "identity", "project_roles", "INSERT"),
        role_acl("relation", "identity", "project_roles", "UPDATE"),
        role_acl("relation", "identity", "project_env_memberships", "INSERT"),
        role_acl("relation", "identity", "project_env_memberships", "UPDATE"),
        role_acl("relation", "identity", "project_env_memberships", "DELETE"),
        role_acl("column", "identity", "pats.token_hash", "UPDATE"),
        // …and the other reader's plane.
        role_acl("schema", "registry", "registry", "USAGE"),
        role_acl("relation", "registry", "event_readers", "SELECT"),
    ] {
        let mut widened = exact.clone();
        widened.push(widening.clone());
        let error = verify(&widened, "wamn_system")
            .expect_err("a widened identity reader passed its own matrix");
        assert!(
            error
                .to_string()
                .contains("are not the exact identity-reader grant set"),
            "refused for the wrong reason: {error}"
        );
    }
    verify(&[], "wamn_system").expect_err("an empty control-database grant set passed");
    verify(&[], "some_project_db").unwrap();
}

#[test]
fn the_http_admitter_grants_require_fresh_reads_and_refuse_writes() {
    let mut exact = vec![
        role_acl("schema", "app_system", "app_system", "USAGE"),
        role_acl("schema", "catalog", "catalog", "USAGE"),
        role_acl("relation", "app_system", "permissions", "SELECT"),
        role_acl("relation", "app_system", "users", "SELECT"),
        role_acl("relation", "app_system", "user_roles", "SELECT"),
    ];
    for relation in sql::HTTP_ADMITTER_CATALOG_RELATIONS {
        exact.push(role_acl("relation", "catalog", relation, "SELECT"));
    }
    let verify = |grants: &[RoleAcl]| {
        verify_http_admitter_grants(
            WorkloadRoleFamily::HttpAdmitter.acl_role(),
            "project_db",
            "project_db",
            grants,
        )
    };
    verify(&exact).expect("the fresh permission reads have their exact grant set");
    for relation in ["users", "user_roles", "permissions"] {
        let mut missing = exact.clone();
        missing.retain(|acl| acl.schema_name != "app_system" || acl.object_name != relation);
        verify(&missing).expect_err("a required fresh permission read is missing");
        for privilege in ["INSERT", "UPDATE", "DELETE"] {
            let mut widened = exact.clone();
            widened.push(role_acl("relation", "app_system", relation, privilege));
            verify(&widened).expect_err("the HTTP admitter cannot write permission authority");
        }
    }
}

/// Every family publishes a Secret whose name, component label and body are
/// DERIVED, and the four frozen names are unchanged.
#[test]
fn every_family_derives_its_credential_secret_name() {
    let frozen = [
        (
            WorkloadRoleFamily::EffectWriter,
            "wamn-effect-writer-acme--billing--dev",
        ),
        (
            WorkloadRoleFamily::ControlAuthor,
            "wamn-authoring-acme--billing--dev",
        ),
        (
            WorkloadRoleFamily::ManagementAdmitter,
            "wamn-mgmt-admitter-acme--billing--dev",
        ),
        (WorkloadRoleFamily::App, "wamn-guest-acme--billing--dev"),
    ];
    for (family, name) in frozen {
        assert_eq!(
            wamn_control_provision::workload_secret_name(family, "acme", "billing", "dev"),
            name,
            "{family:?}"
        );
    }
    let mut names = BTreeSet::new();
    for family in WorkloadRoleFamily::ALL {
        let name =
            wamn_control_provision::workload_secret_name(family, "acme", "billing", "dev");
        assert!(name.starts_with("wamn-"), "{family:?}: {name}");
        assert!(names.insert(name), "{family:?} shares a Secret name");
    }
    assert_eq!(names.len(), WorkloadRoleFamily::ALL.len());
}

fn role_acl(kind: &str, schema: &str, object: &str, privilege: &str) -> RoleAcl {
    RoleAcl {
        object_kind: kind.to_string(),
        schema_name: schema.to_string(),
        object_name: object.to_string(),
        privilege: privilege.to_string(),
        grantable: false,
    }
}

#[test]
fn session_reader_requires_exact_grants() {
    let mut exact = vec![role_acl("schema", "app_system", "app_system", "USAGE")];
    for column in [
        "users.tenant_id",
        "users.id",
        "users.status",
        "user_roles.tenant_id",
        "user_roles.user_id",
        "user_roles.role_name",
    ] {
        exact.push(role_acl("column", "app_system", column, "SELECT"));
    }
    let verify = |rows: &[RoleAcl]| {
        verify_session_role_reader_grants(
            "wamn_session_role_reader",
            "project-db",
            "project-db",
            rows,
        )
    };
    assert!(verify(&exact).is_ok());
    for index in 0..exact.len() {
        let mut missing = exact.clone();
        missing.remove(index);
        assert!(verify(&missing).is_err());
    }
    for extra in [
        role_acl("relation", "app_system", "users", "SELECT"),
        role_acl("column", "app_system", "users.email", "SELECT"),
        role_acl("column", "app_system", "users.status", "UPDATE"),
        role_acl("relation", "app_system", "permissions", "SELECT"),
        role_acl("schema", "catalog", "catalog", "USAGE"),
    ] {
        let mut widened = exact.clone();
        widened.push(extra);
        assert!(verify(&widened).is_err());
    }
    let mut grantable = exact.clone();
    grantable[0].grantable = true;
    assert!(verify(&grantable).is_err());
    assert!(
        verify_session_role_reader_grants(
            "wamn_session_role_reader",
            "other-db",
            "project-db",
            &[],
        )
        .is_ok()
    );
}

#[test]
fn materializer_grants_cover_exactly_the_two_production_reads() {
    let mut exact = vec![role_acl("schema", "catalog", "catalog", "USAGE")];
    for relation in sql::EVENT_MATERIALIZER_CATALOG_RELATIONS {
        exact.push(role_acl("relation", "catalog", relation, "SELECT"));
    }
    assert!(
        verify_event_materializer_grants(
            "wamn_event_materializer",
            "wamn",
            "wamn",
            &exact,
        )
        .is_ok()
    );

    let mut widened = exact.clone();
    widened.push(role_acl("relation", "catalog", "packages", "INSERT"));
    assert!(
        verify_event_materializer_grants(
            "wamn_event_materializer",
            "wamn",
            "wamn",
            &widened,
        )
        .is_err()
    );
    assert!(
        verify_event_materializer_grants(
            "wamn_event_materializer",
            "wamn",
            "wamn",
            &[],
        )
        .is_err()
    );
}

#[test]
fn stable_writer_grants_require_the_complete_schema_set() {
    let schema = "wamn_runner_demo";
    let mut exact = vec![role_acl("schema", schema, schema, "USAGE")];
    for table in [
        "effect_attempts",
        "effect_attempt_dispatches",
        "effect_attempt_outcomes",
    ] {
        exact.push(role_acl("relation", schema, table, "SELECT"));
        exact.push(role_acl("relation", schema, table, "INSERT"));
    }
    for (table, columns) in [
        ("runs", &["tenant_id", "run_id", "status"][..]),
        (
            "run_queue",
            &[
                "tenant_id",
                "run_id",
                "lease_owner",
                "lease_expires_at",
                "lease_generation",
            ][..],
        ),
    ] {
        for column in columns {
            exact.push(role_acl(
                "column",
                schema,
                &format!("{table}.{column}"),
                "SELECT",
            ));
        }
    }
    verify_effect_writer_grants(EFFECT_WRITER_ROLE, "project_db", &exact).unwrap();

    let mut partial = exact.clone();
    partial.pop();
    assert!(
        verify_effect_writer_grants(EFFECT_WRITER_ROLE, "project_db", &partial)
            .is_err()
    );
    let mut unrelated = exact;
    unrelated.push(role_acl("relation", schema, "other_table", "SELECT"));
    assert!(
        verify_effect_writer_grants(EFFECT_WRITER_ROLE, "project_db", &unrelated)
            .is_err()
    );

    let mut reserved = vec![role_acl("schema", "app", "app", "USAGE")];
    for table in [
        "effect_attempts",
        "effect_attempt_dispatches",
        "effect_attempt_outcomes",
    ] {
        reserved.push(role_acl("relation", "app", table, "SELECT"));
        reserved.push(role_acl("relation", "app", table, "INSERT"));
    }
    assert!(
        verify_effect_writer_grants(EFFECT_WRITER_ROLE, "project_db", &reserved)
            .is_err()
    );
}

#[test]
fn stable_writer_grants_refuse_unrelated_object_kinds() {
    // The kind filter is this guard's first refusal, and the complete-set test
    // above cannot observe it: every fixture there is a schema, relation or
    // column, so admitting one further kind left that test green under the
    // wamn-0h0g.15.107 mutation run. A database CONNECT ACL is the realistic
    // out-of-set kind — it is what a scoped generation role is granted — and a
    // stable role must never hold one. Asserting the named refusal is the
    // load-bearing part: a widened kind set still fails, but on the exact
    // grant set instead, and would leave the filter unverified again.
    let error = verify_effect_writer_grants(
        EFFECT_WRITER_ROLE,
        "project_db",
        &[role_acl("database", "project_db", "project_db", "CONNECT")],
    )
    .expect_err("a database ACL is not an effect-writer ACL");
    assert!(
        error
            .to_string()
            .contains("carries non-writer database ACL"),
        "refused for the wrong reason: {error}"
    );
}

#[test]
fn management_grants_are_exact_and_required_in_the_target_database() {
    let mut exact = vec![
        role_acl("schema", "catalog", "catalog", "USAGE"),
        role_acl("schema", "wamn_run", "wamn_run", "USAGE"),
        role_acl("relation", "wamn_run", "environment_policies", "SELECT"),
        role_acl("routine", "wamn_authority", "tenant_key", "EXECUTE"),
    ];
    for relation in sql::MANAGEMENT_ADMITTER_CATALOG_RELATIONS {
        exact.push(role_acl("relation", "catalog", relation, "SELECT"));
    }
    for column in sql::MANAGEMENT_ADMITTER_WIRING_INSERT_COLUMNS {
        exact.push(role_acl(
            "column",
            "catalog",
            &format!("wirings.{column}"),
            "INSERT",
        ));
    }
    for (relation, privilege, columns) in [
        (
            "runs",
            "SELECT",
            &sql::MANAGEMENT_ADMITTER_RUN_SELECT_COLUMNS[..],
        ),
        (
            "runs",
            "INSERT",
            &sql::MANAGEMENT_ADMITTER_RUN_INSERT_COLUMNS[..],
        ),
        (
            "run_queue",
            "SELECT",
            &sql::MANAGEMENT_ADMITTER_QUEUE_SELECT_COLUMNS[..],
        ),
        (
            "run_queue",
            "INSERT",
            &sql::MANAGEMENT_ADMITTER_QUEUE_INSERT_COLUMNS[..],
        ),
    ] {
        for column in columns {
            exact.push(role_acl(
                "column",
                "wamn_run",
                &format!("{relation}.{column}"),
                privilege,
            ));
        }
    }
    verify_management_admitter_grants(
        MANAGEMENT_ADMITTER_ROLE,
        "project_db",
        "project_db",
        &exact,
    )
    .unwrap();

    let mut widened = exact.clone();
    widened.push(role_acl(
        "relation",
        "wamn_run",
        "environment_policies",
        "UPDATE",
    ));
    assert!(
        verify_management_admitter_grants(
            MANAGEMENT_ADMITTER_ROLE,
            "project_db",
            "project_db",
            &widened,
        )
        .is_err()
    );
    assert!(
        verify_management_admitter_grants(
            MANAGEMENT_ADMITTER_ROLE,
            "project_db",
            "project_db",
            &[],
        )
        .is_err()
    );
    verify_management_admitter_grants(
        MANAGEMENT_ADMITTER_ROLE,
        "unprovisioned_db",
        "project_db",
        &[],
    )
    .unwrap();
}

/// `wamn-0h0g.12.176`: the management-admitter action is one more STAMP of
/// the `wamn-0h0g.13.59` unified lifecycle, not a fourth mechanism.
///
/// This COMPLETES `wamn-0h0g.12.118`'s deferral — "no bespoke prepare,
/// retire, Secret, or A/B implementation", closed for want of "a ctl
/// lifecycle or call site". `wamn-0h0g.8.5.3` is the first consumer, so the
/// deferral reached its trigger; nothing here reverses it, and every assert
/// below is that the generic machinery, not a bespoke path, produced the
/// result.
#[test]
fn the_management_admitter_action_is_one_more_stamp_of_the_workload_lifecycle() {
    const DATABASE: &str = "wamn-db-acme--receiving--dev--k3m9x2p7";

    // The ONE lifecycle derivation pairs the seventh family with its exact
    // scope grain, and carries no control tenant: the tenant mapping row
    // belongs to the control plane, which this credential never reaches.
    let identity = WorkloadActionIdentity {
        org: "acme",
        project: "receiving",
        environment: "dev",
        tenant: "tenant",
    };
    let lifecycle =
        workload_lifecycle(WorkloadRoleFamily::ManagementAdmitter, identity, DATABASE);
    assert_eq!(lifecycle.family, WorkloadRoleFamily::ManagementAdmitter);
    assert_eq!(lifecycle.database(), DATABASE);
    assert_eq!(lifecycle.label(), "management-admitter");
    assert!(lifecycle.control_tenant.is_none());
    assert!(matches!(
        lifecycle.scope,
        WorkloadRoleScope::ProjectEnvironment { .. }
    ));

    // The A/B pair is the crate's derivation, never a second spelling here.
    let a = lifecycle.role(CredentialGeneration::A);
    let b = lifecycle.role(CredentialGeneration::B);
    for (generation, derived) in [(CredentialGeneration::A, &a), (CredentialGeneration::B, &b)]
    {
        assert_eq!(
            derived,
            &wamn_control_provision::management_admitter_generation_role(
                "acme",
                "receiving",
                "dev",
                DATABASE,
                generation,
            )
        );
        assert!(is_workload_generation_role(
            WorkloadRoleFamily::ManagementAdmitter,
            derived
        ));
        assert_eq!(derived.len(), 61);
    }
    assert_ne!(a, b);
    assert!(a.starts_with("wamn_mgmt_admitter_") && a.ends_with("_a"));
    assert!(b.ends_with("_b"));
    // The generation prefix is the short frozen one, never the 24-byte stable
    // ACL role name (wamn-0h0g.13.62).
    assert!(!a.starts_with(MANAGEMENT_ADMITTER_ROLE));
    assert_eq!(lifecycle.family.acl_role(), MANAGEMENT_ADMITTER_ROLE);

    // Each family locks on its own key, so three lifecycles never serialize
    // against one another.
    let keys: BTreeSet<String> = WorkloadRoleFamily::ALL
        .into_iter()
        .map(|family| workload_lifecycle(family, identity, DATABASE).family_lock_key())
        .collect();
    assert_eq!(keys.len(), WorkloadRoleFamily::ALL.len());

    // The published Secret is the crate renderer's, named by the crate helper
    // the wamn-0h0g.8.5.3 Deployment reference derives from. One derivation,
    // so the mint and the reference cannot drift apart.
    let secret = render_workload_secret_manifest(
        WorkloadRoleFamily::ManagementAdmitter,
        &Triple::new("acme", "receiving", "dev"),
        "wamn-system",
        WorkloadSecretBody::Url(
            "postgres://role:pw@acme-dev-rw:5432/wamn-db-acme--receiving--dev--k3m9x2p7",
        ),
    );
    assert_eq!(
        secret["metadata"]["name"].as_str().expect("Secret name"),
        wamn_control_provision::management_admitter_secret_name("acme", "receiving", "dev")
    );

    // One action per invocation and one Secret bound to its own prepare are
    // now group-derived properties, proven for EVERY pair and EVERY family
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

#[test]
fn every_workload_family_carries_a_distinct_frozen_label() {
    // `wamn-0fqa` takes the vocabulary to ten and `wamn-0h0g.13.63` to
    // twelve. `wamn-ctc8.15.2` adds the session-role reader as the thirteenth.
    // `label` reads only the family, so the scope is deliberately uniform.
    let expected = [
        (WorkloadRoleFamily::EffectWriter, "effect-writer"),
        (WorkloadRoleFamily::ControlAuthor, "control-author"),
        (
            WorkloadRoleFamily::ManagementAdmitter,
            "management-admitter",
        ),
        (WorkloadRoleFamily::DispatchReader, "dispatch-reader"),
        (WorkloadRoleFamily::ServiceReader, "service-reader"),
        (WorkloadRoleFamily::App, "app"),
        (WorkloadRoleFamily::Retention, "retention"),
        (WorkloadRoleFamily::ExecutorPlatform, "executor-platform"),
        (WorkloadRoleFamily::HttpAdmitter, "http-admitter"),
        (WorkloadRoleFamily::EventMaterializer, "event-materializer"),
        (WorkloadRoleFamily::RegistryReader, "registry-reader"),
        (WorkloadRoleFamily::IdentityReader, "identity-reader"),
        (WorkloadRoleFamily::SessionRoleReader, "session-role-reader"),
    ];
    assert_eq!(expected.len(), WorkloadRoleFamily::ALL.len());
    let mut seen = Vec::new();
    for (family, label) in expected {
        let lifecycle = WorkloadLifecycle {
            family,
            scope: WorkloadRoleScope::Tenant {
                tenant: "t",
                database: "db",
            },
            control_tenant: None,
        };
        assert_eq!(lifecycle.label(), label, "{family:?}");
        assert_eq!(family.label(), label, "{family:?}");
        seen.push(label);
    }
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), expected.len(), "labels must stay distinct");
}

#[test]
fn stable_acl_role_members_are_only_scoped_generation_roles() {
    assert!(is_workload_generation_role(
        WorkloadRoleFamily::EffectWriter,
        "wamn_effect_writer_0123456789abcdef0123456789abcdef01234567_a"
    ));
    assert!(is_workload_generation_role(
        WorkloadRoleFamily::ControlAuthor,
        "wamn_control_author_0123456789abcdef0123456789abcdef01234567_b"
    ));
    // `wamn-0h0g.13.62`: management generations carry the short frozen
    // prefix, never the 24-byte stable ACL role name.
    assert!(is_workload_generation_role(
        WorkloadRoleFamily::ManagementAdmitter,
        "wamn_mgmt_admitter_0123456789abcdef0123456789abcdef01234567_a"
    ));
    assert!(!is_workload_generation_role(
        WorkloadRoleFamily::ManagementAdmitter,
        "wamn_management_admitter_0123456789abcdef0123456789abcdef01234567_a"
    ));
    // `wamn-0fqa`: the executor-platform and event-materializer families
    // carry short frozen prefixes for the same reason; the callable-HTTP
    // admitter's name fits and keeps its ACL role name as the prefix.
    assert!(is_workload_generation_role(
        WorkloadRoleFamily::ExecutorPlatform,
        "wamn_exec_platform_0123456789abcdef0123456789abcdef01234567_a"
    ));
    assert!(!is_workload_generation_role(
        WorkloadRoleFamily::ExecutorPlatform,
        "wamn_executor_platform_0123456789abcdef0123456789abcdef01234567_a"
    ));
    assert!(is_workload_generation_role(
        WorkloadRoleFamily::EventMaterializer,
        "wamn_materializer_0123456789abcdef0123456789abcdef01234567_b"
    ));
    assert!(!is_workload_generation_role(
        WorkloadRoleFamily::EventMaterializer,
        "wamn_event_materializer_0123456789abcdef0123456789abcdef01234567_b"
    ));
    assert!(is_workload_generation_role(
        WorkloadRoleFamily::HttpAdmitter,
        "wamn_http_admitter_0123456789abcdef0123456789abcdef01234567_a"
    ));
    for invalid in [
        "wamn_effect_writer_a",
        "wamn_effect_writer_0123456789ABCDEF0123456789abcdef01234567_a",
        "wamn_effect_writer_0123456789abcdef0123456789abcdef01234567_c",
        "unrelated_0123456789abcdef0123456789abcdef01234567_a",
    ] {
        assert!(
            !is_workload_generation_role(WorkloadRoleFamily::EffectWriter, invalid),
            "accepted {invalid}"
        );
    }
}
