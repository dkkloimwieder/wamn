use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const TIER_MANIFEST: &str = "architecture/workspace-tiers.json";
const ROOT_MANIFEST: &str = "Cargo.toml";
/// The guests live in more than one Cargo workspace. Feature unification is
/// additive-only inside one invocation, so the `no_std` palette guests are
/// isolated from the members that reach `serde_json/std` (wamn-0h0g.11.56).
const COMPONENT_MANIFESTS: [&str; 2] = ["components/Cargo.toml", "components/no-std/Cargo.toml"];
const PROFILE_TOOL: &str = "tools/profile";
const COMPONENT_TOOL: &str = "tools/build-components";

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
    workspace_members: Vec<String>,
    workspace_default_members: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    id: String,
    name: String,
    features: BTreeMap<String, Vec<String>>,
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

fn read_contract(root: &Path) -> Value {
    let path = root.join(TIER_MANIFEST);
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    serde_json::from_str(&source)
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()))
}

fn cargo_metadata_output(root: &Path, manifest: &str) -> Output {
    Command::new(env!("CARGO"))
        .current_dir(root)
        .args([
            "metadata",
            "--manifest-path",
            manifest,
            "--locked",
            "--offline",
            "--no-deps",
            "--format-version",
            "1",
        ])
        .output()
        .unwrap_or_else(|error| panic!("failed to run Cargo metadata for {manifest}: {error}"))
}

fn parse_metadata(output: &Output, manifest: &str) -> CargoMetadata {
    assert!(
        output.status.success(),
        "Cargo metadata failed for {manifest}:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("invalid Cargo metadata for {manifest}: {error}"))
}

fn names_for_ids(metadata: &CargoMetadata, ids: &[String]) -> Vec<String> {
    let names = metadata
        .packages
        .iter()
        .map(|package| (package.id.as_str(), package.name.as_str()))
        .collect::<BTreeMap<_, _>>();
    ids.iter()
        .map(|id| {
            names
                .get(id.as_str())
                .unwrap_or_else(|| panic!("workspace package id {id} missing from metadata"))
                .to_string()
        })
        .collect()
}

fn string_array(contract: &Value, pointer: &str) -> Vec<String> {
    contract
        .pointer(pointer)
        .unwrap_or_else(|| panic!("profile contract omitted {pointer}"))
        .as_array()
        .unwrap_or_else(|| panic!("profile contract {pointer} must be an array"))
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("profile contract {pointer} must contain strings"))
                .to_string()
        })
        .collect()
}

fn string_value<'a>(contract: &'a Value, pointer: &str) -> &'a str {
    contract
        .pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("profile contract omitted string {pointer}"))
}

fn root_profile_packages(contract: &Value, metadata: &CargoMetadata, profile: &str) -> Vec<String> {
    if matches!(profile, "full" | "ops") {
        let tier = string_value(contract, "/profiles/root/full_inventory_tier");
        return string_array(contract, &format!("/tiers/{tier}/root_packages"));
    }

    let mut selected = names_for_ids(metadata, &metadata.workspace_default_members);
    selected.extend(string_array(contract, "/profiles/root/m1_additions"));
    if matches!(profile, "m2" | "deploy") {
        selected.extend(string_array(contract, "/profiles/root/m2_additions"));
    }
    if profile == "deploy" {
        selected.extend(string_array(contract, "/profiles/root/deploy_additions"));
    }
    selected
}

fn component_profile_packages(contract: &Value, profile: &str) -> Vec<String> {
    let pointer = if profile == "m1" {
        "/profiles/components/m1_inventory_tier"
    } else {
        "/profiles/components/proof_inventory_tier"
    };
    let tier = string_value(contract, pointer);
    string_array(contract, &format!("/tiers/{tier}/component_packages"))
}

fn set(values: &[String]) -> BTreeSet<String> {
    values.iter().cloned().collect()
}

fn assert_exact_set(label: &str, actual: &[String], expected: &[&str]) {
    let actual = set(actual);
    let expected = expected
        .iter()
        .map(|value| (*value).to_string())
        .collect::<BTreeSet<_>>();
    let extra = actual.difference(&expected).cloned().collect::<Vec<_>>();
    let missing = expected.difference(&actual).cloned().collect::<Vec<_>>();
    assert!(
        extra.is_empty() && missing.is_empty(),
        "{label} drifted; extra={extra:?}; missing={missing:?}"
    );
}

fn assert_unique(label: &str, values: &[String]) {
    assert_eq!(
        values.len(),
        set(values).len(),
        "{label} contains a duplicate package"
    );
}

#[test]
fn profile_contract_matches_locked_metadata() {
    let root = repository_root();
    let contract = read_contract(&root);
    let root_output = cargo_metadata_output(&root, ROOT_MANIFEST);
    let root_metadata = parse_metadata(&root_output, ROOT_MANIFEST);
    let root_members = names_for_ids(&root_metadata, &root_metadata.workspace_members);
    let mut component_members = Vec::new();
    for manifest in COMPONENT_MANIFESTS {
        let output = cargo_metadata_output(&root, manifest);
        let metadata = parse_metadata(&output, manifest);
        let members = names_for_ids(&metadata, &metadata.workspace_members);
        assert_unique(manifest, &members);
        component_members.extend(members);
    }

    assert_eq!(root_members.len(), 35);
    assert_eq!(component_members.len(), 7);
    assert_unique("root workspace metadata", &root_members);
    assert_unique("component workspace metadata", &component_members);

    assert_exact_set(
        "m1 additions",
        &string_array(&contract, "/profiles/root/m1_additions"),
        &[
            "wamn-event-reg",
            "wamn-event-wire",
            "wamn-materializer",
            "wamn-cdc-reader",
        ],
    );
    assert_exact_set(
        "m2 additions",
        &string_array(&contract, "/profiles/root/m2_additions"),
        &["wamn-dispatcher", "wamn-waker"],
    );
    assert_exact_set(
        "deploy additions",
        &string_array(&contract, "/profiles/root/deploy_additions"),
        &[
            "wamn-ctl",
            "wamn-control-provision",
            "wamn-control-registry",
            "wamn-project-state",
            "wamn-schema-control",
            "wamn-schema-compiler",
        ],
    );
    assert_eq!(
        string_array(&contract, "/profiles/root/ops_features"),
        ["wamn-ctl/ops"]
    );
    assert!(
        root_metadata
            .packages
            .iter()
            .find(|package| package.name == "wamn-ctl")
            .expect("wamn-ctl must exist in locked metadata")
            .features
            .contains_key("ops"),
        "wamn-ctl/ops must exist in locked metadata"
    );

    let profile_counts = [
        ("m1", 21),
        ("m2", 23),
        ("deploy", 29),
        ("full", 35),
        ("ops", 35),
    ];
    let mut profiles = BTreeMap::new();
    for (profile, expected_count) in profile_counts {
        let packages = root_profile_packages(&contract, &root_metadata, profile);
        assert_eq!(packages.len(), expected_count, "{profile} package count");
        assert_unique(profile, &packages);
        assert!(
            set(&packages).is_subset(&set(&root_members)),
            "{profile} selected a name outside locked metadata"
        );
        assert_eq!(
            contract
                .pointer(&format!("/profiles/root/expected_package_counts/{profile}"))
                .and_then(Value::as_u64),
            Some(expected_count as u64),
            "{profile} manifest count"
        );
        profiles.insert(profile, packages);
    }

    let root_defaults = names_for_ids(&root_metadata, &root_metadata.workspace_default_members);
    assert_eq!(
        set(&profiles["m1"])
            .difference(&set(&root_defaults))
            .cloned()
            .collect::<BTreeSet<_>>(),
        string_array(&contract, "/profiles/root/m1_additions")
            .into_iter()
            .collect()
    );
    assert_eq!(set(&profiles["ops"]), set(&profiles["full"]));
    assert_eq!(
        set(&profiles["m2"])
            .difference(&set(&profiles["m1"]))
            .cloned()
            .collect::<BTreeSet<_>>(),
        ["wamn-dispatcher", "wamn-waker"]
            .into_iter()
            .map(str::to_string)
            .collect()
    );
    assert_eq!(
        set(&profiles["deploy"])
            .difference(&set(&profiles["m2"]))
            .cloned()
            .collect::<BTreeSet<_>>(),
        string_array(&contract, "/profiles/root/deploy_additions")
            .into_iter()
            .collect()
    );
    assert_eq!(
        set(&profiles["deploy"]),
        set(&string_array(
            &contract,
            "/tiers/fast_developer_native/root_packages"
        ))
    );
    assert_eq!(set(&profiles["full"]), set(&root_members));

    let component_m1 = component_profile_packages(&contract, "m1");
    let component_proof = component_profile_packages(&contract, "proof");
    assert_exact_set(
        "component m1",
        &component_m1,
        &["flow-http", "http-request", "materializer", "transform"],
    );
    assert_exact_set(
        "component proof",
        &component_proof,
        &[
            "busyloop",
            "connection-http-standard",
            "flow-http",
            "http-request",
            "materializer",
            "sockprobe",
            "transform",
        ],
    );
    assert_eq!(set(&component_proof), set(&component_members));
    assert_eq!(component_m1.len(), 4);
    assert_eq!(component_proof.len(), 7);
    assert_unique("component m1", &component_m1);
    assert_unique("component proof", &component_proof);
    assert_eq!(
        set(&component_proof)
            .difference(&set(&component_m1))
            .cloned()
            .collect::<BTreeSet<_>>(),
        ["busyloop", "connection-http-standard", "sockprobe"]
            .into_iter()
            .map(str::to_string)
            .collect()
    );
}

fn scratch_directory(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must follow Unix epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "wamn profile selectors {label} {} {nonce}",
        std::process::id()
    ));
    fs::create_dir(&path).expect("failed to create selector scratch directory");
    path
}

fn write_fake_cargo(scratch: &Path) -> PathBuf {
    let fake = scratch.join("fake cargo");
    // A metadata reply is keyed by the manifest it was asked about: more than
    // one component workspace exists, and one canned reply for all of them
    // would let a tool that reads the wrong workspace still pass.
    fs::write(
        &fake,
        r#"#!/usr/bin/env bash
set -euo pipefail
{
  printf '%s\0' "$PWD" "$@"
  printf '\036'
} >> "$WAMN_FAKE_CARGO_LOG"
if [[ "${1:-}" == metadata ]]; then
  manifest=''
  while (($# > 0)); do
    if [[ "$1" == --manifest-path ]]; then
      manifest="$2"
    fi
    shift
  done
  command cat -- "$WAMN_FAKE_METADATA_DIRECTORY/${manifest//\//_}"
  exit 0
fi
exit 23
"#,
    )
    .expect("failed to write fake Cargo");
    let mut permissions = fs::metadata(&fake)
        .expect("failed to read fake Cargo permissions")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&fake, permissions).expect("failed to make fake Cargo executable");
    fake
}

fn write_fake_metadata(directory: &Path, manifest: &Path, metadata: &[u8]) {
    fs::write(
        directory.join(manifest.display().to_string().replace('/', "_")),
        metadata,
    )
    .expect("failed to write canned Cargo metadata");
}

fn captured_invocations(path: &Path) -> Vec<Vec<String>> {
    fs::read(path)
        .expect("fake Cargo did not capture an invocation")
        .split(|byte| *byte == 0x1e)
        .filter(|record| !record.is_empty())
        .map(|record| {
            record
                .split(|byte| *byte == 0)
                .filter(|field| !field.is_empty())
                .map(|field| {
                    String::from_utf8(field.to_vec()).expect("captured argv must be UTF-8")
                })
                .collect()
        })
        .collect()
}

fn expected_metadata_invocation(root: &Path, manifest: &Path) -> Vec<String> {
    [
        root.display().to_string(),
        "metadata".to_string(),
        "--manifest-path".to_string(),
        manifest.display().to_string(),
        "--locked".to_string(),
        "--offline".to_string(),
        "--no-deps".to_string(),
        "--format-version".to_string(),
        "1".to_string(),
    ]
    .into()
}

fn append_packages(arguments: &mut Vec<String>, packages: &[String]) {
    for package in packages {
        arguments.extend(["-p".to_string(), package.clone()]);
    }
}

#[test]
fn selector_tools_execute_exact_fake_cargo_argv() {
    let root = repository_root();
    let contract = read_contract(&root);
    let root_output = cargo_metadata_output(&root, ROOT_MANIFEST);
    let root_metadata = parse_metadata(&root_output, ROOT_MANIFEST);
    let scratch = scratch_directory("argv");
    let fake_cargo = write_fake_cargo(&scratch);
    let capture = scratch.join("captured argv");
    let metadata_directory = scratch.join("canned metadata");
    fs::create_dir(&metadata_directory).expect("failed to create canned metadata directory");
    write_fake_metadata(
        &metadata_directory,
        &root.join(ROOT_MANIFEST),
        &root_output.stdout,
    );
    let mut component_members = Vec::new();
    for manifest in COMPONENT_MANIFESTS {
        let output = cargo_metadata_output(&root, manifest);
        let metadata = parse_metadata(&output, manifest);
        component_members.push(set(&names_for_ids(&metadata, &metadata.workspace_members)));
        write_fake_metadata(&metadata_directory, &root.join(manifest), &output.stdout);
    }

    for profile in ["m1", "m2", "deploy", "full", "ops"] {
        let _ = fs::remove_file(&capture);
        let output = Command::new(root.join(PROFILE_TOOL))
            .current_dir(&scratch)
            .env("CARGO", &fake_cargo)
            .env("WAMN_FAKE_CARGO_LOG", &capture)
            .env("WAMN_FAKE_METADATA_DIRECTORY", &metadata_directory)
            .arg(profile)
            .output()
            .unwrap_or_else(|error| panic!("failed to execute profile {profile}: {error}"));
        assert_eq!(output.status.code(), Some(23), "profile {profile}");

        let packages = root_profile_packages(&contract, &root_metadata, profile);
        let root_manifest = root.join(ROOT_MANIFEST);
        let mut expected_run = vec![
            root.display().to_string(),
            "test".to_string(),
            "--locked".to_string(),
            "--offline".to_string(),
            "--no-fail-fast".to_string(),
            "--manifest-path".to_string(),
            root_manifest.display().to_string(),
        ];
        append_packages(&mut expected_run, &packages);
        if profile == "ops" {
            expected_run.extend(["--features".to_string(), "wamn-ctl/ops".to_string()]);
        }
        assert_eq!(
            captured_invocations(&capture),
            vec![
                expected_metadata_invocation(&root, &root_manifest),
                expected_run
            ],
            "profile {profile} Cargo argv drifted"
        );
    }

    for profile in ["m1", "proof"] {
        let _ = fs::remove_file(&capture);
        let output = Command::new(root.join(COMPONENT_TOOL))
            .current_dir(&scratch)
            .env("CARGO", &fake_cargo)
            .env("WAMN_FAKE_CARGO_LOG", &capture)
            .env("WAMN_FAKE_METADATA_DIRECTORY", &metadata_directory)
            .arg(profile)
            .output()
            .unwrap_or_else(|error| {
                panic!("failed to execute component profile {profile}: {error}")
            });
        assert_eq!(output.status.code(), Some(23), "component {profile}");

        // One metadata read per component workspace, then one build leg per
        // workspace that owns a selected package. Every leg runs: the second
        // one is not described by the first one's failure.
        let selected = component_profile_packages(&contract, profile);
        let mut expected = COMPONENT_MANIFESTS
            .iter()
            .map(|manifest| expected_metadata_invocation(&root, &root.join(manifest)))
            .collect::<Vec<_>>();
        for (manifest, members) in COMPONENT_MANIFESTS.iter().zip(&component_members) {
            let owned = selected
                .iter()
                .filter(|package| members.contains(*package))
                .cloned()
                .collect::<Vec<_>>();
            if owned.is_empty() {
                continue;
            }
            let component_manifest = root.join(manifest);
            let mut expected_run = vec![
                root.display().to_string(),
                "build".to_string(),
                "--locked".to_string(),
                "--offline".to_string(),
                "--target".to_string(),
                "wasm32-wasip2".to_string(),
                "--manifest-path".to_string(),
                component_manifest.display().to_string(),
            ];
            append_packages(&mut expected_run, &owned);
            expected.push(expected_run);
        }
        assert_eq!(
            captured_invocations(&capture),
            expected,
            "component profile {profile} Cargo argv drifted"
        );
    }

    fs::remove_dir_all(&scratch).expect("failed to remove selector scratch directory");
}

#[test]
fn unknown_selector_modes_refuse_before_cargo() {
    let root = repository_root();
    let scratch = scratch_directory("refusal");
    let fake_cargo = write_fake_cargo(&scratch);

    for (tool, arguments, expected_message) in [
        (PROFILE_TOOL, vec!["unknown"], "unknown profile"),
        (COMPONENT_TOOL, vec!["unknown"], "unknown component profile"),
        (PROFILE_TOOL, Vec::new(), "exactly one profile"),
        (
            COMPONENT_TOOL,
            vec!["m1", "extra"],
            "exactly one component profile",
        ),
    ] {
        let capture = scratch.join(format!("{} capture", tool.replace('/', "-")));
        let _ = fs::remove_file(&capture);
        let output = Command::new(root.join(tool))
            .current_dir(&scratch)
            .env("CARGO", &fake_cargo)
            .env("WAMN_FAKE_CARGO_LOG", &capture)
            .args(arguments)
            .output()
            .unwrap_or_else(|error| panic!("failed to execute refusing selector: {error}"));
        assert_eq!(output.status.code(), Some(64));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected_message),
            "selector refusal omitted {expected_message}:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !capture.exists(),
            "refused selector invoked Cargo before validating its mode"
        );
    }

    fs::remove_dir_all(&scratch).expect("failed to remove refusal scratch directory");
}

#[test]
fn selector_tools_do_not_duplicate_canonical_package_inventory() {
    let root = repository_root();
    let root_output = cargo_metadata_output(&root, ROOT_MANIFEST);
    let root_metadata = parse_metadata(&root_output, ROOT_MANIFEST);
    let component_metadata = COMPONENT_MANIFESTS
        .iter()
        .map(|manifest| parse_metadata(&cargo_metadata_output(&root, manifest), manifest))
        .collect::<Vec<_>>();

    for tool in [PROFILE_TOOL, COMPONENT_TOOL] {
        let source = fs::read_to_string(root.join(tool))
            .unwrap_or_else(|error| panic!("failed to read {tool}: {error}"));
        for package in root_metadata
            .packages
            .iter()
            .chain(component_metadata.iter().flat_map(|one| &one.packages))
        {
            assert!(
                !source.contains(&package.name),
                "{tool} hardcodes {} instead of reading the canonical inventory",
                package.name
            );
        }
    }
}
