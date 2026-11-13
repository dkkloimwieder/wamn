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
/// (`wamn_package_control::package_artifact::package_artifact_digest`).
///
/// # Errors
///
/// When the tree cannot be read.
pub fn fixture_artifact_digest(root: &Path) -> anyhow::Result<String> {
    let mut context = wamn_digest::Hasher::new();
    let mut frame = |bytes: &[u8]| {
        context.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
        context.update(bytes);
    };
    let manifest_path = wamn_compiler::package_manifest_path(root);
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
    Ok(wamn_digest::format_sha256(&context.finalize()))
}

/// Write physical SQL for a synthetic package and its declared relation columns.
pub fn write_physical_schema(
    root: &Path,
    columns: std::collections::BTreeMap<
        String,
        Vec<wamn_schema_control::physical_schema::RelationColumn>,
    >,
) -> anyhow::Result<()> {
    use wamn_schema_control::physical_schema::{PhysicalSchema, render};

    let manifest = wamn_compiler::manifest::PackageManifest::from_slice(&std::fs::read(
        wamn_compiler::package_manifest_path(root),
    )?)?;
    let mut migrations = Vec::new();
    for entry in std::fs::read_dir(root.join("migrations"))? {
        let path = entry?.path();
        if path.extension().is_some_and(|extension| extension == "sql") {
            migrations.push(wamn_schema_control::MigrationSource {
                relative_path: path.strip_prefix(root)?.to_string_lossy().into_owned(),
                bytes: std::fs::read(path)?,
            });
        }
    }
    let statements = render(&(&manifest).into(), &migrations, &columns)?;
    let output = wamn_compiler::output_root(root);
    std::fs::create_dir_all(&output)?;
    std::fs::write(
        output.join("physical-schema.json"),
        serde_json::to_vec(&PhysicalSchema {
            columns,
            statements,
        })?,
    )?;
    Ok(())
}

/// Record output that a test writes, without compiling a guest.
pub fn write_build_record(root: &Path) {
    use wamn_compiler::build_record;

    let manifest_path = wamn_compiler::package_manifest_path(root);
    let manifest_bytes = std::fs::read(manifest_path).unwrap();
    // Refusal tests can deliberately write an invalid package declaration.
    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
    let output = wamn_compiler::output_root(root);
    std::fs::create_dir_all(&output).unwrap();
    let build_record = build_record::BuildRecord {
        schema: build_record::BUILD_RECORD_SCHEMA.to_owned(),
        package: build_record::BuildPackage {
            id: manifest["package"]["id"]
                .as_str()
                .expect("fixture package id")
                .to_owned(),
            version: manifest["package"]["version"]
                .as_str()
                .expect("fixture package version")
                .to_owned(),
        },

        dependencies: Vec::new(),
        generator: build_record::BuildGenerator {
            id: "test".into(),
            toolchain: "test".into(),
            sqlx: "0.9.0".into(),
        },
        outputs: build_record::BuildOutputs {
            manifest: wamn_digest::bytes_sha256(&manifest_bytes),
            files: std::collections::BTreeMap::new(),
            components: Vec::new(),
        },
    };
    std::fs::write(
        output.join(build_record::BUILD_RECORD_FILE),
        build_record.to_bytes(),
    )
    .unwrap();
}
