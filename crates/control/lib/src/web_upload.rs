//! Web client files in a bucket (docs/plan/web-deployment.md).
//!
//! `wamn web upload` writes the built client of each package of the release
//! an environment serves, under `<prefix>/<package_id>/<release hex>/`, every
//! built file create-only, then `config.json`, and the index last. The release
//! must be the one the newest successful revision of the environment's release
//! chart installed, and the built files are the `web` layer of each package
//! artifact, which unpacks to the web build of its output root
//! (docs/plan/platform-deploy.md §13, §7.2, §7.1 step 8).

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail, ensure};
use object_store::aws::AmazonS3Builder;
use object_store::gcp::GoogleCloudStorageBuilder;
use object_store::{Attribute, Attributes, ObjectStore, ObjectStoreExt as _, PutMode, PutOptions};
use wamn_control_registry::Triple;

use crate::environment::Platform;
use crate::package_artifact::{PackageRegistry, PackageSource, open_package_source, web_build};
use crate::web_scope::{SCOPE_CACHE, SCOPE_CONTENT_TYPE, SCOPE_FILE, scope_file_bytes};

/// A built file name carries its content hash, so a browser keeps it.
const ASSET_CACHE: &str = "public, max-age=31536000, immutable";
/// The index names the current files, and the service worker `sw.js` keeps
/// its name across builds, so a browser asks again at every load.
const INDEX_CACHE: &str = "no-cache";
/// The client of one package, as [`upload_release`] left it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadedClient {
    pub package: String,
    /// `<scheme><bucket>/<root>/`.
    pub location: String,
    /// False when the index was already there, so nothing was written.
    pub written: bool,
}

/// Write the built client of each package of `release` that has one.
///
/// # Errors
///
/// When `release` is not the release the environment serves, a package
/// artifact cannot be read or its web build holds no index, or a write fails. An
/// object is never replaced.
pub async fn upload_release(
    platform: &Platform,
    triple: &Triple,
    release: &str,
    bucket: &str,
) -> anyhow::Result<Vec<UploadedClient>> {
    let hex = release_hex(release)?;
    let env_key =
        wamn_control_provision::project_env_key(&triple.org, &triple.project, triple.env.as_str());
    let deployed = crate::environment::analyse::deployed_release(&platform.target, &env_key)
        .await?
        .with_context(|| format!("{triple} serves no release"))?;
    ensure!(
        deployed == release,
        "release {release} is not the release {deployed} that {triple} serves"
    );
    let manifest = crate::environment::analyse::pull_release_manifest(platform, release).await?;
    let registry = PackageRegistry {
        artifact_base: platform
            .package_artifact_base
            .clone()
            .context("set WAMN_PACKAGE_ARTIFACT_BASE; the built clients are package artifacts")?,
        registry_auth_file: platform.registry_auth_file.clone(),
        insecure_registry: false,
        oci_ca_paths: platform.oci_ca_paths.clone(),
    };
    let sink = Sink::parse(bucket)?;
    let store = sink.store()?;
    let mut uploaded = Vec::new();
    for package in &manifest.release.packages {
        let tag = format!("{}-{}", package.package_id(), package.package_version());
        let opened = open_package_source(PackageSource::Artifact {
            tag: tag.clone(),
            registry: registry.clone(),
        })
        .await?;
        let manifest_path = wamn_schema_generator::package_manifest_path(opened.root());
        let compiled: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&manifest_path)
                .with_context(|| format!("read {}", manifest_path.display()))?,
        )
        .with_context(|| format!("decode {}", manifest_path.display()))?;
        let Some(client) = compiled
            .pointer("/client_package/name")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let project = client_project(client)?;
        let dist = web_build(opened.root());
        let Some(mut files) = built_files(&dist, &tag)? else {
            continue;
        };
        let root = client_root(sink.prefix, package.package_id(), hex);
        let location = format!("{}{}/{root}/", sink.scheme.prefix(), sink.bucket);
        let index = object_store::path::Path::from(format!("{root}/index.html"));
        if store.head(&index).await.is_ok() {
            uploaded.push(UploadedClient {
                package: package.package_id().to_owned(),
                location,
                written: false,
            });
            continue;
        }
        write_files(
            store.as_ref(),
            &root,
            &dist,
            &mut files,
            &scope_file_bytes(&triple.org, project),
        )
        .await?;
        uploaded.push(UploadedClient {
            package: package.package_id().to_owned(),
            location,
            written: true,
        });
    }
    Ok(uploaded)
}

/// The platform UI has one bucket and one prefix per platform version.
pub const PLATFORM_UI_BUCKET: &str = "gs://wamn-dev-web";

/// Upload the local platform UI only when its digest matches the stamped chart.
/// The current directory is the repository whose platform UI was built.
pub async fn upload_platform_ui(
    repository: &Path,
    chart: &Path,
    org: &str,
    bucket: &str,
) -> anyhow::Result<String> {
    let (dist, root) = platform_ui_source(repository, chart)?;
    let sink = Sink::parse(bucket)?;
    let root = [sink.prefix.trim_matches('/'), root.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    let store = sink.store()?;
    write_platform_ui(store.as_ref(), &root, &dist, org).await?;
    Ok(format!("{}{}/{root}/", sink.scheme.prefix(), sink.bucket))
}

fn platform_ui_source(repository: &Path, chart: &Path) -> anyhow::Result<(PathBuf, String)> {
    use crate::platform_build::{
        PlatformBuildReceipt, RECEIPT_FILE, platform_ui_digest, platform_ui_dist,
    };
    let receipt_path = chart.join(RECEIPT_FILE);
    let receipt: PlatformBuildReceipt =
        serde_json::from_slice(&std::fs::read(&receipt_path).with_context(|| {
            format!(
                "read {}; run `wamn-ctl build-platform`",
                receipt_path.display()
            )
        })?)?;
    let version = crate::release_chart::chart_version(chart)?;
    let platform =
        crate::release_chart::chart_version(&repository.join("deploy/platform/release"))?;
    ensure!(
        version == platform,
        "the stamped chart version {version} differs from platform {platform}"
    );
    let dist = platform_ui_dist(repository);
    ensure!(
        platform_ui_digest(&dist)? == receipt.platform_ui,
        "the platform UI build {} differs from the platform build receipt {}",
        dist.display(),
        receipt_path.display()
    );
    Ok((dist, format!("platform-ui/{version}")))
}

async fn write_platform_ui(
    store: &dyn ObjectStore,
    root: &str,
    dist: &Path,
    org: &str,
) -> anyhow::Result<()> {
    ensure!(!org.is_empty(), "the platform UI requires an org");
    let mut files = Vec::new();
    collect(dist, &mut files)?;
    write_files(
        store,
        root,
        dist,
        &mut files,
        &crate::web_scope::platform_scope_file_bytes(org),
    )
    .await
}

/// The hexadecimal digits of a release `sha256:<64 lowercase hex>`.
pub fn release_hex(release: &str) -> anyhow::Result<&str> {
    release
        .strip_prefix("sha256:")
        .filter(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .context("--release must be sha256: and 64 lowercase hexadecimal digits")
}

/// The object root of one client: `<prefix>/<package_id>/<release hex>`.
pub fn client_root(prefix: &str, package: &str, hex: &str) -> String {
    [prefix.trim_matches('/'), package, hex]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

/// Write every built file create-only, then `config.json`, and the index
/// last, so no reader meets an index whose files are absent.
pub async fn write_files(
    store: &dyn ObjectStore,
    root: &str,
    dist: &Path,
    files: &mut [PathBuf],
    scope: &[u8],
) -> anyhow::Result<()> {
    files.sort_by_key(|file| file == &dist.join("index.html"));
    for file in files.iter() {
        if file == &dist.join("index.html") {
            put(
                store,
                root,
                SCOPE_FILE,
                scope.to_vec(),
                SCOPE_CACHE,
                SCOPE_CONTENT_TYPE,
            )
            .await?;
        }
        let relative = file
            .strip_prefix(dist)?
            .to_str()
            .context("a built file name is UTF-8")?;
        let cache = if relative == "index.html" || relative == "sw.js" {
            INDEX_CACHE
        } else if relative.starts_with("assets/") {
            ASSET_CACHE
        } else {
            bail!("the build wrote {relative}, which has no declared cache rule")
        };
        put(
            store,
            root,
            relative,
            std::fs::read(file)?,
            cache,
            content_type(relative)?,
        )
        .await?;
    }
    Ok(())
}

/// Write one object create-only with its Cache-Control and content type. An
/// existing object with equal bytes is the write of an earlier run that
/// stopped, so it stays. An existing object with other bytes refuses.
async fn put(
    store: &dyn ObjectStore,
    root: &str,
    relative: &str,
    bytes: Vec<u8>,
    cache: &'static str,
    content_type: &'static str,
) -> anyhow::Result<()> {
    let mut attributes = Attributes::new();
    attributes.insert(Attribute::CacheControl, cache.into());
    attributes.insert(Attribute::ContentType, content_type.into());
    let key = object_store::path::Path::from(format!("{root}/{relative}"));
    let written = store
        .put_opts(
            &key,
            bytes.clone().into(),
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
                existing.as_ref() == bytes.as_slice(),
                "write {key}; an existing object with other bytes is never replaced"
            );
            Ok(())
        }
        written => written
            .map(|_| ())
            .with_context(|| format!("write {key}; an existing object is never replaced")),
    }
}

/// The project that a client package `@wamn/<project>-client` names.
pub fn client_project(client: &str) -> anyhow::Result<&str> {
    client
        .strip_prefix("@wamn/")
        .and_then(|rest| rest.strip_suffix("-client"))
        .filter(|project| !project.is_empty())
        .with_context(|| format!("the client package {client} is not @wamn/<project>-client"))
}

/// The object store that a bucket value names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    /// `s3://`, an S3 API endpoint, as kind uses.
    S3,
    /// `gs://`, Google Cloud Storage with Application Default Credentials.
    Gcs,
}

impl Scheme {
    pub fn prefix(self) -> &'static str {
        match self {
            Self::S3 => "s3://",
            Self::Gcs => "gs://",
        }
    }
}

/// A parsed bucket value, `s3://<bucket>[/<prefix>]` or `gs://<bucket>[/<prefix>]`.
#[derive(Debug, PartialEq, Eq)]
pub struct Sink<'a> {
    pub scheme: Scheme,
    pub bucket: &'a str,
    pub prefix: &'a str,
}

impl<'a> Sink<'a> {
    pub fn parse(value: &'a str) -> anyhow::Result<Self> {
        let (scheme, rest) = [Scheme::S3, Scheme::Gcs]
            .into_iter()
            .find_map(|scheme| {
                value
                    .strip_prefix(scheme.prefix())
                    .map(|rest| (scheme, rest))
            })
            .context("--bucket must be s3://<bucket>[/<prefix>] or gs://<bucket>[/<prefix>]")?;
        let (bucket, prefix) = rest.split_once('/').unwrap_or((rest, ""));
        Ok(Self {
            scheme,
            bucket,
            prefix,
        })
    }

    /// For `s3://` the `AWS_*` variables supply the endpoint and the
    /// credentials. For `gs://` the `GOOGLE_*` variables and Application
    /// Default Credentials do.
    pub fn store(&self) -> anyhow::Result<Box<dyn ObjectStore>> {
        Ok(match self.scheme {
            Scheme::S3 => Box::new(
                AmazonS3Builder::from_env()
                    .with_bucket_name(self.bucket)
                    .build()
                    .context("configure the bucket from the AWS_* variables")?,
            ),
            Scheme::Gcs => Box::new(
                GoogleCloudStorageBuilder::from_env()
                    .with_bucket_name(self.bucket)
                    .build()
                    .context(
                        "configure the bucket from the GOOGLE_* variables and \
                         Application Default Credentials",
                    )?,
            ),
        })
    }
}

/// The built files of a package web build, or `None` when the package has no
/// web app: its empty web layer unpacks to no web build (wamn-21cms).
fn built_files(dist: &Path, tag: &str) -> anyhow::Result<Option<Vec<PathBuf>>> {
    if !dist.is_dir() {
        return Ok(None);
    }
    let mut files = Vec::new();
    collect(dist, &mut files)
        .with_context(|| format!("the package artifact {tag} holds no built web client"))?;
    ensure!(
        files.iter().any(|file| file == &dist.join("index.html")),
        "the package artifact {tag} holds no web client index.html"
    );
    Ok(Some(files))
}

/// Every file under `directory`.
pub fn collect(directory: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(directory)
        .with_context(|| format!("read the build directory {}", directory.display()))?
    {
        let path = entry?.path();
        if path.is_dir() {
            collect(&path, files)?;
        } else {
            files.push(path);
        }
    }
    Ok(())
}

fn content_type(name: &str) -> anyhow::Result<&'static str> {
    let extension = Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    Ok(match extension {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        _ => bail!("the build wrote {name}, which has no declared content type"),
    })
}

#[cfg(test)]
mod tests {
    use object_store::ObjectStoreExt as _;
    use object_store::memory::InMemory;

    use super::{Scheme, Sink, built_files, collect, write_files};

    const HEAD: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SCOPE: &[u8] = br#"{"org":"acme","project":"receiving"}"#;

    #[test]
    fn a_package_without_a_web_build_has_no_files_and_one_without_an_index_refuses() {
        let dist = std::env::temp_dir().join(format!("wamn-web-built-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dist);
        assert!(built_files(&dist, "wamn_wms-2.1.3").unwrap().is_none());
        std::fs::create_dir_all(&dist).unwrap();
        std::fs::write(dist.join("app.js"), "x").unwrap();
        let error = built_files(&dist, "wamn_wms-2.1.3").unwrap_err();
        assert_eq!(
            error.to_string(),
            "the package artifact wamn_wms-2.1.3 holds no web client index.html"
        );
        std::fs::write(dist.join("index.html"), "x").unwrap();
        assert_eq!(
            built_files(&dist, "wamn_wms-2.1.3").unwrap().unwrap().len(),
            2
        );
        std::fs::remove_dir_all(&dist).unwrap();
    }

    #[tokio::test]
    async fn a_rerun_keeps_equal_objects_and_the_service_worker_is_not_cached() {
        let dist = std::env::temp_dir().join(format!("wamn-web-rerun-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dist);
        std::fs::create_dir_all(dist.join("assets")).unwrap();
        std::fs::write(dist.join("index.html"), "index").unwrap();
        std::fs::write(dist.join("sw.js"), "worker").unwrap();
        std::fs::write(dist.join("assets/main.css"), "style").unwrap();
        let store = InMemory::new();
        // An earlier run wrote one asset, then stopped.
        store
            .put(
                &object_store::path::Path::from(format!("{HEAD}/assets/main.css")),
                "style".into(),
            )
            .await
            .unwrap();
        let mut files = Vec::new();
        collect(&dist, &mut files).unwrap();
        write_files(&store, HEAD, &dist, &mut files, SCOPE)
            .await
            .expect("the rerun keeps the equal asset and writes the rest");
        let worker = store
            .get(&object_store::path::Path::from(format!("{HEAD}/sw.js")))
            .await
            .unwrap();
        assert_eq!(
            worker
                .attributes
                .get(&object_store::Attribute::CacheControl)
                .map(AsRef::as_ref),
            Some(super::INDEX_CACHE)
        );
        assert_eq!(&worker.bytes().await.unwrap()[..], b"worker");
        std::fs::remove_dir_all(&dist).unwrap();
    }

    #[tokio::test]
    async fn an_existing_object_is_never_replaced() {
        let dist = std::env::temp_dir().join(format!("wamn-web-upload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dist);
        std::fs::create_dir_all(dist.join("assets")).unwrap();
        std::fs::write(dist.join("index.html"), "first").unwrap();
        std::fs::write(dist.join("assets/app.js"), "first").unwrap();
        let store = InMemory::new();
        let mut files = Vec::new();
        collect(&dist, &mut files).unwrap();
        write_files(&store, HEAD, &dist, &mut files, SCOPE)
            .await
            .expect("the first upload writes");

        std::fs::write(dist.join("index.html"), "second").unwrap();
        std::fs::write(dist.join("assets/app.js"), "second").unwrap();
        let error = write_files(&store, HEAD, &dist, &mut files, SCOPE)
            .await
            .expect_err("the second upload refuses");
        assert!(
            error
                .to_string()
                .starts_with(&format!("write {HEAD}/assets/app.js")),
            "{error}"
        );
        for (name, bytes) in [
            ("index.html", &b"first"[..]),
            ("assets/app.js", b"first"),
            ("config.json", SCOPE),
        ] {
            let stored = store
                .get(&object_store::path::Path::from(format!("{HEAD}/{name}")))
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap();
            assert_eq!(&stored[..], bytes, "{name}");
        }
        std::fs::remove_dir_all(&dist).unwrap();
    }

    #[tokio::test]
    async fn platform_ui_upload_uses_the_receipt_and_a_version_prefix_with_org_only() {
        use crate::platform_build::{
            PlatformBuildReceipt, RECEIPT_FILE, platform_ui_digest, platform_ui_dist,
        };
        let repository =
            std::env::temp_dir().join(format!("wamn-platform-upload-{}", std::process::id()));
        let dist = platform_ui_dist(&repository);
        let chart = repository.join("chart");
        std::fs::create_dir_all(dist.join("assets")).unwrap();
        std::fs::create_dir_all(&chart).unwrap();
        std::fs::write(dist.join("index.html"), b"platform index").unwrap();
        std::fs::write(dist.join("assets/ui.js"), b"platform script").unwrap();
        std::fs::write(chart.join("Chart.yaml"), "version: 0.1.7").unwrap();
        let platform = repository.join("deploy/platform/release");
        std::fs::create_dir_all(&platform).unwrap();
        std::fs::write(platform.join("Chart.yaml"), "version: 0.1.7").unwrap();
        std::fs::write(
            chart.join(RECEIPT_FILE),
            serde_json::to_vec(&PlatformBuildReceipt {
                platform_ui: platform_ui_digest(&dist).unwrap(),
            })
            .unwrap(),
        )
        .unwrap();
        let (source, root) = super::platform_ui_source(&repository, &chart).unwrap();
        assert_eq!(source, dist);
        assert_eq!(root, "platform-ui/0.1.7");
        assert_eq!(super::PLATFORM_UI_BUCKET, "gs://wamn-dev-web");
        let store = InMemory::new();
        super::write_platform_ui(&store, &root, &source, "dkk")
            .await
            .unwrap();
        for (name, bytes) in [
            ("index.html", &b"platform index"[..]),
            ("assets/ui.js", &b"platform script"[..]),
            ("config.json", &br#"{"org":"dkk"}"#[..]),
        ] {
            let stored = store
                .get(&object_store::path::Path::from(format!("{root}/{name}")))
                .await
                .unwrap();
            assert_eq!(stored.bytes().await.unwrap().as_ref(), bytes);
        }
        std::fs::write(dist.join("assets/ui.js"), b"changed").unwrap();
        assert!(
            super::platform_ui_source(&repository, &chart)
                .unwrap_err()
                .to_string()
                .contains("differs from the platform build receipt")
        );
        std::fs::remove_file(chart.join(RECEIPT_FILE)).unwrap();
        assert!(
            super::platform_ui_source(&repository, &chart)
                .unwrap_err()
                .to_string()
                .contains("run `wamn-ctl build-platform`")
        );
        std::fs::remove_dir_all(repository).unwrap();
    }

    #[test]
    fn the_sink_scheme_selects_the_builder() {
        assert_eq!(
            Sink::parse("gs://wamn-dev-web/clients").unwrap(),
            Sink {
                scheme: Scheme::Gcs,
                bucket: "wamn-dev-web",
                prefix: "clients",
            }
        );
        assert_eq!(
            Sink::parse("s3://web").unwrap(),
            Sink {
                scheme: Scheme::S3,
                bucket: "web",
                prefix: "",
            }
        );
        assert!(Sink::parse("https://web").is_err());
    }
}
