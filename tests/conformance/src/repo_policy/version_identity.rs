//! Every governed first-party version identity stays at the MVP `0.1` line.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use wamn_run_state::invocation_context::INVOCATION_CONTEXT_VERSION;

use super::Problems;

const MVP_CARGO_VERSION: &str = "0.1.0";
const MVP_SCHEMA_VERSION: &str = "0.1";
const MVP_WIT_VERSION: &str = "0.1.0";

#[derive(Clone, Copy, Debug)]
struct GovernedLiteral {
    path: &'static str,
    exact: &'static str,
    expected_count: usize,
}

// This is deliberately a list of positive definitions, not a repository-wide
// search for version-looking text. Upstream identities and refusal/mutation fixtures
// must remain free to carry the foreign versions they show are rejected.
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
        path: "apps/platform/events/registration/src/model.rs",
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
    // that declaration set, which `75ffd53e7` moved out of `run_plane.rs` into
    // `run_plane/declarations.rs` unchanged.
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
        path: "crates/schema/control/src/run_plane/declarations.rs",
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
];

pub(super) fn check(root: &Path, problems: &mut Problems) {
    for manifest in [root.join("Cargo.toml"), root.join("apps/Cargo.toml")] {
        match cargo_metadata(&manifest) {
            Ok(metadata) => workspace_version_violations(&metadata, problems),
            Err(problem) => problems.push(problem),
        }
    }
    wamn_wit_packages_stay_at_mvp_version(root, problems);
    governed_literal_violations(root, problems);
    problems.require(INVOCATION_CONTEXT_VERSION == MVP_SCHEMA_VERSION, || {
        format!(
            "invocation-context owner is {INVOCATION_CONTEXT_VERSION}, expected {MVP_SCHEMA_VERSION}"
        )
    });
}

fn cargo_metadata(workspace_manifest: &Path) -> Result<Value, String> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
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
        .map_err(|error| format!("run cargo metadata for the MVP version lint: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed for {}:\n{}",
            workspace_manifest.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| format!("parse cargo metadata: {error}"))
}

fn workspace_version_violations(metadata: &Value, problems: &mut Problems) {
    let members: HashSet<&str> = metadata["workspace_members"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();

    for package in metadata["packages"].as_array().into_iter().flatten() {
        if !package["id"]
            .as_str()
            .is_some_and(|id| members.contains(id))
        {
            continue;
        }
        let version = package["version"].as_str().unwrap_or_default();
        problems.require(version == MVP_CARGO_VERSION, || {
            format!(
                "workspace package {} is version {version}, expected {MVP_CARGO_VERSION}",
                package["name"].as_str().unwrap_or_default()
            )
        });
    }
}

fn tracked_wit_files(repository: &Path) -> Result<Vec<PathBuf>, String> {
    let output = Command::new("git")
        .args(["-C"])
        .arg(repository)
        .args(["ls-files", "-z", "--", "*.wit"])
        .output()
        .map_err(|error| format!("list tracked WIT files: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git ls-files failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .filter_map(|path| std::str::from_utf8(path).ok())
        .filter(|path| !Path::new(path).starts_with("docs/archive"))
        .filter(|path| repository.join(path).is_file())
        .map(|path| repository.join(path))
        .collect())
}

fn wamn_wit_packages_stay_at_mvp_version(root: &Path, problems: &mut Problems) {
    let files = match tracked_wit_files(root) {
        Ok(files) => files,
        Err(problem) => {
            problems.push(problem);
            return;
        }
    };
    let mut governed_count = 0;
    for path in files {
        let source = match std::fs::read_to_string(&path) {
            Ok(source) => source,
            Err(error) => {
                problems.push(format!("{}: {error}", path.display()));
                continue;
            }
        };
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
            problems.require(version == Some(MVP_WIT_VERSION), || {
                format!(
                    "{}:{}: WAMN package `{declaration}` must use @{MVP_WIT_VERSION}",
                    path.display(),
                    line_index + 1
                )
            });
        }
    }
    problems.require(governed_count > 0, || {
        "WAMN WIT package list must not be empty".to_owned()
    });
}

fn governed_literal_violations(repository: &Path, problems: &mut Problems) {
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
                problems.push(format!("{}: {error}", identity.path));
                String::new()
            }
        };
        let actual_count = source.matches(identity.exact).count();
        problems.require(actual_count == identity.expected_count, || {
            format!(
                "{}: expected {} occurrence(s) of governed identity `{}`, found {actual_count}",
                identity.path, identity.expected_count, identity.exact
            )
        });
    }
}
