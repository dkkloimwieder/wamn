//! The authored manifest, `wamn.k`, and its compiled form, `wamn.json` in the
//! package's build output (logical path `generated/wamn.json`).
//!
//! A package authors its manifest in KCL. The generator compiles `wamn.k` with
//! the pinned `kcl` CLI against the `manifest` schema module that ships in this
//! crate. Generation adds the members it derives, such as each custom
//! operation's relations, and writes the JSON to the package's
//! [`output_root`]. The
//! author states none of them. Consumers read that file, while authored
//! publication facts come from the compiled operation description. Every
//! package under `apps/` authors `wamn.k`, and the repository policy lint
//! refuses a root `wamn.json` there. Unpacked artifacts retain their compiled `generated/wamn.json`.

#[cfg(test)]
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, ensure};

use crate::output_root;

/// The manifest file an author writes.
pub const AUTHORED_MANIFEST: &str = "wamn.k";
/// The compiled manifest's logical path, relative to the package root. A
/// package artifact carries it at this path. In a package tree the file lies
/// in the package's [`output_root`].
pub const COMPILED_MANIFEST: &str = "generated/wamn.json";
/// The compiled manifest's file name in the package's [`output_root`].
pub(crate) const COMPILED_MANIFEST_NAME: &str = "wamn.json";
/// A source-root JSON manifest is refused.
const HAND_WRITTEN_MANIFEST: &str = "wamn.json";

const SCHEMA_MODULE: &str = include_str!("../kcl/manifest/manifest.k");
const SCHEMA_MODULE_DECLARATION: &str = include_str!("../kcl/manifest/kcl.mod");
/// Whether the package authors its manifest in `wamn.k`.
#[must_use]
pub fn is_authored(package_root: &Path) -> bool {
    package_root.join(AUTHORED_MANIFEST).is_file()
}

/// The compiled manifest in the package's [`output_root`].
#[must_use]
pub fn package_manifest_path(package_root: &Path) -> PathBuf {
    compiled_manifest_path(package_root)
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

/// Whether a directory has package authorship or compiled package output.
#[must_use]
pub fn is_package_root(directory: &Path) -> bool {
    is_authored(directory) || package_manifest_path(directory).is_file()
}

/// Compile package authorship with the SQL facts from its describe evidence.
///
/// # Errors
///
/// Returns an error if KCL or WIT is invalid or a statement description is missing.
pub fn compile_manifest(package_root: &Path) -> Result<Vec<u8>> {
    let source = crate::build::prepare_source(package_root)?;
    let mut descriptions = std::collections::BTreeMap::new();
    if source.document()["custom_operations"]
        .as_object()
        .is_some_and(|operations| {
            operations.values().any(|operation| {
                operation["statements"]
                    .as_object()
                    .is_some_and(|statements| !statements.is_empty())
            })
        })
    {
        let evidence: crate::describe_cache::Evidence =
            serde_json::from_slice(&crate::describe_cache::read(package_root)?)?;
        descriptions = evidence.statements;
    }
    let mut document = source.document().clone();
    crate::build::apply_statement_types(&mut document, &descriptions)?;
    let bytes = serde_json::to_vec(&document)?;
    wamn_component_policy::PackageManifest::from_authored_slice(&bytes)?;
    Ok(bytes)
}

/// Compile a package's `wamn.k` to its authored form: every fact the author
/// states and no derived member
/// (docs/architecture/data-access.md#application-declarations).
///
/// The authored form may leave out the type of a statement parameter or row
/// value, which `wamn build` derives from the statement. It therefore is not
/// yet a [`wamn_component_policy::PackageManifest`], and this function does not parse it as one.
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
    let bytes =
        crate::kcl::compile_package(package_root, SCHEMA_MODULE_DECLARATION, SCHEMA_MODULE)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GenerationInput, StatementTransactionality, generate};
    use wamn_component_policy::PackageManifest;
    use wamn_package::manifest::CustomOperationResultDeclaration;
    use wamn_schema_introspection::ir::CatalogIr;

    #[test]
    #[ignore = "requires: bin:kcl"]
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
            result = {signed_reads = {document = {lifetime_seconds = 300}}}
        }
    }
    components = {files = {}}
}
"#;
        fs::write(root.join(AUTHORED_MANIFEST), source).unwrap();
        let wit = root.join("component/wit/deps/files-file");
        fs::create_dir_all(&wit).unwrap();
        fs::write(wit.join("custom.wit"), r"
interface read {
    use wamn:node/types@0.1.0.{node-context, node-error};
    type uuid = string;
    enum read-request { request }
    record invalid-input-detail { field: string }
    record permission-denied-detail { operation: string }
    variant read-error {
        internal-error,
        invalid-input(invalid-input-detail),
        permission-denied(permission-denied-detail),
    }
    record read-item { request-id: string, input: result<read-request, invalid-input-detail> }
    record read-result { document: uuid }
    record read-outcome { request-id: string, outcome: result<read-result, read-error> }
    run: async func(ctx: node-context, input: list<read-item>) -> result<list<read-outcome>, node-error>;
}
").unwrap();
        let compiled = compile_manifest(&root);
        fs::remove_dir_all(&root).unwrap();
        let bytes = compiled.unwrap();
        let catalog = CatalogIr::new(Vec::new());
        let transactions = StatementTransactionality::from_paths(std::collections::BTreeMap::new());
        let package =
            generate(&GenerationInput::new(&catalog, &bytes, &[], &transactions)).unwrap();
        let result: CustomOperationResultDeclaration =
            serde_json::from_value(package.operations()["file.read"].result.clone()).unwrap();
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
}
