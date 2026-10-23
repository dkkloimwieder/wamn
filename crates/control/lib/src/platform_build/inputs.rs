//! Input hashes of the platform build targets (wamn-vcpvf).
//!
//! An input hash covers the Dockerfile, the build arguments and every
//! build-context file that can change the bytes of one target. A target whose
//! input hash a pushed tag already carries is copied instead of built.
//!
//! The file set of a target is the directory of each local Cargo package in the
//! dependency closure of its binaries, every file those packages read at
//! compile time from outside their directory, every Cargo manifest and lock
//! file, and the paths that its stages copy whole. Only files that a `COPY`
//! from the build context can reach, and that `.dockerignore` keeps, count.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, bail, ensure};
use serde_json::{Value, json};
use wamn_execution_contract::canonical_json_bytes;

/// The guests that the component-builder stage compiles in one invocation.
const COMPONENTS: &[&str] = &[
    "http-route",
    "materializer",
    "busyloop",
    "connection-http-standard",
    "sockprobe",
];

/// The build-context paths that a `COPY` without `--from` names in the
/// Dockerfile. A file outside them never reaches an image.
pub(super) const COPIED: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    ".cargo/config.toml",
    "crates",
    "vendor",
    "apps",
    "services",
    "test-support",
    "tests",
    "tools",
    "deploy",
];

/// Files that every target hashes besides its own inputs. The manifests
/// matter to every target because one Cargo invocation unifies the features
/// of all the packages it builds.
const COMMON: &[&str] = &[
    "Dockerfile",
    ".dockerignore",
    "Cargo.toml",
    "Cargo.lock",
    ".cargo/config.toml",
    "apps/Cargo.toml",
    "apps/Cargo.lock",
];

/// The macros whose string arguments name files that rustc reads.
const READERS: &[&str] = &[
    "include_str!",
    "include_bytes!",
    "include!",
    "#[path",
    "bindgen!",
    "generate!",
];

/// What one Dockerfile target is built from.
struct Declared {
    /// Root-workspace packages whose binaries the target carries.
    native: &'static [&'static str],
    /// Root-workspace packages whose test executables the target carries.
    cases: &'static [&'static str],
    /// Packages of the `apps` workspace that the target's stages compile.
    components: &'static [&'static str],
    /// Context paths that the target's stages copy whole.
    paths: &'static [&'static str],
    /// Paths below `paths` that the target's stages remove again.
    dropped: &'static [&'static str],
    /// The target reads the `apps-target` named context.
    apps_target: bool,
}

fn declared(target: &str) -> anyhow::Result<Declared> {
    let none: &[&str] = &[];
    Ok(match target {
        "host" => Declared {
            native: &["wamn-host", "wamn-runtime"],
            cases: none,
            components: none,
            paths: none,
            dropped: none,
            apps_target: false,
        },
        "identity" => Declared {
            native: &["wamn-identity"],
            cases: none,
            components: none,
            paths: none,
            dropped: none,
            apps_target: false,
        },
        "edge-web" => Declared {
            native: &["wamn-edge"],
            cases: none,
            components: none,
            paths: none,
            dropped: none,
            apps_target: false,
        },
        "gates" => Declared {
            native: &[
                "wamn-host",
                "wamn-runtime",
                "wamn-gates",
                "wamn-ctl",
                "wamn-cdc-reader",
            ],
            cases: none,
            components: COMPONENTS,
            // The component builder runs the guests with these flags.
            paths: &["tools/guest-rustflags"],
            dropped: none,
            apps_target: false,
        },
        "runner" => Declared {
            native: &["wamn-ctl", "wamn-scenario-worker"],
            cases: &["wamn-wms-tests", "wamn-receiving-tests"],
            components: COMPONENTS,
            paths: &["apps", "deploy", "tools"],
            // The `runner-deploy` stage removes the release chart, whose
            // version is the platform version (wamn-6qo7v).
            dropped: &["deploy/platform/release"],
            apps_target: true,
        },
        _ => bail!("{target} is not a platform build target"),
    })
}

/// One local package of a Cargo workspace.
#[derive(Debug)]
struct Package {
    directory: PathBuf,
    /// Target source files, which may lie outside the package directory.
    sources: Vec<PathBuf>,
    /// A proc macro, or a build script that does not declare what it reads,
    /// can read any file.
    opaque: bool,
    /// The package-relative paths that its build script declares with
    /// `cargo:rerun-if-changed`.
    reads: Vec<String>,
    /// Local dependencies, each with whether it is a dev dependency only.
    dependencies: Vec<(String, bool)>,
}

/// The local packages of one Cargo workspace, from `cargo metadata`.
#[derive(Debug)]
pub(super) struct Graph {
    packages: BTreeMap<String, Package>,
    names: BTreeMap<String, String>,
}

/// Read the output of `cargo metadata --format-version 1`.
pub(super) fn parse_metadata(metadata: &Value) -> anyhow::Result<Graph> {
    let text = |value: &Value| value.as_str().map(str::to_owned);
    let mut packages = BTreeMap::new();
    let mut names = BTreeMap::new();
    for package in metadata["packages"]
        .as_array()
        .context("metadata packages")?
    {
        if !package["source"].is_null() {
            continue;
        }
        let id = text(&package["id"]).context("a package id")?;
        let manifest = PathBuf::from(text(&package["manifest_path"]).context("a manifest path")?);
        let mut sources = Vec::new();
        let mut opaque = false;
        let mut reads = Vec::new();
        for target in package["targets"].as_array().context("package targets")? {
            let source = PathBuf::from(text(&target["src_path"]).context("a target source")?);
            let kinds = target["kind"].as_array().into_iter().flatten();
            for kind in kinds {
                if kind == "proc-macro" {
                    opaque = true;
                } else if kind == "custom-build" {
                    let script = std::fs::read_to_string(&source)
                        .with_context(|| format!("read {}", source.display()))?;
                    match declared_reads(&script) {
                        Some(declared) => reads.extend(declared),
                        None => opaque = true,
                    }
                }
            }
            sources.push(source);
        }
        names.insert(
            text(&package["name"]).context("a package name")?,
            id.clone(),
        );
        packages.insert(
            id,
            Package {
                directory: manifest.parent().context("a manifest directory")?.into(),
                sources,
                opaque,
                reads,
                dependencies: Vec::new(),
            },
        );
    }
    for node in metadata["resolve"]["nodes"]
        .as_array()
        .context("metadata resolve nodes")?
    {
        let Some(package) = node["id"]
            .as_str()
            .and_then(|id| packages.contains_key(id).then(|| id.to_owned()))
        else {
            continue;
        };
        let mut dependencies = Vec::new();
        for dependency in node["deps"].as_array().context("node dependencies")? {
            let id = text(&dependency["pkg"]).context("a dependency id")?;
            if !packages.contains_key(&id) {
                continue;
            }
            let dev = dependency["dep_kinds"]
                .as_array()
                .context("dependency kinds")?
                .iter()
                .all(|kind| kind["kind"] == "dev");
            dependencies.push((id, dev));
        }
        if let Some(package) = packages.get_mut(&package) {
            package.dependencies = dependencies;
        }
    }
    Ok(Graph { packages, names })
}

/// The paths that a build script declares with `cargo:rerun-if-changed`, or
/// `None` when it declares none or builds one at run time. Cargo runs the
/// script again only when a declared path changes, so the script reads no
/// other file outside its package.
fn declared_reads(script: &str) -> Option<Vec<String>> {
    let mut reads = Vec::new();
    for (start, directive) in script.match_indices("rerun-if-changed=") {
        let rest = &script[start + directive.len()..];
        let path = &rest[..rest.find('"')?];
        if path.is_empty() || path.contains(['{', '}', '\\']) {
            return None;
        }
        reads.push(path.to_owned());
    }
    (!reads.is_empty()).then_some(reads)
}

/// The packages that building `roots` compiles, with the dev dependencies of
/// `cases` because their test executables need them.
fn closure<'a>(
    graph: &'a Graph,
    roots: &[&str],
    cases: &[&str],
) -> anyhow::Result<Vec<&'a Package>> {
    let mut stack = Vec::new();
    for (names, dev) in [(roots, false), (cases, true)] {
        for name in names {
            let id = graph
                .names
                .get(*name)
                .with_context(|| format!("cargo metadata has no package {name}"))?;
            stack.push((id, dev));
        }
    }
    let mut seen = BTreeSet::new();
    let mut packages = Vec::new();
    while let Some((id, dev)) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let package = &graph.packages[id];
        for (dependency, dev_only) in &package.dependencies {
            if dev || !dev_only {
                stack.push((dependency, false));
            }
        }
        packages.push(package);
    }
    Ok(packages)
}

/// `relative` joined to the repository-relative `base`, or `None` when it
/// leaves the repository.
fn resolve(base: &Path, relative: &str) -> Option<PathBuf> {
    let joined = base.join(relative);
    let mut path = Vec::new();
    for component in joined.components() {
        match component {
            Component::Normal(part) => path.push(part),
            Component::ParentDir => {
                path.pop()?;
            }
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(path.iter().collect())
}

/// The string literals in `text`, without unescaping.
fn literals(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut start = None;
    let mut escaped = false;
    for (index, character) in text.char_indices() {
        match (start, character) {
            (Some(_), _) if escaped => escaped = false,
            (Some(_), '\\') => escaped = true,
            (Some(open), '"') => {
                found.push(&text[open..index]);
                start = None;
            }
            (None, '"') => start = Some(index + 1),
            _ => {}
        }
    }
    found
}

/// The invocation that starts at `text`: up to the `]` of an attribute, or
/// to the delimiter that closes the macro's first delimiter.
fn invocation(text: &str) -> &str {
    let mut depth = 0_usize;
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in text.char_indices() {
        if quoted {
            match character {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => quoted = false,
                _ => {}
            }
            continue;
        }
        match character {
            '"' => quoted = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return &text[..=index];
                }
            }
            _ => {}
        }
    }
    text
}

/// The repository-relative paths outside `directory` that its Rust sources
/// name in a file-reading macro, resolved against the source file and the
/// package directory. A literal that names no existing path is not a file
/// rustc reads, because such a build fails.
fn references(repository: &Path, directory: &Path) -> anyhow::Result<BTreeSet<PathBuf>> {
    let mut found = BTreeSet::new();
    let mut pending = vec![directory.to_owned()];
    while let Some(folder) = pending.pop() {
        for entry in std::fs::read_dir(repository.join(&folder))
            .with_context(|| format!("read {}", folder.display()))?
        {
            let entry = entry?;
            let path = folder.join(entry.file_name());
            let kind = entry.file_type()?;
            if kind.is_dir() && entry.file_name() != "target" {
                pending.push(path);
                continue;
            }
            if !kind.is_file() || path.extension().is_none_or(|extension| extension != "rs") {
                continue;
            }
            let text =
                String::from_utf8_lossy(&std::fs::read(repository.join(&path))?).into_owned();
            for reader in READERS {
                for (start, _) in text.match_indices(reader) {
                    for literal in literals(invocation(&text[start..])) {
                        let literal = literal.trim_start_matches('/');
                        for base in [path.parent().unwrap_or(directory), directory] {
                            let Some(resolved) = resolve(base, literal) else {
                                continue;
                            };
                            if !resolved.starts_with(directory)
                                && std::fs::symlink_metadata(repository.join(&resolved)).is_ok()
                            {
                                found.insert(resolved);
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(found)
}

/// The root-relative paths `.dockerignore` excludes. Patterns are plain
/// paths, so a glob is refused rather than misread.
fn ignored(repository: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let text =
        std::fs::read_to_string(repository.join(".dockerignore")).context("read .dockerignore")?;
    let mut paths = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        ensure!(
            !line.contains(['*', '?', '[', '!']),
            "the input hash reads .dockerignore paths only, not the pattern {line}"
        );
        paths.push(PathBuf::from(line.trim_start_matches('/')));
    }
    Ok(paths)
}

/// Record each file below `relative` with its digest and mode, or the target
/// of a symbolic link. `keep` says which paths count.
fn walk(
    repository: &Path,
    relative: &Path,
    keep: &dyn Fn(&Path) -> Option<bool>,
    files: &mut BTreeMap<String, String>,
) -> anyhow::Result<()> {
    // `None` drops the path, `Some(false)` descends without recording it.
    let Some(recorded) = keep(relative) else {
        return Ok(());
    };
    let path = repository.join(relative);
    let metadata =
        std::fs::symlink_metadata(&path).with_context(|| format!("read {}", path.display()))?;
    let key = || {
        relative
            .to_str()
            .map(str::to_owned)
            .with_context(|| format!("{} is not UTF-8", relative.display()))
    };
    if metadata.is_dir() {
        for entry in std::fs::read_dir(&path)? {
            walk(repository, &relative.join(entry?.file_name()), keep, files)?;
        }
    } else if recorded && metadata.is_symlink() {
        let target = std::fs::read_link(&path)?;
        files.insert(key()?, format!("symlink {}", target.display()));
    } else if recorded {
        let mode = metadata.permissions().mode() & 0o7777;
        files.insert(
            key()?,
            format!("{} {mode:o}", super::sha256(&std::fs::read(&path)?)),
        );
    }
    Ok(())
}

/// Every file of `target`'s inputs, each with its digest and mode.
pub(super) fn input_files(
    repository: &Path,
    target: &str,
    root: &Graph,
    apps: &Graph,
) -> anyhow::Result<BTreeMap<String, String>> {
    let declared = declared(target)?;
    // cargo metadata reports canonical paths.
    let repository = &std::fs::canonicalize(repository).context("find the checkout")?;
    let relative = |path: &Path| {
        path.strip_prefix(repository)
            .map(Path::to_path_buf)
            .with_context(|| format!("{} lies outside the checkout", path.display()))
    };
    let mut paths: BTreeSet<PathBuf> = COMMON.iter().map(PathBuf::from).collect();
    for graph in [root, apps] {
        for package in graph.packages.values() {
            paths.insert(relative(&package.directory)?.join("Cargo.toml"));
        }
    }
    let mut packages = closure(root, declared.native, declared.cases)?;
    packages.extend(closure(apps, declared.components, &[])?);
    for package in packages {
        let directory = relative(&package.directory)?;
        if package.opaque {
            // A proc macro or an undeclared build script may read any copied file.
            paths.insert(PathBuf::new());
        }
        paths.extend(
            package
                .reads
                .iter()
                .filter_map(|read| resolve(&directory, read)),
        );
        for source in &package.sources {
            let source = relative(source)?;
            if !source.starts_with(&directory) {
                paths.insert(source.parent().map(Path::to_path_buf).unwrap_or_default());
            }
        }
        paths.extend(references(repository, &directory)?);
        paths.insert(directory);
    }
    let ignored = ignored(repository)?;
    let copied: Vec<&Path> = COPIED.iter().chain(COMMON).map(Path::new).collect();
    let keep = |path: &Path| {
        if ignored.iter().any(|ignored| path.starts_with(ignored)) {
            None
        } else if copied.iter().any(|copied| path.starts_with(copied)) {
            Some(true)
        } else if copied.iter().any(|copied| copied.starts_with(path)) {
            Some(false)
        } else {
            None
        }
    };
    let mut files = BTreeMap::new();
    for path in &paths {
        walk(repository, path, &keep, &mut files)?;
    }
    let copied_whole = |path: &Path| {
        if declared
            .dropped
            .iter()
            .any(|dropped| path.starts_with(dropped))
        {
            None
        } else {
            keep(path)
        }
    };
    for path in declared.paths {
        walk(repository, Path::new(path), &copied_whole, &mut files)?;
    }
    if declared.apps_target {
        let context = Path::new("apps/target/wamn");
        ensure!(
            repository.join(context).is_dir(),
            "{} does not exist; run `wamn build` on the apps first",
            context.display()
        );
        let all = |_: &Path| Some(true);
        let mut context_files = BTreeMap::new();
        walk(repository, context, &all, &mut context_files)?;
        for (name, digest) in context_files {
            let name = name.strip_prefix("apps/target/wamn/").unwrap_or(&name);
            files.insert(format!("apps-target:{name}"), digest);
        }
    }
    Ok(files)
}

/// The hex SHA-256 of `target`'s build arguments and input files.
pub(super) fn input_hash(target: &str, files: &BTreeMap<String, String>) -> String {
    let document = json!({
        "format": "wamn-input-hash/1",
        "target": target,
        // The checkout key names cache mounts only, so it is left out.
        "arguments": super::docker_arguments("", "Dockerfile", "apps-target"),
        "labels": ["wamn.dev/build-profile=release"],
        "files": files,
    });
    let digest = super::sha256(&canonical_json_bytes(&document));
    digest.trim_start_matches("sha256:").to_owned()
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// One fake package: name, directory, `src/lib.rs`, and its local
    /// dependencies with their kind.
    type Fake = (
        &'static str,
        &'static str,
        &'static str,
        &'static [(&'static str, Option<&'static str>)],
    );

    const ROOT: &[Fake] = &[
        (
            "wamn-host",
            "services/host",
            "",
            &[("wamn-run-state", None), ("serde", None)],
        ),
        ("wamn-runtime", "crates/runtime", "", &[]),
        (
            "wamn-run-state",
            "crates/run-state",
            "const SQL: &str = include_str!(\"../../../deploy/sql/run.sql\");",
            &[],
        ),
        (
            "wamn-identity",
            "services/identity",
            "",
            &[("wamn-run-state", None)],
        ),
        ("wamn-edge", "services/edge", "", &[]),
        (
            "wamn-gates",
            "tests/orchestrator",
            "",
            &[("wamn-control", None)],
        ),
        ("wamn-ctl", "services/ctl", "", &[("wamn-control", None)]),
        (
            "wamn-control",
            "crates/control",
            "const VALUES: &str = include_str!(\"../../../deploy/platform/release/values.yaml\");",
            &[("wamn-test-infrastructure", Some("dev"))],
        ),
        ("wamn-cdc-reader", "services/cdc-reader", "", &[]),
        ("wamn-scenario-worker", "services/scenario-worker", "", &[]),
        (
            "wamn-wms-tests",
            "apps/wamn_wms/tests",
            "",
            &[("wamn-test-infrastructure", Some("dev"))],
        ),
        ("wamn-receiving-tests", "apps/wamn_receiving/tests", "", &[]),
        (
            "wamn-test-infrastructure",
            "test-support/infrastructure",
            "",
            &[],
        ),
        ("wamn-component-composer", "crates/composer", "", &[]),
    ];

    const APPS: &[Fake] = &[
        ("http-route", "apps/platform/http-route", "", &[]),
        ("materializer", "apps/platform/materializer", "", &[]),
        ("busyloop", "apps/platform/busyloop", "", &[]),
        (
            "connection-http-standard",
            "apps/platform/connection",
            "",
            &[],
        ),
        ("sockprobe", "apps/platform/sockprobe", "", &[]),
    ];

    /// `cargo metadata` of a fake workspace with one lib target per package.
    fn metadata(repository: &Path, packages: &[Fake]) -> String {
        let mut listed = vec![json!({
            "id": "serde", "name": "serde",
            "source": "registry+https://github.com/rust-lang/crates.io-index",
            "manifest_path": "/registry/serde/Cargo.toml", "targets": [],
        })];
        let mut nodes = Vec::new();
        for (name, directory, _, dependencies) in packages {
            let directory = repository.join(directory);
            listed.push(json!({
                "id": name, "name": name, "source": null,
                "manifest_path": directory.join("Cargo.toml"),
                "targets": [{"kind": ["lib"], "src_path": directory.join("src/lib.rs")}],
            }));
            let deps: Vec<_> = dependencies
                .iter()
                .map(|(pkg, kind)| json!({"pkg": pkg, "dep_kinds": [{"kind": kind}]}))
                .collect();
            nodes.push(json!({"id": name, "deps": deps}));
        }
        json!({"packages": listed, "resolve": {"nodes": nodes}}).to_string()
    }

    /// A fresh checkout named `name`, and the `cargo metadata` of its root and
    /// `apps` workspaces.
    pub(in crate::platform_build) fn fake_checkout(name: &str) -> (PathBuf, String, String) {
        let repository =
            std::env::temp_dir().join(format!("wamn-input-hash-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repository);
        let write = |path: &str, text: &str| {
            let path = repository.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        for (path, text) in [
            ("Dockerfile", "FROM scratch AS host\n"),
            (".dockerignore", "/target\n/apps/target\ndocs\n"),
            ("Cargo.toml", "[workspace]\n"),
            ("Cargo.lock", "version = 4\n"),
            (".cargo/config.toml", "\n"),
            ("apps/Cargo.toml", "[workspace]\n"),
            ("apps/Cargo.lock", "version = 4\n"),
            ("apps/target/wamn/wamn_wms/build.json", "{}"),
            ("deploy/sql/run.sql", "create table run ();"),
            (
                "deploy/platform/release/Chart.yaml",
                "version: 0.1.1\nappVersion: \"0.1.1\"\n",
            ),
            ("deploy/platform/release/values.yaml", "host: {}\n"),
            ("tools/guest-rustflags", "#!/bin/sh\n"),
            ("docs/readme.md", "docs\n"),
            ("target/release/wamn-host", "binary"),
        ] {
            write(path, text);
        }
        for (name, directory, source, _) in ROOT.iter().chain(APPS) {
            write(
                &format!("{directory}/Cargo.toml"),
                &format!("[package]\nname = \"{name}\"\n"),
            );
            write(&format!("{directory}/src/lib.rs"), source);
        }
        let root = metadata(&repository, ROOT);
        let apps = metadata(&repository, APPS);
        (repository, root, apps)
    }

    /// The input hash of every target of `repository`.
    fn hashes(repository: &Path, root: &str, apps: &str) -> BTreeMap<&'static str, String> {
        let root = parse_metadata(&serde_json::from_str(root).unwrap()).unwrap();
        let apps = parse_metadata(&serde_json::from_str(apps).unwrap()).unwrap();
        ["host", "runner", "gates", "identity", "edge-web"]
            .into_iter()
            .map(|target| {
                let files = input_files(repository, target, &root, &apps).unwrap();
                (target, input_hash(target, &files))
            })
            .collect()
    }

    #[test]
    fn the_input_hash_is_stable_and_follows_only_the_inputs_of_each_target() {
        let (repository, root, apps) = fake_checkout("follows");
        assert_eq!(
            hashes(&repository, &root, &apps),
            hashes(&repository, &root, &apps)
        );
        // The targets whose hash changes when `path` is written with `text`.
        let change = |path: &str, text: &str| {
            let before = hashes(&repository, &root, &apps);
            let path = repository.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
            let after = hashes(&repository, &root, &apps);
            before
                .into_iter()
                .filter(|(target, hash)| after[target] != *hash)
                .map(|(target, _)| target)
                .collect::<Vec<_>>()
        };
        let all = ["edge-web", "gates", "host", "identity", "runner"];
        // The chart values reach the images whose binaries embed them.
        assert_eq!(
            change("deploy/platform/release/values.yaml", "host: {a: 1}\n"),
            ["gates", "runner"]
        );
        assert_eq!(
            change("crates/control/src/lib.rs", "// 0.1.2"),
            ["gates", "runner"]
        );
        // A dev dependency of the cases reaches the runner only.
        assert_eq!(
            change("test-support/infrastructure/src/lib.rs", "// new"),
            ["runner"]
        );
        assert_eq!(
            change("apps/target/wamn/wamn_wms/build.json", "{\"a\":1}"),
            ["runner"]
        );
        // A file that a package includes from outside its directory counts.
        assert_eq!(
            change("deploy/sql/run.sql", "create table run (id int);"),
            ["gates", "host", "identity", "runner"]
        );
        assert_eq!(
            change("services/host/src/lib.rs", "// new"),
            ["gates", "host"]
        );
        assert_eq!(
            change("apps/platform/http-route/src/lib.rs", "// new"),
            ["gates", "runner"]
        );
        assert_eq!(
            change("tools/guest-rustflags", "#!/bin/sh\necho\n"),
            ["gates", "runner"]
        );
        assert_eq!(change("deploy/gcp/cluster.k", "new"), ["runner"]);
        // Shared build inputs reach every target.
        assert_eq!(change("Dockerfile", "FROM scratch AS gates\n"), all);
        assert_eq!(change("Cargo.lock", "version = 4\n# new\n"), all);
        assert_eq!(
            change("services/edge/Cargo.toml", "[package]\n# new\n"),
            all
        );
        // Files that no COPY reaches, or that .dockerignore drops, do not.
        assert!(change("docs/readme.md", "new").is_empty());
        assert!(change("README.md", "new").is_empty());
        assert!(change("target/release/wamn-host", "new").is_empty());
        std::fs::remove_dir_all(repository).unwrap();
    }

    /// The platform version lives in `Chart.yaml` alone, which no binary
    /// embeds and the runner drops (wamn-6qo7v).
    #[test]
    fn a_version_only_bump_keeps_the_input_hash_of_every_image() {
        let (repository, root, apps) = fake_checkout("version");
        let before = hashes(&repository, &root, &apps);
        std::fs::write(
            repository.join("deploy/platform/release/Chart.yaml"),
            "version: 0.1.2\nappVersion: \"0.1.2\"\n",
        )
        .unwrap();
        assert_eq!(hashes(&repository, &root, &apps), before);
        std::fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn the_mode_of_a_file_is_an_input() {
        use std::os::unix::fs::PermissionsExt as _;
        let (repository, root, apps) = fake_checkout("mode");
        let before = hashes(&repository, &root, &apps);
        std::fs::set_permissions(
            repository.join("tools/guest-rustflags"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let after = hashes(&repository, &root, &apps);
        assert_ne!(before["gates"], after["gates"]);
        assert_eq!(before["host"], after["host"]);
        std::fs::remove_dir_all(repository).unwrap();
    }

    /// A bump changes `Chart.yaml` alone. Over this checkout's real Cargo
    /// graph, no image hashes it (wamn-6qo7v). The bump to 0.1.96 also
    /// changed one telemetry check of the qualification cases, which reaches
    /// the gates and runner inputs only.
    #[test]
    fn a_version_bump_of_this_checkout_leaves_every_image_unchanged() {
        let repository =
            std::fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")).unwrap();
        let graph = |manifest: &str| {
            let output = std::process::Command::new(env!("CARGO"))
                .current_dir(&repository)
                .args([
                    "metadata",
                    "--format-version",
                    "1",
                    "--locked",
                    "--all-features",
                    "--manifest-path",
                    manifest,
                ])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            parse_metadata(&serde_json::from_slice(&output.stdout).unwrap()).unwrap()
        };
        let (root, apps) = (graph("Cargo.toml"), graph("apps/Cargo.toml"));
        let bump = "deploy/platform/release/Chart.yaml";
        for target in ["host", "identity", "edge-web", "gates", "runner"] {
            let files = input_files(&repository, target, &root, &apps).unwrap();
            assert!(!files.contains_key(bump), "{target} hashes {bump}");
            assert!(files.contains_key("Dockerfile"), "{target}");
        }
        for target in ["host", "identity", "edge-web"] {
            let files = input_files(&repository, target, &root, &apps).unwrap();
            let path = "test-support/infrastructure/telemetry.rs";
            assert!(!files.contains_key(path), "{target} hashes {path}");
        }
        let gates = input_files(&repository, "gates", &root, &apps).unwrap();
        for path in [
            "test-support/infrastructure/telemetry.rs",
            "deploy/platform/release/values.yaml",
        ] {
            assert!(gates.contains_key(path), "gates does not hash {path}");
        }
        let host = input_files(&repository, "host", &root, &apps).unwrap();
        assert!(host.contains_key("deploy/sql/run-state.sql"));
        assert!(host.contains_key("services/host/src/main.rs"));
    }

    #[test]
    fn a_build_script_reads_what_it_declares_or_anything() {
        assert_eq!(
            declared_reads(
                r#"println!("cargo:rerun-if-changed=Cargo.toml"); println!("cargo:rerun-if-changed=../x");"#
            ),
            Some(vec!["Cargo.toml".to_owned(), "../x".to_owned()])
        );
        assert_eq!(declared_reads("fn main() {}"), None);
        assert_eq!(
            declared_reads(r#"println!("cargo:rerun-if-changed={path}");"#),
            None
        );
    }

    #[test]
    fn literals_resolve_inside_the_repository_only() {
        assert_eq!(
            resolve(Path::new("crates/a/src"), "../../../deploy/sql/x.sql"),
            Some(PathBuf::from("deploy/sql/x.sql"))
        );
        assert_eq!(resolve(Path::new("crates/a"), "../../../x"), None);
        assert_eq!(
            resolve(Path::new("crates/a"), "../.."),
            Some(PathBuf::new())
        );
        assert_eq!(
            literals(invocation(r#"include_str!("../a.sql")); let b = "../b";"#)),
            ["../a.sql"]
        );
        assert_eq!(
            literals(invocation("#[path = \"../x.rs\"] mod x; \"../y\"")),
            ["../x.rs"]
        );
        assert_eq!(
            literals(invocation(
                "generate!({ path: [\"../w\", \"wit\"], world: \"w\" }); \"z\""
            )),
            ["../w", "wit", "w"]
        );
    }

    #[test]
    fn the_dockerfile_copies_from_the_context_only_the_hashed_paths() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let dockerfile = std::fs::read_to_string(repository.join("Dockerfile")).unwrap();
        let mut sources = BTreeSet::new();
        for line in dockerfile.lines() {
            let Some(arguments) = line.strip_prefix("COPY ") else {
                continue;
            };
            let words: Vec<_> = arguments
                .split_whitespace()
                .filter(|word| !word.starts_with("--chmod"))
                .collect();
            if words.iter().any(|word| word.starts_with("--from")) {
                continue;
            }
            sources.extend(words[..words.len() - 1].iter().map(ToString::to_string));
        }
        for source in &sources {
            assert!(
                COPIED
                    .iter()
                    .any(|copied| Path::new(source).starts_with(copied)),
                "the Dockerfile copies {source}, which the input hash does not cover"
            );
        }
        assert!(sources.contains("tools"), "{sources:?}");
    }
}
