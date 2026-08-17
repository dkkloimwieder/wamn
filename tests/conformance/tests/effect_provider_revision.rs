//! Deterministic proofs for the native effect-provider revision contract.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use syn::visit::{self, Visit};
use syn::{Attribute, Item, LitStr, Macro, Meta};

#[path = "../../../crates/execution/host/src/effect_provider_revision.rs"]
mod effect_provider_revision;

use effect_provider_revision::{
    ExternalPackage, ExternalSourceKind, LocalPackage, Manifest, ResolutionRecipe, RevisionInput,
    SemanticRecord, SourceRoot,
};

const HOST_PACKAGE: &str = "wamn-execution-host";
const EXECUTOR_PACKAGE: &str = "wamn-executor";
static TEMP_REPOSITORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct PackageIdentity {
    name: String,
    version: String,
    source: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProjectedPackage {
    identity: PackageIdentity,
    features: BTreeSet<String>,
    local_root: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LockPackage {
    name: String,
    version: String,
    source: Option<String>,
    checksum: Option<String>,
}

#[derive(Debug)]
struct TempRepository {
    root: PathBuf,
}

impl TempRepository {
    fn new(label: &str) -> Self {
        let sequence = TEMP_REPOSITORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "wamn-effect-provider-revision-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap_or_else(|error| {
            panic!("create temporary repository {}: {error}", root.display())
        });
        Self { root }
    }

    fn write(&self, relative: &str, bytes: impl AsRef<[u8]>) {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().expect("fixture file must have a parent"))
            .unwrap_or_else(|error| panic!("create fixture parent {}: {error}", path.display()));
        fs::write(&path, bytes)
            .unwrap_or_else(|error| panic!("write fixture {}: {error}", path.display()));
    }
}

impl Drop for TempRepository {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap_or_else(|error| {
            panic!(
                "remove temporary repository {}: {error}",
                self.root.display()
            )
        });
    }
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

fn source_section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let (_, remainder) = source
        .split_once(start)
        .unwrap_or_else(|| panic!("missing source section start {start:?}"));
    remainder
        .split_once(end)
        .map(|(section, _)| section)
        .unwrap_or_else(|| panic!("missing source section end {end:?}"))
}

fn synthetic_manifest() -> Manifest {
    Manifest {
        schema_version: 1,
        resolution: ResolutionRecipe {
            cargo_version: "1.97.0".to_string(),
            locked: true,
            offline: true,
            package_scoped: true,
            root_package: EXECUTOR_PACKAGE.to_string(),
            projection_root: HOST_PACKAGE.to_string(),
            target: "all".to_string(),
            edge_kinds: vec!["normal".to_string()],
            feature_unification: "union-per-full-package-identity-across-resolver-units"
                .to_string(),
        },
        local_packages: vec![LocalPackage {
            name: "wamn-local".to_string(),
            version: "0.1.0".to_string(),
            root: "crates/local".to_string(),
            features: vec!["provider".to_string()],
        }],
        external_packages: vec![
            ExternalPackage {
                name: "registry-dependency".to_string(),
                version: "1.2.3".to_string(),
                source_kind: ExternalSourceKind::Registry,
                source: "registry+https://github.com/rust-lang/crates.io-index".to_string(),
                revision: None,
                checksum: Some("a".repeat(64)),
                features: vec!["std".to_string()],
            },
            ExternalPackage {
                name: "wash-runtime".to_string(),
                version: "2.6.1".to_string(),
                source_kind: ExternalSourceKind::Git,
                source: "https://github.com/dkkloimwieder/wasmCloud".to_string(),
                revision: Some("b".repeat(40)),
                checksum: None,
                features: vec!["oci".to_string(), "washlet".to_string()],
            },
        ],
        workspace_inputs: vec![SemanticRecord {
            tag: "package/wamn-local/edition".to_string(),
            value: "2024".to_string(),
        }],
        executor_composition: SourceRoot {
            name: EXECUTOR_PACKAGE.to_string(),
            version: "0.1.0".to_string(),
            root: "services/executor".to_string(),
            features: Vec::new(),
        },
        assets: vec!["assets/provider-policy.bin".to_string()],
    }
}

fn populate_synthetic_repository(repository: &TempRepository, reverse: bool) {
    let files: [(&str, &[u8]); 8] = [
        ("crates/local/Cargo.toml", b"[package]\nname='wamn-local'\n"),
        ("crates/local/build.rs", b"fn main() {}\n"),
        ("crates/local/src/a.rs", b"pub const A: u8 = 1;\n"),
        ("crates/local/src/z.rs", b"pub const Z: u8 = 2;\n"),
        ("crates/local/wit/provider.wit", b"package wamn:provider;\n"),
        (
            "services/executor/Cargo.toml",
            b"[package]\nname='wamn-executor'\n",
        ),
        ("services/executor/src/lib.rs", b"pub fn compose() {}\n"),
        ("assets/provider-policy.bin", b"policy\0bytes\n"),
    ];
    if reverse {
        for (path, bytes) in files.into_iter().rev() {
            repository.write(path, bytes);
        }
    } else {
        for (path, bytes) in files {
            repository.write(path, bytes);
        }
    }
}

fn synthetic_revision(repository: &TempRepository, manifest: &Manifest) -> String {
    let inputs = effect_provider_revision::collect_revision_inputs(&repository.root, manifest)
        .expect("collect synthetic provider inputs");
    effect_provider_revision::revision(&inputs).expect("hash synthetic provider inputs")
}

fn assert_input_mutants_change_revision(root: &str, baseline_inputs: &[RevisionInput]) {
    let baseline =
        effect_provider_revision::revision(baseline_inputs).expect("hash baseline inputs");
    let prefix = format!("file/{root}/");
    let position = baseline_inputs
        .iter()
        .position(|input| input.tag.starts_with(&prefix))
        .unwrap_or_else(|| panic!("governed root has no file inputs: {root}"));

    let mut added = baseline_inputs.to_vec();
    added.push(RevisionInput {
        tag: format!("file/{root}/__added_mutant__"),
        value: b"added".to_vec(),
    });
    assert_ne!(
        effect_provider_revision::revision(&added).expect("hash add mutant"),
        baseline,
        "adding a file under {root} must mint a revision"
    );

    let mut removed = baseline_inputs.to_vec();
    removed.remove(position);
    assert_ne!(
        effect_provider_revision::revision(&removed).expect("hash remove mutant"),
        baseline,
        "removing a file under {root} must mint a revision"
    );

    let mut renamed = baseline_inputs.to_vec();
    renamed[position].tag.push_str("-renamed");
    assert_ne!(
        effect_provider_revision::revision(&renamed).expect("hash rename mutant"),
        baseline,
        "renaming a file under {root} must mint a revision"
    );

    let mut changed = baseline_inputs.to_vec();
    changed[position].value.push(0xff);
    assert_ne!(
        effect_provider_revision::revision(&changed).expect("hash byte mutant"),
        baseline,
        "changing a file byte under {root} must mint a revision"
    );
}

#[test]
fn effect_provider_revision_golden_preimage_and_literal() {
    let inputs = vec![
        RevisionInput {
            tag: "z".to_string(),
            value: vec![0, 255],
        },
        RevisionInput {
            tag: "a".to_string(),
            value: b"alpha".to_vec(),
        },
    ];
    let expected = hex::decode("000000000000002077616d6e2e6566666563742d70726f76696465722d7265766973696f6e2e76310000000000000001610000000000000005616c70686100000000000000017a000000000000000200ff")
        .expect("golden preimage is valid hex");

    assert_eq!(
        effect_provider_revision::preimage(&inputs).expect("frame golden inputs"),
        expected
    );
    assert_eq!(
        effect_provider_revision::revision(&inputs).expect("hash golden inputs"),
        "sha256:4451bcecde799965ee45dbb638226e29f8555dbd851a6c2d52a3fca2ce4f9c3c"
    );
}

#[test]
fn effect_provider_revision_is_checkout_path_and_discovery_order_independent() {
    let first = TempRepository::new("checkout-a");
    let second = TempRepository::new("checkout-b");
    populate_synthetic_repository(&first, false);
    populate_synthetic_repository(&second, true);
    let manifest = synthetic_manifest();

    let first_inputs = effect_provider_revision::collect_revision_inputs(&first.root, &manifest)
        .expect("collect first checkout");
    let second_inputs = effect_provider_revision::collect_revision_inputs(&second.root, &manifest)
        .expect("collect second checkout");
    assert_eq!(first_inputs, second_inputs);
    assert_eq!(
        effect_provider_revision::revision(&first_inputs).expect("hash first checkout"),
        effect_provider_revision::revision(&second_inputs).expect("hash second checkout")
    );
}

#[test]
fn effect_provider_revision_rejects_noncanonical_manifest_entries() {
    let repository = TempRepository::new("noncanonical");
    populate_synthetic_repository(&repository, false);
    let manifest = synthetic_manifest();
    let canonical = effect_provider_revision::canonical_manifest_bytes(&manifest)
        .expect("encode canonical synthetic manifest");
    assert_eq!(
        effect_provider_revision::parse_manifest(&canonical)
            .expect("parse canonical synthetic manifest"),
        manifest
    );

    let mut noncanonical_json = canonical.clone();
    noncanonical_json.push(b' ');
    assert!(effect_provider_revision::parse_manifest(&noncanonical_json).is_err());

    let mut cases = Vec::new();
    let mut wrong_recipe = manifest.clone();
    wrong_recipe.resolution.target = "host".to_string();
    cases.push(wrong_recipe);
    let mut unlocked_recipe = manifest.clone();
    unlocked_recipe.resolution.locked = false;
    cases.push(unlocked_recipe);
    let mut online_recipe = manifest.clone();
    online_recipe.resolution.offline = false;
    cases.push(online_recipe);
    let mut workspace_scoped_recipe = manifest.clone();
    workspace_scoped_recipe.resolution.package_scoped = false;
    cases.push(workspace_scoped_recipe);
    let mut last_unit_wins = manifest.clone();
    last_unit_wins.resolution.feature_unification = "last-resolver-unit-wins".to_string();
    cases.push(last_unit_wins);
    let mut duplicate_feature = manifest.clone();
    duplicate_feature.local_packages[0]
        .features
        .push("provider".to_string());
    cases.push(duplicate_feature);
    let mut absolute_root = manifest.clone();
    absolute_root.local_packages[0].root = "/crates/local".to_string();
    cases.push(absolute_root);
    let mut traversing_asset = manifest.clone();
    traversing_asset.assets[0] = "../provider-policy.bin".to_string();
    cases.push(traversing_asset);
    for alias in ["crates//local", "crates/./local", "crates/local/"] {
        let mut aliased_root = manifest.clone();
        aliased_root.local_packages[0].root = alias.to_string();
        cases.push(aliased_root);
    }
    let mut duplicate_root = manifest.clone();
    duplicate_root.local_packages.push(LocalPackage {
        name: "wamn-other".to_string(),
        version: "0.1.0".to_string(),
        root: "crates/local".to_string(),
        features: Vec::new(),
    });
    cases.push(duplicate_root);
    let mut noncanonical_workspace_tag = manifest.clone();
    noncanonical_workspace_tag.workspace_inputs[0].tag = "package//edition".to_string();
    cases.push(noncanonical_workspace_tag);
    let mut registry_without_checksum = manifest.clone();
    registry_without_checksum.external_packages[0].checksum = None;
    cases.push(registry_without_checksum);
    let mut git_with_checksum = manifest.clone();
    git_with_checksum.external_packages[1].checksum = Some("c".repeat(64));
    cases.push(git_with_checksum);
    let mut abbreviated_revision = manifest.clone();
    abbreviated_revision.external_packages[1].revision = Some("b".repeat(12));
    cases.push(abbreviated_revision);
    for case in cases {
        assert!(
            effect_provider_revision::validate_manifest(&case).is_err(),
            "noncanonical provider manifest was accepted: {case:?}"
        );
    }
    let commit = "b".repeat(40);
    for source in [
        format!("https://example.invalid/provider?branch=main#{commit}"),
        format!("https://example.invalid/provider?tag=v1#{commit}"),
        "https://example.invalid/provider?rev=abcdef#abcdef".to_string(),
        format!(
            "https://example.invalid/provider?rev={}#{commit}",
            "c".repeat(40)
        ),
    ] {
        assert!(
            immutable_git_source(&source).is_err(),
            "mutable or noncanonical git source was accepted: {source}"
        );
    }

    let mut missing_root = manifest.clone();
    missing_root.local_packages[0].root = "crates/missing".to_string();
    assert!(
        effect_provider_revision::collect_revision_inputs(&repository.root, &missing_root).is_err()
    );
    fs::remove_file(repository.root.join("assets/provider-policy.bin"))
        .expect("remove governed asset");
    assert!(
        effect_provider_revision::collect_revision_inputs(&repository.root, &manifest).is_err()
    );

    repository.write("assets/provider-policy.bin", b"policy\0bytes\n");
    repository.write("outside/unlisted.bin", b"unlisted\n");
    repository.write(
        "crates/local/src/a.rs",
        b"const UNLISTED: &[u8] = include_bytes!(\"../../../outside/unlisted.bin\");\n",
    );
    let discovered = discovered_production_include_assets(&repository.root, &manifest);
    assert!(
        discovered.iter().any(|path| path == "outside/unlisted.bin")
            && !manifest
                .assets
                .iter()
                .any(|path| path == "outside/unlisted.bin"),
        "an unlisted production include asset must drift regeneration"
    );
    repository.write(
        "crates/local/src/z.rs",
        b"#[cfg(test)]\nmod tests { const TEST_ONLY: &[u8] = include_bytes!(\"../../../outside/test-only.bin\"); }\nconst AFTER: &str = std::include_str! (r#\"../../../outside/after-test.bin\"#);\n",
    );
    repository.write("outside/test-only.bin", b"test-only\n");
    repository.write("outside/after-test.bin", b"production\n");
    let discovered = discovered_production_include_assets(&repository.root, &manifest);
    assert!(
        !discovered
            .iter()
            .any(|path| path == "outside/test-only.bin"),
        "cfg(test) includes must stay outside the production closure"
    );
    assert!(
        discovered
            .iter()
            .any(|path| path == "outside/after-test.bin"),
        "raw, whitespace-separated, namespaced production includes after a test module must be discovered"
    );
    repository.write(
        "crates/local/src/concat.rs",
        b"const CONCAT: &str = include_str!(concat!(\"../../../outside/\", \"concat.bin\"));\n",
    );
    assert!(
        std::panic::catch_unwind(|| {
            discovered_production_include_assets(&repository.root, &manifest)
        })
        .is_err(),
        "a nonliteral production include must refuse instead of escaping discovery"
    );
    fs::remove_file(repository.root.join("crates/local/src/concat.rs"))
        .expect("remove noncanonical include mutant");

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        repository.write("assets/provider-policy.bin", b"policy\0bytes\n");
        symlink(
            repository.root.join("crates/local/src/a.rs"),
            repository.root.join("crates/local/src/link.rs"),
        )
        .expect("create governed symlink mutant");
        assert!(
            effect_provider_revision::collect_revision_inputs(&repository.root, &manifest).is_err(),
            "a governed symlink must be refused"
        );
    }
}

#[test]
fn each_local_closure_root_add_remove_rename_and_byte_mutant_changes_revision() {
    let repository = TempRepository::new("local-mutants");
    populate_synthetic_repository(&repository, false);
    let manifest = synthetic_manifest();
    let baseline = synthetic_revision(&repository, &manifest);

    repository.write("crates/local/src/added.rs", b"pub const ADDED: u8 = 3;\n");
    assert_ne!(synthetic_revision(&repository, &manifest), baseline);
    fs::remove_file(repository.root.join("crates/local/src/added.rs"))
        .expect("remove added source mutant");

    fs::remove_file(repository.root.join("crates/local/src/a.rs"))
        .expect("remove governed source mutant");
    assert_ne!(synthetic_revision(&repository, &manifest), baseline);
    repository.write("crates/local/src/a.rs", b"pub const A: u8 = 1;\n");

    fs::rename(
        repository.root.join("crates/local/src/a.rs"),
        repository.root.join("crates/local/src/renamed.rs"),
    )
    .expect("rename governed source mutant");
    assert_ne!(synthetic_revision(&repository, &manifest), baseline);
    fs::rename(
        repository.root.join("crates/local/src/renamed.rs"),
        repository.root.join("crates/local/src/a.rs"),
    )
    .expect("restore governed source name");

    repository.write("crates/local/src/a.rs", b"pub const A: u8 = 9;\n");
    assert_ne!(synthetic_revision(&repository, &manifest), baseline);

    let root = repository_root();
    let production_manifest = regenerated_manifest(&root);
    let production_inputs =
        effect_provider_revision::collect_revision_inputs(&root, &production_manifest)
            .expect("collect production provider inputs");
    for governed_root in production_manifest
        .local_packages
        .iter()
        .map(|package| package.root.as_str())
        .chain(std::iter::once(
            production_manifest.executor_composition.root.as_str(),
        ))
    {
        assert_input_mutants_change_revision(governed_root, &production_inputs);
    }
}

#[test]
fn external_checksum_source_rev_and_feature_mutants_change_revision() {
    let repository = TempRepository::new("external-mutants");
    populate_synthetic_repository(&repository, false);
    let manifest = synthetic_manifest();
    let baseline = synthetic_revision(&repository, &manifest);

    let mut checksum = manifest.clone();
    checksum.external_packages[0].checksum = Some("c".repeat(64));
    assert_ne!(synthetic_revision(&repository, &checksum), baseline);

    let mut source = manifest.clone();
    source.external_packages[0].source =
        "registry+https://example.invalid/crates-index".to_string();
    assert_ne!(synthetic_revision(&repository, &source), baseline);

    let mut feature = manifest.clone();
    feature.external_packages[0]
        .features
        .push("unstable".to_string());
    assert_ne!(synthetic_revision(&repository, &feature), baseline);
}

#[test]
fn fork_rev_and_feature_mutants_change_revision() {
    let repository = TempRepository::new("fork-mutants");
    populate_synthetic_repository(&repository, false);
    let manifest = synthetic_manifest();
    let baseline = synthetic_revision(&repository, &manifest);

    let mut revision = manifest.clone();
    revision.external_packages[1].revision = Some("c".repeat(40));
    assert_ne!(synthetic_revision(&repository, &revision), baseline);

    let mut features = manifest.clone();
    features.external_packages[1]
        .features
        .push("wasi-config".to_string());
    assert_ne!(synthetic_revision(&repository, &features), baseline);
}

#[test]
fn out_of_scope_scheduler_test_doc_and_runtime_config_mutants_do_not_change_revision() {
    let repository = TempRepository::new("out-of-scope");
    populate_synthetic_repository(&repository, false);
    let manifest = synthetic_manifest();
    let baseline = synthetic_revision(&repository, &manifest);

    for (path, bytes) in [
        (
            "crates/execution/scheduler/src/lib.rs",
            b"scheduler".as_slice(),
        ),
        ("tests/system/tests/provider.rs", b"test".as_slice()),
        ("docs/provider.md", b"documentation".as_slice()),
        ("runtime/config/provider.json", b"{}".as_slice()),
    ] {
        repository.write(path, bytes);
        assert_eq!(
            synthetic_revision(&repository, &manifest),
            baseline,
            "out-of-scope mutation unexpectedly changed revision: {path}"
        );
    }
}

fn parse_package_display(root: &Path, display: &str) -> ProjectedPackage {
    let (name, version_and_source) = display
        .split_once(" v")
        .unwrap_or_else(|| panic!("cargo tree package lacks a version: {display:?}"));
    let (version, suffix) = version_and_source
        .split_once(' ')
        .map_or((version_and_source, ""), |(version, suffix)| {
            (version, suffix.trim())
        });
    let suffix = suffix
        .strip_prefix("(proc-macro)")
        .map_or(suffix, str::trim);
    let source = (!suffix.is_empty()).then(|| {
        suffix
            .strip_prefix('(')
            .and_then(|value| value.strip_suffix(')'))
            .unwrap_or_else(|| panic!("noncanonical cargo tree source: {suffix:?}"))
            .to_string()
    });
    let local_root = source.as_deref().and_then(|value| {
        Path::new(value)
            .strip_prefix(root)
            .ok()
            .map(|path| path.to_string_lossy().replace('\\', "/"))
    });

    ProjectedPackage {
        identity: PackageIdentity {
            name: name.to_string(),
            version: version.to_string(),
            source,
        },
        features: BTreeSet::new(),
        local_root,
    }
}

fn parse_host_projection(root: &Path, stdout: &str) -> Vec<ProjectedPackage> {
    let mut in_host_subgraph = false;
    let mut packages = BTreeMap::<PackageIdentity, ProjectedPackage>::new();

    for line in stdout.lines() {
        let mut fields = line.splitn(3, '|');
        let depth = fields
            .next()
            .expect("cargo tree line must have a depth")
            .parse::<usize>()
            .unwrap_or_else(|error| panic!("invalid cargo tree depth in {line:?}: {error}"));
        let display = fields
            .next()
            .unwrap_or_else(|| panic!("cargo tree line lacks a package: {line:?}"));
        let features = fields
            .next()
            .unwrap_or_else(|| panic!("cargo tree line lacks features: {line:?}"));

        if depth == 1 {
            let package = parse_package_display(root, display);
            in_host_subgraph = package.identity.name == HOST_PACKAGE;
        }
        if !in_host_subgraph {
            continue;
        }

        let mut package = parse_package_display(root, display);
        package.features.extend(
            features
                .split(',')
                .filter(|feature| !feature.is_empty())
                .map(str::to_string),
        );
        packages
            .entry(package.identity.clone())
            .and_modify(|existing| existing.features.extend(package.features.iter().cloned()))
            .or_insert(package);
    }

    assert!(
        packages.keys().any(|package| package.name == HOST_PACKAGE),
        "package-scoped executor tree must contain the execution-host subgraph"
    );
    packages.into_values().collect()
}

fn locked_host_projection(root: &Path) -> Vec<ProjectedPackage> {
    let version = Command::new(env!("CARGO"))
        .arg("--version")
        .output()
        .expect("read Cargo version");
    assert!(version.status.success(), "cargo --version must succeed");
    assert!(
        String::from_utf8_lossy(&version.stdout).starts_with("cargo 1.97.0 "),
        "provider closure requires Cargo 1.97.0, got {}",
        String::from_utf8_lossy(&version.stdout).trim()
    );
    let output = Command::new(env!("CARGO"))
        .current_dir(root)
        .args([
            "tree",
            "-p",
            EXECUTOR_PACKAGE,
            "--target",
            "all",
            "--edges",
            "normal",
            "--locked",
            "--offline",
            "--prefix",
            "depth",
            "--format",
            "|{p}|{f}",
            "--no-dedupe",
        ])
        .output()
        .expect("run package-scoped locked/offline cargo tree");
    assert!(
        output.status.success(),
        "package-scoped cargo tree failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("cargo tree output must be UTF-8");
    parse_host_projection(root, &stdout)
}

fn quoted_toml_value(line: &str, key: &str) -> Option<String> {
    line.strip_prefix(key)
        .and_then(|value| value.trim().strip_prefix('='))
        .map(str::trim)
        .and_then(|value| value.strip_prefix('"'))
        .and_then(|value| value.strip_suffix('"'))
        .map(str::to_string)
}

fn locked_packages(root: &Path) -> Vec<LockPackage> {
    let contents = fs::read_to_string(root.join("Cargo.lock")).expect("read Cargo.lock");
    contents
        .split("[[package]]")
        .skip(1)
        .map(|section| {
            let mut name = None;
            let mut version = None;
            let mut source = None;
            let mut checksum = None;
            for line in section.lines().map(str::trim) {
                name = name.or_else(|| quoted_toml_value(line, "name"));
                version = version.or_else(|| quoted_toml_value(line, "version"));
                source = source.or_else(|| quoted_toml_value(line, "source"));
                checksum = checksum.or_else(|| quoted_toml_value(line, "checksum"));
            }
            LockPackage {
                name: name.expect("locked package must have a name"),
                version: version.expect("locked package must have a version"),
                source,
                checksum,
            }
        })
        .collect()
}

fn external_manifest_package(
    projected: &ProjectedPackage,
    locked: &[LockPackage],
) -> ExternalPackage {
    let candidates = locked
        .iter()
        .filter(|package| {
            package.name == projected.identity.name
                && package.version == projected.identity.version
                && package.source.is_some()
        })
        .collect::<Vec<_>>();
    let locked = if let Some(tree_source) = projected.identity.source.as_deref() {
        candidates
            .into_iter()
            .find(|package| {
                package.source.as_deref().is_some_and(|source| {
                    source.strip_prefix("git+").is_some_and(|source| {
                        source.starts_with(tree_source.split('#').next().unwrap())
                    })
                })
            })
            .unwrap_or_else(|| panic!("Cargo.lock lacks tree package {projected:?}"))
    } else {
        assert_eq!(
            candidates.len(),
            1,
            "registry package identity is ambiguous for {projected:?}"
        );
        candidates[0]
    };
    let locked_source = locked
        .source
        .as_deref()
        .expect("external package has source");
    let (source_kind, source, revision, checksum) =
        if let Some(git) = locked_source.strip_prefix("git+") {
            let (source, revision) = immutable_git_source(git)
                .unwrap_or_else(|error| panic!("noncanonical locked git source: {error}"));
            (ExternalSourceKind::Git, source, Some(revision), None)
        } else {
            (
                ExternalSourceKind::Registry,
                locked_source.to_string(),
                None,
                locked.checksum.clone(),
            )
        };
    ExternalPackage {
        name: projected.identity.name.clone(),
        version: projected.identity.version.clone(),
        source_kind,
        source,
        revision,
        checksum,
        features: projected.features.iter().cloned().collect(),
    }
}

fn immutable_git_source(source: &str) -> Result<(String, String), String> {
    let (qualified_url, resolved_revision) = source
        .rsplit_once('#')
        .ok_or_else(|| format!("git source lacks a resolved commit: {source}"))?;
    let (url, query) = qualified_url
        .split_once('?')
        .ok_or_else(|| format!("git source lacks an immutable requested rev: {source}"))?;
    let requested_revision = query
        .strip_prefix("rev=")
        .filter(|revision| !revision.contains('&'))
        .ok_or_else(|| format!("git source uses a branch, tag, or noncanonical query: {source}"))?;
    for (label, revision) in [
        ("requested", requested_revision),
        ("resolved", resolved_revision),
    ] {
        if revision.len() != 40
            || !revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(format!(
                "{label} git revision is not full lowercase 40-hex: {revision}"
            ));
        }
    }
    if requested_revision != resolved_revision {
        return Err(format!(
            "requested git rev {requested_revision} does not equal resolved commit {resolved_revision}"
        ));
    }
    if !url.starts_with("https://") {
        return Err(format!("git source URL is not canonical HTTPS: {url}"));
    }
    Ok((url.to_string(), resolved_revision.to_string()))
}

fn inherited_dependency_names(manifest: &str) -> BTreeSet<String> {
    let mut in_normal_dependencies = false;
    let mut names = BTreeSet::new();
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_normal_dependencies = line == "[dependencies]"
                || (line.starts_with("[target.") && line.ends_with(".dependencies]"));
            continue;
        }
        if in_normal_dependencies && line.contains("workspace = true") {
            let name = line
                .split_once('=')
                .map(|(name, _)| name.trim().trim_matches('"'))
                .expect("workspace dependency declaration must have an equals sign");
            names.insert(name.to_string());
        }
    }
    names
}

fn inherited_package_fields(manifest: &str) -> BTreeSet<String> {
    let mut fields = BTreeSet::new();
    let mut section = "";
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line;
            continue;
        }
        if section == "[package]" && line.ends_with(".workspace = true") {
            fields.insert(
                line.split_once('.')
                    .map(|(field, _)| field)
                    .expect("workspace package field has a dot")
                    .to_string(),
            );
        } else if section != "[package]" && line == "workspace = true" {
            panic!("unsupported inherited workspace field in section {section}");
        }
    }
    assert!(
        fields
            .iter()
            .all(|field| matches!(field.as_str(), "version" | "edition" | "license")),
        "unsupported inherited package field: {fields:?}"
    );
    fields
}

fn canonical_dependency_source(
    source: Option<&str>,
    path: Option<String>,
) -> Vec<(&'static str, String)> {
    let Some(source) = source else {
        return vec![(
            "source",
            path.map(|path| format!("path:{path}"))
                .unwrap_or_else(|| "registry:crates.io".to_string()),
        )];
    };
    if let Some(git) = source.strip_prefix("git+") {
        let (url, query) = git
            .split_once('?')
            .unwrap_or_else(|| panic!("git dependency lacks an immutable rev: {source}"));
        let requested_revision = query
            .strip_prefix("rev=")
            .filter(|revision| !revision.contains('&'))
            .unwrap_or_else(|| {
                panic!("git dependency uses branch/tag/noncanonical query: {source}")
            });
        assert!(
            requested_revision.len() == 40
                && requested_revision
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "requested git revision must be full lowercase 40-hex: {requested_revision}"
        );
        return vec![
            ("source", format!("git:{url}")),
            ("git-revision", requested_revision.to_string()),
        ];
    }
    vec![("source", source.to_string())]
}

fn workspace_inputs(root: &Path, local: &[ProjectedPackage]) -> Vec<SemanticRecord> {
    let output = Command::new(env!("CARGO"))
        .current_dir(root)
        .args([
            "metadata",
            "--no-deps",
            "--locked",
            "--offline",
            "--format-version",
            "1",
        ])
        .output()
        .expect("run non-resolving Cargo metadata for inherited fields");
    assert!(
        output.status.success(),
        "Cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse Cargo metadata JSON");
    let packages = metadata["packages"]
        .as_array()
        .expect("Cargo metadata packages must be an array");
    let local_names = local
        .iter()
        .map(|package| package.identity.name.as_str())
        .collect::<BTreeSet<_>>();
    let mut records = Vec::new();
    for package in packages {
        let name = package["name"].as_str().expect("metadata package name");
        if !local_names.contains(name) && name != EXECUTOR_PACKAGE {
            continue;
        }
        let manifest_path = package["manifest_path"]
            .as_str()
            .expect("metadata manifest path");
        let declaration = fs::read_to_string(manifest_path)
            .unwrap_or_else(|error| panic!("read {manifest_path}: {error}"));
        for field in inherited_package_fields(&declaration) {
            let value = package[field.as_str()]
                .as_str()
                .unwrap_or_else(|| panic!("metadata package {name} lacks {field}"));
            records.push(SemanticRecord {
                tag: format!("package/{name}/{field}"),
                value: value.to_string(),
            });
        }

        let inherited = inherited_dependency_names(&declaration);
        for dependency in package["dependencies"]
            .as_array()
            .expect("metadata dependencies must be an array")
        {
            if !dependency["kind"].is_null() {
                continue;
            }
            let dependency_name = dependency["name"].as_str().expect("dependency name");
            let declared_name = dependency["rename"].as_str().unwrap_or(dependency_name);
            if !inherited.contains(declared_name)
                || (name == EXECUTOR_PACKAGE && dependency_name != "wash-runtime")
            {
                continue;
            }
            let target = dependency["target"].as_str().unwrap_or("");
            let target_tag = if target.is_empty() {
                "all-targets".to_string()
            } else {
                format!("target-{}", hex::encode(target.as_bytes()))
            };
            let prefix = format!("dependency/{name}/{target_tag}/{declared_name}");
            let path = dependency["path"].as_str().map(|path| {
                Path::new(path)
                    .strip_prefix(root)
                    .unwrap_or_else(|_| panic!("dependency path escaped repository: {path}"))
                    .to_string_lossy()
                    .replace('\\', "/")
            });
            let source_fields = canonical_dependency_source(dependency["source"].as_str(), path);
            let mut features = dependency["features"]
                .as_array()
                .expect("dependency features must be an array")
                .iter()
                .map(|value| value.as_str().expect("dependency feature").to_string())
                .collect::<Vec<_>>();
            features.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
            let mut fields = vec![
                ("package", dependency_name.to_string()),
                (
                    "version",
                    dependency["req"]
                        .as_str()
                        .expect("dependency version requirement")
                        .to_string(),
                ),
                (
                    "default-features",
                    dependency["uses_default_features"]
                        .as_bool()
                        .expect("dependency default feature policy")
                        .to_string(),
                ),
                (
                    "optional",
                    dependency["optional"]
                        .as_bool()
                        .expect("dependency optional policy")
                        .to_string(),
                ),
                ("features", features.join(",")),
                ("target", target.to_string()),
                (
                    "registry",
                    dependency["registry"].as_str().unwrap_or("").to_string(),
                ),
            ];
            fields.extend(source_fields);
            fields.sort_by_key(|(field, _)| *field);
            for (field, value) in fields {
                records.push(SemanticRecord {
                    tag: format!("{prefix}/{field}"),
                    value,
                });
            }
        }
    }
    records.sort_by(|left, right| left.tag.as_bytes().cmp(right.tag.as_bytes()));
    records
}

fn normalized_include_path(root: &Path, source: &Path, literal: &str) -> String {
    assert!(
        !literal.contains('\\') && !Path::new(literal).is_absolute(),
        "include path must be normalized and relative: {literal:?}"
    );
    let mut resolved = source
        .parent()
        .expect("Rust source has a parent")
        .to_path_buf();
    for component in Path::new(literal).components() {
        match component {
            std::path::Component::Normal(part) => resolved.push(part),
            std::path::Component::ParentDir => {
                assert!(resolved.pop(), "include path escaped repository: {literal}");
            }
            std::path::Component::CurDir => {}
            _ => panic!("noncanonical include path: {literal:?}"),
        }
    }
    resolved
        .strip_prefix(root)
        .unwrap_or_else(|_| panic!("include path escaped repository: {}", resolved.display()))
        .to_string_lossy()
        .replace('\\', "/")
}

fn exact_cfg_test(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && matches!(&attribute.meta, Meta::List(list) if list.tokens.to_string() == "test")
    })
}

fn item_attributes(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(item) => &item.attrs,
        Item::Enum(item) => &item.attrs,
        Item::ExternCrate(item) => &item.attrs,
        Item::Fn(item) => &item.attrs,
        Item::ForeignMod(item) => &item.attrs,
        Item::Impl(item) => &item.attrs,
        Item::Macro(item) => &item.attrs,
        Item::Mod(item) => &item.attrs,
        Item::Static(item) => &item.attrs,
        Item::Struct(item) => &item.attrs,
        Item::Trait(item) => &item.attrs,
        Item::TraitAlias(item) => &item.attrs,
        Item::Type(item) => &item.attrs,
        Item::Union(item) => &item.attrs,
        Item::Use(item) => &item.attrs,
        Item::Verbatim(_) => &[],
        _ => &[],
    }
}

#[derive(Default)]
struct ProductionIncludeVisitor {
    literals: Vec<String>,
}

impl<'ast> Visit<'ast> for ProductionIncludeVisitor {
    fn visit_item(&mut self, item: &'ast Item) {
        if !exact_cfg_test(item_attributes(item)) {
            visit::visit_item(self, item);
        }
    }

    fn visit_macro(&mut self, item: &'ast Macro) {
        let macro_name = item
            .path
            .segments
            .last()
            .expect("macro path has at least one segment")
            .ident
            .to_string();
        if macro_name == "include_bytes" || macro_name == "include_str" {
            let literal = syn::parse2::<LitStr>(item.tokens.clone()).unwrap_or_else(|error| {
                panic!(
                    "production {}! input must be one canonical string literal: {error}",
                    macro_name
                )
            });
            self.literals.push(literal.value());
            return;
        }
        let tokens = item.tokens.to_string();
        assert!(
            !tokens.contains("include_bytes") && !tokens.contains("include_str"),
            "production include macros nested inside another macro are noncanonical"
        );
        visit::visit_macro(self, item);
    }
}

fn production_include_literals(source: &str) -> Vec<String> {
    let syntax = syn::parse_file(source).expect("governed Rust source must parse");
    let mut visitor = ProductionIncludeVisitor::default();
    visitor.visit_file(&syntax);
    visitor.literals
}

fn collect_rust_sources(directory: &Path, files: &mut Vec<PathBuf>) {
    let mut entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read source directory {}: {error}", directory.display()))
        .collect::<Result<Vec<_>, _>>()
        .expect("read source entries");
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let file_type = entry.file_type().expect("inspect source entry");
        assert!(!file_type.is_symlink(), "source scanner refuses symlinks");
        if file_type.is_dir() {
            collect_rust_sources(&entry.path(), files);
        } else if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "rs")
        {
            files.push(entry.path());
        }
    }
}

fn discovered_production_include_assets(root: &Path, manifest: &Manifest) -> Vec<String> {
    let mut assets = BTreeSet::new();
    for package in manifest
        .local_packages
        .iter()
        .map(|package| (&package.root, &package.features))
        .chain(std::iter::once((
            &manifest.executor_composition.root,
            &manifest.executor_composition.features,
        )))
    {
        let source_root = root.join(package.0).join("src");
        let mut sources = Vec::new();
        if source_root.is_dir() {
            collect_rust_sources(&source_root, &mut sources);
        }
        let build_script = root.join(package.0).join("build.rs");
        if build_script.is_file() && package.0 != &manifest.executor_composition.root {
            sources.push(build_script);
        }
        for source_path in sources {
            let source_relative = source_path
                .strip_prefix(root)
                .expect("source stays inside repository")
                .to_string_lossy()
                .replace('\\', "/");
            if source_relative == "crates/execution/run-state/src/schema_drift.rs"
                && !package.1.iter().any(|feature| feature == "test-util")
            {
                continue;
            }
            let source = fs::read_to_string(&source_path)
                .unwrap_or_else(|error| panic!("read {}: {error}", source_path.display()));
            for literal in production_include_literals(&source) {
                let relative = normalized_include_path(root, &source_path, &literal);
                let covered_by_local = manifest.local_packages.iter().any(|local| {
                    relative == format!("{}/Cargo.toml", local.root)
                        || relative == format!("{}/build.rs", local.root)
                        || relative.starts_with(&format!("{}/src/", local.root))
                        || relative.starts_with(&format!("{}/wit/", local.root))
                });
                let composition = &manifest.executor_composition.root;
                let covered_by_composition = relative == format!("{composition}/Cargo.toml")
                    || relative.starts_with(&format!("{composition}/src/"));
                if covered_by_local || covered_by_composition {
                    continue;
                }
                assets.insert(relative);
            }
        }
    }
    assets.into_iter().collect()
}

fn regenerated_manifest(root: &Path) -> Manifest {
    let packages = locked_host_projection(root);
    let locked = locked_packages(root);
    let mut local_packages = packages
        .iter()
        .filter_map(|package| {
            package.local_root.as_ref().map(|local_root| LocalPackage {
                name: package.identity.name.clone(),
                version: package.identity.version.clone(),
                root: local_root.clone(),
                features: package.features.iter().cloned().collect(),
            })
        })
        .collect::<Vec<_>>();
    local_packages.sort_by(|left, right| {
        format!("{}@{}", left.name, left.version)
            .as_bytes()
            .cmp(format!("{}@{}", right.name, right.version).as_bytes())
    });
    let local_projection = packages
        .iter()
        .filter(|package| package.local_root.is_some())
        .cloned()
        .collect::<Vec<_>>();
    let mut external_packages = packages
        .iter()
        .filter(|package| package.local_root.is_none())
        .map(|package| external_manifest_package(package, &locked))
        .collect::<Vec<_>>();
    external_packages.sort_by(|left, right| {
        format!(
            "{}@{}|{:?}|{}|{}",
            left.name,
            left.version,
            left.source_kind,
            left.source,
            left.revision.as_deref().unwrap_or_default()
        )
        .as_bytes()
        .cmp(
            format!(
                "{}@{}|{:?}|{}|{}",
                right.name,
                right.version,
                right.source_kind,
                right.source,
                right.revision.as_deref().unwrap_or_default()
            )
            .as_bytes(),
        )
    });
    let mut manifest = Manifest {
        schema_version: 1,
        resolution: ResolutionRecipe {
            cargo_version: "1.97.0".to_string(),
            locked: true,
            offline: true,
            package_scoped: true,
            root_package: EXECUTOR_PACKAGE.to_string(),
            projection_root: HOST_PACKAGE.to_string(),
            target: "all".to_string(),
            edge_kinds: vec!["normal".to_string()],
            feature_unification: "union-per-full-package-identity-across-resolver-units"
                .to_string(),
        },
        local_packages,
        external_packages,
        workspace_inputs: workspace_inputs(root, &local_projection),
        executor_composition: SourceRoot {
            name: EXECUTOR_PACKAGE.to_string(),
            version: "0.1.0".to_string(),
            root: "services/executor".to_string(),
            features: Vec::new(),
        },
        assets: Vec::new(),
    };
    manifest.assets = discovered_production_include_assets(root, &manifest);
    let requested_fork_revision = manifest
        .workspace_inputs
        .iter()
        .find(|record| {
            record.tag == "dependency/wamn-executor/all-targets/wash-runtime/git-revision"
        })
        .expect("executor wash-runtime edge must record its requested git revision");
    let resolved_fork_revision = manifest
        .external_packages
        .iter()
        .find(|package| package.name == "wash-runtime")
        .and_then(|package| package.revision.as_deref())
        .expect("locked closure must record wash-runtime's resolved revision");
    assert_eq!(
        requested_fork_revision.value, resolved_fork_revision,
        "executor's requested wash-runtime rev must equal the locked commit"
    );
    effect_provider_revision::validate_manifest(&manifest)
        .expect("regenerated provider manifest must be canonical");
    manifest
}

#[test]
fn locked_effect_provider_closure_matches_manifest() {
    let root = repository_root();
    let regenerated = regenerated_manifest(&root);
    let regenerated_bytes = effect_provider_revision::canonical_manifest_bytes(&regenerated)
        .expect("encode regenerated provider manifest");
    let manifest_path = root.join("crates/execution/host/effect-provider-revision.json");
    if std::env::var_os("WAMN_UPDATE_EFFECT_PROVIDER_MANIFEST").is_some() {
        fs::write(&manifest_path, &regenerated_bytes)
            .unwrap_or_else(|error| panic!("write {}: {error}", manifest_path.display()));
    }
    let checked_bytes = fs::read(&manifest_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", manifest_path.display()));
    let checked = effect_provider_revision::parse_manifest(&checked_bytes)
        .expect("checked provider manifest must be strict canonical JSON");
    assert_eq!(
        checked_bytes, regenerated_bytes,
        "locked package-scoped provider closure drifted; regenerate intentionally"
    );
    assert_eq!(checked, regenerated);
    let watched = effect_provider_revision::governed_watch_paths(&checked)
        .expect("derive governed build watch paths");
    assert_eq!(
        watched.len(),
        checked.local_packages.len() + 1 + checked.assets.len(),
        "build.rs must watch every local root, composition root, and asset"
    );
    assert_eq!(
        checked.assets,
        discovered_production_include_assets(&root, &checked),
        "all production include assets outside governed roots must be explicit"
    );

    let local_packages = regenerated
        .local_packages
        .iter()
        .map(|package| package.name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        local_packages,
        BTreeSet::from([
            "wamn-catalog",
            "wamn-component-policy",
            "wamn-control-registry",
            "wamn-event-wire",
            "wamn-execution-host",
            "wamn-flow",
            "wamn-flow-invocation",
            "wamn-pg-core",
            "wamn-run-state",
            "wamn-runner",
            "wamn-runtime",
        ]),
        "the first-party host closure drifted"
    );

    let wash_runtime = regenerated
        .external_packages
        .iter()
        .find(|package| package.name == "wash-runtime")
        .expect("host closure must contain wash-runtime");
    assert_eq!(
        wash_runtime.features,
        vec!["oci".to_string(), "washlet".to_string()],
        "package-scoped executor resolution must not inherit workspace features"
    );
    assert_eq!(
        wash_runtime.source,
        "https://github.com/dkkloimwieder/wasmCloud"
    );
    assert_eq!(
        wash_runtime.revision.as_deref(),
        Some("daba602901507338e99f277e07a8e923c61dc557")
    );
    assert!(wash_runtime.checksum.is_none());
}

#[test]
fn management_and_executor_embed_identical_revision() {
    let root = repository_root();
    let host = fs::read_to_string(root.join("crates/execution/host/src/lib.rs"))
        .expect("read execution-host source");
    let executor = fs::read_to_string(root.join("services/executor/src/lib.rs"))
        .expect("read executor source");
    let authoring = fs::read_to_string(root.join("services/scenario-worker/src/authoring.rs"))
        .expect("read scenario authoring source");

    let instantiate = source_section(
        &host,
        "pub async fn instantiate(",
        "pub fn runtime_revision(&self)",
    );
    let derivation = instantiate
        .find("TrustedExecutionRuntimeRevision::from_flowrunner_bytes(guest)")
        .expect("execution host must derive its revision from exact guest bytes");
    let compilation = instantiate
        .find("WasmtimeComponent::new")
        .expect("execution host must compile the loaded guest");
    assert!(
        derivation < compilation,
        "the trusted revision must be derived before guest compilation"
    );
    assert!(
        instantiate.contains("runtime_revision,"),
        "ExecutionHost must retain the derived revision"
    );

    let executor_instantiation = source_section(
        &executor,
        "let mut executor = ExecutionHost::instantiate(",
        "    .await?;",
    );
    assert!(
        executor_instantiation.contains("&guest,"),
        "executor must pass the exact loaded flowrunner bytes to ExecutionHost"
    );
    assert!(
        !executor_instantiation.contains("effect_provider_revision")
            && !executor_instantiation.contains("host_effect_contract_version"),
        "executor must not supply overridable runtime-revision members"
    );

    let validate_request = source_section(
        &authoring,
        "pub struct ValidateFlowDraft {",
        "/// Exact immutable pins produced by draft validation.",
    );
    assert!(
        !validate_request.contains("effect_provider_revision")
            && !validate_request.contains("host_effect_contract_version")
            && !validate_request.contains("flowrunner_component_digest"),
        "management validation input must not accept runtime-revision members"
    );

    let validation = source_section(
        &authoring,
        "pub(crate) async fn validate_flow_draft(",
        "#[cfg(test)]",
    );
    assert!(
        validation
            .contains("TrustedExecutionRuntimeRevision::from_flowrunner_bytes(flowrunner_bytes)")
            && validation.contains(".execution_runtime_revision()"),
        "management authoring must derive the same host-owned revision from exact bytes"
    );
    assert!(
        !validation.contains("request.bundle")
            && !validation.contains("effect_provider_revision:")
            && !validation.contains("host_effect_contract_version:"),
        "management authoring must not reconstruct or override trusted revision members"
    );
}
