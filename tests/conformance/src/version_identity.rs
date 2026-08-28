//! Guards every governed first-party version identity at the MVP `0.1` line.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use wamn_run_state::admission::{RunStateSchema, management_admission_transaction};
use wamn_run_state::invocation_context::INVOCATION_CONTEXT_VERSION;

const MVP_CARGO_VERSION: &str = "0.1.0";
const MVP_SCHEMA_VERSION: &str = "0.1";
const MVP_WIT_VERSION: &str = "0.1.0";

#[derive(Clone, Copy, Debug)]
struct GovernedLiteral {
    path: &'static str,
    exact: &'static str,
    expected_count: usize,
}

#[derive(Clone, Copy, Debug)]
struct GovernedJsonSchema {
    path: &'static str,
}

const GOVERNED_JSON_SCHEMAS: &[GovernedJsonSchema] = &[
    GovernedJsonSchema {
        path: "architecture/gate-registry.json",
    },
    GovernedJsonSchema {
        path: "architecture/package-roles.json",
    },
    GovernedJsonSchema {
        path: "architecture/protected-writes.json",
    },
    GovernedJsonSchema {
        path: "architecture/state-owners.json",
    },
    GovernedJsonSchema {
        path: "architecture/workspace-tiers.json",
    },
    GovernedJsonSchema {
        path: "tests/conformance/runtime-inventory.json",
    },
];

// This is deliberately an inventory of positive definitions, not a repository-wide
// search for version-looking text. Upstream identities and refusal/mutation fixtures
// must remain free to carry the foreign versions they prove are rejected.
const GOVERNED_LITERALS: &[GovernedLiteral] = &[
    GovernedLiteral {
        path: "crates/authoring/model/src/lib.rs",
        exact: r#"pub const SCHEMA_VERSION: &str = "0.1";"#,
        expected_count: 1,
    },
    GovernedLiteral {
        path: "crates/control/registry/src/types.rs",
        exact: r#"pub const SCHEMA_VERSION: &str = "0.1";"#,
        expected_count: 1,
    },
    // RETIRED `crates/scenarios/model/src/test_set.rs` /
    // `TEST_SET_SCHEMA_VERSION`: wamn-0h0g.15.27 (3a042d96) deleted the
    // self-describing test-set document, so the constant has no subject.
    // wamn-0h0g.15.76 (eb1c3a88) then moved the surviving file to
    // crates/execution/flow-model/src/test_set.rs, so repointing the path alone
    // would not resurrect the constant.
    //
    // RETIRED `crates/execution/flow-model/src/types.rs` / `SCHEMA_VERSION`:
    // wamn-0h0g.26.5 (7232366f) gutted flow-model down to its survivors and
    // renamed it crates/execution/contract. types.rs was DELETED, not moved —
    // the survivors are node_contract, expect, test_set, status,
    // portable_http_target and ports, none of which carries a SCHEMA_VERSION —
    // so the constant has no subject and there is nothing to repoint to.
    GovernedLiteral {
        path: "crates/events/registration/src/model.rs",
        exact: r#"pub const SCHEMA_VERSION: &str = "0.1";"#,
        expected_count: 1,
    },
    GovernedLiteral {
        path: "crates/schema/compiler/src/rls/model.rs",
        exact: r#"pub const SCHEMA_VERSION: &str = "0.1";"#,
        expected_count: 1,
    },
    GovernedLiteral {
        path: "crates/schema/compiler/src/seed/model.rs",
        exact: r#"pub const SCHEMA_VERSION: &str = "0.1";"#,
        expected_count: 1,
    },
    GovernedLiteral {
        path: "crates/schema/model/src/types.rs",
        exact: r#"pub const SCHEMA_VERSION: &str = "0.1";"#,
        expected_count: 1,
    },
    GovernedLiteral {
        path: "crates/identity/project-state/src/lib.rs",
        exact: r#"pub const SCHEMA_VERSION: &str = "0.1";"#,
        expected_count: 1,
    },
    // RETIRED `deploy/sql/authoring-tests.sql` / the `schema_version` column
    // CHECK: wamn-0h0g.15.27 (3a042d96) dropped `wamn_run.authoring_test_sets`,
    // the only relation that carried a governed schema version. The file
    // retains no `0.1` identity of any form.
    GovernedLiteral {
        path: "deploy/sql/system-schema.sql",
        exact: "INSERT INTO registry.meta (schema_version) VALUES ('0.1');",
        expected_count: 1,
    },
    GovernedLiteral {
        path: "deploy/sql/ops-schema.sql",
        exact: "-- schema_version: 0.1",
        expected_count: 1,
    },
    // RETIRED `crates/schema/control/src/run_plane.rs` /
    // `authoring_test_sets_schema_version_check`: the reconciliation CheckSpec
    // went with its table in wamn-0h0g.15.27 (3a042d96). The admission-context
    // CheckSpec below is the only governed `0.1` reconciliation identity left in
    // that file.
    GovernedLiteral {
        path: "deploy/sql/run-state.sql",
        exact: "admission_context_version text NOT NULL DEFAULT '0.1'",
        expected_count: 1,
    },
    GovernedLiteral {
        path: "deploy/sql/run-state.sql",
        exact: "CHECK (admission_context_version = '0.1'),",
        expected_count: 1,
    },
    GovernedLiteral {
        path: "crates/schema/control/src/run_plane.rs",
        exact: r#"definition: "CHECK (admission_context_version = '0.1'::text)","#,
        expected_count: 1,
    },
    GovernedLiteral {
        path: "tests/conformance/src/schema_drift.rs",
        exact: "admission_context_version text NOT NULL DEFAULT '0.1'",
        expected_count: 1,
    },
    GovernedLiteral {
        path: "crates/catalog/model/src/lib.rs",
        exact: r#"const IDENTITY_FORMAT: &[u8] = b"wamn.catalog.identity.v0.1";"#,
        expected_count: 1,
    },
    GovernedLiteral {
        path: "crates/platform/runtime/src/connection_generation.rs",
        exact: r#"pub const HTTP_CONNECTION_CONTRACT: &str = "wamn:connection/http@0.1.0";"#,
        expected_count: 1,
    },
    GovernedLiteral {
        path: "crates/platform/runtime/src/plugins/connection_http.rs",
        exact: r#"const HTTP_CONTRACT: &str = "wamn:connection/http@0.1.0";"#,
        expected_count: 1,
    },
    GovernedLiteral {
        path: "tests/conformance/src/runtime_inventory.rs",
        exact: r#"assert_eq!(inventory.schema_version, "0.1");"#,
        expected_count: 1,
    },
    GovernedLiteral {
        path: "tests/conformance/tests/package_architecture.rs",
        exact: r#"if manifest.schema_version != "0.1" {"#,
        expected_count: 1,
    },
    GovernedLiteral {
        path: "tests/conformance/tests/state_ownership.rs",
        exact: r#"if manifest.schema_version != "0.1" {"#,
        expected_count: 1,
    },
    GovernedLiteral {
        path: "tests/conformance/tests/gate_registry.rs",
        exact: r#"if registry.schema_version != "0.1" {"#,
        expected_count: 1,
    },
    GovernedLiteral {
        path: "tests/conformance/tests/gate_registry.rs",
        exact: r#".contains("wamn-kubernetes-gate-verdict/v0.1")"#,
        expected_count: 1,
    },
    GovernedLiteral {
        path: "tests/conformance/tests/workspace_tiers.rs",
        exact: r#"assert_eq!(manifest.schema_version, "0.1");"#,
        expected_count: 1,
    },
    GovernedLiteral {
        path: "tools/workspace-tier",
        exact: r#".schema_version == "0.1""#,
        expected_count: 1,
    },
    GovernedLiteral {
        path: "tests/conformance/src/kubernetes_gate_verdict.rs",
        exact: r#"pub const PROTOCOL: &str = "wamn-kubernetes-gate-verdict/v0.1";"#,
        expected_count: 1,
    },
    GovernedLiteral {
        path: "tests/conformance/src/kubernetes_gate_verdict.rs",
        exact: r#"if record.schema_version != "0.1" || record.protocol != PROTOCOL {"#,
        expected_count: 1,
    },
    GovernedLiteral {
        path: "tools/kubernetes-gate-run",
        exact: r#"--arg schema_version "0.1" --arg protocol "wamn-kubernetes-gate-verdict/v0.1""#,
        expected_count: 1,
    },
    GovernedLiteral {
        path: "tests/conformance/tests/kubernetes_gate_runner.rs",
        exact: r#""wamn-kubernetes-gate-verdict/v0.1""#,
        expected_count: 1,
    },
    GovernedLiteral {
        path: "architecture/gate-registry.json",
        exact: "wamn-kubernetes-gate-verdict/v0.1",
        expected_count: 1,
    },
    GovernedLiteral {
        path: "tests/integration/src/catalog_live.rs",
        exact: "VALUES ($1, 'catalog', 1, 'dev', '0.1', 'applied',",
        expected_count: 1,
    },
    GovernedLiteral {
        path: "tests/integration/src/catalog_live.rs",
        exact: r#"{\"schema-version\":\"0.1\",\"catalog-id\":\"catalog\""#,
        expected_count: 1,
    },
];

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("tests/conformance lives two levels below the repository root")
        .to_path_buf()
}

fn cargo_metadata(workspace_manifest: &Path) -> Value {
    let output = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--locked",
            "--offline",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(workspace_manifest)
        .output()
        .expect("run cargo metadata for MVP version guard");
    assert!(
        output.status.success(),
        "cargo metadata failed for {}:\n{}",
        workspace_manifest.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("parse cargo metadata")
}

fn workspace_version_violations(metadata: &Value) -> Vec<String> {
    let members: HashSet<&str> = metadata["workspace_members"]
        .as_array()
        .expect("workspace_members array")
        .iter()
        .map(|member| member.as_str().expect("workspace member id"))
        .collect();

    metadata["packages"]
        .as_array()
        .expect("packages array")
        .iter()
        .filter(|package| {
            members.contains(
                package["id"]
                    .as_str()
                    .expect("package id in cargo metadata"),
            )
        })
        .filter_map(|package| {
            let version = package["version"]
                .as_str()
                .expect("package version in cargo metadata");
            (version != MVP_CARGO_VERSION).then(|| {
                format!(
                    "workspace package {} is version {version}, expected {MVP_CARGO_VERSION}",
                    package["name"]
                        .as_str()
                        .expect("package name in cargo metadata")
                )
            })
        })
        .collect()
}

fn tracked_wit_files(repository: &Path) -> Vec<PathBuf> {
    let output = Command::new("git")
        .args(["-C"])
        .arg(repository)
        .args(["ls-files", "-z", "--", "*.wit"])
        .output()
        .expect("list tracked WIT files");
    assert!(
        output.status.success(),
        "git ls-files failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| std::str::from_utf8(path).expect("tracked WIT path must be valid UTF-8"))
        .filter(|path| !Path::new(path).starts_with("docs/archive"))
        .filter(|path| repository.join(path).is_file())
        .map(|path| repository.join(path))
        .collect()
}

fn wamn_wit_package_violations(path: &Path, source: &str) -> (usize, Vec<String>) {
    let mut governed_count = 0;
    let mut violations = Vec::new();

    for (line_index, line) in source.lines().enumerate() {
        let Some(declaration) = line.trim().strip_prefix("package ") else {
            continue;
        };
        let declaration = declaration
            .split_once(';')
            .map_or(declaration.trim(), |(package, _)| package.trim());
        let package_name = declaration
            .split_once('@')
            .map_or(declaration, |(name, _)| name);
        if !package_name.starts_with("wamn:") {
            continue;
        }

        governed_count += 1;
        let version = declaration.split_once('@').map(|(_, version)| version);
        if version != Some(MVP_WIT_VERSION) {
            violations.push(format!(
                "{}:{}: WAMN package `{declaration}` must use @{MVP_WIT_VERSION}",
                path.display(),
                line_index + 1
            ));
        }
    }

    (governed_count, violations)
}

fn governed_literal_violation(source: &str, identity: GovernedLiteral) -> Option<String> {
    let actual_count = source.matches(identity.exact).count();
    (actual_count != identity.expected_count).then(|| {
        format!(
            "{}: expected {} occurrence(s) of governed identity `{}`, found {actual_count}",
            identity.path, identity.expected_count, identity.exact
        )
    })
}

fn governed_json_schema_violation(source: &str, identity: GovernedJsonSchema) -> Option<String> {
    let document: Value = match serde_json::from_str(source) {
        Ok(document) => document,
        Err(error) => {
            return Some(format!("{}: invalid JSON: {error}", identity.path));
        }
    };
    let actual = document.get("schema_version");
    (actual.and_then(Value::as_str) != Some(MVP_SCHEMA_VERSION)).then(|| {
        format!(
            "{}: schema_version must be textual {MVP_SCHEMA_VERSION}, found {actual:?}",
            identity.path
        )
    })
}

fn admission_version_violations(admission: &str, version: &str) -> Vec<String> {
    let mut violations = Vec::new();
    let admitted_context = admission
        .split_once("expected AS MATERIALIZED (")
        .and_then(|(_, tail)| tail.split_once("keyed_run AS MATERIALIZED ("))
        .map(|(section, _)| section);
    let context_identity = format!("'version', '{version}'");
    if !admitted_context.is_some_and(|section| section.contains(&context_identity)) {
        violations.push(format!(
            "management admission does not stamp invocation-context owner `{version}`"
        ));
    }

    let created_run = admission
        .split_once("created_run AS (")
        .and_then(|(_, tail)| tail.split_once("created_queue AS ("))
        .map(|(section, _)| section);
    let persisted_identity = format!("'{version}', c.platform_revision");
    if !created_run.is_some_and(|section| {
        section.contains("admission_context_version") && section.contains(&persisted_identity)
    }) {
        violations.push(format!(
            "management admission does not persist invocation-context owner `{version}`"
        ));
    }
    violations
}

fn live_invocation_context_version_violations() -> Vec<String> {
    let mut violations = Vec::new();
    if INVOCATION_CONTEXT_VERSION != MVP_SCHEMA_VERSION {
        violations.push(format!(
            "invocation-context owner is {INVOCATION_CONTEXT_VERSION}, expected {MVP_SCHEMA_VERSION}"
        ));
    }
    let admission = management_admission_transaction(&RunStateSchema::default());
    violations.extend(admission_version_violations(
        admission.admit(),
        INVOCATION_CONTEXT_VERSION,
    ));
    violations
}

fn governed_literal_violations(repository: &Path) -> Vec<String> {
    let mut violations = Vec::new();
    for identity in GOVERNED_LITERALS {
        // Absence and count are independent faults and one entry can carry both,
        // so an unreadable file still owes the occurrence it was watched for.
        // wamn-0h0g.15.110's first entry had MOVED (eb1c3a88) and had SEPARATELY
        // lost its constant (3a042d96): reporting only the missing file read as a
        // path needing correction, and correcting the path alone would have turned
        // a file-mode failure into found 0 rather than a pass.
        let source = match std::fs::read_to_string(repository.join(identity.path)) {
            Ok(source) => source,
            Err(error) => {
                violations.push(format!("{}: {error}", identity.path));
                String::new()
            }
        };
        if let Some(violation) = governed_literal_violation(&source, *identity) {
            violations.push(violation);
        }
    }
    for identity in GOVERNED_JSON_SCHEMAS {
        match std::fs::read_to_string(repository.join(identity.path)) {
            Ok(source) => {
                if let Some(violation) = governed_json_schema_violation(&source, *identity) {
                    violations.push(violation);
                }
            }
            Err(error) => violations.push(format!("{}: {error}", identity.path)),
        }
    }
    violations
}

#[test]
fn workspace_owned_packages_stay_at_mvp_version() {
    let repository = repository_root();
    let mut violations = Vec::new();
    for manifest in [
        repository.join("Cargo.toml"),
        repository.join("components/Cargo.toml"),
    ] {
        violations.extend(workspace_version_violations(&cargo_metadata(&manifest)));
    }

    assert!(
        violations.is_empty(),
        "workspace-owned package version drift:\n{}",
        violations.join("\n")
    );
}

#[test]
fn wamn_wit_packages_stay_at_mvp_version() {
    let repository = repository_root();
    let mut governed_count = 0;
    let mut violations = Vec::new();
    for path in tracked_wit_files(&repository) {
        let source = std::fs::read_to_string(&path).expect("read tracked WIT file");
        let (local_count, local_violations) = wamn_wit_package_violations(&path, &source);
        governed_count += local_count;
        violations.extend(local_violations);
    }

    assert!(
        governed_count > 0,
        "WAMN WIT package inventory must not be empty"
    );
    assert!(
        violations.is_empty(),
        "WAMN WIT package version drift:\n{}",
        violations.join("\n")
    );
}

#[test]
fn governed_wire_schema_and_artifact_versions_stay_at_mvp_identity() {
    let mut violations = governed_literal_violations(&repository_root());
    violations.extend(live_invocation_context_version_violations());
    assert!(
        violations.is_empty(),
        "governed first-party version drift:\n{}",
        violations.join("\n")
    );
}

#[test]
fn representative_version_mutants_are_rejected() {
    let cargo_mutant = serde_json::json!({
        "workspace_members": ["path+file:///repo/crates/example#wamn-example@0.2.0"],
        "packages": [{
            "id": "path+file:///repo/crates/example#wamn-example@0.2.0",
            "name": "wamn-example",
            "version": "0.2.0"
        }]
    });
    assert_eq!(workspace_version_violations(&cargo_mutant).len(), 1);

    let (wamn_count, wit_violations) = wamn_wit_package_violations(
        Path::new("mutant.wit"),
        "package wamn:mutant@0.2.0;\npackage wasi:clocks@0.2.0;",
    );
    assert_eq!(wamn_count, 1);
    assert_eq!(wit_violations.len(), 1);

    let schema_identity = GovernedLiteral {
        path: "mutant.rs",
        exact: r#"pub const SCHEMA_VERSION: &str = "0.1";"#,
        expected_count: 1,
    };
    assert!(
        governed_literal_violation(
            r#"pub const SCHEMA_VERSION: &str = "0.2";"#,
            schema_identity
        )
        .is_some()
    );

    let artifact_identity = GovernedLiteral {
        path: "mutant.rs",
        exact: r#"const REVISION: &str = "wamn-runtime@0.1.0";"#,
        expected_count: 1,
    };
    assert!(
        governed_literal_violation(
            r#"const REVISION: &str = "wamn-runtime@0.1.1";"#,
            artifact_identity
        )
        .is_some()
    );

    let governance_identity = GovernedJsonSchema {
        path: "mutant.json",
    };
    assert!(
        governed_json_schema_violation(r#"{"schema_version":1}"#, governance_identity).is_some()
    );

    let admission = management_admission_transaction(&RunStateSchema::default());
    let drifted_admission = admission
        .admit()
        .replace("'version', '0.1'", "'version', '0.2'")
        .replace("'0.1', c.platform_revision", "'0.2', c.platform_revision");
    assert_eq!(
        admission_version_violations(&drifted_admission, INVOCATION_CONTEXT_VERSION).len(),
        2,
        "both live admission uses must reject drift from the version owner"
    );
}

#[test]
fn a_missing_watched_file_still_reports_its_missing_occurrence() {
    // Under a root where nothing is readable, every watch entry carries both
    // faults at once: the file is absent AND the identity it was watched for is
    // unaccounted for. Reporting only the first is what made wamn-0h0g.15.110's
    // first entry read as a path needing correction, when the constant had
    // separately been deleted and no path would have brought it back.
    let violations = governed_literal_violations(&repository_root().join("no-such-subtree"));
    let owed = GOVERNED_LITERALS
        .iter()
        .filter(|identity| identity.expected_count > 0)
        .count();
    let occurrences = violations
        .iter()
        .filter(|violation| violation.contains("occurrence(s) of governed identity"))
        .count();
    let reported = violations.len();
    let expected = GOVERNED_LITERALS.len() + owed + GOVERNED_JSON_SCHEMAS.len();

    assert_eq!(
        occurrences, owed,
        "an unreadable watched file still owes every occurrence its entry expects"
    );
    assert_eq!(
        reported, expected,
        "absence must be reported alongside the missing occurrence, not instead of it"
    );
}
