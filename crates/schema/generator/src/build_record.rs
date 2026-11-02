//! `build.json`, the build record of one package build
//! (docs/operations/building.md#wamn-build).
//!
//! The build record names the build's inputs, the exact dependency packages, the
//! generator and toolchain identity, and the digest of every output. Its bytes
//! are canonical JSON (RFC 8785).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use syn::ext::IdentExt as _;
use syn::parse::Parser as _;
use syn::visit::Visit as _;
use wamn_execution_contract::canonical_json_bytes;

use crate::materialize::{GENERATOR_ID, TOOLCHAIN_ID};
use wamn_execution_contract::bytes_sha256;

/// The build record's file name in a package's build output directory.
pub const BUILD_RECORD_FILE: &str = "build.json";
/// The one build record schema this generator writes and reads.
pub const BUILD_RECORD_SCHEMA: &str = "wamn.build/v1";
/// The build-time digest of generator source, KCL templates, and embedded SQL.
pub const GENERATOR_FINGERPRINT: &str = env!("WAMN_GENERATOR_FINGERPRINT");
/// The SQLx version whose describe path derives statement types.
pub(crate) const SQLX_VERSION: &str = "0.9.0";

/// Directories of a package tree that are never build inputs: build output and
/// installed dependencies. `generated` is the old in-tree build output, which
/// left git at 2f7271361; a checkout from before then can still hold it.
const EXCLUDED_DIRECTORIES: &[&str] = &["target", "node_modules", "generated", "dist"];
/// The lock file of the enclosing Cargo workspace, relative to the package root.
const WORKSPACE_LOCK: &str = "../Cargo.lock";
/// The platform trees that a package's components link, relative to the
/// application workspace: the guest crates its generated code calls.
/// A change in any of them changes the inputs
/// of every package, so `wamn build` rebuilds a component that was built
/// against an older interface.
const PLATFORM_INPUTS: &[&str] = &[
    "platform/data",
    "platform/execution/contract",
    "platform/execution/jsonata",
];

/// One package build: `build.json`, schema `wamn.build/v1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildRecord {
    /// Always [`BUILD_RECORD_SCHEMA`].
    pub schema: String,
    pub package: BuildPackage,
    pub inputs: BuildInputs,
    /// The exact base packages, in the order of their dependency names.
    pub dependencies: Vec<BuildDependency>,
    pub generator: BuildGenerator,
    pub outputs: BuildOutputs,
}

/// The package that was built.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildPackage {
    pub id: String,
    pub version: String,
}

/// The build's inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildInputs {
    /// The digest of [`input_digest`]: the files, generator identity, and fingerprint.
    pub digest: String,
    /// The digest without package-local component, data, and web source files.
    pub generation_digest: String,
    /// The `sha256:` digest of each input file, by its path relative to the
    /// package root.
    pub files: BTreeMap<String, String>,
}

/// One base package of an overlay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildDependency {
    pub id: String,
    pub version: String,
    /// The digest of the base's canonical `wamn.json`, until a package
    /// artifact digest replaces it.
    pub digest: String,
}

/// The generator and toolchain identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildGenerator {
    pub id: String,
    pub toolchain: String,
    pub sqlx: String,
}

/// The build's outputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildOutputs {
    /// The digest of the canonical `wamn.json`, hashed once.
    pub manifest: String,
    /// The `sha256:` digest of each output file, by its path relative to the
    /// package's build output directory. `build.json` is not one of them.
    pub files: BTreeMap<String, String>,
    /// The package's components. Empty until the build compiles them.
    pub components: Vec<BuildComponent>,
    /// The digest of the web client build (§7.1 step 8): the canonical JSON
    /// of each built file's `sha256:` digest by its path below `web/`, or
    /// `null` for a package without a web client.
    pub web: Option<String>,
}

/// One built component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildComponent {
    pub name: String,
    pub path: String,
    pub sha256: String,
}

impl BuildRecord {
    /// The build record's canonical bytes.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        canonical_json_bytes(&serde_json::to_value(self).expect("a build record always serializes"))
    }
}

/// The generator identity this build writes.
pub(crate) fn generator() -> BuildGenerator {
    BuildGenerator {
        id: GENERATOR_ID.to_owned(),
        toolchain: TOOLCHAIN_ID.to_owned(),
        sqlx: SQLX_VERSION.to_owned(),
    }
}

/// Read the build record in a package's build output directory.
///
/// # Errors
///
/// When the file is missing, is not a `wamn.build/v1` build record, or is not in
/// canonical form.
pub fn read(package_output: &Path) -> Result<BuildRecord> {
    let path = package_output.join(BUILD_RECORD_FILE);
    let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let build_record: BuildRecord = serde_json::from_slice(&bytes)
        .with_context(|| format!("{} is not a build record", path.display()))?;
    ensure!(
        build_record.schema == BUILD_RECORD_SCHEMA,
        "{} has schema {}, not {BUILD_RECORD_SCHEMA}",
        path.display(),
        build_record.schema
    );
    ensure!(
        build_record.to_bytes() == bytes,
        "{} is not canonical JSON",
        path.display()
    );
    Ok(build_record)
}

/// Read a build whose inputs still match its package tree.
/// `package_root` names the source tree, not its build output directory.
///
/// # Errors
///
/// When the build record is missing or invalid, or the package inputs differ.
pub fn read_current(package_root: &Path, output_dir: &Path) -> Result<BuildRecord> {
    let build_record = read(output_dir).with_context(|| {
        format!(
            "read the build of {}; run wamn build first",
            package_root.display()
        )
    })?;
    let inputs = input_digest(package_root)?;
    ensure!(
        build_record.inputs.digest == inputs,
        "the build of {} is stale: build.json records inputs {}, and the tree is {inputs}; run wamn build again",
        package_root.display(),
        build_record.inputs.digest
    );
    Ok(build_record)
}

/// Refuse workspace build output without a build record that matches its package source.
///
/// Unpacked artifacts and local fixtures keep their own `generated` directory.
/// They do not consume workspace build output.
///
/// # Errors
///
/// When workspace output has no current build record.
pub fn check_current(package_root: &Path) -> Result<()> {
    let output = crate::output_root(package_root);
    if crate::output::is_build_output(&output) {
        if crate::output::is_build_output(package_root) {
            read_current(&source_root(package_root)?, &output)?;
        } else {
            read_current(package_root, &output)?;
        }
    }
    Ok(())
}

/// Resolve a workspace build directory to its package source root.
///
/// # Errors
///
/// When the path does not name `<workspace>/target/wamn/<package>`.
pub fn source_root(output: &Path) -> Result<PathBuf> {
    ensure!(
        crate::output::is_build_output(output),
        "{} is not a workspace package output under target/wamn",
        output.display()
    );
    let workspace = output
        .ancestors()
        .nth(3)
        .filter(|path| !path.as_os_str().is_empty())
        .with_context(|| format!("{} has no package workspace parent", output.display()))?;
    let package = output
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("{} has no UTF-8 package name", output.display()))?;
    Ok(crate::output::dependency_root(
        &workspace.join(package),
        package,
    ))
}

/// The input digest of the package at `package_root`, as `build.json` records it.
///
/// # Errors
///
/// When the package tree cannot be read.
pub fn input_digest(package_root: &Path) -> Result<String> {
    Ok(inputs(package_root)?.digest)
}

/// The generation digest excludes package-local component, data, and web trees.
///
/// # Errors
///
/// When the package inputs cannot be read.
pub fn generation_digest(package_root: &Path) -> Result<String> {
    Ok(inputs(package_root)?.generation_digest)
}

/// The generation input digest without package-local query files.
///
/// # Errors
///
/// When the package inputs cannot be read.
pub fn schema_digest(package_root: &Path) -> Result<String> {
    let mut files = generation_files(&inputs(package_root)?.files);
    files.retain(|path, _| !Path::new(path).starts_with("query"));
    Ok(digest_inputs(&files, &generator(), GENERATOR_FINGERPRINT))
}

/// Read generated output whose generation inputs still match its source.
///
/// # Errors
///
/// When the build record is missing, invalid, or records other generation inputs.
pub fn read_generated(package_root: &Path, output_dir: &Path) -> Result<BuildRecord> {
    let build_record = read(output_dir)?;
    ensure!(
        build_record.inputs.generation_digest == generation_digest(package_root)?,
        "the generated output of {} is stale; run wamn build again",
        package_root.display()
    );
    Ok(build_record)
}

/// The build inputs of the package at `package_root`: every file of the package
/// directory except build output and installed dependencies, the lock file of
/// the enclosing Cargo workspace when there is one, every file of each
/// [`PLATFORM_INPUTS`] tree that exists, the existing WIT paths bound by guests
/// in the enclosing workspace, and the generator identity and fingerprint.
pub(crate) fn inputs(package_root: &Path) -> Result<BuildInputs> {
    let mut files = BTreeMap::new();
    insert_tree(&mut files, package_root, Path::new(""))?;
    // A platform file is keyed by its path relative to the package root, so
    // its key cannot collide with a file of the package.
    let workspace = crate::output::package_workspace(package_root);
    let to_workspace = if package_root.parent() == Some(workspace.as_path()) {
        Path::new("..")
    } else {
        Path::new("../..")
    };
    for tree in PLATFORM_INPUTS {
        let root = workspace.join(tree);
        if root.is_dir() {
            insert_tree(&mut files, &root, &to_workspace.join(tree))?;
        }
    }
    insert_guest_wit(&mut files, &workspace, to_workspace)?;
    let lock = package_root.join(WORKSPACE_LOCK);
    if lock.is_file() {
        let bytes = fs::read(&lock).with_context(|| format!("read {}", lock.display()))?;
        files.insert(WORKSPACE_LOCK.to_owned(), bytes_sha256(&bytes));
    }
    Ok(digests(files, &generator(), GENERATOR_FINGERPRINT))
}

fn digests(
    files: BTreeMap<String, String>,
    generator: &BuildGenerator,
    fingerprint: &str,
) -> BuildInputs {
    BuildInputs {
        digest: digest_inputs(&files, generator, fingerprint),
        generation_digest: digest_inputs(&generation_files(&files), generator, fingerprint),
        files,
    }
}

fn generation_files(files: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    files
        .iter()
        .filter(|(path, _)| {
            !["component", "data", "web"]
                .iter()
                .any(|tree| Path::new(path).starts_with(tree))
        })
        .map(|(path, digest)| (path.clone(), digest.clone()))
        .collect()
}

fn digest_inputs(
    files: &BTreeMap<String, String>,
    generator: &BuildGenerator,
    fingerprint: &str,
) -> String {
    bytes_sha256(&canonical_json_bytes(&serde_json::json!({
        "files": files,
        "generator": generator,
        "generator_fingerprint": fingerprint,
    })))
}

/// Derive shared WIT inputs from bindings, including platform guests and SDKs.
fn insert_guest_wit(
    files: &mut BTreeMap<String, String>,
    workspace: &Path,
    to_workspace: &Path,
) -> Result<()> {
    if !workspace.join("Cargo.toml").is_file() {
        return Ok(());
    }
    let mut roots = BTreeSet::new();
    for relative in input_files(workspace)? {
        if relative
            .extension()
            .is_none_or(|extension| extension != "rs")
        {
            continue;
        }
        let source = workspace.join(relative);
        let text = fs::read_to_string(&source)
            .with_context(|| format!("read guest source {}", source.display()))?;
        if !text.contains("wit_bindgen") {
            continue;
        }
        let mut bindings = WitBindings::default();
        let syntax = syn::parse_file(&text)
            .with_context(|| format!("parse guest source {}", source.display()))?;
        bindings.visit_file(&syntax);
        for binding in bindings.0 {
            let directory = source
                .ancestors()
                .find(|ancestor| ancestor.join("Cargo.toml").is_file())
                .with_context(|| format!("find guest manifest for {}", source.display()))?;
            for path in binding_paths(&binding)
                .with_context(|| format!("read WIT paths in {}", source.display()))?
            {
                // Generated contracts are outputs, tracked through base build records.
                if Path::new(&path).components().any(|component| {
                    EXCLUDED_DIRECTORIES
                        .iter()
                        .any(|excluded| component.as_os_str() == *excluded)
                }) {
                    continue;
                }
                let root = directory.join(path);
                if !root.exists() {
                    continue;
                }
                roots.insert(
                    root.canonicalize()
                        .with_context(|| format!("resolve guest WIT {}", root.display()))?,
                );
            }
        }
    }
    let workspace = workspace
        .canonicalize()
        .context("resolve guest workspace")?;
    for root in roots {
        let mut ancestor = workspace.as_path();
        let mut prefix = to_workspace.to_path_buf();
        while !root.starts_with(ancestor) {
            ancestor = ancestor.parent().context("find shared WIT ancestor")?;
            prefix.push("..");
        }
        let key = prefix.join(root.strip_prefix(ancestor)?);
        if root.is_dir() {
            insert_tree(files, &root, &key)?;
        } else {
            let bytes = fs::read(&root).with_context(|| format!("read {}", root.display()))?;
            files.insert(
                key.to_str()
                    .context("guest WIT path is not UTF-8")?
                    .to_owned(),
                bytes_sha256(&bytes),
            );
        }
    }
    Ok(())
}

#[derive(Default)]
struct WitBindings(Vec<syn::Macro>);

impl<'ast> syn::visit::Visit<'ast> for WitBindings {
    fn visit_macro(&mut self, binding: &'ast syn::Macro) {
        if binding
            .path
            .segments
            .iter()
            .map(|segment| &segment.ident)
            .eq(["wit_bindgen", "generate"])
        {
            self.0.push(binding.clone());
        }
    }
}

fn binding_paths(binding: &syn::Macro) -> syn::Result<Vec<String>> {
    let parser = |input: syn::parse::ParseStream<'_>| {
        let content;
        syn::braced!(content in input);
        let mut paths = None;
        let mut inline = false;
        while !content.is_empty() {
            if content.peek(syn::Ident::peek_any) {
                let name = content.call(syn::Ident::parse_any)?;
                inline |= name == "inline";
                if name == "path" {
                    content.parse::<syn::Token![:]>()?;
                    paths = Some(if content.peek(syn::LitStr) {
                        vec![content.parse::<syn::LitStr>()?.value()]
                    } else {
                        let array;
                        syn::bracketed!(array in content);
                        array
                            .parse_terminated(
                                syn::parse::ParseBuffer::parse::<syn::LitStr>,
                                syn::Token![,],
                            )?
                            .iter()
                            .map(syn::LitStr::value)
                            .collect()
                    });
                }
            } else {
                // Skip one token tree so other binding options stay opaque.
                content.step(|cursor| {
                    let (_, next) = cursor.token_tree().expect("binding content is not empty");
                    Ok(((), next))
                })?;
            }
        }
        Ok(paths.unwrap_or_else(|| {
            if inline {
                Vec::new()
            } else {
                vec!["wit".into()]
            }
        }))
    };
    parser.parse2(binding.tokens.clone())
}

/// Insert the digest of every input file below `root`, keyed by `prefix`
/// joined with the file's path relative to `root`.
fn insert_tree(files: &mut BTreeMap<String, String>, root: &Path, prefix: &Path) -> Result<()> {
    for relative in input_files(root)? {
        let key = prefix.join(&relative);
        let key = key
            .to_str()
            .with_context(|| format!("input path is not UTF-8: {}", key.display()))?
            .to_owned();
        let path = root.join(&relative);
        let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        files.insert(key, bytes_sha256(&bytes));
    }
    Ok(())
}

/// Whether a path belongs to the source tree, excluding generated output and dependencies.
///
/// The path can name a new or deleted file or directory.
#[must_use]
pub fn is_input_path(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root).is_ok_and(|relative| {
        relative.components().all(|component| {
            !EXCLUDED_DIRECTORIES
                .iter()
                .any(|excluded| component.as_os_str() == *excluded)
        })
    })
}

/// The input files below `package_root`, sorted, as paths relative to it.
fn input_files(package_root: &Path) -> Result<Vec<PathBuf>> {
    let mut pending = vec![PathBuf::new()];
    let mut files = Vec::new();
    while let Some(relative) = pending.pop() {
        let directory = package_root.join(&relative);
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("read input directory {}", directory.display()))?
        {
            let entry = entry
                .with_context(|| format!("enumerate input directory {}", directory.display()))?;
            let path = relative.join(entry.file_name());
            if !is_input_path(package_root, &entry.path()) {
                continue;
            }
            let file_type = entry
                .file_type()
                .with_context(|| format!("inspect input {}", entry.path().display()))?;
            if file_type.is_dir() {
                pending.push(path);
            } else {
                ensure!(
                    file_type.is_file(),
                    "a build input is not a regular file: {}",
                    entry.path().display()
                );
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_root_refuses_short_paths_and_resolves_nested_packages() {
        for path in ["", "/", "package", "wamn/package", "target/wamn/package"] {
            assert!(source_root(Path::new(path)).is_err(), "{path}");
        }
        let scratch = scratch("source-root");
        let package = scratch.0.join("apps/platform/package");
        fs::create_dir_all(&package).unwrap();
        assert_eq!(
            source_root(&scratch.0.join("apps/target/wamn/package")).unwrap(),
            package
        );
        let package = scratch.0.join("apps/package");
        fs::create_dir_all(&package).unwrap();
        assert_eq!(
            source_root(&scratch.0.join("apps/target/wamn/package")).unwrap(),
            package
        );
    }

    #[test]
    fn read_current_requires_matching_inputs_and_a_build_record() {
        let scratch = scratch("current");
        let package = scratch.0.join("apps/package");
        let output = crate::output_root(&package);
        fs::create_dir_all(&package).unwrap();
        fs::create_dir_all(&output).unwrap();
        fs::write(package.join("wamn.k"), "original").unwrap();
        let missing = read_current(&package, &output).unwrap_err();
        assert!(format!("{missing:#}").contains("build.json"));
        let build_record = BuildRecord {
            schema: BUILD_RECORD_SCHEMA.to_owned(),
            package: BuildPackage {
                id: "package".to_owned(),
                version: "1.0.0".to_owned(),
            },
            inputs: inputs(&package).unwrap(),
            dependencies: Vec::new(),
            generator: generator(),
            outputs: BuildOutputs {
                manifest: bytes_sha256(b"manifest"),
                files: BTreeMap::new(),
                components: Vec::new(),
                web: None,
            },
        };
        fs::write(output.join(BUILD_RECORD_FILE), build_record.to_bytes()).unwrap();
        assert_eq!(read_current(&package, &output).unwrap(), build_record);
        check_current(&package).unwrap();
        check_current(&output).unwrap();
        fs::write(package.join("wamn.k"), "edited").unwrap();
        let error = read_current(&package, &output).unwrap_err().to_string();
        for detail in [
            package.to_str().unwrap(),
            &build_record.inputs.digest,
            &input_digest(&package).unwrap(),
            "wamn build",
        ] {
            assert!(error.contains(detail), "{error}");
        }
        fs::remove_file(package.join("wamn.k")).unwrap();
        let error = check_current(&package).unwrap_err().to_string();
        assert!(error.contains("is stale"), "{error}");
    }

    #[test]
    fn generator_changes_invalidate_both_digests() {
        let files = BTreeMap::from([
            ("wamn.k".to_owned(), bytes_sha256(b"package")),
            ("component/src/lib.rs".to_owned(), bytes_sha256(b"source")),
        ]);
        let generator = generator();
        let before = digests(files.clone(), &generator, "old templates");
        let mut changed = generator.clone();
        changed.id.push_str("-changed");
        for after in [
            digests(files.clone(), &generator, "new templates"),
            digests(files, &changed, "old templates"),
        ] {
            assert_ne!(before.digest, after.digest);
            assert_ne!(before.generation_digest, after.generation_digest);
        }
    }

    #[test]
    fn package_sources_change_only_the_full_digest() {
        let scratch = scratch("source-digests");
        let package = scratch.0.join("package");
        fs::write(package.join("wamn.k"), "manifest").unwrap();
        for relative in ["component/src/lib.rs", "data/src/lib.rs", "web/src/main.ts"] {
            let path = package.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let before = inputs(&package).unwrap();
            fs::write(&path, "original").unwrap();
            let added = inputs(&package).unwrap();
            assert_ne!(before.digest, added.digest, "{relative}");
            assert_eq!(
                before.generation_digest, added.generation_digest,
                "{relative}"
            );
            fs::write(&path, "changed").unwrap();
            let edited = inputs(&package).unwrap();
            assert_ne!(added.digest, edited.digest, "{relative}");
            assert_eq!(
                added.generation_digest, edited.generation_digest,
                "{relative}"
            );
        }
    }

    #[test]
    fn generation_inputs_change_both_digests() {
        let scratch = scratch("generation-digests");
        let package = scratch.0.join("package");
        for relative in [
            "wamn.k",
            "query/list.sql",
            "migrations/0001_initial.sql",
            "../Cargo.lock",
        ] {
            let path = package.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "original").unwrap();
            let before = inputs(&package).unwrap();
            fs::write(&path, "changed").unwrap();
            let after = inputs(&package).unwrap();
            assert_ne!(before.digest, after.digest, "{relative}");
            assert_ne!(
                before.generation_digest, after.generation_digest,
                "{relative}"
            );
        }
    }

    #[test]
    fn query_edits_keep_the_schema_digest_and_schema_edits_change_it() {
        let scratch = scratch("schema-digest");
        let package = scratch.0.join("package");
        for relative in [
            "query/location.sql",
            "migrations/0001_initial.sql",
            "wamn.k",
        ] {
            let path = package.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "original").unwrap();
            let generation = generation_digest(&package).unwrap();
            let schema = schema_digest(&package).unwrap();
            fs::write(&path, "changed").unwrap();
            assert_ne!(
                generation,
                generation_digest(&package).unwrap(),
                "{relative}"
            );
            if relative.starts_with("query/") {
                assert_eq!(schema, schema_digest(&package).unwrap(), "{relative}");
            } else {
                assert_ne!(schema, schema_digest(&package).unwrap(), "{relative}");
            }
        }
    }

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("remove the scratch package");
        }
    }

    fn scratch(name: &str) -> Scratch {
        let root = std::env::temp_dir().join(format!(
            "wamn-schema-generator-build-record-{}-{name}",
            std::process::id()
        ));
        if root.exists() {
            fs::remove_dir_all(&root).expect("remove a stale scratch package");
        }
        fs::create_dir_all(root.join("package")).expect("create the scratch package");
        Scratch(root)
    }

    #[test]
    fn inputs_leave_out_build_output_and_installed_dependencies() {
        let scratch = scratch("inputs");
        let package = scratch.0.join("package");
        for (path, bytes) in [
            ("wamn.k", "a"),
            ("command/move/lock.sql", "b"),
            ("web/src/main.ts", "c"),
            ("web/dist/index.html", "d"),
            ("web/node_modules/x/index.js", "e"),
            ("target/wamn/out", "f"),
            ("generated/wamn.json", "g"),
        ] {
            let path = package.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        fs::write(scratch.0.join("Cargo.lock"), "lock").unwrap();

        let inputs = inputs(&package).expect("read the inputs");
        assert_eq!(
            inputs.files.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "../Cargo.lock",
                "command/move/lock.sql",
                "wamn.k",
                "web/src/main.ts"
            ]
        );
        assert_eq!(input_digest(&package).unwrap(), inputs.digest);

        fs::write(package.join("generated/wamn.json"), "changed").unwrap();
        fs::write(package.join("web/dist/index.html"), "changed").unwrap();
        assert_eq!(input_digest(&package).unwrap(), inputs.digest);
        fs::write(package.join("wamn.k"), "changed").unwrap();
        assert_ne!(input_digest(&package).unwrap(), inputs.digest);
    }

    #[test]
    fn a_platform_interface_change_makes_every_package_stale() {
        let scratch = scratch("platform");
        let apps = scratch.0.join("apps");
        let wit = scratch
            .0
            .join("crates/platform/runtime/wit/deps/wamn-postgres-0.4/package.wit");
        let sdk = apps.join("platform/data/postgres-statements/src/lib.rs");
        let bindings = r#"wit_bindgen::generate!({
            path: "../../../../crates/platform/runtime/wit/deps/wamn-postgres-0.4",
        });"#;
        for (path, bytes) in [
            (apps.join("Cargo.toml"), ""),
            (apps.join("receiving/wamn.k"), "a"),
            (apps.join("platform/nested/wamn.k"), "b"),
            (wit.clone(), "interface statements {}"),
            (sdk.clone(), bindings),
            (
                sdk.parent().unwrap().parent().unwrap().join("Cargo.toml"),
                "",
            ),
        ] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        let top = apps.join("receiving");
        let nested = apps.join("platform/nested");

        let top_inputs = inputs(&top).expect("read the inputs");
        assert!(
            top_inputs.files.contains_key(
                "../../crates/platform/runtime/wit/deps/wamn-postgres-0.4/package.wit"
            ),
            "{:?}",
            top_inputs.files.keys()
        );
        assert!(
            top_inputs
                .files
                .contains_key("../platform/data/postgres-statements/src/lib.rs")
        );
        assert!(
            inputs(&nested)
                .unwrap()
                .files
                .contains_key("../../platform/data/postgres-statements/src/lib.rs")
        );

        let generation_before = [
            generation_digest(&top).unwrap(),
            generation_digest(&nested).unwrap(),
        ];
        let before = [input_digest(&top).unwrap(), input_digest(&nested).unwrap()];
        fs::write(&wit, "interface statements { next-number: func(); }").unwrap();
        let after_wit = [input_digest(&top).unwrap(), input_digest(&nested).unwrap()];
        assert_ne!(before[0], after_wit[0]);
        assert_ne!(before[1], after_wit[1]);
        let generation_after_wit = [
            generation_digest(&top).unwrap(),
            generation_digest(&nested).unwrap(),
        ];
        assert_ne!(generation_before[0], generation_after_wit[0]);
        assert_ne!(generation_before[1], generation_after_wit[1]);
        fs::write(&sdk, format!("{bindings}\npub fn next_number() {{}}")).unwrap();
        assert_ne!(input_digest(&top).unwrap(), after_wit[0]);
        assert_ne!(input_digest(&nested).unwrap(), after_wit[1]);
        assert_ne!(generation_digest(&top).unwrap(), generation_after_wit[0]);
        assert_ne!(generation_digest(&nested).unwrap(), generation_after_wit[1]);
    }

    #[test]
    fn imported_wit_changes_the_key_but_unrelated_files_do_not() {
        let scratch = scratch("bound-wit");
        let apps = scratch.0.join("apps");
        let package = apps.join("rest_connector");
        let nested = apps.join("platform/nested");
        let imported = [
            "crates/execution/workflow/router/wit/package.wit",
            "crates/execution/host/wit/deps/delivery/package.wit",
            "crates/platform/component-composer/wit/package.wit",
            "crates/new-provider/wit/deps/imported/package.wit",
            "apps/platform/guest/wit/deps/shared/package.wit",
        ];
        for (path, text) in [
            ("apps/Cargo.toml", ""),
            ("apps/rest_connector/wamn.k", "a"),
            ("apps/platform/nested/wamn.k", "b"),
            ("apps/rest_connector/component/Cargo.toml", ""),
            (
                "apps/rest_connector/component/src/lib.rs",
                r#"wit_bindgen::generate!({
                    path: [
                        "../../../crates/execution/workflow/router/wit",
                        "../../../crates/execution/host/wit/deps/delivery",
                        "../../target/wamn/rest_connector/wit",
                    ],
                });
                wit_bindgen::generate!({ inline: "package test:inline; world inline {}" });"#,
            ),
            ("apps/platform/guest/Cargo.toml", ""),
            (
                "apps/platform/guest/src/bindings.rs",
                r##"mod bindings {
                    wit_bindgen::generate!({ path: r#"../../../crates/new-provider/wit"# });
                    wit_bindgen::generate!({ path: "../../../crates/platform/component-composer/wit/package.wit" });
                    wit_bindgen::generate!({ world: "shared" });
                }"##,
            ),
        ] {
            let path = scratch.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        for path in imported {
            let path = scratch.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "package test:shared; interface shared {}").unwrap();
        }
        let mut before = [
            input_digest(&package).unwrap(),
            input_digest(&nested).unwrap(),
        ];
        // Neither an unbound interface nor another guest's source is an input.
        for path in [
            "crates/new-provider/unrelated.wit",
            "apps/platform/guest/src/unrelated.rs",
            "apps/target/wamn/rest_connector/wit/package.wit",
        ] {
            let path = scratch.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "unrelated").unwrap();
        }
        assert_eq!(input_digest(&package).unwrap(), before[0]);
        assert_eq!(input_digest(&nested).unwrap(), before[1]);
        for path in imported {
            fs::write(
                scratch.0.join(path),
                "package test:shared; interface shared { run: func(); }",
            )
            .unwrap();
            let after = [
                input_digest(&package).unwrap(),
                input_digest(&nested).unwrap(),
            ];
            assert_ne!(before[0], after[0], "{path}");
            assert_ne!(before[1], after[1], "{path}");
            before = after;
        }
    }

    #[test]
    fn missing_bound_wit_is_omitted_until_it_exists() {
        let scratch = scratch("missing-bound-wit");
        let apps = scratch.0.join("apps");
        let package = apps.join("package");
        for (path, text) in [
            ("Cargo.toml", ""),
            ("package/wamn.k", "a"),
            ("package/component/Cargo.toml", ""),
            (
                "package/component/src/lib.rs",
                r#"wit_bindgen::generate!({ path: "../../../crates/shared/wit" });"#,
            ),
        ] {
            let path = apps.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let before = inputs(&package).expect("read inputs without the bound WIT");
        assert_eq!(
            before.files.keys().map(String::as_str).collect::<Vec<_>>(),
            ["component/Cargo.toml", "component/src/lib.rs", "wamn.k"]
        );

        let wit = scratch.0.join("crates/shared/wit/package.wit");
        fs::create_dir_all(wit.parent().unwrap()).unwrap();
        fs::write(&wit, "package test:shared; interface shared {}").unwrap();
        let after = inputs(&package).expect("read inputs with the bound WIT");
        assert!(
            after
                .files
                .contains_key("../../crates/shared/wit/package.wit")
        );
        assert_ne!(before.digest, after.digest);
    }

    #[test]
    fn repository_guests_contribute_their_shared_wit_inputs() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let inputs = inputs(&repository.join("apps/rest_connector")).unwrap();
        for path in [
            "../../crates/execution/workflow/router/wit/package.wit",
            "../../crates/execution/host/wit/deps/wamn-router-delivery-0.4/package.wit",
            "../../crates/platform/component-composer/wit/package.wit",
        ] {
            assert!(inputs.files.contains_key(path), "missing {path}");
        }
    }

    #[test]
    fn read_refuses_a_build_record_that_is_not_canonical() {
        let scratch = scratch("read");
        let build_record = BuildRecord {
            schema: BUILD_RECORD_SCHEMA.to_owned(),
            package: BuildPackage {
                id: "p".to_owned(),
                version: "1.0.0".to_owned(),
            },
            inputs: BuildInputs {
                digest: bytes_sha256(b"i"),
                generation_digest: bytes_sha256(b"g"),
                files: BTreeMap::new(),
            },
            dependencies: Vec::new(),
            generator: generator(),
            outputs: BuildOutputs {
                manifest: bytes_sha256(b"m"),
                files: BTreeMap::new(),
                components: Vec::new(),
                web: None,
            },
        };
        let bytes = build_record.to_bytes();
        assert!(
            String::from_utf8(bytes.clone())
                .unwrap()
                .contains(r#""web":null"#)
        );
        fs::write(scratch.0.join(BUILD_RECORD_FILE), &bytes).unwrap();
        assert_eq!(read(&scratch.0).unwrap(), build_record);

        let mut pretty = serde_json::to_vec_pretty(&build_record).unwrap();
        pretty.push(b'\n');
        fs::write(scratch.0.join(BUILD_RECORD_FILE), pretty).unwrap();
        let error = read(&scratch.0).expect_err("a pretty build record was read");
        assert!(error.to_string().contains("not canonical"), "{error:#}");
    }
}
