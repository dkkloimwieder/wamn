//! The stand-in artifact digest of a fixture tree that a test applies as a
//! release.

use std::path::Path;

use anyhow::Context as _;

/// The stand-in artifact digest of the package tree at `root`.
///
/// No registry records it. It is the SHA-256 of the length-framed `wamn.json`
/// and migration bytes, so a test that changes those bytes under one
/// coordinate presents another digest, as a second push would. It is not the
/// package artifact digest of the tree
/// (`wamn_control::package_artifact::package_artifact_digest`).
///
/// # Errors
///
/// When the tree cannot be read.
pub fn fixture_artifact_digest(root: &Path) -> anyhow::Result<String> {
    wamn_schema_generator::receipt::check_current(root)?;
    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
    let mut frame = |bytes: &[u8]| {
        context.update(&u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
        context.update(bytes);
    };
    let manifest_path = wamn_schema_generator::package_manifest_path(root);
    frame(
        &std::fs::read(&manifest_path)
            .with_context(|| format!("read {}", manifest_path.display()))?,
    );
    let migrations = root.join("migrations");
    let mut names = Vec::new();
    if migrations.exists() {
        for entry in std::fs::read_dir(&migrations)
            .with_context(|| format!("read {}", migrations.display()))?
        {
            names.push(
                entry?
                    .file_name()
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("package migration file name is not UTF-8"))?,
            );
        }
    }
    names.sort_unstable();
    for name in names {
        frame(format!("migrations/{name}").as_bytes());
        let path = migrations.join(&name);
        frame(&std::fs::read(&path).with_context(|| format!("read {}", path.display()))?);
    }
    Ok(format!("sha256:{}", hex::encode(context.finish())))
}

/// Record output that a test writes, without compiling a guest.
pub fn write_receipt(root: &Path) {
    use wamn_schema_generator::receipt;

    let manifest_path = wamn_schema_generator::package_manifest_path(root);
    let manifest_bytes = std::fs::read(manifest_path).unwrap();
    // Refusal tests can deliberately write an invalid package declaration.
    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
    let output = wamn_schema_generator::output_root(root);
    std::fs::create_dir_all(&output).unwrap();
    let receipt = receipt::BuildReceipt {
        schema: receipt::RECEIPT_SCHEMA.to_owned(),
        package: receipt::ReceiptPackage {
            id: manifest["package"]["id"]
                .as_str()
                .expect("fixture package id")
                .to_owned(),
            version: manifest["package"]["version"]
                .as_str()
                .expect("fixture package version")
                .to_owned(),
        },
        inputs: receipt::ReceiptInputs {
            digest: receipt::input_digest(root).unwrap(),
            files: std::collections::BTreeMap::new(),
        },
        dependencies: Vec::new(),
        generator: receipt::ReceiptGenerator {
            id: "test".into(),
            toolchain: "test".into(),
            sqlx: "0.9.0".into(),
        },
        outputs: receipt::ReceiptOutputs {
            manifest: format!(
                "sha256:{}",
                hex::encode(ring::digest::digest(&ring::digest::SHA256, &manifest_bytes))
            ),
            files: std::collections::BTreeMap::new(),
            components: Vec::new(),
            web: None,
        },
    };
    std::fs::write(output.join(receipt::RECEIPT_FILE), receipt.to_bytes()).unwrap();
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_fixture_receipt_matches_its_tree_and_refuses_a_later_edit() {
        let scratch = crate::scratch::ScratchRoot::create().unwrap();
        let package = scratch.path().join("package");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(
            package.join("wamn.json"),
            r#"{"package":{"id":"fixture","version":"1.0.0"}}"#,
        )
        .unwrap();
        super::write_receipt(&package);
        wamn_schema_generator::receipt::check_current(&package).unwrap();
        std::fs::write(package.join("statement.sql"), "SELECT 1").unwrap();
        let error = wamn_schema_generator::receipt::check_current(&package).unwrap_err();
        assert!(error.to_string().contains("is stale"), "{error:#}");
    }
}
