//! Upload pinned browser files to object storage.

use anyhow::{Context as _, ensure};
use object_store::aws::AmazonS3Builder;
use object_store::gcp::GoogleCloudStorageBuilder;
use object_store::{Attribute, Attributes, ObjectStore, ObjectStoreExt as _, PutMode, PutOptions};
use wamn_release::web_artifact::{
    Scheme, Sink, UploadedClient, WEB_MANIFEST_FILE, WebArtifactError, WebArtifactErrorType,
    WebArtifactReader, content_type, validate_path,
};

use std::path::Path;

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
    let index = object_store::path::Path::from(format!("{root}/index.html"));
    match store.head(&index).await {
        Ok(_) => return Ok((package.clone(), root, false)),
        Err(object_store::Error::NotFound { .. }) => {}
        Err(error) => return Err(error).context("read the web upload completion index"),
    }
    let mut files = web_dist_files(&tar)?;
    let index = files
        .remove("index.html")
        .context("web artifact has no dist/index.html")?;
    for (name, bytes) in files {
        put(store, &root, &name, bytes, content_type(&name)?).await?;
    }
    put(
        store,
        &root,
        "index.html",
        index,
        content_type("index.html")?,
    )
    .await?;
    Ok((package.clone(), root, true))
}

/// Write one object create-only with its content type. An
/// existing object with equal bytes is the write of an earlier run that
/// stopped, so it stays. An existing object with other bytes refuses.
async fn put(
    store: &dyn ObjectStore,
    root: &str,
    relative: &str,
    bytes: Vec<u8>,
    content_type: &'static str,
) -> anyhow::Result<()> {
    let mut attributes = Attributes::new();
    attributes.insert(Attribute::ContentType, content_type.into());
    let key = object_store::path::Path::from(format!("{root}/{relative}"));
    let bytes = object_store::PutPayload::from(bytes);
    let written = store
        .put_opts(
            &key,
            bytes.clone(),
            PutOptions {
                mode: PutMode::Create,
                attributes,
                ..PutOptions::default()
            },
        )
        .await;
    match written {
        Err(
            object_store::Error::AlreadyExists { .. } | object_store::Error::Precondition { .. },
        ) => {
            let existing = store
                .get(&key)
                .await
                .with_context(|| format!("read the existing {key}"))?
                .bytes()
                .await
                .with_context(|| format!("read the existing {key}"))?;
            ensure!(
                existing
                    .as_ref()
                    .iter()
                    .copied()
                    .eq(bytes.iter().flat_map(|part| part.iter().copied())),
                "write {key}; an existing object with other bytes is never replaced"
            );
            Ok(())
        }
        written => written
            .map(|_| ())
            .with_context(|| format!("write {key}; an existing object is never replaced")),
    }
}

/// For `s3://` the `AWS_*` variables supply the endpoint and the
/// credentials. For `gs://` the `GOOGLE_*` variables and Application
/// Default Credentials do.
fn store(sink: &Sink<'_>) -> anyhow::Result<Box<dyn ObjectStore>> {
    Ok(match sink.scheme {
        Scheme::S3 => Box::new(
            AmazonS3Builder::from_env()
                .with_bucket_name(sink.bucket)
                .build()
                .context("configure the bucket from the AWS_* variables")?,
        ),
        Scheme::Gcs => Box::new(
            GoogleCloudStorageBuilder::from_env()
                .with_bucket_name(sink.bucket)
                .build()
                .context(
                    "configure the bucket from the GOOGLE_* variables and \
                         Application Default Credentials",
                )?,
        ),
    })
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
    match store.head(&format!("{root}/index.html").into()).await {
        Ok(_) => return Ok((root, false)),
        Err(object_store::Error::NotFound { .. }) => {}
        Err(error) => return Err(error).context("read the platform UI completion index"),
    }
    for (name, bytes) in files {
        put(store, &root, &name, bytes, content_type(&name)?).await?;
    }
    put(
        store,
        &root,
        "index.html",
        index,
        content_type("index.html")?,
    )
    .await?;
    Ok((root, true))
}
