use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const ROOT_MANIFEST: &str = "Cargo.toml";
/// The guests live in more than one Cargo workspace. Feature unification is
/// additive-only inside one invocation, so the `no_std` palette guests are
/// isolated from the members that reach `serde_json/std` (wamn-0h0g.11.56).
const COMPONENT_MANIFESTS: [&str; 2] = ["components/Cargo.toml", "components/no-std/Cargo.toml"];
const COMPONENT_TOOL: &str = "tools/build-components";
const COMPONENT_VIRTUALIZATION: &str = "tools/component-virtualization.json";

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
    workspace_members: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    id: String,
    name: String,
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

fn package_components(root: &Path) -> Vec<String> {
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(root.join("packages")).expect("read application packages") {
        let path = entry
            .expect("read application entry")
            .path()
            .join("wamn.json");
        if path.is_file() {
            let manifest: Value =
                serde_json::from_slice(&fs::read(path).expect("read application manifest"))
                    .expect("parse application manifest");
            if let Some(components) = manifest.get("components").and_then(Value::as_object) {
                names.extend(components.keys().map(|name| name.replace('_', "-")));
            }
        }
    }
    names.into_iter().collect()
}

fn set(values: &[String]) -> BTreeSet<String> {
    values.iter().cloned().collect()
}

#[test]
fn virtualization_allowlist_matches_component_metadata() {
    let root = repository_root();
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
if [[ "${1:-}" == build && -n "${WAMN_FAKE_FAIL_PACKAGE:-}" ]]; then
  while (($# > 0)); do
    if [[ "$1" == -p && "${2:-}" == "$WAMN_FAKE_FAIL_PACKAGE" ]]; then
      exit 23
    fi
    shift
  done
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

#[test]
fn selector_tools_execute_exact_fake_cargo_argv() {
    let root = repository_root();
    let scratch = scratch_directory("argv");
    let fake_cargo = write_fake_cargo(&scratch);
    let capture = scratch.join("captured argv");
    let metadata_directory = scratch.join("canned metadata");
    fs::create_dir(&metadata_directory).expect("failed to create canned metadata directory");
    let mut component_members = Vec::new();
    for manifest in COMPONENT_MANIFESTS {
        let output = cargo_metadata_output(&root, manifest);
        let metadata = parse_metadata(&output, manifest);
        component_members.push(set(&names_for_ids(&metadata, &metadata.workspace_members)));
        write_fake_metadata(&metadata_directory, &root.join(manifest), &output.stdout);
    }

    for profile in ["app", "proof"] {
        let app = root.join("packages/receiving");
        let application_arguments = if profile == "app" { vec![app] } else { vec![] };
        let selected = if profile == "app" {
            let manifest: Value = serde_json::from_slice(
                &fs::read(application_arguments[0].join("wamn.json"))
                    .expect("read Receiving manifest"),
            )
            .expect("parse Receiving manifest");
            manifest["components"]
                .as_object()
                .expect("Receiving components")
                .keys()
                .map(|name| name.replace('_', "-"))
                .collect::<Vec<_>>()
        } else {
            component_members
                .iter()
                .flat_map(|members| members.iter().cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        };
        let first_package = component_members
            .iter()
            .find_map(|members| selected.iter().find(|package| members.contains(*package)))
            .expect("component profile must select at least one package");
        let _ = fs::remove_file(&capture);
        let output = Command::new(root.join(COMPONENT_TOOL))
            .current_dir(&scratch)
            .env("CARGO", &fake_cargo)
            .env("WAMN_FAKE_CARGO_LOG", &capture)
            .env("WAMN_FAKE_METADATA_DIRECTORY", &metadata_directory)
            .env("WAMN_FAKE_BUILD_STATUS", "0")
            .env("WAMN_FAKE_FAIL_PACKAGE", first_package)
            .arg(profile)
            .args(&application_arguments)
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

        // Read each workspace once, then build each selected package separately.
        // The first build fails, but all later builds must run and succeed.
        // Preserve the failure status after those successful invocations.
        let expected_metadata = COMPONENT_MANIFESTS
            .iter()
            .map(|manifest| expected_metadata_invocation(&root, &root.join(manifest)))
            .collect::<Vec<_>>();
        let mut expected = expected_metadata.clone();
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
            let expected_run = vec![
                root.display().to_string(),
                "build".to_string(),
                "--locked".to_string(),
                "--offline".to_string(),
                // The served guest is a RELEASE artifact: 669,593 bytes against
                // 20,634,368 for the same component built debug. This pin is what
                // makes the profile a reviewed decision rather than a default.
                "--release".to_string(),
                "--target".to_string(),
                "wasm32-wasip2".to_string(),
                "--manifest-path".to_string(),
                component_manifest.display().to_string(),
            ];
            for package in owned {
                let mut invocation = expected_run.clone();
                invocation.extend(["-p".to_string(), package]);
                expected.push(invocation);
            }
        }
        assert_eq!(
            captured_invocations(&capture),
            expected,
            "component profile {profile} Cargo argv drifted"
        );

        let _ = fs::remove_file(&capture);
        let watch_roots = Command::new(root.join(COMPONENT_TOOL))
            .current_dir(&scratch)
            .env("CARGO", &fake_cargo)
            .env("WAMN_FAKE_CARGO_LOG", &capture)
            .env("WAMN_FAKE_METADATA_DIRECTORY", &metadata_directory)
            .args(["watch-roots", profile])
            .args(&application_arguments)
            .output()
            .expect("failed to execute component watch roots");
        assert!(
            watch_roots.status.success(),
            "component watch roots {profile}: {}",
            String::from_utf8_lossy(&watch_roots.stderr)
        );
        let roots: Value = serde_json::from_slice(&watch_roots.stdout)
            .expect("watch roots must be machine-readable JSON");
        let expected_roots = COMPONENT_MANIFESTS
            .iter()
            .zip(&component_members)
            .filter(|(_, members)| selected.iter().any(|name| members.contains(name)))
            .map(|(manifest, _)| {
                manifest
                    .strip_suffix("/Cargo.toml")
                    .expect("component manifest must name a workspace")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            roots,
            serde_json::json!({"profile": profile, "workspace_roots": expected_roots}),
            "separate package builds must not duplicate a watched workspace"
        );
        assert_eq!(
            captured_invocations(&capture),
            expected_metadata,
            "watch roots must only read each workspace's metadata once"
        );
    }

    fs::remove_dir_all(&scratch).expect("failed to remove selector scratch directory");
}

#[test]
fn component_build_requires_declared_app_crates_and_accepts_new_cargo_members() {
    let root = repository_root();
    let scratch = scratch_directory("component absence");
    let fake_cargo = write_fake_cargo(&scratch);
    let capture = scratch.join("captured argv");
    let metadata_directory = scratch.join("canned metadata");
    fs::create_dir(&metadata_directory).expect("failed to create canned metadata directory");
    for relative in [COMPONENT_TOOL, COMPONENT_VIRTUALIZATION] {
        let destination = scratch.join(relative);
        fs::create_dir_all(destination.parent().expect("fixture file has a parent"))
            .expect("failed to create fixture directory");
        fs::copy(root.join(relative), destination).expect("failed to copy component tool fixture");
    }
    for entry in fs::read_dir(root.join("packages")).expect("failed to list packages") {
        let package = entry.expect("package entry must be readable");
        let manifest = package.path().join("wamn.json");
        if manifest.is_file() {
            let destination = scratch.join("packages").join(package.file_name());
            fs::create_dir_all(&destination).expect("failed to create package fixture");
            fs::copy(manifest, destination.join("wamn.json"))
                .expect("failed to copy package declaration");
        }
    }
    let new_package = scratch.join("packages/fresh-package");
    fs::create_dir_all(&new_package).expect("failed to create new package fixture");
    let new_manifest = new_package.join("wamn.json");
    fs::write(&new_manifest, r#"{"components":{"fresh_component":{}}}"#)
        .expect("failed to declare new component");
    let mut metadata = Vec::new();
    for manifest in COMPONENT_MANIFESTS {
        let output = cargo_metadata_output(&root, manifest);
        parse_metadata(&output, manifest);
        write_fake_metadata(&metadata_directory, &scratch.join(manifest), &output.stdout);
        metadata.push(serde_json::from_slice::<Value>(&output.stdout).expect("valid metadata"));
    }
    let run = |arguments: &[&str]| {
        let _ = fs::remove_file(&capture);
        Command::new(scratch.join(COMPONENT_TOOL))
            .env("CARGO", &fake_cargo)
            .env("WAMN_FAKE_CARGO_LOG", &capture)
            .env("WAMN_FAKE_METADATA_DIRECTORY", &metadata_directory)
            .args(arguments)
            .output()
            .expect("failed to execute component tool fixture")
    };
    let assert_metadata_only = || {
        assert_eq!(
            captured_invocations(&capture),
            COMPONENT_MANIFESTS
                .iter()
                .map(|manifest| expected_metadata_invocation(&scratch, &scratch.join(manifest)))
                .collect::<Vec<_>>(),
            "refusal must occur after metadata and before any build"
        );
    };
    let new_app = new_package
        .to_str()
        .expect("application path must be UTF-8");
    for arguments in [
        vec!["app", new_app],
        vec!["proof"],
        vec!["build-only", "app", new_app],
        vec!["watch-roots", "app", new_app],
    ] {
        let absent = run(&arguments);
        assert_eq!(absent.status.code(), Some(65));
        assert_eq!(
            String::from_utf8_lossy(&absent.stderr),
            "build-components: package component crates are absent from the declared component workspaces: fresh-component. Create each missing crate. Add each crate to its Cargo workspace members.\n"
        );
        assert_metadata_only();
    }

    metadata[0]["packages"]
        .as_array_mut()
        .expect("metadata packages must be an array")
        .push(serde_json::json!({
            "id": "fresh-component-id",
            "name": "fresh-component",
            "targets": [{"name": "fresh_component", "crate_types": ["cdylib"]}]
        }));
    metadata[0]["workspace_members"]
        .as_array_mut()
        .expect("metadata workspace members must be an array")
        .push(serde_json::json!("fresh-component-id"));
    write_fake_metadata(
        &metadata_directory,
        &scratch.join(COMPONENT_MANIFESTS[0]),
        &serde_json::to_vec(&metadata[0]).expect("metadata must serialize"),
    );
    let present = run(&["app", new_app]);
    assert_eq!(
        present.status.code(),
        Some(23),
        "declared crate must reach the build: {}",
        String::from_utf8_lossy(&present.stderr)
    );
    assert!(captured_invocations(&capture).iter().any(|invocation| {
        invocation
            .get(1)
            .is_some_and(|argument| argument == "build")
            && invocation
                .windows(2)
                .any(|arguments| arguments == ["-p", "fresh-component"])
    }));

    fs::remove_file(new_manifest).expect("failed to remove package declaration");
    let proof = run(&["proof"]);
    assert_eq!(
        proof.status.code(),
        Some(23),
        "proof must build new Cargo members"
    );
    assert!(captured_invocations(&capture).iter().any(|invocation| {
        invocation
            .get(1)
            .is_some_and(|argument| argument == "build")
            && invocation
                .windows(2)
                .any(|arguments| arguments == ["-p", "fresh-component"])
    }));
    fs::remove_dir_all(&scratch).expect("failed to remove component absence fixture");
}

#[test]
fn component_build_normalizes_only_declared_artifacts_to_separate_outputs() {
    let root = repository_root();
    let virtualization: Value = serde_json::from_str(
        &fs::read_to_string(root.join(COMPONENT_VIRTUALIZATION))
            .expect("failed to read component virtualization contract"),
    )
    .expect("component virtualization contract must be JSON");
    let declared = virtualization["artifacts"]
        .as_array()
        .expect("virtualization artifacts must be an array");
    let output_subdirectory = virtualization["output_subdirectory"]
        .as_str()
        .expect("virtualization output subdirectory must be a string");

    // The contract declares the platform half. The package half is synthesized
    // from the package manifests, exactly as tools/build-components synthesizes
    // it, and the owning workspace is whichever one actually holds the crate.
    let mut artifacts = declared.clone();
    for component in package_components(&root) {
        let owner = COMPONENT_MANIFESTS
            .iter()
            .find(|manifest| {
                let metadata = parse_metadata(&cargo_metadata_output(&root, manifest), manifest);
                names_for_ids(&metadata, &metadata.workspace_members).contains(&component)
            })
            .unwrap_or_else(|| panic!("no component workspace holds {component}"));
        let file = format!("{}.wasm", component.replace('-', "_"));
        artifacts.push(serde_json::json!({
            "package": component,
            "workspace_manifest": owner,
            "raw_file": file,
            "output_file": file,
        }));
    }
    let artifacts = &artifacts;

    let scratch = scratch_directory("virtualization");
    let fake_cargo = write_fake_cargo(&scratch);
    let capture = scratch.join("captured argv");
    let metadata_directory = scratch.join("canned metadata");
    fs::create_dir(&metadata_directory).expect("failed to create canned metadata directory");

    let mut target_directories = BTreeMap::new();
    let mut component_members = BTreeMap::new();
    for manifest in COMPONENT_MANIFESTS {
        let output = cargo_metadata_output(&root, manifest);
        let metadata = parse_metadata(&output, manifest);
        component_members.insert(
            manifest,
            set(&names_for_ids(&metadata, &metadata.workspace_members)),
        );
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
            .join("release")
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
        .arg("proof")
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
        fs::write(normalized, "preserved-other-app").expect("seed outputs before app build");
    }
    let receiving = root.join("packages/receiving");
    let _ = fs::remove_file(&capture);
    let app_build = Command::new(root.join(COMPONENT_TOOL))
        .current_dir(&scratch)
        .env("CARGO", &fake_cargo)
        .env("WAMN_FAKE_CARGO_LOG", &capture)
        .env("WAMN_FAKE_METADATA_DIRECTORY", &metadata_directory)
        .env("WAMN_FAKE_BUILD_STATUS", "0")
        .args(["build-only", "app"])
        .arg(&receiving)
        .output()
        .expect("run Receiving build-only");
    assert!(
        app_build.status.success(),
        "{}",
        String::from_utf8_lossy(&app_build.stderr)
    );
    let app_plan: Value =
        serde_json::from_slice(&app_build.stdout).expect("parse app artifact plan");
    assert_eq!(app_plan["profile"], "app");
    assert_eq!(app_plan["applications"], serde_json::json!([receiving]));
    let app_outputs = app_plan["virtualization"]["artifacts"]
        .as_array()
        .expect("app plan artifacts")
        .iter()
        .map(|artifact| PathBuf::from(artifact["output"].as_str().expect("app artifact output")))
        .collect::<BTreeSet<_>>();
    assert!(!app_outputs.is_empty());
    assert!(
        app_outputs.len() < normalized_outputs.len(),
        "app must select fewer artifacts than proof"
    );
    assert!(
        normalized_outputs.iter().all(|path| {
            fs::read_to_string(path).expect("read output after app build") == "preserved-other-app"
        }),
        "app build-only must preserve all normalized outputs"
    );
    let app_plan_path = scratch.join("app-artifact-plan.json");
    fs::write(&app_plan_path, &app_build.stdout).expect("write app artifact plan");
    let app_virtualize = Command::new(root.join(COMPONENT_TOOL))
        .current_dir(&scratch)
        .env("CARGO", &fake_cargo)
        .env("WAMN_FAKE_CARGO_LOG", &capture)
        .env("WAMN_FAKE_METADATA_DIRECTORY", &metadata_directory)
        .env("WAMN_FAKE_BUILD_STATUS", "31")
        .env("WAMN_FAKE_VIRTUALIZER_STATUS", "0")
        .arg("virtualize-only")
        .arg(&app_plan_path)
        .output()
        .expect("run Receiving virtualization");
    assert!(
        app_virtualize.status.success(),
        "{}",
        String::from_utf8_lossy(&app_virtualize.stderr)
    );
    for (normalized, combined) in normalized_outputs.iter().zip(&combined_outputs) {
        let actual = fs::read(normalized).expect("read normalized output after app virtualization");
        if app_outputs.contains(normalized) {
            assert_eq!(&actual, combined);
        } else {
            assert_eq!(
                actual, b"preserved-other-app",
                "app virtualization changed another app output"
            );
        }
    }

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
        .args(["build-only", "proof"])
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
    assert_eq!(artifact_plan["profile"], "proof");
    assert_eq!(artifact_plan["applications"], serde_json::json!([]));
    let build_plan = artifact_plan["build"]
        .as_array()
        .expect("artifact plan must contain the Cargo selections");
    let selected = component_members
        .values()
        .flat_map(|members| members.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut expected_build_plan = Vec::new();
    for manifest in COMPONENT_MANIFESTS {
        for package in &selected {
            if component_members[manifest].contains(package) {
                expected_build_plan
                    .push(serde_json::json!({"manifest": manifest, "packages": [package]}));
            }
        }
    }
    assert_eq!(build_plan.len(), selected.len());
    assert_eq!(
        build_plan, &expected_build_plan,
        "the artifact plan must build each selected package once, in deterministic order"
    );
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
        .join("release")
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
        .arg("proof")
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
        (
            COMPONENT_TOOL,
            vec!["unknown"],
            "expected app APP_DIRECTORY... or proof",
        ),
        (
            COMPONENT_TOOL,
            vec!["m1"],
            "expected app APP_DIRECTORY... or proof",
        ),
        (
            COMPONENT_TOOL,
            vec!["app"],
            "app requires an application directory",
        ),
        (
            COMPONENT_TOOL,
            vec!["proof", "extra"],
            "proof takes no application directories",
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
