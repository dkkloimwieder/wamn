//! Live PostgreSQL 18 tests of `wamn build` (docs/plan/platform-deploy.md §7.1, R10).
//!
//! Each test copies the platform fixture, without its committed `generated/`,
//! into a directory of its own and builds it against the server of the test
//! process. The build creates and drops its own verification database there.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use wamn_schema_generator::build::{BuildRequest, build_package, package_output};
use wamn_schema_generator::receipt;

/// A test-owned copy of the fixture and its overlay, side by side.
struct Workspace {
    root: PathBuf,
}

impl Workspace {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("wamn-build-live-{}-{name}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).expect("remove a stale workspace");
        }
        for (source, package) in [
            (
                wamn_fixture_package::package_root(),
                wamn_fixture_package::PACKAGE_ID,
            ),
            (
                wamn_fixture_package::overlay_root(),
                wamn_fixture_package::OVERLAY_PACKAGE_ID,
            ),
        ] {
            copy_source(&source, &root.join(package));
        }
        Self { root }
    }

    fn package(&self) -> PathBuf {
        self.root.join(wamn_fixture_package::PACKAGE_ID)
    }

    fn overlay(&self) -> PathBuf {
        self.root.join(wamn_fixture_package::OVERLAY_PACKAGE_ID)
    }

    /// Replace one exact text of the fixture's `wamn.k`.
    fn edit_manifest(&self, from: &str, to: &str) {
        let path = self.package().join("wamn.k");
        let source = fs::read_to_string(&path).expect("read the fixture wamn.k");
        assert_eq!(source.matches(from).count(), 1, "{from} is not unique");
        fs::write(&path, source.replace(from, to)).expect("write the fixture wamn.k");
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove the workspace");
    }
}

/// Copy a package's source tree: everything except build output.
fn copy_source(source: &Path, target: &Path) {
    fs::create_dir_all(target).expect("create the package copy");
    for entry in fs::read_dir(source).expect("read the package") {
        let entry = entry.expect("read a package entry");
        let name = entry.file_name();
        if name == "generated" || name == "target" {
            continue;
        }
        let from = entry.path();
        let to = target.join(&name);
        if entry.file_type().expect("inspect a package entry").is_dir() {
            copy_source(&from, &to);
        } else {
            fs::copy(&from, &to).expect("copy a package file");
        }
    }
}

/// Every file below `root`, by its path relative to `root`.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("read an output directory") {
            let path = entry.expect("read an output entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let bytes = fs::read(&path).expect("read an output file");
                files.insert(path.strip_prefix(root).unwrap().to_owned(), bytes);
            }
        }
    }
    files
}

async fn build(package: &Path, output_root: &Path, database: &str) -> anyhow::Result<Value> {
    build_package(&BuildRequest::new(package, output_root, database)).await?;
    let path = package_output(output_root, wamn_fixture_package::PACKAGE_ID).join("wamn.json");
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn statement<'a>(manifest: &'a Value, operation: &str, name: &str) -> &'a Value {
    &manifest["custom_operations"][operation]["statements"][name]
}

/// R10(2) and R10(3): the copy holds no generated verifier, so pass one
/// describes without one; two builds give the same bytes and receipts; the
/// manifest is canonical and hashed once; an overlay records its base.
#[tokio::test(flavor = "current_thread")]
async fn two_builds_give_identical_outputs_and_receipts_without_a_verifier() {
    let workspace = Workspace::new("identical");
    let database = wamn_test_postgres::database();
    assert!(!workspace.package().join("generated").exists());

    let first = workspace.root.join("target-first");
    let second = workspace.root.join("target-second");
    let receipt_first = build_package(&BuildRequest::new(
        &workspace.package(),
        &first,
        database.url(),
    ))
    .await
    .expect("build the fixture");
    let receipt_second = build_package(&BuildRequest::new(
        &workspace.package(),
        &second,
        database.url(),
    ))
    .await
    .expect("build the fixture again");

    let output_first = package_output(&first, wamn_fixture_package::PACKAGE_ID);
    let output_second = package_output(&second, wamn_fixture_package::PACKAGE_ID);
    let files = snapshot(&output_first);
    assert_eq!(files, snapshot(&output_second));
    assert_eq!(receipt_first, receipt_second);
    assert_eq!(receipt_first, receipt::read(&output_first).unwrap());
    assert!(
        files.keys().any(|path| path.starts_with("native-verifier")),
        "pass two generated no verifier"
    );
    assert!(!workspace.package().join("generated").exists());

    let manifest = &files[Path::new("wamn.json")];
    let value: Value = serde_json::from_slice(manifest).unwrap();
    assert_eq!(
        manifest,
        &wamn_execution_contract::canonical_json_bytes(&value),
        "wamn.json is not canonical"
    );
    let digest = receipt_first.outputs.manifest.clone();
    let data_access: Value =
        serde_json::from_slice(&files[Path::new("platform-policy/data-access.json")]).unwrap();
    assert_eq!(data_access["manifest_sha256"], digest.as_str());
    assert_eq!(
        receipt_first.outputs.files.len(),
        files.len() - 1,
        "the receipt lists every output but itself"
    );
    assert_eq!(
        receipt_first.inputs.digest,
        receipt::input_digest(&workspace.package()).unwrap()
    );

    let overlay = build_package(&BuildRequest::new(
        &workspace.overlay(),
        &first,
        database.url(),
    ))
    .await
    .expect("build the overlay over the built base");
    assert_eq!(overlay.dependencies.len(), 1);
    assert_eq!(overlay.dependencies[0].id, wamn_fixture_package::PACKAGE_ID);
    assert_eq!(overlay.dependencies[0].digest, digest);
}

/// §6.1 row 3 with the owner's nullability rule (wamn-00rts.3): a left-out
/// type is derived; a nullable declaration over a column that SQLx says cannot
/// be null is kept; a not-null declaration over a column that SQLx says can be
/// null is refused.
#[tokio::test(flavor = "current_thread")]
async fn statement_types_are_derived_and_nullability_only_widens() {
    let database = wamn_test_postgres::database();

    let accepted = Workspace::new("accepted");
    accepted.edit_manifest(
        "row = [manifest.uuid(\"widget_id\")]",
        "row = [manifest.Value {name = \"widget_id\"}]",
    );
    accepted.edit_manifest(
        "manifest.int64(\"edit_version\"), manifest.nullable",
        "manifest.nullable(manifest.int64(\"edit_version\")), manifest.nullable",
    );
    let manifest = build(
        &accepted.package(),
        &accepted.root.join("target"),
        database.url(),
    )
    .await
    .expect("build with a left-out type and a nullable declaration over NOT NULL");
    let record_batch = statement(&manifest, "widget.record_batch", "find_widget");
    assert_eq!(record_batch["row"][0]["type"], "uuid");
    let archive = statement(&manifest, "widget.archive", "archive");
    assert_eq!(archive["row"][1]["name"], "edit_version");
    assert_eq!(archive["row"][1]["type"], "int64");
    assert_eq!(archive["row"][1]["nullable"], true);

    let refused = Workspace::new("refused");
    refused.edit_manifest(
        "manifest.nullable(manifest.text(\"note\"))",
        "manifest.text(\"note\")",
    );
    let error = build(
        &refused.package(),
        &refused.root.join("target"),
        database.url(),
    )
    .await
    .expect_err("a not-null declaration on a nullable column was accepted");
    assert!(
        format!("{error:#}").contains("row value note is declared not null"),
        "{error:#}"
    );
}
