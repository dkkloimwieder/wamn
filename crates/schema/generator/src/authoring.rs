//! The authored manifest, `wamn.k`, and its compiled form, `generated/wamn.json`.
//!
//! A package authors its manifest in KCL. The generator compiles `wamn.k` with
//! the pinned `kcl` CLI against the `manifest` schema module that ships in this
//! crate, and writes the JSON to `generated/wamn.json`. Every other reader reads
//! that file and never `wamn.k` (docs/plan/manifest-authoring.md §4.3). Every
//! package under `apps/` authors `wamn.k`, and the repository policy lint
//! refuses a root `wamn.json` there. A test fixture keeps its hand-written
//! `wamn.json`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context as _, Result, bail, ensure};
use serde_json::Value;

use crate::PackageManifest;

/// The manifest file an author writes.
pub const AUTHORED_MANIFEST: &str = "wamn.k";
/// The compiled manifest, relative to the package root.
pub const COMPILED_MANIFEST: &str = "generated/wamn.json";
/// The hand-written manifest of a package that is not converted.
const HAND_WRITTEN_MANIFEST: &str = "wamn.json";
/// The environment variable that names a `kcl` binary in place of `tools/install-kcl`.
pub const KCL_ENV: &str = "WAMN_KCL";

const SCHEMA_MODULE: &str = include_str!("../kcl/manifest/manifest.k");
const SCHEMA_MODULE_DECLARATION: &str = include_str!("../kcl/manifest/kcl.mod");

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
        package_root.join(COMPILED_MANIFEST)
    } else {
        package_root.join(HAND_WRITTEN_MANIFEST)
    }
}

/// The package root of a manifest path that [`package_manifest_path`] gives:
/// the directory above `generated/` for a compiled manifest, otherwise the
/// manifest's own directory.
#[must_use]
pub fn manifest_package_root(manifest: &Path) -> Option<&Path> {
    let directory = manifest.parent()?;
    if directory
        .file_name()
        .is_some_and(|name| name == "generated")
        && directory
            .parent()
            .is_some_and(|root| root.join(AUTHORED_MANIFEST).is_file())
    {
        directory.parent()
    } else {
        Some(directory)
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
/// compiled JSON is not a valid manifest.
pub fn compile_manifest(package_root: &Path) -> Result<Vec<u8>> {
    ensure!(
        !package_root.join(HAND_WRITTEN_MANIFEST).exists(),
        "{}: the manifest is authored in wamn.k; wamn.json is generated",
        package_root.display()
    );
    let kcl = kcl_binary()?;
    let module = SchemaModule::write()?;
    // A private package cache: kcl prints "waiting for package-cache lock..."
    // to stdout while another run holds the shared one.
    let output = Command::new(&kcl)
        .env("KCL_PKG_PATH", module.0.join("packages"))
        .args(["run", AUTHORED_MANIFEST, "-E"])
        .arg(format!("manifest={}", module.0.join("manifest").display()))
        .args(["--format", "json", "--disable_none"])
        .current_dir(package_root)
        .output()
        .with_context(|| format!("run {}", kcl.display()))?;
    if !output.status.success() {
        bail!(
            "compile {}:\n{}",
            package_root.join(AUTHORED_MANIFEST).display(),
            without_terminal_colors(&String::from_utf8_lossy(&output.stderr)).trim_end()
        );
    }
    let json = String::from_utf8(output.stdout).context("kcl wrote JSON that is not UTF-8")?;
    let bytes = two_space_indent(&json).into_bytes();
    PackageManifest::from_slice(&bytes).with_context(|| {
        format!(
            "the compiled {} is not a valid manifest",
            package_root.join(AUTHORED_MANIFEST).display()
        )
    })?;
    Ok(bytes)
}

/// Compile `wamn.k` and write `generated/wamn.json` when the bytes differ.
///
/// A package without `wamn.k` is left unchanged. A reader that runs before
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
    let path = package_root.join(COMPILED_MANIFEST);
    if fs::read(&path).ok().as_deref() != Some(bytes.as_slice()) {
        let directory = package_root.join("generated");
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
    let path = package_root.join(COMPILED_MANIFEST);
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

/// The schema module and a package cache, in a private directory for one compile.
struct SchemaModule(PathBuf);

impl SchemaModule {
    fn write() -> Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "wamn-kcl-manifest-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let module = Self(root);
        let manifest = module.0.join("manifest");
        fs::create_dir_all(&manifest).with_context(|| format!("create {}", manifest.display()))?;
        fs::write(manifest.join("kcl.mod"), SCHEMA_MODULE_DECLARATION)
            .context("write the manifest schema module")?;
        fs::write(manifest.join("manifest.k"), SCHEMA_MODULE)
            .context("write the manifest schema module")?;
        Ok(module)
    }
}

impl Drop for SchemaModule {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
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
        fs::create_dir_all(root.join("generated")).expect("create the package");
        assert_eq!(package_manifest_path(&root), root.join("wamn.json"));
        assert_eq!(
            manifest_package_root(&root.join("wamn.json")),
            Some(root.as_path())
        );
        fs::write(root.join(AUTHORED_MANIFEST), "").expect("write wamn.k");
        assert_eq!(package_manifest_path(&root), root.join(COMPILED_MANIFEST));
        assert_eq!(
            manifest_package_root(&root.join(COMPILED_MANIFEST)),
            Some(root.as_path())
        );
        fs::remove_dir_all(&root).expect("remove the package");
    }

    #[test]
    fn removes_terminal_colors() {
        assert_eq!(
            without_terminal_colors("\u{1b}[1;38;5;9merror\u{1b}[0m here"),
            "error here"
        );
    }
}
