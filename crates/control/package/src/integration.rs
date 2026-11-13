//! Build and publish digest-addressed wasm integration artifacts.

pub(super) mod plan;
use plan::require_code;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use oci_client::manifest::OCI_IMAGE_MEDIA_TYPE;
use serde_json::{Value, json};
use wamn_catalog::RuntimeType;
use wamn_digest::bytes_sha256;
use wamn_package_artifact::{PackageRegistry, Repository};

pub use wamn_package_artifact::integration::ARTIFACT_TYPE;
use wamn_package_artifact::integration::{CONFIG_TYPE, WASM_TYPE};

/// The immutable output of a local integration build.
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

fn source_digest(root: &Path) -> anyhow::Result<String> {
    let mut files = BTreeMap::new();
    source_files(root, root, &mut files)?;
    if let Some(workspace) = root
        .parent()
        .filter(|path| path.join("Cargo.toml").is_file())
    {
        for name in ["Cargo.toml", "Cargo.lock"] {
            files.insert(
                format!("../{name}"),
                bytes_sha256(&std::fs::read(workspace.join(name))?),
            );
        }
    }
    Ok(bytes_sha256(&wamn_digest::canonical_json_bytes(&json!(
        files
    ))))
}

/// Build an integration for `wasm32-wasip2` and write its artifact locally.
///
/// # Errors
///
/// Refuses invalid codes, failed builds, and outputs that are not components.
pub async fn build(repository: &Path, code: &str) -> anyhow::Result<BuiltIntegration> {
    require_code(code)?;
    let repository = repository.canonicalize()?;
    let guest = repository.join("apps/platform/no-std").join(code);
    let source = if guest.join("Cargo.toml").is_file() {
        guest
    } else {
        repository.join("integrations").join(code)
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
        .join(code)
        .join(&digest[7..]);
    std::fs::create_dir_all(&directory)?;
    for (name, bytes) in [
        ("integration.wasm", &component),
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

/// The immutable schema and operations of one published version.
#[derive(Debug)]
pub struct Publication {
    pub version: i32,
    pub parameter_schema: Value,
    pub operations: String,
    pub write_interfaces: Vec<String>,
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
    publication: &Publication,
) -> anyhow::Result<()> {
    let exports = crate::connection_requirement::exported_interfaces(&built.component)?;
    plan::require_write_interfaces(publication, &exports)?;
    let repository = Repository::open(registry, &built.digest)?;
    // Authenticate before the low-level blob and manifest writes.
    repository.manifest().await?;
    for bytes in [&built.component, &built.config] {
        repository
            .client
            .push_blob(&repository.reference, bytes.clone(), &bytes_sha256(bytes))
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
        .context("published integration is absent")?;
    plan::require_published_manifest(built, &manifest, &digest)?;
    let pulled =
        wamn_package_artifact::integration::pull(registry, &built.code, &built.digest).await?;
    plan::require_published_component(built, &pulled)?;
    let engine = wamn_engine::build_engine(&[])?;
    wamn_engine::component_admission::validate_integration_admission(
        &engine,
        &pulled,
        &built.code,
    )?;
    let mut system = crate::database::connect(database_url).await?;
    let tx = system.transaction().await?;
    tx.batch_execute("SET LOCAL ROLE wamn_system").await?;
    tx.query_one(
        wamn_registry::sql::CONTROL_PACKAGE_SRC_INTEGRATION_PUBLISH,
        &[
            &built.code,
            &publication.version,
            &built.digest,
            &publication.parameter_schema.to_string(),
            &publication.operations,
            &exports,
            &publication.write_interfaces,
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
