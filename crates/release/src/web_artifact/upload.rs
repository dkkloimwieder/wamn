//! Upload pinned browser files to object storage.

use std::path::Path;

use anyhow::{Context as _, ensure};
use object_store::ObjectStore;
use wamn_storage::publication;

use super::{
    Scheme, Sink, UploadedClient, WEB_MANIFEST_FILE, WebArtifactError, WebArtifactErrorType,
    WebArtifactReader, content_type, validate_path,
};

fn web_dist_files(tar: &[u8]) -> anyhow::Result<std::collections::BTreeMap<String, Vec<u8>>> {
    use std::io::Read as _;

    let mut archive = tar::Archive::new(tar);
    let mut files = std::collections::BTreeMap::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if path == Path::new(WEB_MANIFEST_FILE) {
            continue;
        }
        ensure!(
            path.components()
                .all(|part| matches!(part, std::path::Component::Normal(_))),
            "invalid web artifact path {}",
            path.display()
        );
        let relative = path
            .strip_prefix("dist")
            .with_context(|| format!("web artifact path {} is outside dist", path.display()))?;
        if entry.header().entry_type().is_dir() {
            continue;
        }
        ensure!(
            entry.header().entry_type().is_file(),
            "web artifact path {} is not a regular file",
            path.display()
        );
        let name = relative
            .to_str()
            .context("web artifact paths must be UTF-8")?;
        validate_path(name)?;
        content_type(name)?;
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        ensure!(
            files.insert(name.to_owned(), bytes).is_none(),
            "web artifact repeats {name}"
        );
    }
    ensure!(
        files.contains_key("index.html"),
        "web artifact has no dist/index.html"
    );
    Ok(files)
}

/// Copy a pinned web artifact to the bucket without replacing existing objects.
///
/// # Errors
///
/// Refuses invalid artifacts, registry failures, or conflicting bucket objects.
pub async fn upload_web_artifact(
    artifacts: &dyn WebArtifactReader,
    digest: &wamn_digest::Digest,
    bucket: &str,
) -> Result<UploadedClient, WebArtifactError> {
    async {
        let sink = Sink::parse(bucket)?;
        let store = store(&sink)?;
        let (package, root, written) =
            upload_web_to_store(artifacts, digest, sink.prefix, store.as_ref()).await?;
        Ok(UploadedClient {
            package,
            location: format!("{}{}/{root}/", sink.scheme.prefix(), sink.bucket),
            written,
        })
    }
    .await
    .map_err(|source| WebArtifactError {
        type_: WebArtifactErrorType::Upload,
        context: format!("{}@{digest} to {bucket}", artifacts.artifact_base()),
        source,
    })
}

/// Copy a verified web archive and write its completion index last.
pub async fn upload_web_to_store(
    artifacts: &dyn WebArtifactReader,
    digest: &wamn_digest::Digest,
    prefix: &str,
    store: &dyn ObjectStore,
) -> anyhow::Result<(wamn_package::PackageName, String, bool)> {
    let (metadata, tar) = artifacts.read_web(digest).await?;
    let package = metadata.package.package_id();
    let root = [
        prefix.trim_matches('/'),
        "clients",
        package.as_str(),
        &digest.hex(),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join("/");
    if publication::exists(store, &format!("{root}/index.html"))
        .await
        .context("read the web upload completion index")?
    {
        return Ok((package.clone(), root, false));
    }
    let mut files = web_dist_files(&tar)?;
    let index = files
        .remove("index.html")
        .context("web artifact has no dist/index.html")?;
    for (name, bytes) in files {
        publication::put(
            store,
            &format!("{root}/{name}"),
            bytes,
            content_type(&name)?,
        )
        .await?;
    }
    publication::put(
        store,
        &format!("{root}/index.html"),
        index,
        content_type("index.html")?,
    )
    .await?;
    Ok((package.clone(), root, true))
}

fn store(sink: &Sink<'_>) -> anyhow::Result<Box<dyn ObjectStore>> {
    match sink.scheme {
        Scheme::S3 => publication::s3(sink.bucket),
        Scheme::Gcs => publication::gcs(sink.bucket),
    }
}

/// Upload a verified platform UI digest without reading a checkout or build output.
///
/// # Errors
/// Refuses invalid artifacts, failed transfers, or conflicting bucket objects.
pub async fn upload_platform_ui(
    artifacts: &dyn WebArtifactReader,
    digest: &wamn_digest::Digest,
    bucket: &str,
) -> Result<UploadedClient, WebArtifactError> {
    async {
        let sink = Sink::parse(bucket)?;
        let store = store(&sink)?;
        let (root, written) =
            upload_platform_ui_to_store(artifacts, digest, sink.prefix, store.as_ref()).await?;
        Ok(UploadedClient {
            package: "platform-ui".parse().unwrap(),
            location: format!("{}{}/{root}/", sink.scheme.prefix(), sink.bucket),
            written,
        })
    }
    .await
    .map_err(|source| WebArtifactError {
        type_: WebArtifactErrorType::Upload,
        context: format!("{}@{digest} to {bucket}", artifacts.artifact_base()),
        source,
    })
}

/// Copy a verified platform UI artifact to an object store.
///
/// # Errors
/// Refuses invalid artifacts before writing, or conflicting existing objects.
pub async fn upload_platform_ui_to_store(
    artifacts: &dyn WebArtifactReader,
    digest: &wamn_digest::Digest,
    prefix: &str,
    store: &dyn ObjectStore,
) -> anyhow::Result<(String, bool)> {
    let tar = artifacts.read_platform_ui(digest).await?;
    let mut files = web_dist_files(&tar)?;
    let index = files
        .remove("index.html")
        .expect("verified web archive has an index");
    let base = prefix.trim_matches('/');
    let base = if base.is_empty() {
        String::new()
    } else {
        format!("{base}/")
    };
    let root = format!("{base}platform-ui/sha256-{}", digest.hex());
    if publication::exists(store, &format!("{root}/index.html"))
        .await
        .context("read the platform UI completion index")?
    {
        return Ok((root, false));
    }
    for (name, bytes) in files {
        publication::put(
            store,
            &format!("{root}/{name}"),
            bytes,
            content_type(&name)?,
        )
        .await?;
    }
    publication::put(
        store,
        &format!("{root}/index.html"),
        index,
        content_type("index.html")?,
    )
    .await?;
    Ok((root, true))
}
