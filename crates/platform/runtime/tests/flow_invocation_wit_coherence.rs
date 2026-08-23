//! Static conformance guard for the production flow-invocation provider.

const CONTRACT: &str = include_str!("../../../execution/flow-invocation/wit/package.wit");
const HOST_COPY: &str = include_str!("../wit/deps/wamn-flow-invocation/package.wit");
const WORLD: &str = include_str!("../wit/world.wit");
const SERVICE: &str = include_str!("../src/flow_invocation.rs");
const PLUGIN: &str = include_str!("../src/plugins/wamn_flow_invocation.rs");
const HOST: &str = include_str!("../../../../services/host/src/host.rs");
const HOST_MANIFEST: &str = include_str!("../../../../services/host/Cargo.toml");

fn without_wit_comments(wit: &str) -> String {
    let mut code = String::with_capacity(wit.len());
    let mut chars = wit.chars().peekable();

    while let Some(ch) = chars.next() {
        match (ch, chars.peek().copied()) {
            ('"', _) => {
                code.push(ch);
                let mut escaped = false;
                for string_ch in chars.by_ref() {
                    code.push(string_ch);
                    if escaped {
                        escaped = false;
                    } else if string_ch == '\\' {
                        escaped = true;
                    } else if string_ch == '"' {
                        break;
                    }
                }
            }
            ('/', Some('/')) => {
                chars.next();
                for comment_ch in chars.by_ref() {
                    if comment_ch == '\n' {
                        code.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                // A WIT comment is whitespace. Keep one separator so removing a
                // comment cannot fuse otherwise distinct tokens.
                code.push(' ');
                let mut depth = 1_u32;
                while depth > 0 {
                    let comment_ch = chars.next().expect("unterminated WIT block comment");
                    match (comment_ch, chars.peek().copied()) {
                        ('/', Some('*')) => {
                            chars.next();
                            depth = depth.checked_add(1).expect("WIT block comment nesting");
                        }
                        ('*', Some('/')) => {
                            chars.next();
                            depth -= 1;
                        }
                        ('\n', _) => code.push('\n'),
                        _ => {}
                    }
                }
            }
            _ => code.push(ch),
        }
    }

    code
}

fn code_lines(wit: &str) -> Vec<String> {
    without_wit_comments(wit)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

#[test]
fn plain_line_comments_are_not_wit_code() {
    let source = "interface invocation {\n  // cancel: func();\n  begin: func(); // pending(\n}\n";

    assert_eq!(
        code_lines(source),
        ["interface invocation {", "begin: func();", "}"]
    );
}

#[test]
fn multiline_and_nested_block_comments_are_not_wit_code() {
    let source = "interface invocation {\n  /* outcome-expired\n     /* accepted( */\n     pending( */\n  begin: func();\n}\n";

    assert_eq!(
        code_lines(source),
        ["interface invocation {", "begin: func();", "}"]
    );
}

#[test]
fn deleted_vocabulary_in_live_wit_code_is_preserved() {
    let source = "interface invocation {\n  /* cancel: func(); */ cancel: func();\n}\n";
    let code = without_wit_comments(source);

    assert_eq!(code.matches("cancel:").count(), 1);
    assert_eq!(
        code_lines(source),
        ["interface invocation {", "cancel: func();", "}"]
    );
}

#[test]
fn every_flow_invocation_copy_shares_the_contract_code() {
    let contract = code_lines(CONTRACT);
    for (name, copy) in [("runtime", HOST_COPY)] {
        assert_eq!(
            code_lines(copy),
            contract,
            "{name} flow-invocation WIT copy drifted from the canonical contract"
        );
    }
}

#[test]
fn host_copy_preserves_the_frozen_interface_surface() {
    for anchor in [
        "package wamn:flow-invocation@0.1.0;",
        // Both operations carry the wamn-0h0g.15.40 error channel in every copy,
        // so a host failure answers rather than trapping the ingress instance.
        "begin: func(req: invoke-request) -> result<begin-result, invocation-error>;",
        "wait: func(run-id: string, timeout-ms: u32) -> result<option<invoke-result>, invocation-error>;",
        "variant invocation-error {",
        "store-unavailable,",
        "store-corrupt,",
        "unknown-run,",
        "invalid-request,",
        "expected-catalog-version: u64,",
        "expected-definition-hash: string,",
        "client-request-fingerprint: string,",
    ] {
        assert!(CONTRACT.contains(anchor), "contract missing {anchor}");
        assert!(HOST_COPY.contains(anchor), "host copy missing {anchor}");
    }
}

#[test]
fn every_vendored_contract_refuses_deleted_vocabulary() {
    for (name, source) in [("canonical", CONTRACT), ("host", HOST_COPY)] {
        let code = without_wit_comments(source);
        for deleted in [
            "cancel:",
            "cancelled(",
            "outcome-expired",
            "accepted(",
            "pending(",
        ] {
            assert!(
                !code.contains(deleted),
                "{name} contract restored deleted vocabulary {deleted:?}"
            );
        }
    }
}

#[test]
fn runtime_world_and_plugin_register_the_exact_import() {
    assert!(WORLD.contains("world flow-invocation-plugin"));
    assert!(WORLD.contains("import wamn:flow-invocation/invocation@0.1.0;"));
    assert!(PLUGIN.contains("\"wamn:flow-invocation/invocation@0.1.0\""));
    assert!(PLUGIN.contains("invocation::add_to_linker"));
}

// wamn-0h0g.15.40: before the error channel existed the plugin converted every
// host-side failure with `Error::msg(error.to_string())`, which is a wasm trap —
// a transient store outage trapped an invocation guest and leaked the host's
// error text. Both halves must stay gone: the trap conversion, and any second
// place that decides an arm.
#[test]
fn host_failures_are_translated_once_and_never_trap_the_guest() {
    assert!(
        !PLUGIN.contains("Error::msg(error.to_string())"),
        "a host-side invocation failure must not be converted into a trap"
    );
    assert!(PLUGIN.contains("fn map_invocation_error(failure: InvocationFailure)"));
    // One definition plus exactly two uses: `begin` and `wait`, nowhere else.
    assert_eq!(PLUGIN.matches("map_invocation_error").count(), 3);
    for arm in [
        "invocation::InvocationError::StoreUnavailable",
        "invocation::InvocationError::StoreCorrupt",
        "invocation::InvocationError::UnknownRun",
        "invocation::InvocationError::InvalidRequest",
    ] {
        assert!(PLUGIN.contains(arm), "plugin never answers {arm}");
    }
    assert!(
        SERVICE.contains("pub fn kind(&self) -> InvocationError"),
        "the service must classify its own failures, not the plugin"
    );
}

#[test]
fn production_host_constructs_the_provider_without_inline_execution() {
    assert!(HOST.contains("WamnFlowInvocation::from_env()"));
    assert!(HOST.contains(".with_plugin(Arc::new("));
    assert!(HOST.contains("wamn:flow-invocation plugin init"));
    // wamn-0h0g.15.131: two needle shapes, and the split is deliberate. An
    // identifier unique to the deleted surface stays a bare substring — nothing
    // else in a host spells it. A needle whose name is ordinary English is pinned
    // as the clap field it actually was, `pub <field>:`, the shape the SERVICE list
    // below already uses for `pub project:` and `pub schema:`. Bare, those three
    // forbade naming a sibling process in a doc comment and forbade the upstream
    // `wash_runtime::host::allowed_hosts::AllowedHost` path; bare `flowrunner` did
    // fire, on prose, from wamn-0h0g.15.101 until here — unnoticed for a whole wave
    // because no sweep ran an integration-test binary. Restoring the surface still
    // means declaring the field, which `446cdca8~1` spelled exactly as
    // `pub flowrunner: PathBuf`, `pub credentials_file: Option<PathBuf>` and
    // `pub allowed_hosts: Vec<String>`. Do not re-broaden these to bare words.
    for deleted in [
        "InlineExecutionDriver",
        "inline_driver",
        "pub flowrunner:",
        "pub credentials_file:",
        "pub allowed_hosts:",
        "inline_lease_ttl_ms",
    ] {
        assert!(
            !HOST.contains(deleted),
            "production host restored deleted inline execution surface {deleted:?}"
        );
    }
    assert!(!HOST_MANIFEST.contains("wamn-execution-host"));

    for deleted in [
        "InlineRunDriver",
        "InlineRunClaim",
        "pub project:",
        "pub schema:",
        "pub executor_id:",
        "pub lease_ttl:",
    ] {
        assert!(
            !SERVICE.contains(deleted),
            "flow-invocation service restored deleted inline execution surface {deleted:?}"
        );
    }
    for deleted in [
        "InlineRunDriver",
        "inline_executor_id",
        "PROJECT_CONFIG_KEY",
        "SCHEMA_CONFIG_KEY",
    ] {
        assert!(
            !PLUGIN.contains(deleted),
            "flow-invocation plugin restored deleted inline execution surface {deleted:?}"
        );
    }
}
