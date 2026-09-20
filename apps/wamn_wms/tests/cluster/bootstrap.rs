//! Private WMS inputs for the existing disposable service lifecycle.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, ensure};
use object_store::aws::{AmazonS3, AmazonS3Builder};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::{Value, json};
use wamn_test_infrastructure::rendering::render_kind_cluster;
pub(super) use wamn_test_infrastructure::workload::{kind_address, postgres_host_port};

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
pub(super) fn prepare(repository: &Path, work: &Path) -> anyhow::Result<BootstrapFiles> {
    let metadata = fs::metadata(work).context("read the owned WMS work directory")?;
    ensure!(
        metadata.is_dir() && metadata.permissions().mode().trailing_zeros() >= 6,
        "the WMS work directory must be private"
    );
    let kind = render_kind_cluster(&fs::read_to_string(
        repository.join("deploy/infra/kind-config.yaml"),
    )?)?;
    for directory in ["registry", "docker"] {
        DirBuilder::new()
            .mode(0o700)
            .create(work.join(directory))
            .with_context(|| format!("create the private WMS {directory} directory"))?;
    }
    write_private(&work.join("kind.yaml"), kind.as_bytes())?;
    let files = BootstrapFiles {
        registry_password: random_password()?,
        minio_password: random_password()?,
    };
    let mut child = Command::new(repository.join("tools/wms-cluster-journey-run"))
        .args(["registry-password", REGISTRY_USERNAME])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("start the existing registry password helper")?;
    let written = child
        .stdin
        .take()
        .context("open the registry password input")
        .and_then(|mut input| {
            writeln!(input, "{}", files.registry_password)
                .context("write the registry password input")
        });
    let output = child
        .wait_with_output()
        .context("wait for the registry password helper")?;
    written?;
    ensure!(
        output.status.success(),
        "the registry password helper failed: {}",
        output.status
    );
    write_private(&work.join("registry/htpasswd"), &output.stdout)?;
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

/// Write OCI credentials once the owned registry's address is known.
pub(super) fn write_registry_auth(
    files: &BootstrapFiles,
    work: &Path,
    authority: &str,
) -> anyhow::Result<PathBuf> {
    let path = work.join("docker/config.json");
    let value = json!({"auths":{authority:{
        "username":REGISTRY_USERNAME,"password":files.registry_password,
    }}});
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
