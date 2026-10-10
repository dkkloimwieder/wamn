//! `build.json`, the content receipt of one package build
//! (docs/plan/platform-deploy.md §7.1 step 9, §7.2).
//!
//! The receipt names the build's inputs, the exact dependency packages, the
//! generator and toolchain identity, and the digest of every output. Its bytes
//! are canonical JSON (RFC 8785). Schema `wamn.build/v1` is frozen: a change of
//! shape is a new schema name.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use wamn_execution_contract::canonical_json_bytes;

use crate::generate::sha256;
use crate::materialize::{GENERATOR_ID, TOOLCHAIN_ID};

/// The receipt's file name in a package's build output directory.
pub const RECEIPT_FILE: &str = "build.json";
/// The one receipt schema this generator writes and reads.
pub const RECEIPT_SCHEMA: &str = "wamn.build/v1";
/// The SQLx version whose describe path derives statement types.
pub(crate) const SQLX_VERSION: &str = "0.9.0";

/// Directories of a package tree that are never build inputs: build output and
/// installed dependencies. `generated` is the committed build output until it
/// leaves git.
const EXCLUDED_DIRECTORIES: &[&str] = &["target", "node_modules", "generated"];
/// The web client's own build output, relative to the package root.
const WEB_DIST: &str = "web/dist";
/// The lock file of the enclosing Cargo workspace, relative to the package root.
const WORKSPACE_LOCK: &str = "../Cargo.lock";

/// One package build: `build.json`, schema `wamn.build/v1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildReceipt {
    /// Always [`RECEIPT_SCHEMA`].
    pub schema: String,
    pub package: ReceiptPackage,
    pub inputs: ReceiptInputs,
    /// The exact base packages, in the order of their dependency names.
    pub dependencies: Vec<ReceiptDependency>,
    pub generator: ReceiptGenerator,
    pub outputs: ReceiptOutputs,
}

/// The package that was built.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptPackage {
    pub id: String,
    pub version: String,
}

/// The build's inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptInputs {
    /// The digest of [`input_digest`]: the files and the generator identity.
    pub digest: String,
    /// The `sha256:` digest of each input file, by its path relative to the
    /// package root.
    pub files: BTreeMap<String, String>,
}

/// One base package of an overlay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptDependency {
    pub id: String,
    pub version: String,
    /// The digest of the base's canonical `wamn.json`, until a package
    /// artifact digest replaces it.
    pub digest: String,
}

/// The generator and toolchain identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptGenerator {
    pub id: String,
    pub toolchain: String,
    pub sqlx: String,
}

/// The build's outputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptOutputs {
    /// The digest of the canonical `wamn.json`, hashed once.
    pub manifest: String,
    /// The `sha256:` digest of each output file, by its path relative to the
    /// package's build output directory. `build.json` is not one of them.
    pub files: BTreeMap<String, String>,
    /// The package's components. Empty until the build compiles them.
    pub components: Vec<ReceiptComponent>,
    /// The digest of the web client build, or `null` while the build does
    /// not make one.
    pub web: Option<String>,
}

/// One built component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptComponent {
    pub name: String,
    pub path: String,
    pub sha256: String,
}

impl BuildReceipt {
    /// The receipt's canonical bytes.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        canonical_json_bytes(
            &serde_json::to_value(self).expect("a build receipt always serializes"),
        )
    }
}

/// The generator identity this build writes.
pub(crate) fn generator() -> ReceiptGenerator {
    ReceiptGenerator {
        id: GENERATOR_ID.to_owned(),
        toolchain: TOOLCHAIN_ID.to_owned(),
        sqlx: SQLX_VERSION.to_owned(),
    }
}

/// Read the receipt in a package's build output directory.
///
/// # Errors
///
/// When the file is missing, is not a `wamn.build/v1` receipt, or is not in
/// canonical form.
pub fn read(package_output: &Path) -> Result<BuildReceipt> {
    let path = package_output.join(RECEIPT_FILE);
    let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let receipt: BuildReceipt = serde_json::from_slice(&bytes)
        .with_context(|| format!("{} is not a build receipt", path.display()))?;
    ensure!(
        receipt.schema == RECEIPT_SCHEMA,
        "{} has schema {}, not {RECEIPT_SCHEMA}",
        path.display(),
        receipt.schema
    );
    ensure!(
        receipt.to_bytes() == bytes,
        "{} is not canonical JSON",
        path.display()
    );
    Ok(receipt)
}

/// The input digest of the package at `package_root`, as `build.json` records it.
///
/// # Errors
///
/// When the package tree cannot be read.
pub fn input_digest(package_root: &Path) -> Result<String> {
    Ok(inputs(package_root)?.digest)
}

/// The build inputs of the package at `package_root`: every file of the package
/// directory except build output and installed dependencies, the lock file of
/// the enclosing Cargo workspace when there is one, and the generator identity.
pub(crate) fn inputs(package_root: &Path) -> Result<ReceiptInputs> {
    let mut files = BTreeMap::new();
    for relative in input_files(package_root)? {
        let key = relative
            .to_str()
            .with_context(|| format!("input path is not UTF-8: {}", relative.display()))?
            .to_owned();
        let path = package_root.join(&relative);
        let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        files.insert(key, sha256(&bytes));
    }
    let lock = package_root.join(WORKSPACE_LOCK);
    if lock.is_file() {
        let bytes = fs::read(&lock).with_context(|| format!("read {}", lock.display()))?;
        files.insert(WORKSPACE_LOCK.to_owned(), sha256(&bytes));
    }
    let digest = sha256(&canonical_json_bytes(&serde_json::json!({
        "files": files,
        "generator": generator(),
    })));
    Ok(ReceiptInputs { digest, files })
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
            let file_type = entry
                .file_type()
                .with_context(|| format!("inspect input {}", entry.path().display()))?;
            if file_type.is_dir() {
                let excluded = EXCLUDED_DIRECTORIES
                    .iter()
                    .any(|name| entry.file_name() == *name)
                    || path == Path::new(WEB_DIST);
                if !excluded {
                    pending.push(path);
                }
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

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("remove the scratch package");
        }
    }

    fn scratch(name: &str) -> Scratch {
        let root = std::env::temp_dir().join(format!(
            "wamn-schema-generator-receipt-{}-{name}",
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
            ("web/dist/index.js", "d"),
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
        assert_eq!(input_digest(&package).unwrap(), inputs.digest);
        fs::write(package.join("wamn.k"), "changed").unwrap();
        assert_ne!(input_digest(&package).unwrap(), inputs.digest);
    }

    #[test]
    fn read_refuses_a_receipt_that_is_not_canonical() {
        let scratch = scratch("read");
        let receipt = BuildReceipt {
            schema: RECEIPT_SCHEMA.to_owned(),
            package: ReceiptPackage {
                id: "p".to_owned(),
                version: "1.0.0".to_owned(),
            },
            inputs: ReceiptInputs {
                digest: sha256(b"i"),
                files: BTreeMap::new(),
            },
            dependencies: Vec::new(),
            generator: generator(),
            outputs: ReceiptOutputs {
                manifest: sha256(b"m"),
                files: BTreeMap::new(),
                components: Vec::new(),
                web: None,
            },
        };
        let bytes = receipt.to_bytes();
        assert!(
            String::from_utf8(bytes.clone())
                .unwrap()
                .contains(r#""web":null"#)
        );
        fs::write(scratch.0.join(RECEIPT_FILE), &bytes).unwrap();
        assert_eq!(read(&scratch.0).unwrap(), receipt);

        let mut pretty = serde_json::to_vec_pretty(&receipt).unwrap();
        pretty.push(b'\n');
        fs::write(scratch.0.join(RECEIPT_FILE), pretty).unwrap();
        let error = read(&scratch.0).expect_err("a pretty receipt was read");
        assert!(error.to_string().contains("not canonical"), "{error:#}");
    }
}
