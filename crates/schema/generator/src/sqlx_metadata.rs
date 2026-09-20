//! Builds and runs an isolated SQLx verifier for a generated package.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, ensure};
use serde_json::Value;

use crate::PackageManifest;
use crate::data_access_schemas;

const SQLX_VERSION: &str = "0.9.0";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SqlxMetadataMode {
    Compile,
    Check,
    Prepare,
}

#[derive(Debug, Eq, PartialEq)]
pub struct SqlxVerifier {
    pub queries: usize,
    pub root: PathBuf,
}

pub fn verify_sqlx_metadata(mode: SqlxMetadataMode, package_root: &Path) -> Result<()> {
    let package_root = fs::canonicalize(package_root).context("resolve package root")?;
    let package_root = package_root.as_path();
    let repository_root = repository_root(package_root)?;
    let verifier_root = repository_root
        .join("target/wamn-sqlx-verifier")
        .join(format!(
            "{}-{}",
            package_key(package_root),
            std::process::id()
        ));
    let verifier = stage_sqlx_verifier(package_root, &verifier_root, &repository_root)?;
    ensure!(
        verifier.queries > 0,
        "package has no generated SQLx queries"
    );
    let build_root = repository_root.join("target/wamn-sqlx-verifier-build");

    if mode == SqlxMetadataMode::Compile {
        ensure!(
            verifier.root.join(".sqlx").is_dir(),
            "package has no tests/.sqlx metadata"
        );
        let output = Command::new("cargo")
            .current_dir(&verifier.root)
            .args(["check", "--lib", "--locked", "--offline"])
            .env("SQLX_OFFLINE", "true")
            .env_remove("DATABASE_URL")
            .env("CARGO_TARGET_DIR", &build_root)
            .output()
            .context("compile isolated SQLx verifier offline")?;
        ensure!(
            output.status.success(),
            "offline SQLx verifier compilation failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return Ok(());
    }

    let database_url = std::env::var("DATABASE_URL")
        .context("DATABASE_URL must name the package's migrated PostgreSQL database")?;
    let manifest = fs::read(package_root.join("wamn.json")).context("read package manifest")?;
    let manifest = PackageManifest::from_slice(&manifest).context("parse package manifest")?;
    let database_url = package_database_url(&database_url, &manifest)?;

    let version = Command::new("cargo")
        .args(["sqlx", "--version"])
        .output()
        .context("run cargo sqlx --version")?;
    ensure!(version.status.success(), "cargo sqlx --version failed");
    let reported = String::from_utf8_lossy(&version.stdout);
    ensure!(
        matches!(reported.trim(), "sqlx-cli 0.9.0" | "sqlx-cli-sqlx 0.9.0"),
        "cargo sqlx {SQLX_VERSION} is required; found {reported}"
    );

    let mut command = Command::new("cargo");
    command
        .current_dir(&verifier.root)
        .args(["sqlx", "prepare"]);
    if mode == SqlxMetadataMode::Check {
        command.arg("--check");
    }
    command
        .args(["--", "--lib", "--locked", "--offline"])
        .env("DATABASE_URL", database_url)
        .env("SQLX_OFFLINE", "false")
        .env("CARGO_TARGET_DIR", build_root);
    let output = command.output().context("run isolated SQLx verifier")?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    ensure!(
        output.status.success(),
        "SQLx metadata verification failed:\n{stderr}"
    );
    ensure!(
        !stderr.contains("potentially unused queries")
            && !stdout.contains("potentially unused queries"),
        "SQLx metadata contains unused queries:\n{stdout}{stderr}"
    );

    if mode == SqlxMetadataMode::Prepare {
        replace_metadata(
            &verifier.root.join(".sqlx"),
            &package_root.join("tests/.sqlx"),
        )?;
    }
    Ok(())
}

/// Add the package schemas to PostgreSQL's existing connection options.
pub fn package_database_url(database_url: &str, manifest: &PackageManifest) -> Result<String> {
    let schemas = data_access_schemas(&serde_json::to_vec(manifest)?)
        .context("resolve the package SQLx schemas")?;
    let connection = database_url
        .parse::<tokio_postgres::Config>()
        .context("parse SQLx connection options")?;
    let mut url = url::Url::parse(database_url).context("parse the SQLx database URL")?;
    let options = format!(
        "{} -csearch_path={},public",
        connection.get_options().unwrap_or_default(),
        schemas.join(",")
    );
    let encoded = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("options", options.trim())
        .finish()
        .replace('+', "%20");
    let query = match url.query().filter(|query| !query.is_empty()) {
        Some(query) => format!("{query}&{encoded}"),
        None => encoded,
    };
    url.set_query(Some(&query));
    Ok(url.into())
}

pub fn stage_sqlx_verifier(
    package_root: &Path,
    verifier_root: &Path,
    repository_root: &Path,
) -> Result<SqlxVerifier> {
    if verifier_root.exists() {
        fs::remove_dir_all(verifier_root).context("clear staged SQLx verifier")?;
    }
    fs::create_dir_all(verifier_root.join("src")).context("create staged SQLx verifier")?;
    copy_file(
        &repository_root.join("Cargo.lock"),
        &verifier_root.join("Cargo.lock"),
    )?;
    writeln!(
        fs::OpenOptions::new()
            .append(true)
            .open(verifier_root.join("Cargo.lock"))?,
        "\n[[package]]\nname = \"wamn-generated-sqlx-verifier\"\nversion = \"0.0.0\"\ndependencies = [\n \"chrono\",\n \"rust_decimal\",\n \"serde_json\",\n \"sqlx\",\n \"uuid\",\n]\n"
    )?;
    let metadata = package_root.join("tests/.sqlx");
    if metadata.exists() {
        copy_tree(&metadata, &verifier_root.join(".sqlx"))?;
    }

    let source_maps = package_root.join("generated/source-map");
    let native_root = package_root.join("generated/native-verifier");
    let mut modules = Vec::new();
    let mut query_count = 0;
    let mut native_files = file_stems(&native_root, "rs")?;
    for stem in file_stems(&source_maps, "json")? {
        let bytes = fs::read(source_maps.join(format!("{stem}.json")))?;
        let map: Value = serde_json::from_slice(&bytes)?;
        let Some(accessors) = accessors(&map) else {
            continue;
        };
        ensure!(
            native_files.remove(&stem),
            "source map {stem} has accessors but no native verifier"
        );
        let fixtures = bind_fixtures(&map)?;
        let mut calls = String::new();
        let mut operation_offsets = BTreeMap::<String, usize>::new();
        let mut accessor_names = BTreeSet::new();
        for accessor in accessors {
            let name = string(accessor, "name")?;
            let row = string(accessor, "row")?;
            accessor_names.insert(name);
            let path = statement_path(&map, accessor, &mut operation_offsets)?;
            copy_file(&package_root.join(path), &verifier_root.join(path))?;
            let binds = accessor
                .get("binds")
                .and_then(Value::as_array)
                .context("accessor binds")?;
            let owned = fixtures.get(name).cloned().unwrap_or_default();
            ensure!(
                owned.len() == binds.len(),
                "{stem}.{name} bind fixture count differs from accessor"
            );
            let mut arguments = Vec::new();
            for (bind, fixture) in binds.iter().zip(&owned) {
                ensure!(
                    string(bind, "parameter")? == fixture.0,
                    "{stem}.{name} bind fixture order differs from accessor"
                );
                arguments.push(format!("native::{stem}::{}()", fixture.1));
            }
            writeln!(
                calls,
                "    let _ = sqlx::query_file_as!(native::{stem}::{row}, {path:?}{});",
                if arguments.is_empty() {
                    String::new()
                } else {
                    format!(", {}", arguments.join(", "))
                }
            )?;
            query_count += 1;
        }
        ensure!(
            fixtures.keys().all(|name| accessor_names.contains(name)),
            "{stem} has bind fixtures without accessors"
        );
        if let Some(statements) = map.get("statements").and_then(Value::as_object) {
            ensure!(
                statements
                    .keys()
                    .all(|name| accessor_names.contains(name.as_str())),
                "{stem} has statements without accessors"
            );
        }
        if let Some(operations) = map.get("operations").and_then(Value::as_object) {
            for (operation, paths) in operations {
                let count = paths.as_array().context("operation statement paths")?.len();
                ensure!(
                    operation_offsets
                        .get(operation)
                        .copied()
                        .unwrap_or_default()
                        == count,
                    "{stem}.{operation} has statement paths without accessors"
                );
            }
        }
        copy_file(
            &native_root.join(format!("{stem}.rs")),
            &verifier_root.join(format!("generated/native-verifier/{stem}.rs")),
        )?;
        modules.push((stem, calls));
    }
    ensure!(
        native_files.is_empty(),
        "native verifier files lack source-map accessors: {native_files:?}"
    );

    fs::write(
        verifier_root.join("Cargo.toml"),
        verifier_manifest(repository_root),
    )?;
    fs::write(verifier_root.join("src/lib.rs"), verifier_source(&modules))?;
    let lock = Command::new("cargo")
        .current_dir(verifier_root)
        .args(["metadata", "--offline", "--format-version", "1"])
        .output()
        .context("trim the repository lockfile to the staged verifier")?;
    ensure!(
        lock.status.success(),
        "resolve the staged verifier from the repository lockfile:\n{}",
        String::from_utf8_lossy(&lock.stderr)
    );
    Ok(SqlxVerifier {
        queries: query_count,
        root: verifier_root.to_path_buf(),
    })
}

fn accessors(map: &Value) -> Option<&Vec<Value>> {
    map.get("wamn_accessors")
        .or_else(|| map.pointer("/wamn_api/accessors"))?
        .as_array()
}

fn bind_fixtures(map: &Value) -> Result<BTreeMap<&str, Vec<(&str, &str)>>> {
    let mut result = BTreeMap::<&str, Vec<(&str, &str)>>::new();
    for fixture in map
        .get("native_bind_fixtures")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        result
            .entry(string(fixture, "accessor")?)
            .or_default()
            .push((string(fixture, "parameter")?, string(fixture, "function")?));
    }
    Ok(result)
}

fn statement_path<'a>(
    map: &'a Value,
    accessor: &'a Value,
    offsets: &mut BTreeMap<String, usize>,
) -> Result<&'a str> {
    let name = string(accessor, "name")?;
    if let Some(statement) = map.pointer(&format!("/statements/{name}")) {
        return string(statement, "path");
    }
    let operation = string(accessor, "operation")?;
    let paths = map
        .pointer(&format!("/operations/{operation}"))
        .and_then(Value::as_array)
        .with_context(|| format!("missing statement paths for {name}"))?;
    let offset = offsets.entry(operation.to_owned()).or_default();
    let path = paths
        .get(*offset)
        .and_then(Value::as_str)
        .with_context(|| format!("missing statement path for {name}"))?;
    *offset += 1;
    Ok(path)
}

fn verifier_source(modules: &[(String, String)]) -> String {
    let mut source = String::from(
        "#![expect(dead_code, reason = \"compile-only SQLx verification owns generated fields\")]\nmod native {\n",
    );
    for (stem, _) in modules {
        writeln!(source, "    pub mod {stem} {{ include!(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/generated/native-verifier/{stem}.rs\")); }}").expect("write verifier module");
    }
    source.push_str("}\n#[allow(clippy::needless_borrow)]\npub fn verify() {\n");
    for (_, calls) in modules {
        source.push_str(calls);
    }
    source.push_str("}\n");
    source
}

fn verifier_manifest(_repository_root: &Path) -> String {
    r#"[package]
name = "wamn-generated-sqlx-verifier"
version = "0.0.0"
edition = "2024"

[workspace]

[dependencies]
chrono = { version = "0.4", default-features = false }
rust_decimal = { version = "1", default-features = false }
serde_json = "1"
sqlx = { version = "=0.9.0", default-features = false, features = ["chrono", "json", "macros", "postgres", "runtime-tokio", "rust_decimal", "uuid"] }
uuid = "1"
"#.to_owned()
}

fn repository_root(package_root: &Path) -> Result<PathBuf> {
    package_root
        .ancestors()
        .find(|path| path.join("crates/schema/generator/Cargo.toml").is_file())
        .map(Path::to_path_buf)
        .context("package is not inside the WAMN repository")
}

fn package_key(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("package")
        .chars()
        .map(|value| {
            if value.is_ascii_alphanumeric() {
                value
            } else {
                '_'
            }
        })
        .collect()
}

fn file_stems(root: &Path, extension: &str) -> Result<BTreeSet<String>> {
    let mut result = BTreeSet::new();
    for entry in fs::read_dir(root).with_context(|| format!("read {}", root.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|value| value.to_str()) == Some(extension) {
            result.insert(
                path.file_stem()
                    .and_then(|value| value.to_str())
                    .context("non-UTF-8 generated filename")?
                    .to_owned(),
            );
        }
    }
    Ok(result)
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("missing string field {key}"))
}

fn copy_file(source: &Path, destination: &Path) -> Result<()> {
    let parent = destination.parent().context("destination has no parent")?;
    fs::create_dir_all(parent)?;
    fs::copy(source, destination).with_context(|| format!("copy {}", source.display()))?;
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            copy_file(&entry.path(), &target)?;
        }
    }
    Ok(())
}

fn replace_metadata(source: &Path, destination: &Path) -> Result<()> {
    ensure!(source.is_dir(), "SQLx prepare did not produce metadata");
    let staged = destination.with_extension("sqlx.next");
    if staged.exists() {
        fs::remove_dir_all(&staged)?;
    }
    copy_tree(source, &staged)?;
    let backup = destination.with_extension("sqlx.previous");
    if backup.exists() {
        fs::remove_dir_all(&backup)?;
    }
    if destination.exists() {
        fs::rename(destination, &backup)?;
    }
    if let Err(error) = fs::rename(&staged, destination) {
        if backup.exists() {
            fs::rename(&backup, destination)?;
        }
        return Err(error).context("install prepared SQLx metadata");
    }
    if backup.exists() {
        fs::remove_dir_all(backup)?;
    }
    Ok(())
}
