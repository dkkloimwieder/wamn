use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const BUILD_AND_TEST_DOC: &str = "docs/build-and-test.md";
const BUILD_RECIPE_HELPER: &str = "tools/build-recipe-test-check";
const ROOT_MANIFEST: &str = "Cargo.toml";
const COMPONENT_MANIFEST: &str = "components/Cargo.toml";
const ROUTER_ONLY_TEST_PACKAGES: &[&str] = &["wamn-gates", "wamn-host"];
const REQUIRED_RECIPE_IDS: &[&str] = &[
    "H5-API-FIXTURE",
    "H5-API-PUBLISH",
    "H5-BUILDPROOF",
    "H5-CATALOG-IDENTITY",
    "H5-CAUSATION",
    "H5-CDCBENCH",
    "H5-COMPONENT-POLICY",
    "H5-CREDENTIALS",
    "H5-CREDPROOF",
    "H5-DASHBOARDS",
    "H5-EGRESSBENCH",
    "H5-F1-FIXTURE",
    "H5-JETSTREAM-UNIT",
    "H5-JETSTREAM-WIT-MATERIALIZER",
    "H5-JETSTREAM-WIT-SAMPLE",
    "H5-METRIC-PROOF",
    "H5-METRIC-RUNTIME",
    "H5-POCSUITEPROOF",
    "H5-R18-NEG",
    "H5-RIE2EBENCH",
    "H5-S2-GUARDS",
    "H5-SOCKETGUARD",
    "H5-TESTGATE",
    "H5-TESTKITBENCH",
    "H5-TRACEPROOF",
    "H5-WAKEPROOF",
    "H5-WALBENCH",
];

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    name: String,
    targets: Vec<CargoTarget>,
}

#[derive(Debug, Deserialize)]
struct CargoTarget {
    name: String,
    kind: Vec<String>,
}

#[derive(Debug)]
struct BashLine {
    block: usize,
    number: usize,
    text: String,
}

#[derive(Debug)]
struct LogicalCommand {
    block: usize,
    number: usize,
    text: String,
}

#[derive(Debug)]
struct RecipeDirective {
    block: usize,
    number: usize,
    id: String,
    proof: String,
    package: String,
    kind: String,
    target: String,
    filter: String,
    minimum: usize,
    boundary: String,
}

#[derive(Debug)]
struct PackageTargets {
    workspace: &'static str,
    targets: BTreeMap<String, BTreeSet<String>>,
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

fn cargo_metadata(root: &Path, manifest: &str) -> CargoMetadata {
    let output = Command::new(env!("CARGO"))
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
        .unwrap_or_else(|error| panic!("failed to run cargo metadata for {manifest}: {error}"));
    assert!(
        output.status.success(),
        "cargo metadata failed for {manifest}:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("invalid cargo metadata for {manifest}: {error}"))
}

fn package_inventory(root: &Path) -> BTreeMap<String, PackageTargets> {
    let mut inventory = BTreeMap::new();
    for (workspace, manifest) in [("root", ROOT_MANIFEST), ("components", COMPONENT_MANIFEST)] {
        for package in cargo_metadata(root, manifest).packages {
            let mut targets = BTreeMap::<String, BTreeSet<String>>::new();
            for target in package.targets {
                for kind in target.kind {
                    targets.entry(kind).or_default().insert(target.name.clone());
                }
            }
            let previous =
                inventory.insert(package.name.clone(), PackageTargets { workspace, targets });
            assert!(
                previous.is_none(),
                "package name {} is ambiguous across workspaces",
                package.name
            );
        }
    }
    inventory
}

fn bash_lines(source: &str) -> Vec<BashLine> {
    let mut lines = Vec::new();
    let mut active_block = None;
    let mut next_block = 0;
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed == "```bash" {
            next_block += 1;
            active_block = Some(next_block);
            continue;
        }
        if trimmed.starts_with("```") {
            active_block = None;
            continue;
        }
        if let Some(block) = active_block {
            lines.push(BashLine {
                block,
                number: index + 1,
                text: line.to_string(),
            });
        }
    }
    lines
}

fn logical_commands(lines: &[BashLine]) -> Vec<LogicalCommand> {
    let mut commands = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let line = &lines[index];
        let trimmed = line.text.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            index += 1;
            continue;
        }

        let mut text = trimmed.to_string();
        let block = line.block;
        let number = line.number;
        while text.trim_end().ends_with('\\') {
            text.truncate(text.trim_end().len() - 1);
            index += 1;
            let continuation = lines
                .get(index)
                .filter(|next| next.block == block)
                .unwrap_or_else(|| panic!("line {number} ends with an unterminated continuation"));
            text.push(' ');
            text.push_str(continuation.text.trim());
        }
        commands.push(LogicalCommand {
            block,
            number,
            text,
        });
        index += 1;
    }
    commands
}

fn shell_tokens(command: &str) -> Vec<String> {
    command
        .replace(['(', ')', '\\'], " ")
        .split_whitespace()
        .map(|token| {
            token
                .trim_matches(|character: char| {
                    matches!(character, '"' | '\'' | '`' | ',' | '(' | ')')
                })
                .to_string()
        })
        .take_while(|token| !token.starts_with('#'))
        .collect()
}

fn command_segments<'a>(tokens: &'a [String], executable: &str) -> Vec<&'a [String]> {
    let mut segments = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if token != executable {
            continue;
        }
        let end = tokens[index + 1..]
            .iter()
            .position(|candidate| matches!(candidate.as_str(), "&&" | ";" | "|"))
            .map_or(tokens.len(), |offset| index + 1 + offset);
        segments.push(&tokens[index..end]);
    }
    segments
}

fn option_values(segment: &[String], option: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut index = 0;
    while index < segment.len() {
        if segment[index] == option {
            let value = segment
                .get(index + 1)
                .unwrap_or_else(|| panic!("{option} has no value in {segment:?}"));
            values.push(value.trim_end_matches(';').to_string());
            index += 2;
        } else {
            index += 1;
        }
    }
    values
}

fn docker_stages(root: &Path) -> BTreeSet<String> {
    fs::read_to_string(root.join("Dockerfile"))
        .expect("read Dockerfile")
        .lines()
        .filter_map(|line| {
            let tokens = line.split_whitespace().collect::<Vec<_>>();
            tokens
                .windows(2)
                .find(|pair| pair[0].eq_ignore_ascii_case("AS"))
                .map(|pair| pair[1].to_string())
        })
        .collect()
}

fn is_repository_path(candidate: &str) -> bool {
    [
        "architecture/",
        "components/",
        "crates/",
        "deploy/",
        "docs/",
        "poc/",
        "services/",
        "test-support/",
        "tests/",
        "tools/",
    ]
    .iter()
    .any(|prefix| candidate.starts_with(prefix))
}

fn clean_repository_path(token: &str) -> Option<String> {
    let mut candidate = token.trim_matches(|character: char| {
        matches!(
            character,
            '"' | '\'' | '`' | '(' | ')' | '<' | '>' | ',' | ';'
        )
    });
    if let Some((_, suffix)) = candidate.rsplit_once('=')
        && is_repository_path(suffix)
    {
        candidate = suffix;
    }
    candidate = candidate.strip_prefix("$PWD/").unwrap_or(candidate);
    candidate = candidate.strip_prefix("./").unwrap_or(candidate);
    if !is_repository_path(candidate) {
        return None;
    }
    if let Some((host_path, _)) = candidate.split_once(':') {
        candidate = host_path;
    }
    let candidate = candidate.trim_end_matches(['\\', '"', '\'', '`', ')', ',']);
    if candidate.contains('$') || candidate.contains('{') || candidate.contains("...") {
        return None;
    }
    Some(candidate.to_string())
}

fn component_artifact_name(path: &str) -> Option<&str> {
    let marker = "wasm32-wasip2/release/";
    let file = path
        .split_once(marker)
        .map(|(_, file)| file)
        .or_else(|| (!path.contains('/') && path.ends_with(".wasm")).then_some(path))?;
    Some(file.trim_end_matches(".wasm"))
}

fn validate_component_artifact(path: &str, component_targets: &BTreeSet<String>, context: &str) {
    let Some(name) = component_artifact_name(path) else {
        return;
    };
    assert!(
        component_targets.contains(name) || matches!(name, "flow_composed" | "node-ts"),
        "{context} references unknown component artifact {path}"
    );
}

fn recipe_directives(lines: &[BashLine]) -> Vec<RecipeDirective> {
    let mut directives = Vec::new();
    for line in lines {
        let Some(raw) = line.text.trim().strip_prefix("# recipe-test:") else {
            continue;
        };
        let fields = raw.split('|').map(str::trim).collect::<Vec<_>>();
        assert_eq!(
            fields.len(),
            8,
            "line {} recipe-test directive must have 8 fields",
            line.number
        );
        directives.push(RecipeDirective {
            block: line.block,
            number: line.number,
            id: fields[0].to_string(),
            proof: fields[1].to_string(),
            package: fields[2].to_string(),
            kind: fields[3].to_string(),
            target: fields[4].to_string(),
            filter: fields[5].to_string(),
            minimum: fields[6].parse().unwrap_or_else(|error| {
                panic!(
                    "line {} has invalid minimum {}: {error}",
                    line.number, fields[6]
                )
            }),
            boundary: fields[7].to_string(),
        });
    }
    directives
}

#[test]
fn active_recipes_reference_current_packages_targets_stages_and_paths() {
    let root = repository_root();
    let source = fs::read_to_string(root.join(BUILD_AND_TEST_DOC)).expect("read build recipes");
    let lines = bash_lines(&source);
    let commands = logical_commands(&lines);
    let inventory = package_inventory(&root);
    let stages = docker_stages(&root);
    let component_targets = inventory
        .values()
        .filter(|package| package.workspace == "components")
        .flat_map(|package| package.targets.values())
        .flatten()
        .cloned()
        .collect::<BTreeSet<_>>();

    let mut cargo_package_references = 0;
    let mut docker_target_references = 0;
    let mut manifest_references = 0;
    let mut built_images = BTreeSet::new();
    let mut loaded_images = BTreeSet::new();

    for command in &commands {
        let tokens = shell_tokens(&command.text);
        let component_context = command.text.contains("cd components")
            || command.text.contains("--manifest-path components/");

        for segment in command_segments(&tokens, "cargo") {
            let action = segment.get(1).map(String::as_str).unwrap_or_default();
            let packages = option_values(segment, "-p")
                .into_iter()
                .chain(option_values(segment, "--package"))
                .collect::<Vec<_>>();
            for package_name in &packages {
                cargo_package_references += 1;
                let package = inventory.get(package_name).unwrap_or_else(|| {
                    panic!(
                        "line {} references unknown Cargo package {}",
                        command.number, package_name
                    )
                });
                let expected_workspace = if component_context {
                    "components"
                } else {
                    "root"
                };
                assert_eq!(
                    package.workspace, expected_workspace,
                    "line {} runs {} from the wrong workspace",
                    command.number, package_name
                );
            }
            if action == "test" {
                for package in &packages {
                    assert!(
                        !ROUTER_ONLY_TEST_PACKAGES.contains(&package.as_str()),
                        "line {} tests router-only package {}; select its owning library",
                        command.number,
                        package
                    );
                }
            }

            let named_targets = [
                ("--test", "test"),
                ("--bin", "bin"),
                ("--example", "example"),
            ];
            for (option, kind) in named_targets {
                for target in option_values(segment, option) {
                    assert!(
                        !packages.is_empty(),
                        "line {} uses {option} without a named package",
                        command.number
                    );
                    for package_name in &packages {
                        let package = &inventory[package_name];
                        assert!(
                            package
                                .targets
                                .get(kind)
                                .is_some_and(|names| names.contains(&target)),
                            "line {} selects missing {kind} target {target} in {package_name}",
                            command.number
                        );
                    }
                }
            }
            if segment.iter().any(|token| token == "--lib") {
                for package_name in &packages {
                    assert!(
                        inventory[package_name].targets.contains_key("lib"),
                        "line {} selects --lib for binary-only package {}",
                        command.number,
                        package_name
                    );
                }
            }
        }

        for segment in command_segments(&tokens, "docker") {
            if segment.get(1).map(String::as_str) != Some("build") {
                continue;
            }
            for target in option_values(segment, "--target") {
                docker_target_references += 1;
                assert!(
                    stages.contains(&target),
                    "line {} selects missing Dockerfile stage {}",
                    command.number,
                    target
                );
            }
            built_images.extend(option_values(segment, "-t"));
        }

        for segment in command_segments(&tokens, "kind") {
            if segment.get(1).map(String::as_str) == Some("load")
                && segment.get(2).map(String::as_str) == Some("docker-image")
                && let Some(image) = segment.get(3)
            {
                loaded_images.insert(image.trim_end_matches(';').to_string());
            }
        }

        for token in &tokens {
            if let Some(path) = clean_repository_path(token) {
                if path.contains("components/target/") {
                    validate_component_artifact(
                        &path,
                        &component_targets,
                        &format!("line {}", command.number),
                    );
                    continue;
                }
                if path == "components/samples/node-ts/node-ts.wasm" {
                    continue;
                }
                let absolute = root.join(&path);
                if absolute.exists() {
                    manifest_references += usize::from(
                        path.ends_with(".yaml")
                            || path.ends_with(".json")
                            || path.ends_with(".sql")
                            || path.ends_with(".toml"),
                    );
                    continue;
                }
                let resource_like = path.starts_with("deploy/")
                    && path.matches('/').count() == 1
                    && !path.contains('.');
                assert!(
                    resource_like,
                    "line {} references missing repository path {}",
                    command.number, path
                );
            }
            if let Some((_, artifact)) = token.split_once("$REL/") {
                validate_component_artifact(
                    artifact,
                    &component_targets,
                    &format!("line {}", command.number),
                );
            }
        }
    }

    for loaded in &loaded_images {
        assert!(
            built_images.contains(loaded) || loaded == "registry:2",
            "kind loads image {loaded} that no active Docker recipe builds"
        );
    }
    assert!(
        cargo_package_references > 100,
        "Cargo recipe parser selected suspiciously few package references"
    );
    assert!(
        docker_target_references > 20,
        "Docker recipe parser selected suspiciously few stages"
    );
    assert!(
        manifest_references > 50,
        "manifest/path recipe parser selected suspiciously few inputs"
    );
}

#[test]
fn recipe_test_directives_name_real_owners_and_commands() {
    let root = repository_root();
    let source = fs::read_to_string(root.join(BUILD_AND_TEST_DOC)).expect("read build recipes");
    let lines = bash_lines(&source);
    let commands = logical_commands(&lines);
    let directives = recipe_directives(&lines);
    let inventory = package_inventory(&root);
    let mut ids = BTreeSet::new();

    for directive in &directives {
        assert!(
            ids.insert(directive.id.clone()),
            "duplicate recipe id {}",
            directive.id
        );
        assert!(
            !directive.proof.is_empty(),
            "{} has no proof class",
            directive.id
        );
        assert!(
            !directive.boundary.is_empty(),
            "{} has no boundary",
            directive.id
        );
        assert!(directive.minimum > 0, "{} has a zero minimum", directive.id);

        let package = inventory.get(&directive.package).unwrap_or_else(|| {
            panic!(
                "{} names missing package {}",
                directive.id, directive.package
            )
        });
        assert_eq!(
            package.workspace, "root",
            "{} must name a root proof owner",
            directive.id
        );
        match directive.kind.as_str() {
            "lib" => {
                assert_eq!(
                    directive.target, "-",
                    "{} lib target must be '-'",
                    directive.id
                );
                assert!(
                    package.targets.contains_key("lib"),
                    "{} names a package without a lib target",
                    directive.id
                );
            }
            "test" => assert!(
                package
                    .targets
                    .get("test")
                    .is_some_and(|targets| targets.contains(&directive.target)),
                "{} names missing integration-test target {}",
                directive.id,
                directive.target
            ),
            other => panic!("{} has unsupported target kind {other}", directive.id),
        }

        let command = commands
            .iter()
            .find(|command| command.block == directive.block && command.number > directive.number)
            .unwrap_or_else(|| panic!("{} has no following command", directive.id));
        assert!(
            command.text.contains("cargo test")
                && command.text.contains(&format!("-p {}", directive.package)),
            "{} is not followed by its Cargo test command: {}",
            directive.id,
            command.text
        );
        match directive.kind.as_str() {
            "lib" => assert!(
                command.text.contains("--lib"),
                "{} command does not select --lib",
                directive.id
            ),
            "test" => assert!(
                command
                    .text
                    .contains(&format!("--test {}", directive.target)),
                "{} command does not select test target {}",
                directive.id,
                directive.target
            ),
            _ => unreachable!(),
        }
        if directive.filter != "-" {
            assert!(
                command.text.contains(&directive.filter),
                "{} command does not carry filter {}",
                directive.id,
                directive.filter
            );
        }
    }

    for required in REQUIRED_RECIPE_IDS {
        assert!(
            ids.contains(*required),
            "missing required corrected recipe directive {required}"
        );
    }
}

#[test]
fn catalog_identity_recipe_pins_the_real_owner_and_filter() {
    let root = repository_root();
    let source = fs::read_to_string(root.join(BUILD_AND_TEST_DOC)).expect("read build recipes");
    let lines = bash_lines(&source);
    let commands = logical_commands(&lines);
    let directives = recipe_directives(&lines);
    let directive = directives
        .iter()
        .find(|directive| directive.id == "H5-CATALOG-IDENTITY")
        .expect("H5-CATALOG-IDENTITY directive");

    assert_eq!(directive.package, "wamn-proof-conformance");
    assert_eq!(directive.kind, "lib");
    assert_eq!(directive.target, "-");
    assert_eq!(directive.filter, "catalog::tests::");
    assert_eq!(directive.minimum, 3);

    let command = commands
        .iter()
        .find(|command| command.block == directive.block && command.number > directive.number)
        .expect("H5-CATALOG-IDENTITY command");
    assert_eq!(
        command.text,
        "cargo test --locked -p wamn-proof-conformance --lib catalog::tests::"
    );
}

fn temporary_directory(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time after epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "wamn-build-recipes-{label}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir(&path).expect("create temporary directory");
    path
}

fn fake_cargo(directory: &Path, selected_tests: usize) -> PathBuf {
    let path = directory.join("cargo");
    let script = format!(
        "#!/usr/bin/env bash\n\
         for ((index = 0; index < {selected_tests}; index++)); do\n\
           printf 'fake::test_%s: test\\n' \"$index\"\n\
         done\n\
         printf '{selected_tests} tests, 0 benchmarks\\n'\n"
    );
    fs::write(&path, script).expect("write fake cargo");
    let mut permissions = fs::metadata(&path)
        .expect("fake cargo metadata")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("make fake cargo executable");
    path
}

fn helper_output(root: &Path, current_dir: &Path, cargo: &Path) -> Output {
    Command::new(root.join(BUILD_RECIPE_HELPER))
        .current_dir(current_dir)
        .env("CARGO", cargo)
        .output()
        .expect("run build recipe helper")
}

#[test]
fn recipe_test_helper_fails_when_cargo_selects_zero_tests() {
    let root = repository_root();
    let directory = temporary_directory("zero");
    let cargo = fake_cargo(&directory, 0);
    let output = helper_output(&root, &directory, &cargo);
    assert!(
        !output.status.success(),
        "zero selected tests must fail the recipe helper"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("selected 0 tests"),
        "zero-match failure must name the selected count:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(directory).expect("remove temporary directory");
}

#[test]
fn recipe_test_helper_accepts_nonzero_named_targets_from_any_working_directory() {
    let root = repository_root();
    let directory = temporary_directory("nonzero");
    let cargo = fake_cargo(&directory, 64);
    let output = helper_output(&root, &directory, &cargo);
    assert!(
        output.status.success(),
        "nonzero fake selections should satisfy every minimum:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    for required in REQUIRED_RECIPE_IDS {
        assert!(
            stdout.contains(&format!("{required}:")),
            "helper output omitted {required}"
        );
    }
    fs::remove_dir_all(directory).expect("remove temporary directory");
}
