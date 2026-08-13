use std::process::Command;

const MVP_VERBS: &[&str] = &[
    "publish-catalog",
    "provision-project",
    "provision-org",
    "provision-project-env",
    "enable-cdc-project-env",
    "migrate-catalog",
    "reconcile-replica-identity",
    "reconcile-run-plane",
];

const OPS_VERBS: &[&str] = &[
    "dump-project-env",
    "restore-project-env",
    "copy-project-env",
    "prune-run-history",
    "impact-report",
];

fn help(binary: &str) -> String {
    let output = Command::new(binary)
        .arg("--help")
        .output()
        .expect("run ctl help");
    assert!(output.status.success());
    String::from_utf8(output.stdout).expect("help is UTF-8")
}

fn command_help(binary: &str, command: &str) -> String {
    let output = Command::new(binary)
        .args([command, "--help"])
        .output()
        .expect("run ctl command help");
    assert!(output.status.success());
    String::from_utf8(output.stdout).expect("help is UTF-8")
}

#[test]
fn mvp_binary_exposes_only_mvp_verbs() {
    let output = help(env!("CARGO_BIN_EXE_wamn-ctl"));
    for verb in MVP_VERBS {
        assert!(output.contains(verb), "MVP help omitted {verb}");
    }
    for verb in OPS_VERBS.iter().chain(["pin-run"].iter()) {
        assert!(!output.contains(verb), "MVP help exposed {verb}");
    }
}

#[test]
fn mvp_migrate_catalog_has_no_destructive_override() {
    let output = command_help(env!("CARGO_BIN_EXE_wamn-ctl"), "migrate-catalog");
    for flag in ["--confirm-with-backup", "--acknowledge-impact"] {
        assert!(!output.contains(flag), "MVP migrate-catalog exposed {flag}");
    }
    assert!(
        output.contains("--dry-run"),
        "MVP migrate-catalog lost dry-run"
    );
}

#[test]
fn impact_effect_shell_is_ops_only() {
    let source = include_str!("../src/lib.rs");
    assert!(
        source.contains("#[cfg(feature = \"ops\")]\npub mod impact_report;"),
        "default ctl exposed the impact-report effect shell"
    );
}

#[cfg(feature = "ops")]
#[test]
fn ops_binary_exposes_only_ops_verbs() {
    let output = help(env!("CARGO_BIN_EXE_wamn-ctl-ops"));
    for verb in OPS_VERBS {
        assert!(output.contains(verb), "ops help omitted {verb}");
    }
    for verb in MVP_VERBS.iter().chain(["pin-run"].iter()) {
        assert!(!output.contains(verb), "ops help exposed {verb}");
    }
}
