//! Web client files in a bucket (docs/plan/web-deployment.md).
//!
//! `wamn web upload` writes the built client of each package of the release
//! an environment serves, under `<prefix>/<package_id>/<release hex>/`, every
//! built file create-only, then `config.json`, and the index last. The release
//! must be the one the newest successful revision of the environment's release
//! chart installed, and the built files are the `web/dist` layer of each
//! package artifact (docs/plan/platform-deploy.md §13, §7.2).

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail, ensure};
use object_store::aws::AmazonS3Builder;
use object_store::gcp::GoogleCloudStorageBuilder;
use object_store::{Attribute, Attributes, ObjectStore, ObjectStoreExt as _, PutMode, PutOptions};
use wamn_control_registry::Triple;

use crate::environment::Platform;
use crate::package_artifact::{PackageRegistry, PackageSource, open_package_source};
use crate::web_scope::{SCOPE_CACHE, SCOPE_CONTENT_TYPE, SCOPE_FILE, scope_file_bytes};

/// A built file name carries its content hash, so a browser keeps it.
const ASSET_CACHE: &str = "public, max-age=31536000, immutable";
/// The index names the current files, so a browser asks again at every load.
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
/// artifact cannot be read or holds no built client, or a write fails. An
/// object is never replaced.
pub async fn upload_release(
    platform: &Platform,
    triple: &Triple,
    release: &str,
    bucket: &str,
) -> anyhow::Result<Vec<UploadedClient>> {
    let hex = release_hex(release)?;
    let tenant = wamn_control_provision::project_env_tenant(
        &triple.org,
        &triple.project,
        triple.env.as_str(),
    );
    let deployed = crate::environment::analyse::deployed_release(&platform.target, &tenant)
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
        control_database_url: platform.system_database_url.clone(),
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
        let dist = opened.root().join("web/dist");
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
        let mut files = Vec::new();
        collect(&dist, &mut files)
            .with_context(|| format!("the package artifact {tag} holds no built web client"))?;
        ensure!(
            files.iter().any(|file| file == &dist.join("index.html")),
            "the package artifact {tag} holds no web/dist/index.html"
        );
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
        let cache = if relative == "index.html" {
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

/// Write one object create-only with its Cache-Control and content type.
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
    store
        .put_opts(
            &key,
            bytes.into(),
            PutOptions {
                mode: PutMode::Create,
                attributes,
                ..PutOptions::default()
            },
        )
        .await
        .map(|_| ())
        .with_context(|| format!("write {key}; an existing object is never replaced"))
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

    use super::{Scheme, Sink, collect, write_files};

    const HEAD: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SCOPE: &[u8] = br#"{"org":"acme","project":"receiving"}"#;

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
