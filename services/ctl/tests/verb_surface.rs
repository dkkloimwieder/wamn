mod support;

use std::collections::BTreeSet;
use std::fs::{OpenOptions, TryLockError};
use std::process::Command;

const LOCK_CHILD_ENV: &str = "WAMN_CTL_LOCK_CHILD";

const MVP_VERBS: &[&str] = &[
    "provision-project",
    "provision-org",
    "provision-project-env",
    "enable-cdc-project-env",
    "migrate-catalog",
    "push-component",
    "reconcile-replica-identity",
    "reconcile-run-plane",
    "terminalize-effect-uncertain",
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
fn ctl_live_database_lock_inventory_is_exact() {
    const CTL_DATABASE_URL_ENV: &str = concat!("WAMN_CTL_", "PG_URL");
    const SOURCES: [(&str, &str, usize, usize); 9] = [
        (
            "catalog_confinement_live.rs",
            include_str!("catalog_confinement_live.rs"),
            1,
            0,
        ),
        (
            "dispatch_reader_provisioning_live.rs",
            include_str!("dispatch_reader_provisioning_live.rs"),
            1,
            0,
        ),
        (
            "impact_report_live.rs",
            include_str!("impact_report_live.rs"),
            1,
            0,
        ),
        (
            "orphan_guard_live.rs",
            include_str!("orphan_guard_live.rs"),
            0,
            3,
        ),
        (
            "protected_relations_live.rs",
            include_str!("protected_relations_live.rs"),
            0,
            1,
        ),
        (
            "replica_identity_live.rs",
            include_str!("replica_identity_live.rs"),
            1,
            0,
        ),
        (
            "replica_identity_unreadable_live.rs",
            include_str!("replica_identity_unreadable_live.rs"),
            2,
            0,
        ),
        ("ri_orch_live.rs", include_str!("ri_orch_live.rs"), 0, 2),
        (
            "run_plane_live.rs",
            include_str!("run_plane_live.rs"),
            11,
            4,
        ),
    ];

    let _optional: fn() -> Option<support::LockedUrl> = support::LockedUrl::optional;
    let _required: fn(&str) -> support::LockedUrl = support::LockedUrl::required;
    let expected: BTreeSet<String> = SOURCES
        .iter()
        .map(|(path, _, _, _)| (*path).to_string())
        .collect();
    let mut actual = BTreeSet::new();
    let tests = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    for entry in std::fs::read_dir(tests).expect("read the wamn-ctl test directory") {
        let path = entry.expect("read a wamn-ctl test entry").path();
        if path.extension().is_none_or(|extension| extension != "rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("read a wamn-ctl test source");
        if source.contains(CTL_DATABASE_URL_ENV) {
            actual.insert(
                path.file_name()
                    .expect("test source has a file name")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    assert_eq!(actual, expected, "control live-URL test-binary inventory");

    let direct_env_read = format!("std::env::var(\"{CTL_DATABASE_URL_ENV}\")");
    let mut optional = 0;
    let mut required = 0;
    for (path, source, expected_optional, expected_required) in SOURCES {
        assert_eq!(
            source.matches("mod support;").count(),
            1,
            "{path} must import the shared lock exactly once"
        );
        assert!(
            !source.contains(&direct_env_read),
            "{path} bypasses the shared lock"
        );
        let actual_optional = source.matches("support::LockedUrl::optional()").count();
        let actual_required = source.matches("support::LockedUrl::required(").count();
        assert_eq!(
            actual_optional, expected_optional,
            "{path} optional entries"
        );
        assert_eq!(
            actual_required, expected_required,
            "{path} required entries"
        );
        optional += actual_optional;
        required += actual_required;
    }
    assert_eq!((optional, required), (17, 10));
}

#[test]
#[ignore = "child process for ctl_live_database_lock_excludes_another_process"]
fn ctl_live_database_lock_child_observes_parent() {
    if std::env::var_os(LOCK_CHILD_ENV).is_none() {
        return;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(std::env::temp_dir().join(support::LOCK_FILE_NAME))
        .expect("open the child-process lock file");
    assert!(
        matches!(file.try_lock(), Err(TryLockError::WouldBlock)),
        "the child process acquired the parent process's live-database lock"
    );
}

#[test]
fn ctl_live_database_lock_excludes_another_process() {
    let _lock = support::lock();
    let output =
        Command::new(std::env::current_exe().expect("locate the verb-surface test binary"))
            .args([
                "--ignored",
                "--exact",
                "ctl_live_database_lock_child_observes_parent",
            ])
            .env(LOCK_CHILD_ENV, "1")
            .output()
            .expect("run the child-process lock probe");
    assert!(
        output.status.success(),
        "child-process lock probe failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
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
fn replica_identity_repair_is_an_explicit_one_shot_operator_surface() {
    let output = command_help(env!("CARGO_BIN_EXE_wamn-ctl"), "reconcile-replica-identity");
    assert!(
        output.contains("Detect or repair per-entity REPLICA IDENTITY drift"),
        "operator help must state the command's drift-repair purpose"
    );
    for flag in ["--admin-database-url", "--catalog", "--schema", "--dry-run"] {
        assert!(output.contains(flag), "operator repair omitted {flag}");
    }
    for recurring in ["--schedule", "--cadence"] {
        assert!(
            !output.contains(recurring),
            "one-shot operator repair exposed recurring control {recurring}"
        );
    }
}

#[test]
fn run_plane_reconcile_requires_the_project_target_key() {
    let binary = env!("CARGO_BIN_EXE_wamn-ctl");
    let output = Command::new(binary)
        .args([
            "reconcile-run-plane",
            "--system-database-url",
            "postgres://registry.invalid/system",
            "--admin-database-url",
            "postgres://target.invalid/project",
            "--org",
            "acme",
            "--tenant",
            "t1",
            "--env",
            "dev",
            "--schema",
            "run_plane",
        ])
        .output()
        .expect("parse reconcile-run-plane without a project");
    assert!(!output.status.success(), "missing --project was accepted");
    let stderr = String::from_utf8(output.stderr).expect("parse error is UTF-8");
    assert!(
        stderr.contains("--project <PROJECT>"),
        "missing-target refusal did not name --project: {stderr}"
    );

    let help = command_help(binary, "reconcile-run-plane");
    assert!(
        help.contains("--project <PROJECT>"),
        "run-plane help omitted the project target key"
    );
}

#[test]
#[cfg(not(feature = "ops"))]
fn mvp_provision_org_has_no_backup_surface() {
    let output = command_help(env!("CARGO_BIN_EXE_wamn-ctl"), "provision-org");
    for flag in ["--emit-object-store", "--emit-scheduled-backup"] {
        assert!(!output.contains(flag), "MVP provision-org exposed {flag}");
    }
}

#[test]
#[cfg(feature = "ops")]
fn ops_feature_restores_provision_org_backup_surface() {
    let output = command_help(env!("CARGO_BIN_EXE_wamn-ctl"), "provision-org");
    for flag in ["--emit-object-store", "--emit-scheduled-backup"] {
        assert!(output.contains(flag), "ops provision-org omitted {flag}");
    }
}

#[test]
fn mvp_dependency_tree_does_not_enable_ops() {
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("ctl crate is below the repository root");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .current_dir(repository)
        .args([
            "tree",
            "--locked",
            "--offline",
            "-p",
            "wamn-ctl",
            "--edges",
            "features",
        ])
        .output()
        .expect("run cargo tree for the default ctl package");
    assert!(output.status.success());
    let tree = String::from_utf8(output.stdout).expect("cargo tree output is UTF-8");
    for feature in [
        "wamn-control-provision feature \"ops\"",
        "wamn-schema-compiler feature \"ops\"",
        "wamn-schema-control feature \"ops\"",
    ] {
        assert!(
            !tree.contains(feature),
            "default ctl enabled {feature}\n{tree}"
        );
    }

    let direct = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .current_dir(repository)
        .args([
            "tree",
            "--locked",
            "--offline",
            "-p",
            "wamn-ctl",
            "--edges",
            "normal",
            "--depth",
            "1",
        ])
        .output()
        .expect("run direct dependency tree for default ctl");
    assert!(direct.status.success());
    let direct = String::from_utf8(direct.stdout).expect("cargo tree output is UTF-8");
    assert!(
        direct.lines().any(|line| line.contains("wamn-run-state ")),
        "default ctl omitted the terminalization transaction owner\n{direct}"
    );
}

#[test]
fn terminalize_surface_accepts_no_asserted_effect_identity_or_outcome() {
    let output = command_help(
        env!("CARGO_BIN_EXE_wamn-ctl"),
        "terminalize-effect-uncertain",
    );
    for required in [
        "--tenant",
        "--run",
        "--basis",
        "--evidence-ref",
        "--correlation-id",
    ] {
        assert!(output.contains(required), "terminalize omitted {required}");
    }
    for forbidden in [
        "--attempt",
        "--node",
        "--outcome",
        "--success",
        "--continue",
        "--selector",
        "--all",
    ] {
        assert!(
            !output.contains(forbidden),
            "terminalize exposed {forbidden}"
        );
    }
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
