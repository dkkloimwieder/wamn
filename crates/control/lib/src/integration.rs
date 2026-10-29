//! Build and publish digest-addressed wasm connector artifacts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use oci_client::manifest::OCI_IMAGE_MEDIA_TYPE;
use serde_json::{Value, json};
use tokio::process::Command;
use wamn_catalog::RuntimeType;
use wamn_control_provision::WorkloadRoleFamily;
use wamn_package_artifact::{PackageRegistry, Repository, sha256_digest};

/// OCI artifact type for one independently published connector.
pub use wamn_package_artifact::connector::ARTIFACT_TYPE;
use wamn_package_artifact::connector::{CONFIG_TYPE, WASM_TYPE};

/// The immutable output of a local connector build.
#[derive(Debug)]
pub struct BuiltIntegration {
    pub code: String,
    pub runtime_type: RuntimeType,
    pub digest: String,
    pub directory: PathBuf,
    component: Vec<u8>,
    config: Vec<u8>,
    manifest: Vec<u8>,
}

fn check_code(code: &str) -> anyhow::Result<()> {
    ensure!(
        code.bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase())
            && code.bytes().all(|byte| byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'_' | b'-')),
        "integration code must start with a lowercase letter and contain lowercase letters, digits, underscores or hyphens"
    );
    Ok(())
}

fn source_files(
    root: &Path,
    directory: &Path,
    files: &mut BTreeMap<String, String>,
) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if matches!(entry.file_name().to_str(), Some("target" | ".git")) {
            continue;
        }
        let kind = entry.file_type()?;
        ensure!(
            !kind.is_symlink(),
            "connector source contains a symbolic link: {}",
            path.display()
        );
        if kind.is_dir() {
            source_files(root, &path, files)?;
        } else if kind.is_file() {
            files.insert(
                path.strip_prefix(root)?.to_string_lossy().into_owned(),
                sha256_digest(&std::fs::read(path)?),
            );
        }
    }
    Ok(())
}

fn source_digest(root: &Path) -> anyhow::Result<String> {
    let mut files = BTreeMap::new();
    source_files(root, root, &mut files)?;
    Ok(sha256_digest(
        &wamn_execution_contract::canonical_json_bytes(&json!(files)),
    ))
}

/// Build `integrations/<code>` for `wasm32-wasip2` and write its artifact locally.
///
/// # Errors
///
/// Refuses invalid codes, failed builds, and outputs that are not components.
pub async fn build(repository: &Path, code: &str) -> anyhow::Result<BuiltIntegration> {
    check_code(code)?;
    let repository = repository.canonicalize()?;
    let root = repository
        .join("integrations")
        .join(code)
        .canonicalize()
        .with_context(|| format!("find integrations/{code}"))?;
    let manifest_path = root.join("Cargo.toml");
    let inputs = source_digest(&root)?;
    let flags = Command::new(repository.join("tools/guest-rustflags"))
        .current_dir(&repository)
        .output()
        .await
        .context("read guest build flags")?;
    ensure!(
        flags.status.success(),
        "guest-rustflags failed: {}",
        String::from_utf8_lossy(&flags.stderr)
    );
    let output = Command::new("cargo")
        .current_dir(&repository)
        .env("RUSTFLAGS", String::from_utf8(flags.stdout)?.trim())
        .args([
            "build",
            "--locked",
            "--offline",
            "--target",
            "wasm32-wasip2",
            "--message-format",
            "json-render-diagnostics",
            "--manifest-path",
        ])
        .arg(&manifest_path)
        .output()
        .await
        .context("build connector")?;
    ensure!(
        output.status.success(),
        "build integration {code}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut component_path = None;
    for line in output.stdout.split(|byte| *byte == b'\n') {
        let Ok(message) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if message["reason"] == "compiler-artifact"
            && message["manifest_path"]
                .as_str()
                .is_some_and(|path| Path::new(path) == manifest_path)
            && message["target"]["crate_types"]
                .as_array()
                .is_some_and(|types| types.iter().any(|kind| kind == "cdylib"))
        {
            component_path = message["filenames"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .find(|file| {
                    Path::new(file)
                        .extension()
                        .is_some_and(|extension| extension == "wasm")
                })
                .map(PathBuf::from);
        }
    }
    let component = std::fs::read(component_path.context("connector build gave no wasm cdylib")?)?;
    ensure!(
        wasmparser::Parser::is_component(&component),
        "connector output is not a WebAssembly component"
    );
    wasmparser::Validator::new()
        .validate_all(&component)
        .context("invalid connector component")?;
    ensure!(
        inputs == source_digest(&root)?,
        "connector sources changed during the build"
    );
    let config = wamn_execution_contract::canonical_json_bytes(&json!({
        "code":code, "runtime_type":RuntimeType::Wasm, "source_digest":inputs
    }));
    let manifest = wamn_execution_contract::canonical_json_bytes(&json!({
        "schemaVersion":2, "mediaType":OCI_IMAGE_MEDIA_TYPE, "artifactType":ARTIFACT_TYPE,
        "config":{"mediaType":CONFIG_TYPE,"digest":sha256_digest(&config),"size":config.len()},
        "layers":[{"mediaType":WASM_TYPE,"digest":sha256_digest(&component),"size":component.len()}]
    }));
    let digest = sha256_digest(&manifest);
    let directory = repository
        .join("integrations/target/wamn")
        .join(code)
        .join(&digest[7..]);
    std::fs::create_dir_all(&directory)?;
    for (name, bytes) in [
        ("connector.wasm", &component),
        ("config.json", &config),
        ("manifest.json", &manifest),
    ] {
        std::fs::write(directory.join(name), bytes)?;
    }
    Ok(BuiltIntegration {
        code: code.into(),
        runtime_type: RuntimeType::Wasm,
        digest,
        directory,
        component,
        config,
        manifest,
    })
}

/// Push the exact build by digest, read it back, then record its catalog entry.
///
/// # Errors
///
/// Refuses registry failures, changed artifact bytes, and catalog write failures.
pub async fn publish(
    built: &BuiltIntegration,
    registry: &PackageRegistry,
    database_url: &str,
) -> anyhow::Result<()> {
    let repository = Repository::open(registry, &built.digest)?;
    // Authenticate before the low-level blob and manifest writes.
    repository.manifest().await?;
    for bytes in [&built.component, &built.config] {
        repository
            .client
            .push_blob(&repository.reference, bytes.clone(), &sha256_digest(bytes))
            .await?;
    }
    repository
        .client
        .push_manifest_raw(
            &repository.reference,
            built.manifest.clone(),
            reqwest::header::HeaderValue::from_static(OCI_IMAGE_MEDIA_TYPE),
        )
        .await?;
    let (manifest, digest) = repository
        .manifest()
        .await?
        .context("published connector is absent")?;
    ensure!(
        manifest == built.manifest && digest == built.digest,
        "published connector manifest differs from its build"
    );
    let pulled =
        wamn_package_artifact::connector::pull(registry, &built.code, &built.digest).await?;
    ensure!(
        pulled == built.component,
        "published connector bytes differ from its build"
    );
    let mut system = crate::environment::analyse::connect(database_url).await?;
    let tx = system.transaction().await?;
    crate::connection_configuration::use_role(&tx, WorkloadRoleFamily::Control).await?;
    tx.query_one(
        "SELECT registry.publish_integration($1,$2)",
        &[&built.code, &built.digest],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
