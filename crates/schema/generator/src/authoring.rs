//! The authored manifest, `wamn.k`, and its compiled form, `wamn.json` in the
//! package's build output (logical path `generated/wamn.json`).
//!
//! A package authors its manifest in KCL. The generator compiles `wamn.k` with
//! the pinned `kcl` CLI against the `manifest` schema module that ships in this
//! crate. Generation adds the members it derives, such as each custom
//! operation's relations, and writes the JSON to the package's
//! [`output_root`]. The
//! author states none of them. Consumers read that file, while authored
//! publication references resolve owners from the current source manifest. Every
//! package under `apps/` authors `wamn.k`, and the repository policy lint
//! refuses a root `wamn.json` there. A test fixture keeps its hand-written
//! `wamn.json`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context as _, Result, bail, ensure};

use crate::manifest::PackageManifest;
use crate::output_root;

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
const CLUSTER_MODULE: &str = include_str!("../kcl/cluster/cluster.k");
const CLUSTER_MODULE_DECLARATION: &str = include_str!("../kcl/cluster/kcl.mod");

/// Whether the package authors its manifest in `wamn.k`.
#[must_use]
pub fn is_authored(package_root: &Path) -> bool {
    package_root.join(AUTHORED_MANIFEST).is_file()
}

/// The manifest file that a reader of the package reads: `wamn.json` in the
/// package's [`output_root`] for a package authored in `wamn.k`, otherwise
/// the package's own `wamn.json`.
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
/// the authored package whose `generated/` directory holds a compiled
/// manifest, otherwise the manifest's own directory. A manifest in a build
/// output directory, `target/wamn/<package>`, names that directory, which is
/// its own [`output_root`].
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
/// The bytes decode to a validated [`PackageManifest`].
///
/// # Errors
///
/// When the package also holds a hand-written `wamn.json`, when no `kcl` runs,
/// when KCL refuses the file (the error names its path and line), or when the
/// compiled JSON is not a valid manifest. A statement value that states no
/// type is not valid here: only `wamn build` derives it.
pub fn compile_manifest(package_root: &Path) -> Result<Vec<u8>> {
    let bytes = compile_authored_manifest(package_root)?;
    PackageManifest::from_authored_slice(&bytes).with_context(|| {
        format!(
            "the compiled {} is not a valid manifest",
            package_root.join(AUTHORED_MANIFEST).display()
        )
    })?;
    Ok(bytes)
}

/// Compile a package's `wamn.k` to its authored form: every fact the author
/// states and no derived member
/// (docs/architecture/data-access.md#application-declarations).
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
    Ok(bytes)
}

/// Compile an environment document
/// (docs/architecture/overview.md#environment-lifecycle) to JSON.
///
/// The file imports the `environment` schema module that ships in this crate.
/// The caller parses the JSON bytes into its own type.
///
/// # Errors
///
/// When no `kcl` runs, or when KCL refuses the file. The error names `file`
/// and the line: KCL reports the private copy it compiled, and the error maps
/// that copy's path back to `file`.
pub fn compile_environment(file: &Path) -> Result<Vec<u8>> {
    compile_schema_file(file, "environment")
}

/// Compile a file against a named embedded schema module to JSON.
///
/// The modules are `environment` and `cluster`. The bytes are
/// JSON from the KCL binary.
///
/// # Errors
///
/// When the module is unknown, no `kcl` runs, or KCL refuses the file.
/// The error names the original file and line.
pub fn compile_schema_file(file: &Path, name: &str) -> Result<Vec<u8>> {
    let source = fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let module = match name {
        "environment" => SchemaModule::write(
            "environment",
            ENVIRONMENT_MODULE_DECLARATION,
            ENVIRONMENT_MODULE,
        ),
        "cluster" => SchemaModule::write("cluster", CLUSTER_MODULE_DECLARATION, CLUSTER_MODULE),
        "project" => SchemaModule::write(
            "project",
            include_str!("../kcl/project/kcl.mod"),
            include_str!("../kcl/project/project.k"),
        ),
        _ => bail!("unknown embedded schema module {name:?}"),
    }?;
    // KCL also resolves imports in the document's own directory. A document
    // named after its schema module would clash, so compile a private copy.
    let directory = module.root.join("document");
    fs::create_dir_all(&directory).with_context(|| format!("create {}", directory.display()))?;
    let copy = directory.join("document.k");
    fs::write(&copy, source).with_context(|| format!("copy {}", file.display()))?;
    run_kcl(&module, &directory, Path::new("document.k")).map_err(|error| {
        anyhow::anyhow!(
            "{}",
            format!("{error:#}").replace(&copy.display().to_string(), &file.display().to_string())
        )
    })
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
    Ok(json.into_bytes())
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
    use crate::manifest::CustomOperationResultDeclaration;
    use crate::{GenerationInput, StatementTransactionality, generate};
    use wamn_schema_introspection::ir::CatalogIr;

    #[test]
    fn authored_signed_reads_reach_the_generated_result() {
        let root = std::env::temp_dir().join(format!("wamn-signed-result-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let source = r#"import manifest
manifest.Package {
    package = {id = "files", version = "1.0.0"}
    required_platform_policy_contract = {id = "files_access", state = "satisfied"}
    models = {}
    custom_operations = {
        "file.read" = manifest.Command("file.read") {
            connection = None
            transaction = None
            automatic_retry = None
            idempotent_by = "stateless"
            input.fields = []
            result = {
                class = "one"
                fields = [{path = "document", type = "uuid"}]
                signed_reads = {document = {lifetime_seconds = 300}}
            }
            errors = ["invalid_input", "permission_denied", "internal_error"]
        }
    }
    components = {files = {connections = []}}
}
"#;
        fs::write(root.join(AUTHORED_MANIFEST), source).unwrap();
        let compiled = compile_manifest(&root);
        fs::remove_dir_all(&root).unwrap();
        let bytes = compiled.unwrap();
        let catalog = CatalogIr::new(Vec::new());
        let transactions = StatementTransactionality::unclassified();
        let package =
            generate(&GenerationInput::new(&catalog, &bytes, &[], &transactions)).unwrap();
        let result: CustomOperationResultDeclaration = serde_json::from_slice(
            package
                .file("generated/contracts/file/read.result.json")
                .unwrap()
                .bytes(),
        )
        .unwrap();
        assert_eq!(
            result.signed_reads["document"].lifetime_seconds().unwrap(),
            300
        );

        let document: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        for (member, value) in [
            ("relations", serde_json::json!([])),
            ("feed_source", serde_json::Value::Null),
        ] {
            let mut invalid = document.clone();
            invalid["custom_operations"]["file.read"][member] = value;
            let bytes = serde_json::to_vec(&invalid).unwrap();
            assert!(PackageManifest::from_authored_slice(&bytes).is_err());
            assert!(crate::derive_manifest(&catalog, &bytes, &[], &[]).is_err());
        }
        for seconds in [0, 901] {
            let mut invalid = document.clone();
            invalid["custom_operations"]["file.read"]["result"]["signed_reads"]["document"]["lifetime_seconds"] =
                seconds.into();
            assert!(
                PackageManifest::from_authored_slice(&serde_json::to_vec(&invalid).unwrap())
                    .is_err()
            );
        }
        for (member, value) in [
            ("visibility", serde_json::json!("private")),
            (
                "result.fields",
                serde_json::json!([{ "path": "other", "type": "uuid", "nullable": false }]),
            ),
            (
                "result.fields",
                serde_json::json!([{ "path": "document", "type": "text", "nullable": false }]),
            ),
        ] {
            let mut invalid = document.clone();
            if member == "visibility" {
                invalid["custom_operations"]["file.read"][member] = value;
            } else {
                invalid["custom_operations"]["file.read"]["result"]["fields"] = value;
            }
            assert!(
                PackageManifest::from_authored_slice(&serde_json::to_vec(&invalid).unwrap())
                    .is_err()
            );
        }
        for field in ["workflows", "registration"] {
            let mut invalid = document.clone();
            if field == "workflows" {
                invalid[field] = serde_json::json!({});
            } else {
                invalid["custom_operations"]["file.read"][field] = serde_json::json!({});
            }
            assert!(
                PackageManifest::from_authored_slice(&serde_json::to_vec(&invalid).unwrap())
                    .is_err()
            );
        }
    }
    #[test]
    fn a_compiled_manifest_belongs_to_its_build_output_or_its_unpacked_package() {
        let scratch =
            std::env::temp_dir().join(format!("wamn-authoring-root-{}", std::process::id()));
        let root = scratch.join("package");
        fs::create_dir_all(&root).expect("create the package");
        assert_eq!(package_manifest_path(&root), root.join("wamn.json"));
        assert_eq!(
            manifest_package_root(&root.join("wamn.json")),
            Some(root.as_path())
        );

        fs::write(root.join(AUTHORED_MANIFEST), "").expect("write wamn.k");
        let compiled = output_root(&root).join("wamn.json");
        assert_eq!(compiled, scratch.join("target/wamn/package/wamn.json"));
        assert_eq!(package_manifest_path(&root), compiled);
        let output = manifest_package_root(&compiled).expect("a build output directory");
        assert_eq!(output, scratch.join("target/wamn/package"));
        assert_eq!(output_root(output), output);

        // An unpacked package artifact keeps its generated/ layer paths.
        fs::create_dir_all(root.join("generated")).expect("create generated/");
        let unpacked = root.join("generated/wamn.json");
        assert_eq!(package_manifest_path(&root), unpacked);
        assert_eq!(manifest_package_root(&unpacked), Some(root.as_path()));
        fs::remove_dir_all(&scratch).expect("remove the package");
    }

    #[test]
    fn an_invalid_environment_document_is_refused() {
        let directory =
            std::env::temp_dir().join(format!("wamn-authoring-environment-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let file = directory.join("environment.k");
        fs::write(&file, "a = 1\nb = a +\n").unwrap();
        let result = compile_environment(&file);
        fs::remove_dir_all(directory).unwrap();
        assert!(result.is_err());
    }
}
