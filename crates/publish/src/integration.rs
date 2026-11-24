//! Build digest-addressed wasm integration artifacts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use oci_client::manifest::OCI_IMAGE_MEDIA_TYPE;
use serde_json::json;
use wamn_connection_type::RuntimeType;
use wamn_digest::bytes_sha256;
use wamn_registry::integration_code::IntegrationCode;

pub use wamn_oci::package_artifact::integration::ARTIFACT_TYPE;
use wamn_oci::package_artifact::integration::{CONFIG_TYPE, WASM_TYPE};

/// The immutable output of a local integration build.
#[derive(Debug)]
pub struct BuiltIntegration {
    pub code: IntegrationCode,
    pub runtime_type: RuntimeType,
    pub digest: wamn_digest::Digest,
    pub directory: PathBuf,
}

fn source_files(
    root: &Path,
    directory: &Path,
    files: &mut BTreeMap<String, wamn_digest::Digest>,
) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if matches!(entry.file_name().to_str(), Some("target" | ".git")) {
            continue;
        }
        let type_ = entry.file_type()?;
        ensure!(
            !type_.is_symlink(),
            "integration source contains a symbolic link: {}",
            path.display()
        );
        if type_.is_dir() {
            source_files(root, &path, files)?;
        } else if type_.is_file() {
            files.insert(
                path.strip_prefix(root)?.to_string_lossy().into_owned(),
                bytes_sha256(&std::fs::read(path)?),
            );
        }
    }
    Ok(())
}

fn source_digest(root: &Path) -> anyhow::Result<wamn_digest::Digest> {
    let mut files = BTreeMap::new();
    source_files(root, root, &mut files)?;
    let output = std::process::Command::new("cargo")
        .args([
            "locate-project",
            "--workspace",
            "--message-format",
            "plain",
            "--manifest-path",
        ])
        .arg(root.join("Cargo.toml"))
        .output()
        .context("resolve integration workspace")?;
    ensure!(
        output.status.success(),
        "resolve integration workspace: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let manifest = PathBuf::from(std::str::from_utf8(&output.stdout)?.trim());
    let workspace = manifest
        .parent()
        .context("integration workspace directory")?;
    for name in ["Cargo.toml", "Cargo.lock"] {
        files.insert(
            format!("workspace/{name}"),
            bytes_sha256(&std::fs::read(workspace.join(name))?),
        );
    }
    Ok(bytes_sha256(&wamn_digest::canonical_json_bytes(&json!(
        files
    ))))
}

/// Build an integration for `wasm32-wasip2` and write its artifact locally.
///
/// # Errors
///
/// Refuses failed builds and outputs that are not components.
pub async fn build(repository: &Path, code: &IntegrationCode) -> anyhow::Result<BuiltIntegration> {
    let repository = repository.canonicalize()?;
    let guest = repository.join("apps/guests").join(code.as_str());
    let source = if guest.join("Cargo.toml").is_file() {
        guest
    } else {
        repository.join("integrations").join(code.as_str())
    };
    let root = source
        .canonicalize()
        .with_context(|| format!("find integration {code}"))?;
    let manifest_path = root.join("Cargo.toml");
    let inputs = source_digest(&root)?;
    let component_path = wamn_build::build_component(&repository, &manifest_path).await?;
    let component = std::fs::read(component_path)?;
    ensure!(
        wasmparser::Parser::is_component(&component),
        "integration output is not a WebAssembly component"
    );
    wasmparser::Validator::new()
        .validate_all(&component)
        .context("invalid integration component")?;
    ensure!(
        inputs == source_digest(&root)?,
        "integration sources changed during the build"
    );
    let config = wamn_digest::canonical_json_bytes(&json!({
        "code":code, "runtime_type":RuntimeType::Wasm, "source_digest":inputs
    }));
    let manifest = wamn_digest::canonical_json_bytes(&json!({
        "schemaVersion":2, "mediaType":OCI_IMAGE_MEDIA_TYPE, "artifactType":ARTIFACT_TYPE,
        "config":{"mediaType":CONFIG_TYPE,"digest":bytes_sha256(&config),"size":config.len()},
        "layers":[{"mediaType":WASM_TYPE,"digest":bytes_sha256(&component),"size":component.len()}]
    }));
    let digest = bytes_sha256(&manifest);
    let directory = repository
        .join("integrations/target/wamn")
        .join(code.as_str())
        .join(digest.hex());
    std::fs::create_dir_all(&directory)?;
    for (name, bytes) in [
        ("integration.wasm", &component),
        ("config.json", &config),
        ("manifest.json", &manifest),
    ] {
        std::fs::write(directory.join(name), bytes)?;
    }
    Ok(BuiltIntegration {
        code: code.clone(),
        runtime_type: RuntimeType::Wasm,
        digest,
        directory,
    })
}
