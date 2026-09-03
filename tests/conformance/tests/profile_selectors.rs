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
const COMPONENT_VIRTUALIZATION: &str = "tools/component-virtualization.json";

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
    targets: Vec<CargoTarget>,
}

#[derive(Debug, Deserialize)]
struct CargoTarget {
    name: String,
    crate_types: Vec<String>,
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
fn virtualization_allowlist_matches_component_metadata() {
    let root = repository_root();
    let contract = read_contract(&root);
    let virtualization: Value = serde_json::from_str(
        &fs::read_to_string(root.join(COMPONENT_VIRTUALIZATION))
            .expect("failed to read component virtualization contract"),
    )
    .expect("component virtualization contract must be JSON");
    let profile = virtualization["profile"]
        .as_str()
        .expect("virtualization profile must be a string");
    let expected_output_subdirectory = format!("virtualized/{profile}");
    assert_eq!(
        virtualization["output_subdirectory"].as_str(),
        Some(expected_output_subdirectory.as_str())
    );

    let root_metadata = parse_metadata(&cargo_metadata_output(&root, ROOT_MANIFEST), ROOT_MANIFEST);
    let tool_package = virtualization["tool"]["package"]
        .as_str()
        .expect("virtualizer package must be a string");
    assert_eq!(
        virtualization["tool"]["manifest"].as_str(),
        Some(ROOT_MANIFEST)
    );
    assert!(
        names_for_ids(&root_metadata, &root_metadata.workspace_members)
            .iter()
            .any(|package| package == tool_package),
        "virtualizer tool package must be a root workspace member"
    );

    let component_metadata = COMPONENT_MANIFESTS
        .iter()
        .map(|manifest| {
            (
                *manifest,
                parse_metadata(&cargo_metadata_output(&root, manifest), manifest),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let product_components =
        string_array(&contract, "/tiers/product_components/component_packages")
            .into_iter()
            .collect::<BTreeSet<_>>();

    let artifacts = virtualization["artifacts"]
        .as_array()
        .expect("virtualization artifacts must be an array");
    let mut configured = BTreeSet::new();
    let mut outputs = BTreeSet::new();
    for artifact in artifacts {
        let package_name = artifact["package"]
            .as_str()
            .expect("virtualization package must be a string");
        let workspace_manifest = artifact["workspace_manifest"]
            .as_str()
            .expect("virtualization workspace manifest must be a string");
        let raw_file = artifact["raw_file"]
            .as_str()
            .expect("virtualization raw file must be a string");
        let output_file = artifact["output_file"]
            .as_str()
            .expect("virtualization output file must be a string");

        assert!(configured.insert(package_name.to_owned()));
        assert!(outputs.insert(output_file.to_owned()));
        assert!(product_components.contains(package_name));
        assert_eq!(
            Path::new(raw_file)
                .file_name()
                .and_then(|name| name.to_str()),
            Some(raw_file)
        );
        assert_eq!(
            Path::new(output_file)
                .file_name()
                .and_then(|name| name.to_str()),
            Some(output_file)
        );

        let metadata = component_metadata
            .get(workspace_manifest)
            .unwrap_or_else(|| panic!("unknown component workspace {workspace_manifest}"));
        let package = metadata
            .packages
            .iter()
            .find(|package| package.name == package_name)
            .unwrap_or_else(|| panic!("{package_name} is absent from {workspace_manifest}"));
        assert!(metadata.workspace_members.contains(&package.id));
        let cdylib_targets = package
            .targets
            .iter()
            .filter(|target| target.crate_types.iter().any(|kind| kind == "cdylib"))
            .collect::<Vec<_>>();
        assert_eq!(cdylib_targets.len(), 1);
        assert_eq!(raw_file, format!("{}.wasm", cdylib_targets[0].name));
    }
    assert!(!configured.is_empty());
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

    assert_eq!(root_members.len(), 36);
    assert_eq!(component_members.len(), 18);
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
            "wamn-component-virtualizer",
            "wamn-ctl",
            "wamn-control-provision",
            "wamn-control-registry",
            "wamn-project-state",
            "wamn-schema-control",
            "wamn-schema-generator",
            "wamn-schema-introspection",
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
        ("m1", 19),
        ("m2", 21),
        ("deploy", 29),
        ("full", 36),
        ("ops", 36),
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
        &[
            "blob-put",
            "client-acme-receiving",
            "http-route",
            "http-request",
            "label-render",
            "materializer",
            "receiving",
            "transform",
        ],
    );
    assert_exact_set(
        "component proof",
        &component_proof,
        &[
            "blob-put",
            "busyloop",
            "client-acme-receiving",
            "connection-http-standard",
            "http-route",
            "http-request",
            "label-render",
            "label-template",
            "materializer",
            "receiving",
            "sockprobe",
            "sqlx-command",
            "std-virtualization-probe",
            "transform",
            "wamn-client-acme-receiving-data-access",
            "wamn-postgres-statements",
            "wamn-postgres-sqlx",
            "wamn-receiving-data-access",
        ],
    );
    assert_eq!(set(&component_proof), set(&component_members));
    assert_eq!(component_m1.len(), 8);
    assert_eq!(component_proof.len(), 18);
    assert_unique("component m1", &component_m1);
    assert_unique("component proof", &component_proof);
    assert_eq!(
        set(&component_proof)
            .difference(&set(&component_m1))
            .cloned()
            .collect::<BTreeSet<_>>(),
        [
            "busyloop",
            "connection-http-standard",
            "label-template",
            "sockprobe",
            "sqlx-command",
            "std-virtualization-probe",
            "wamn-client-acme-receiving-data-access",
            "wamn-postgres-statements",
            "wamn-postgres-sqlx",
            "wamn-receiving-data-access",
        ]
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
if [[ "${1:-}" == run ]]; then
  status="${WAMN_FAKE_VIRTUALIZER_STATUS:-23}"
  if [[ "$status" == 0 ]]; then
    input=''
    output=''
    while (($# > 0)); do
      case "$1" in
        --input) input="$2"; shift 2 ;;
        --output) output="$2"; shift 2 ;;
        *) shift ;;
      esac
    done
    command cp -- "$input" "$output"
  fi
  exit "$status"
fi
exit "${WAMN_FAKE_BUILD_STATUS:-23}"
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

fn metadata_with_target_directory(metadata: &[u8], target_directory: &Path) -> Vec<u8> {
    let mut value: Value = serde_json::from_slice(metadata).expect("Cargo metadata must be JSON");
    value["target_directory"] = Value::String(target_directory.display().to_string());
    serde_json::to_vec(&value).expect("rewritten Cargo metadata must serialize")
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
        assert_eq!(
            output.status.code(),
            Some(23),
            "profile {profile}: {}",
            String::from_utf8_lossy(&output.stderr)
        );

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
        assert_eq!(
            output.status.code(),
            Some(23),
            "component {profile}: {}",
            String::from_utf8_lossy(&output.stderr)
        );

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
fn component_build_normalizes_only_declared_artifacts_to_separate_outputs() {
    let root = repository_root();
    let virtualization: Value = serde_json::from_str(
        &fs::read_to_string(root.join(COMPONENT_VIRTUALIZATION))
            .expect("failed to read component virtualization contract"),
    )
    .expect("component virtualization contract must be JSON");
    let artifacts = virtualization["artifacts"]
        .as_array()
        .expect("virtualization artifacts must be an array");
    let output_subdirectory = virtualization["output_subdirectory"]
        .as_str()
        .expect("virtualization output subdirectory must be a string");

    let scratch = scratch_directory("virtualization");
    let fake_cargo = write_fake_cargo(&scratch);
    let capture = scratch.join("captured argv");
    let metadata_directory = scratch.join("canned metadata");
    fs::create_dir(&metadata_directory).expect("failed to create canned metadata directory");

    let mut target_directories = BTreeMap::new();
    for manifest in COMPONENT_MANIFESTS {
        let output = cargo_metadata_output(&root, manifest);
        let target_directory = scratch.join(format!("{} target", manifest.replace('/', "-")));
        fs::create_dir(&target_directory).expect("failed to create fake target directory");
        let rewritten = metadata_with_target_directory(&output.stdout, &target_directory);
        write_fake_metadata(&metadata_directory, &root.join(manifest), &rewritten);
        target_directories.insert(manifest.to_owned(), target_directory);
    }

    let mut expected_inputs = BTreeSet::new();
    for artifact in artifacts {
        let package = artifact["package"]
            .as_str()
            .expect("virtualization package must be a string");
        let workspace_manifest = artifact["workspace_manifest"]
            .as_str()
            .expect("workspace manifest must be a string");
        let raw_file = artifact["raw_file"]
            .as_str()
            .expect("raw file must be a string");
        let target_directory = target_directories
            .get(workspace_manifest)
            .expect("configured workspace must have fake metadata");
        let input = target_directory
            .join("wasm32-wasip2")
            .join("debug")
            .join(raw_file);
        fs::create_dir_all(input.parent().expect("raw component must have a parent"))
            .expect("failed to create raw component directory");
        fs::write(&input, format!("raw:{package}")).expect("failed to write fake raw component");
        expected_inputs.insert(input);
    }

    let owned_output_directories = artifacts
        .iter()
        .map(|artifact| {
            let workspace_manifest = artifact["workspace_manifest"]
                .as_str()
                .expect("workspace manifest must be a string");
            target_directories[workspace_manifest].join(output_subdirectory)
        })
        .collect::<BTreeSet<_>>();
    let stale_outputs = owned_output_directories
        .iter()
        .map(|directory| directory.join("undeclared-stale.wasm"))
        .collect::<Vec<_>>();
    for stale in &stale_outputs {
        fs::create_dir_all(stale.parent().expect("stale output must have a parent"))
            .expect("failed to create owned virtualization output directory");
        fs::write(stale, "stale").expect("failed to seed undeclared stale output");
    }

    let output = Command::new(root.join(COMPONENT_TOOL))
        .current_dir(&scratch)
        .env("CARGO", &fake_cargo)
        .env("WAMN_FAKE_CARGO_LOG", &capture)
        .env("WAMN_FAKE_METADATA_DIRECTORY", &metadata_directory)
        .env("WAMN_FAKE_BUILD_STATUS", "0")
        .env("WAMN_FAKE_VIRTUALIZER_STATUS", "0")
        .arg("m1")
        .output()
        .expect("failed to execute component virtualization profile");
    assert!(
        output.status.success(),
        "component virtualization profile failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let invocations = captured_invocations(&capture);
    let virtualizer_runs = invocations
        .iter()
        .filter(|invocation| invocation.get(1).is_some_and(|argument| argument == "run"))
        .collect::<Vec<_>>();
    assert_eq!(virtualizer_runs.len(), artifacts.len());
    let mut actual_inputs = BTreeSet::new();
    for invocation in virtualizer_runs {
        let package_position = invocation
            .iter()
            .position(|argument| argument == "-p")
            .expect("virtualizer invocation must select a package");
        assert_eq!(
            invocation.get(package_position + 1).map(String::as_str),
            virtualization["tool"]["package"].as_str()
        );
        let input_position = invocation
            .iter()
            .position(|argument| argument == "--input")
            .expect("virtualizer invocation must name its raw input");
        let output_position = invocation
            .iter()
            .position(|argument| argument == "--output")
            .expect("virtualizer invocation must name its separate output");
        let input = PathBuf::from(
            invocation
                .get(input_position + 1)
                .expect("--input must have a value"),
        );
        let partial_output = PathBuf::from(
            invocation
                .get(output_position + 1)
                .expect("--output must have a value"),
        );
        assert_ne!(input, partial_output);
        assert!(
            partial_output
                .to_string_lossy()
                .contains(&format!("/{output_subdirectory}/"))
        );
        actual_inputs.insert(input);
    }
    assert_eq!(actual_inputs, expected_inputs);
    assert!(
        stale_outputs.iter().all(|path| !path.exists()),
        "successful virtualization retained an undeclared stale output"
    );

    let normalized_outputs = artifacts
        .iter()
        .map(|artifact| {
            let workspace_manifest = artifact["workspace_manifest"]
                .as_str()
                .expect("workspace manifest must be a string");
            let output_file = artifact["output_file"]
                .as_str()
                .expect("output file must be a string");
            target_directories[workspace_manifest]
                .join(output_subdirectory)
                .join(output_file)
        })
        .collect::<Vec<_>>();
    for (artifact, normalized) in artifacts.iter().zip(&normalized_outputs) {
        let package = artifact["package"]
            .as_str()
            .expect("package must be a string");
        assert_eq!(
            fs::read_to_string(normalized).expect("normalized component must exist"),
            format!("raw:{package}")
        );
    }

    let combined_outputs = normalized_outputs
        .iter()
        .map(|path| fs::read(path).expect("combined output must be readable"))
        .collect::<Vec<_>>();
    for normalized in &normalized_outputs {
        fs::write(normalized, "preserved-build-only").expect("failed to seed a build-only output");
    }
    for stale in &stale_outputs {
        fs::write(stale, "stale").expect("failed to reseed undeclared stale output");
    }

    let _ = fs::remove_file(&capture);
    let build_only = Command::new(root.join(COMPONENT_TOOL))
        .current_dir(&scratch)
        .env("CARGO", &fake_cargo)
        .env("WAMN_FAKE_CARGO_LOG", &capture)
        .env("WAMN_FAKE_METADATA_DIRECTORY", &metadata_directory)
        .env("WAMN_FAKE_BUILD_STATUS", "0")
        .env("WAMN_FAKE_VIRTUALIZER_STATUS", "29")
        .args(["build-only", "m1"])
        .output()
        .expect("failed to execute build-only component profile");
    assert!(
        build_only.status.success(),
        "build-only component profile failed:\n{}",
        String::from_utf8_lossy(&build_only.stderr)
    );
    let build_only_invocations = captured_invocations(&capture);
    assert!(
        build_only_invocations.iter().any(|invocation| invocation
            .get(1)
            .is_some_and(|argument| argument == "build")),
        "build-only mode did not build any workspace"
    );
    assert!(
        build_only_invocations
            .iter()
            .all(|invocation| invocation.get(1).is_none_or(|argument| argument != "run")),
        "build-only mode invoked the virtualizer"
    );
    assert!(
        normalized_outputs.iter().all(|path| {
            fs::read_to_string(path).expect("preserved output must remain readable")
                == "preserved-build-only"
        }),
        "build-only mode mutated a normalized output"
    );
    assert!(
        stale_outputs.iter().all(|path| path.exists()),
        "build-only mode cleaned the virtualization output directory"
    );

    let artifact_plan: Value = serde_json::from_slice(&build_only.stdout)
        .expect("build-only stdout must be one machine-readable artifact plan");
    assert_eq!(artifact_plan["profile"], "m1");
    assert_eq!(
        artifact_plan["virtualization"]["artifacts"]
            .as_array()
            .expect("artifact plan must contain virtualization artifacts")
            .len(),
        artifacts.len()
    );
    let artifact_plan_path = scratch.join("component-artifact-plan.json");
    fs::write(&artifact_plan_path, &build_only.stdout)
        .expect("failed to persist build-only artifact plan");

    let _ = fs::remove_file(&capture);
    let virtualize_only = Command::new(root.join(COMPONENT_TOOL))
        .current_dir(&scratch)
        .env("CARGO", &fake_cargo)
        .env("WAMN_FAKE_CARGO_LOG", &capture)
        .env("WAMN_FAKE_METADATA_DIRECTORY", &metadata_directory)
        .env("WAMN_FAKE_BUILD_STATUS", "31")
        .env("WAMN_FAKE_VIRTUALIZER_STATUS", "0")
        .arg("virtualize-only")
        .arg(&artifact_plan_path)
        .output()
        .expect("failed to execute virtualize-only component profile");
    assert!(
        virtualize_only.status.success(),
        "virtualize-only component profile failed:\n{}",
        String::from_utf8_lossy(&virtualize_only.stderr)
    );
    let virtualize_only_invocations = captured_invocations(&capture);
    assert!(
        virtualize_only_invocations
            .iter()
            .all(|invocation| invocation.get(1).is_none_or(|argument| argument != "build")),
        "virtualize-only mode rebuilt a workspace"
    );
    assert_eq!(
        virtualize_only_invocations
            .iter()
            .filter(|invocation| invocation.get(1).is_some_and(|argument| argument == "run"))
            .count(),
        artifacts.len()
    );
    assert_eq!(
        normalized_outputs
            .iter()
            .map(|path| fs::read(path).expect("split output must be readable"))
            .collect::<Vec<_>>(),
        combined_outputs,
        "split build and virtualization changed the combined output"
    );
    assert!(
        stale_outputs.iter().all(|path| !path.exists()),
        "virtualize-only mode retained an undeclared stale output"
    );

    fs::write(&normalized_outputs[0], "preserved-invalid")
        .expect("failed to seed output before invalid-plan refusal");
    let mut invalid_plan = artifact_plan.clone();
    invalid_plan["unexpected"] = Value::Bool(true);
    let invalid_plan_path = scratch.join("invalid-component-artifact-plan.json");
    fs::write(
        &invalid_plan_path,
        serde_json::to_vec(&invalid_plan).expect("invalid plan fixture must serialize"),
    )
    .expect("failed to write invalid artifact plan");
    let _ = fs::remove_file(&capture);
    let invalid = Command::new(root.join(COMPONENT_TOOL))
        .current_dir(&scratch)
        .env("CARGO", &fake_cargo)
        .env("WAMN_FAKE_CARGO_LOG", &capture)
        .env("WAMN_FAKE_METADATA_DIRECTORY", &metadata_directory)
        .arg("virtualize-only")
        .arg(&invalid_plan_path)
        .output()
        .expect("failed to execute invalid-plan refusal");
    assert_eq!(invalid.status.code(), Some(65));
    assert!(
        !capture.exists(),
        "invalid artifact plan invoked Cargo before refusing"
    );
    assert_eq!(
        fs::read_to_string(&normalized_outputs[0])
            .expect("invalid-plan output must remain readable"),
        "preserved-invalid"
    );

    let first_artifact = &artifacts[0];
    let first_workspace = first_artifact["workspace_manifest"]
        .as_str()
        .expect("workspace manifest must be a string");
    let first_raw_file = first_artifact["raw_file"]
        .as_str()
        .expect("raw file must be a string");
    let first_raw = target_directories[first_workspace]
        .join("wasm32-wasip2")
        .join("debug")
        .join(first_raw_file);
    fs::write(&first_raw, "changed-after-build")
        .expect("failed to mutate raw component after build-only");
    fs::write(&normalized_outputs[0], "preserved-stale")
        .expect("failed to seed output before stale-plan refusal");
    let _ = fs::remove_file(&capture);
    let stale = Command::new(root.join(COMPONENT_TOOL))
        .current_dir(&scratch)
        .env("CARGO", &fake_cargo)
        .env("WAMN_FAKE_CARGO_LOG", &capture)
        .env("WAMN_FAKE_METADATA_DIRECTORY", &metadata_directory)
        .arg("virtualize-only")
        .arg(&artifact_plan_path)
        .output()
        .expect("failed to execute stale-plan refusal");
    assert_eq!(stale.status.code(), Some(65));
    assert!(
        String::from_utf8_lossy(&stale.stderr).contains("artifact plan is stale"),
        "stale-plan refusal omitted its reason:\n{}",
        String::from_utf8_lossy(&stale.stderr)
    );
    assert!(
        captured_invocations(&capture)
            .iter()
            .all(|invocation| invocation
                .get(1)
                .is_some_and(|argument| argument == "metadata")),
        "stale artifact plan built or virtualized before refusing"
    );
    assert_eq!(
        fs::read_to_string(&normalized_outputs[0]).expect("stale-plan output must remain readable"),
        "preserved-stale"
    );
    fs::write(
        &first_raw,
        format!(
            "raw:{}",
            first_artifact["package"]
                .as_str()
                .expect("package must be a string")
        ),
    )
    .expect("failed to restore raw component after stale-plan refusal");

    let first = &artifacts[0];
    let first_workspace = first["workspace_manifest"]
        .as_str()
        .expect("workspace manifest must be a string");
    let first_output = target_directories[first_workspace]
        .join(output_subdirectory)
        .join(
            first["output_file"]
                .as_str()
                .expect("output file must be a string"),
        );
    fs::write(&first_output, "previous-normalized")
        .expect("failed to seed the previous normalized component");
    let failed = Command::new(root.join(COMPONENT_TOOL))
        .current_dir(&scratch)
        .env("CARGO", &fake_cargo)
        .env("WAMN_FAKE_CARGO_LOG", &capture)
        .env("WAMN_FAKE_METADATA_DIRECTORY", &metadata_directory)
        .env("WAMN_FAKE_BUILD_STATUS", "0")
        .env("WAMN_FAKE_VIRTUALIZER_STATUS", "29")
        .arg("m1")
        .output()
        .expect("failed to execute refusing component virtualization profile");
    assert_eq!(failed.status.code(), Some(29));
    assert!(
        !first_output.exists(),
        "failed virtualization retained a stale normalized artifact"
    );
    let first_file = first_output
        .file_name()
        .expect("normalized component must have a file name")
        .to_string_lossy();
    let partial_prefix = format!("{first_file}.partial.");
    assert!(
        fs::read_dir(
            first_output
                .parent()
                .expect("normalized component must have a parent")
        )
        .expect("failed to list normalized component directory")
        .all(|entry| {
            !entry
                .expect("normalized component directory entry must be readable")
                .file_name()
                .to_string_lossy()
                .starts_with(&partial_prefix)
        }),
        "failed virtualization must remove its partial output"
    );

    fs::remove_dir_all(&scratch).expect("failed to remove virtualization scratch directory");
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

/// wamn-0h0g.15.137.4: the needle set below is DERIVED from cargo metadata, so
/// it inherits every ordinary-English package name in the workspace -- today
/// `transform` and `http-request`. Nothing collides yet, but a comment in a
/// selector tool reading "transform the manifest list" makes this guard report
/// a hardcoded package name that is not there. Measured: it does, naming
/// `tools/build-components` and `transform`.
///
/// THE COST IS DELIBERATE, the way a members-line change already is. Both
/// alternatives were measured and are worse:
///
///   * comment-stripping the haystack cannot be done safely here -- both
///     selector tools contain `$#`, so a line-based stripper deletes the
///     executable line it sits on and blinds the guard silently;
///   * a quote- or word-shaped needle wrapper misses the shapes that matter: a
///     bare `m1 | proof | materializer)` case arm is a real hardcode this bare
///     `contains` kills and a quoted shape would not;
///   * an exclusion list of ordinary-English names is a HAND-WRITTEN needle set,
///     which is precisely what deriving the set exists to avoid, and it would
///     blind the guard on two of the eight component packages.
///
/// Nothing else kills the mutant this exists for. A tool that hardcodes a
/// package list EQUAL to the canonical one passes every behavioural proof in
/// this file, `selector_tools_execute_exact_fake_cargo_argv` included, and
/// fails only here. So naming a package after an ordinary English word costs
/// whoever adds it a rename or a fix to this guard. That is the price of the
/// derivation, and it is the cheaper half of the trade.
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
