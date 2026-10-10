//! The authored manifest, `wamn.k`, and its compiled form, `generated/wamn.json`.
//!
//! A package authors its manifest in KCL. The generator compiles `wamn.k` with
//! the pinned `kcl` CLI against the `manifest` schema module that ships in this
//! crate. Generation adds the members it derives, such as each custom
//! operation's relations, and writes the JSON to `generated/wamn.json`. The
//! author states none of them. Every other reader reads
//! that file and never `wamn.k` (docs/plan/manifest-authoring.md §4.3). Every
//! package under `apps/` authors `wamn.k`, and the repository policy lint
//! refuses a root `wamn.json` there. A test fixture keeps its hand-written
//! `wamn.json`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context as _, Result, bail, ensure};
use serde_json::Value;

use crate::{PackageManifest, StaticSqlRelationDeclaration, output_root};

/// The manifest file an author writes.
pub const AUTHORED_MANIFEST: &str = "wamn.k";
/// The compiled manifest's logical path, relative to the package root. A
/// package artifact carries it at this path. In a package tree the file lies
/// in the package's [`output_root`].
pub const COMPILED_MANIFEST: &str = "generated/wamn.json";
/// The compiled manifest's file name in the package's [`output_root`].
pub(crate) const COMPILED_MANIFEST_NAME: &str = "wamn.json";
/// The hand-written manifest of a package that is not converted.
const HAND_WRITTEN_MANIFEST: &str = "wamn.json";
/// The environment variable that names a `kcl` binary in place of `tools/install-kcl`.
pub const KCL_ENV: &str = "WAMN_KCL";

const SCHEMA_MODULE: &str = include_str!("../kcl/manifest/manifest.k");
const SCHEMA_MODULE_DECLARATION: &str = include_str!("../kcl/manifest/kcl.mod");
const ENVIRONMENT_MODULE: &str = include_str!("../kcl/environment/environment.k");
const ENVIRONMENT_MODULE_DECLARATION: &str = include_str!("../kcl/environment/kcl.mod");

/// Whether the package authors its manifest in `wamn.k`.
#[must_use]
pub fn is_authored(package_root: &Path) -> bool {
    package_root.join(AUTHORED_MANIFEST).is_file()
}

/// The manifest file that a reader of the package reads: `generated/wamn.json`
/// for a package authored in `wamn.k`, otherwise `wamn.json`.
#[must_use]
pub fn package_manifest_path(package_root: &Path) -> PathBuf {
    if is_authored(package_root) {
        compiled_manifest_path(package_root)
    } else {
        package_root.join(HAND_WRITTEN_MANIFEST)
    }
}

/// The compiled manifest file of a package tree, in its [`output_root`].
pub(crate) fn compiled_manifest_path(package_root: &Path) -> PathBuf {
    output_root(package_root).join(COMPILED_MANIFEST_NAME)
}

/// The package root of a manifest path that [`package_manifest_path`] gives:
/// the authored package whose [`output_root`] holds a compiled manifest,
/// otherwise the manifest's own directory.
#[must_use]
pub fn manifest_package_root(manifest: &Path) -> Option<&Path> {
    let directory = manifest.parent()?;
    match directory.parent() {
        Some(root) if is_authored(root) && output_root(root) == directory => Some(root),
        _ => Some(directory),
    }
}

/// Whether a directory is a package root: it holds `wamn.k` or `wamn.json`.
#[must_use]
pub fn is_package_root(directory: &Path) -> bool {
    is_authored(directory) || directory.join(HAND_WRITTEN_MANIFEST).is_file()
}

/// Compile a package's `wamn.k` to the bytes of `generated/wamn.json`.
///
/// The bytes are 2-space indented JSON with the keys in the order the schema
/// module lists them, and end with a newline. They parse as a
/// [`PackageManifest`].
///
/// # Errors
///
/// When the package also holds a hand-written `wamn.json`, when no `kcl` runs,
/// when KCL refuses the file (the error names its path and line), or when the
/// compiled JSON is not a valid manifest. A statement value that states no
/// type is not valid here: only `wamn build` derives it.
pub fn compile_manifest(package_root: &Path) -> Result<Vec<u8>> {
    let bytes = compile_authored_manifest(package_root)?;
    PackageManifest::from_slice(&bytes).with_context(|| {
        format!(
            "the compiled {} is not a valid manifest",
            package_root.join(AUTHORED_MANIFEST).display()
        )
    })?;
    Ok(bytes)
}

/// Compile a package's `wamn.k` to its authored form: every fact the author
/// states and no derived member (docs/plan/platform-deploy.md §7.1 step 1).
///
/// The authored form may leave out the type of a statement parameter or row
/// value, which `wamn build` derives from the statement. It therefore is not
/// yet a [`PackageManifest`], and this function does not parse it as one.
///
/// # Errors
///
/// When the package also holds a hand-written `wamn.json`, when no `kcl` runs,
/// when KCL refuses the file, or when the file states a derived member.
pub(crate) fn compile_authored_manifest(package_root: &Path) -> Result<Vec<u8>> {
    ensure!(
        !package_root.join(HAND_WRITTEN_MANIFEST).exists(),
        "{}: the manifest is authored in wamn.k; wamn.json is generated",
        package_root.display()
    );
    let module = SchemaModule::write("manifest", SCHEMA_MODULE_DECLARATION, SCHEMA_MODULE)?;
    let bytes = run_kcl(&module, package_root, Path::new(AUTHORED_MANIFEST))?;
    refuse_derived_members(package_root, &bytes)?;
    Ok(bytes)
}

/// Compile `wamn.k` and write `generated/wamn.json` when its authored members
/// differ.
///
/// Generation writes the same file with the members it derives, such as each
/// custom operation's relations, and a file whose other members equal the
/// compile is left as it is. A package without `wamn.k` is left unchanged. A reader that runs before
/// generation calls this first, so it reads the manifest of the current
/// `wamn.k` from the file (wamn-vgsl).
///
/// # Errors
///
/// When [`compile_manifest`] refuses, or the file cannot be written.
pub fn write_compiled_manifest(package_root: &Path) -> Result<()> {
    if !is_authored(package_root) {
        return Ok(());
    }
    let bytes = compile_manifest(package_root)?;
    let path = compiled_manifest_path(package_root);
    // A file that generation compiled from the same wamn.k is kept, with the
    // members it derived.
    let current = fs::read(&path)
        .ok()
        .and_then(|compiled| without_derived_members(&compiled).ok());
    if current.as_deref() != Some(bytes.as_slice()) {
        let directory = output_root(package_root);
        fs::create_dir_all(&directory)
            .with_context(|| format!("create generated directory {}", directory.display()))?;
        fs::write(&path, &bytes).with_context(|| format!("write {}", path.display()))?;
    }
    Ok(())
}

/// Refuse a committed `generated/wamn.json` that differs from the compiled bytes.
///
/// # Errors
///
/// When the committed file is missing, or differs from `compiled`. The error
/// names the first JSON path that differs, or says that only the formatting does.
pub fn check_compiled_manifest(package_root: &Path, compiled: &[u8]) -> Result<()> {
    let path = compiled_manifest_path(package_root);
    let committed = fs::read(&path).with_context(|| format!("missing {}", path.display()))?;
    if committed == compiled {
        return Ok(());
    }
    let expected: Value =
        serde_json::from_slice(compiled).context("parse the compiled manifest")?;
    let Ok(actual) = serde_json::from_slice::<Value>(&committed) else {
        bail!("{} differs from wamn.k: it is not JSON", path.display());
    };
    match first_difference("", &expected, &actual) {
        Some(at) => bail!("{} differs from wamn.k at {at}", path.display()),
        None => bail!(
            "{} differs from wamn.k in its formatting only",
            path.display()
        ),
    }
}

/// The JSON path of the first difference between two documents, if any.
fn first_difference(at: &str, expected: &Value, actual: &Value) -> Option<String> {
    match (expected, actual) {
        (Value::Object(expected), Value::Object(actual)) => expected
            .keys()
            .chain(actual.keys().filter(|key| !expected.contains_key(*key)))
            .find_map(|key| match (expected.get(key), actual.get(key)) {
                (Some(expected), Some(actual)) => {
                    first_difference(&format!("{at}.{key}"), expected, actual)
                }
                _ => Some(format!("{at}.{key}")),
            }),
        (Value::Array(expected), Value::Array(actual)) if expected.len() == actual.len() => {
            expected
                .iter()
                .zip(actual)
                .enumerate()
                .find_map(|(index, (expected, actual))| {
                    first_difference(&format!("{at}[{index}]"), expected, actual)
                })
        }
        _ if expected == actual => None,
        _ if at.is_empty() => Some(".".to_owned()),
        _ => Some(at.to_owned()),
    }
}

/// Compile an environment document (docs/plan/platform-deploy.md §10.1) to JSON.
///
/// The file imports the `environment` schema module that ships in this crate.
/// The bytes are 2-space indented JSON with the keys in schema order. The
/// caller parses them into its own type.
///
/// # Errors
///
/// When no `kcl` runs, or when KCL refuses the file (the error names its path
/// and line).
pub fn compile_environment(file: &Path) -> Result<Vec<u8>> {
    let directory = file
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let name = file
        .file_name()
        .with_context(|| format!("{} names no file", file.display()))?;
    let module = SchemaModule::write(
        "environment",
        ENVIRONMENT_MODULE_DECLARATION,
        ENVIRONMENT_MODULE,
    )?;
    run_kcl(
        &module,
        directory.unwrap_or_else(|| Path::new(".")),
        Path::new(name),
    )
}

/// Run `kcl` on one file in `directory` against one schema module.
fn run_kcl(module: &SchemaModule, directory: &Path, file: &Path) -> Result<Vec<u8>> {
    let kcl = kcl_binary()?;
    // A private package cache: kcl prints "waiting for package-cache lock..."
    // to stdout while another run holds the shared one.
    let output = Command::new(&kcl)
        .env("KCL_PKG_PATH", module.root.join("packages"))
        .arg("run")
        .arg(file)
        .arg("-E")
        .arg(format!(
            "{}={}",
            module.name,
            module.root.join(module.name).display()
        ))
        .args(["--format", "json", "--disable_none"])
        .current_dir(directory)
        .output()
        .with_context(|| format!("run {}", kcl.display()))?;
    if !output.status.success() {
        bail!(
            "compile {}:\n{}",
            directory.join(file).display(),
            without_terminal_colors(&String::from_utf8_lossy(&output.stderr)).trim_end()
        );
    }
    let json = String::from_utf8(output.stdout).context("kcl wrote JSON that is not UTF-8")?;
    Ok(two_space_indent(&json).into_bytes())
}

/// The `kcl` binary: `$WAMN_KCL`, or the one `tools/install-kcl` installs.
fn kcl_binary() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(KCL_ENV) {
        return Ok(PathBuf::from(path));
    }
    let installer = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tools/install-kcl");
    let output = Command::new(&installer).output().with_context(|| {
        format!(
            "run {}; set {KCL_ENV} to a kcl binary where the repository is absent",
            installer.display()
        )
    })?;
    ensure!(
        output.status.success(),
        "{} failed: {}",
        installer.display(),
        String::from_utf8_lossy(&output.stderr).trim_end()
    );
    let path =
        String::from_utf8(output.stdout).context("install-kcl printed a path that is not UTF-8")?;
    Ok(PathBuf::from(path.trim_end()))
}

/// A schema module and a package cache, in a private directory for one compile.
struct SchemaModule {
    root: PathBuf,
    name: &'static str,
}

impl SchemaModule {
    fn write(name: &'static str, declaration: &str, source: &str) -> Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "wamn-kcl-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let module = Self { root, name };
        let directory = module.root.join(name);
        fs::create_dir_all(&directory)
            .with_context(|| format!("create {}", directory.display()))?;
        fs::write(directory.join("kcl.mod"), declaration)
            .with_context(|| format!("write the {name} schema module"))?;
        fs::write(directory.join(format!("{name}.k")), source)
            .with_context(|| format!("write the {name} schema module"))?;
        Ok(module)
    }
}

impl Drop for SchemaModule {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Re-indent kcl's 4-space JSON to 2 spaces, ending with one newline.
///
/// JSON strings hold no raw newline, so each line's leading spaces are indent.
fn two_space_indent(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    for line in json.lines() {
        let body = line.trim_start_matches(' ');
        let indent = (line.len() - body.len()) / 2;
        out.extend(std::iter::repeat_n(' ', indent));
        out.push_str(body);
        out.push('\n');
    }
    out
}

/// Remove the ANSI color sequences kcl writes into its errors.
fn without_terminal_colors(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The members of a custom operation that generation derives, so `wamn.k`
/// states none of them (docs/plan/platform-deploy.md §6.1, R9).
const DERIVED_OPERATION_MEMBERS: [&str; 1] = ["relations"];

/// Refuse an authored manifest that states a member generation derives.
fn refuse_derived_members(package_root: &Path, authored: &[u8]) -> Result<()> {
    let document: OrderedJson =
        serde_json::from_slice(authored).context("parse the compiled manifest")?;
    for (operation, member) in operation_members(&document) {
        ensure!(
            !DERIVED_OPERATION_MEMBERS.contains(&member),
            "{}: custom operation {operation} states {member}, which the generator derives from its statements; remove it",
            package_root.join(AUTHORED_MANIFEST).display()
        );
    }
    Ok(())
}

/// Each `(operation, member)` pair of the document's custom operations.
fn operation_members(document: &OrderedJson) -> impl Iterator<Item = (&str, &str)> {
    document
        .member("custom_operations")
        .map(OrderedJson::members)
        .unwrap_or_default()
        .iter()
        .flat_map(|(operation, declaration)| {
            declaration
                .members()
                .iter()
                .map(move |(member, _)| (operation.as_str(), member.as_str()))
        })
}

/// The compiled manifest: the authored bytes with each custom operation's
/// derived relations in place, before its statements.
///
/// The members keep the order the schema module gives them, so the compiled
/// file differs from the authored compile only where generation derived.
pub(crate) fn with_derived_relations(
    authored: &[u8],
    relations: &BTreeMap<String, Vec<StaticSqlRelationDeclaration>>,
) -> serde_json::Result<Vec<u8>> {
    let mut document: OrderedJson = serde_json::from_slice(authored)?;
    if let Some(OrderedJson::Object(operations)) = document.member_mut("custom_operations") {
        for (operation, declaration) in operations {
            let (Some(derived), OrderedJson::Object(members)) =
                (relations.get(operation), declaration)
            else {
                continue;
            };
            let value = serde_json::from_slice(&serde_json::to_vec(derived)?)?;
            let at = members
                .iter()
                .position(|(member, _)| member == "statements")
                .unwrap_or(members.len());
            members.insert(at, ("relations".to_owned(), value));
        }
    }
    document.to_bytes()
}

/// The authored compile of a compiled manifest: the bytes without the members
/// generation derived.
fn without_derived_members(compiled: &[u8]) -> serde_json::Result<Vec<u8>> {
    let mut document: OrderedJson = serde_json::from_slice(compiled)?;
    if let Some(OrderedJson::Object(operations)) = document.member_mut("custom_operations") {
        for (_, declaration) in operations {
            if let OrderedJson::Object(members) = declaration {
                members.retain(|(member, _)| !DERIVED_OPERATION_MEMBERS.contains(&member.as_str()));
            }
        }
    }
    document.to_bytes()
}

/// A JSON document whose objects keep their members in the order written.
///
/// `serde_json` here sorts the members of an object, and the compiled
/// manifest keeps the order of the schema module.
#[derive(Debug, Clone, PartialEq)]
enum OrderedJson {
    Scalar(Value),
    Array(Vec<OrderedJson>),
    Object(Vec<(String, OrderedJson)>),
}

impl OrderedJson {
    fn members(&self) -> &[(String, Self)] {
        match self {
            Self::Object(members) => members,
            Self::Scalar(_) | Self::Array(_) => &[],
        }
    }

    fn member(&self, name: &str) -> Option<&Self> {
        self.members()
            .iter()
            .find_map(|(member, value)| (member == name).then_some(value))
    }

    fn member_mut(&mut self, name: &str) -> Option<&mut Self> {
        match self {
            Self::Object(members) => members
                .iter_mut()
                .find_map(|(member, value)| (member == name).then_some(value)),
            Self::Scalar(_) | Self::Array(_) => None,
        }
    }

    /// 2-space indented JSON ending with one newline, as the compile writes it.
    fn to_bytes(&self) -> serde_json::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        let mut serializer = serde_json::Serializer::with_formatter(
            &mut bytes,
            serde_json::ser::PrettyFormatter::with_indent(b"  "),
        );
        serde::Serialize::serialize(self, &mut serializer)?;
        bytes.push(b'\n');
        Ok(bytes)
    }
}

impl serde::Serialize for OrderedJson {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{SerializeMap as _, SerializeSeq as _};
        match self {
            Self::Scalar(value) => value.serialize(serializer),
            Self::Array(items) => {
                let mut sequence = serializer.serialize_seq(Some(items.len()))?;
                for item in items {
                    sequence.serialize_element(item)?;
                }
                sequence.end()
            }
            Self::Object(members) => {
                let mut map = serializer.serialize_map(Some(members.len()))?;
                for (name, value) in members {
                    map.serialize_entry(name, value)?;
                }
                map.end()
            }
        }
    }
}

impl<'de> serde::Deserialize<'de> for OrderedJson {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = OrderedJson;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON value")
            }

            fn visit_bool<E>(self, value: bool) -> Result<OrderedJson, E> {
                Ok(OrderedJson::Scalar(Value::Bool(value)))
            }

            fn visit_i64<E>(self, value: i64) -> Result<OrderedJson, E> {
                Ok(OrderedJson::Scalar(Value::from(value)))
            }

            fn visit_u64<E>(self, value: u64) -> Result<OrderedJson, E> {
                Ok(OrderedJson::Scalar(Value::from(value)))
            }

            fn visit_f64<E>(self, value: f64) -> Result<OrderedJson, E> {
                Ok(OrderedJson::Scalar(Value::from(value)))
            }

            fn visit_str<E>(self, value: &str) -> Result<OrderedJson, E> {
                Ok(OrderedJson::Scalar(Value::from(value)))
            }

            fn visit_unit<E>(self) -> Result<OrderedJson, E> {
                Ok(OrderedJson::Scalar(Value::Null))
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<OrderedJson, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = sequence.next_element()? {
                    items.push(item);
                }
                Ok(OrderedJson::Array(items))
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<OrderedJson, A::Error> {
                let mut members = Vec::new();
                while let Some(member) = map.next_entry()? {
                    members.push(member);
                }
                Ok(OrderedJson::Object(members))
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn re_indents_to_two_spaces() {
        assert_eq!(
            two_space_indent("{\n    \"a\": [\n        1\n    ]\n}"),
            "{\n  \"a\": [\n    1\n  ]\n}\n"
        );
    }

    #[test]
    fn names_the_first_differing_path() {
        let expected = json!({"models": {"widget": {"operations": {"get": {"result": "one"}}}}});
        let actual = json!({"models": {"widget": {"operations": {"get": {"result": "page"}}}}});
        assert_eq!(
            first_difference("", &expected, &actual).as_deref(),
            Some(".models.widget.operations.get.result")
        );
        assert_eq!(
            first_difference("", &json!({"a": [1, 2]}), &json!({"a": [1, 3]})).as_deref(),
            Some(".a[1]")
        );
        assert_eq!(
            first_difference("", &json!({"a": 1}), &json!({"a": 1, "b": 2})).as_deref(),
            Some(".b")
        );
        assert_eq!(
            first_difference("", &json!({"a": 1}), &json!({"a": 1})),
            None
        );
    }

    #[test]
    fn a_compiled_manifest_belongs_to_the_package_above_generated() {
        let root = std::env::temp_dir().join(format!("wamn-authoring-root-{}", std::process::id()));
        fs::create_dir_all(output_root(&root)).expect("create the package");
        assert_eq!(package_manifest_path(&root), root.join("wamn.json"));
        assert_eq!(
            manifest_package_root(&root.join("wamn.json")),
            Some(root.as_path())
        );
        fs::write(root.join(AUTHORED_MANIFEST), "").expect("write wamn.k");
        let compiled = output_root(&root).join("wamn.json");
        assert_eq!(package_manifest_path(&root), compiled);
        assert_eq!(manifest_package_root(&compiled), Some(root.as_path()));
        fs::remove_dir_all(&root).expect("remove the package");
    }

    #[test]
    fn placing_derived_members_keeps_every_authored_byte() {
        let apps = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps");
        for app in [
            "wamn_wms",
            "wamn_receiving",
            "client_acme_receiving",
            "platform_fixture",
            "platform_fixture_overlay",
            "edge_samples",
            "edge_device",
        ] {
            let authored = compile_manifest(&apps.join(app)).expect("compile wamn.k");
            let compiled = with_derived_relations(&authored, &BTreeMap::new()).expect("place");
            assert_eq!(
                String::from_utf8(compiled.clone()).expect("UTF-8"),
                String::from_utf8(authored.clone()).expect("UTF-8"),
                "{app}"
            );
            assert_eq!(without_derived_members(&compiled).expect("strip"), authored);
        }
    }

    #[test]
    fn a_derived_relation_lands_before_the_statements() {
        let authored = br#"{"custom_operations": {"a.b": {"errors": [], "statements": {}}}}"#;
        let relation = StaticSqlRelationDeclaration {
            schema: "s".to_owned(),
            table: "t".to_owned(),
            select_fields: vec!["id".to_owned()],
            insert_fields: Vec::new(),
            update_fields: Vec::new(),
            delete: false,
            lock: true,
            constraints: Vec::new(),
        };
        let compiled = with_derived_relations(
            authored,
            &BTreeMap::from([("a.b".to_owned(), vec![relation])]),
        )
        .expect("place");
        let text = String::from_utf8(compiled.clone()).expect("UTF-8");
        let at = |key: &str| text.find(&format!("\"{key}\"")).expect(key);
        assert!(at("errors") < at("relations") && at("relations") < at("statements"));
        assert!(at("select_fields") < at("lock") && at("lock") < at("constraints"));
        assert!(!text.contains("\"delete\""));
        assert_eq!(
            serde_json::from_slice::<Value>(&without_derived_members(&compiled).expect("strip"))
                .expect("JSON"),
            serde_json::from_slice::<Value>(authored).expect("JSON")
        );
    }

    #[test]
    fn removes_terminal_colors() {
        assert_eq!(
            without_terminal_colors("\u{1b}[1;38;5;9merror\u{1b}[0m here"),
            "error here"
        );
    }
}
