//! Private WMS inputs for the existing disposable service lifecycle.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use object_store::aws::{AmazonS3, AmazonS3Builder};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::{Value, json};
pub(super) use wamn_test_infrastructure::workload::kind_address;

const REGISTRY_USERNAME: &str = "wamn-wms-journey";
const MINIO_ACCESS_KEY: &str = "wamn-labels-store";

pub(super) struct BootstrapFiles {
    registry_password: String,
    minio_password: String,
}

impl std::fmt::Debug for BootstrapFiles {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BootstrapFiles")
            .finish_non_exhaustive()
    }
}

/// Prepare private files without creating the application service containers.
pub(super) async fn prepare(work: &Path) -> anyhow::Result<BootstrapFiles> {
    let metadata = fs::metadata(work).context("read the owned WMS work directory")?;
    ensure!(
        metadata.is_dir() && metadata.permissions().mode().trailing_zeros() >= 6,
        "the WMS work directory must be private"
    );
    for directory in ["registry", "docker"] {
        DirBuilder::new()
            .mode(0o700)
            .create(work.join(directory))
            .with_context(|| format!("create the private WMS {directory} directory"))?;
    }
    let files = BootstrapFiles {
        registry_password: random_password()?,
        minio_password: random_password()?,
    };
    let hash = wamn_test_infrastructure::cluster::registry_password(
        REGISTRY_USERNAME,
        &files.registry_password,
    )
    .await?;
    write_private(&work.join("registry/htpasswd"), &hash)?;
    write_private(
        &work.join("minio.env"),
        format!(
            "MINIO_ROOT_USER={MINIO_ACCESS_KEY}\nMINIO_ROOT_PASSWORD={}\nMC_HOST_labels=http://{MINIO_ACCESS_KEY}:{}@127.0.0.1:9000\n",
            files.minio_password, files.minio_password,
        )
        .as_bytes(),
    )?;
    Ok(files)
}

pub(super) fn random_password() -> anyhow::Result<String> {
    let mut bytes = [0u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("generate a private WMS service password"))?;
    Ok(hex::encode(bytes))
}

fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create private file {}", path.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("write private file {}", path.display()))
}

/// Write the OCI credential of the owned registry once its authority is known.
pub(super) fn write_registry_auth(
    files: &BootstrapFiles,
    work: &Path,
    authority: &str,
) -> anyhow::Result<PathBuf> {
    let path = work.join("docker/config.json");
    let credentials = json!({"username":REGISTRY_USERNAME,"password":files.registry_password});
    let value = json!({"auths":{authority:credentials}});
    write_private(&path, &serde_json::to_vec(&value)?)?;
    Ok(path)
}

/// Build the labels client with the same private credential the host receives.
pub(super) fn object_store(files: &BootstrapFiles, endpoint: &str) -> anyhow::Result<AmazonS3> {
    AmazonS3Builder::new()
        .with_endpoint(endpoint)
        .with_bucket_name("labels")
        .with_access_key_id(MINIO_ACCESS_KEY)
        .with_secret_access_key(&files.minio_password)
        .with_region("us-east-1")
        .with_allow_http(endpoint.starts_with("http://"))
        .with_virtual_hosted_style_request(false)
        .build()
        .context("configure the owned WMS labels client")
}

/// Supply the existing labels-store Secret body to the application setup.
pub(super) fn object_store_credentials(files: &BootstrapFiles) -> Value {
    json!({"ACCESS_KEY_ID":MINIO_ACCESS_KEY,"ACCESS_SECRET_KEY":files.minio_password})
}
