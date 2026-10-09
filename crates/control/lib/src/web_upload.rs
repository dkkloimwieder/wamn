//! Web client files in a bucket (docs/plan/web-deployment.md).
//!
//! `wamn web upload` builds a client and writes it here, and the
//! create-environment saga writes the built client of a package artifact
//! (`wamn-zua8.3`). Both write under `<prefix>/<package_id>/<release hex>/`,
//! every built file create-only, then `config.json`, and the index last.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use object_store::aws::AmazonS3Builder;
use object_store::gcp::GoogleCloudStorageBuilder;
use object_store::{Attribute, Attributes, ObjectStore, ObjectStoreExt as _, PutMode, PutOptions};
use tokio_postgres::NoTls;

use crate::web_scope::{SCOPE_CACHE, SCOPE_CONTENT_TYPE, SCOPE_FILE};

/// A built file name carries its content hash, so a browser keeps it.
const ASSET_CACHE: &str = "public, max-age=31536000, immutable";
/// The index names the current files, so a browser asks again at every load.
const INDEX_CACHE: &str = "no-cache";
/// The manifest digest of the release each environment head of the database
/// names. `select-release` writes the head.
const SELECT_HEADS: &str = "SELECT manifest_digest FROM catalog.effective_release_heads";

/// What a write does when its object exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExistingObject {
    /// Refuse the write. `wamn web upload` never replaces an object.
    Refuse,
    /// Count an object with the same bytes as written, and refuse one with
    /// other bytes. A resumed saga writes the same files again.
    AcceptIdentical,
}

/// Refuse a release that is not the head of the environment.
///
/// An existing release is not enough: an old client written over the current
/// one would serve the current release with stale files.
pub async fn require_head(database_url: &str, release: &str) -> anyhow::Result<()> {
    let (client, connection) = tokio_postgres::connect(database_url, NoTls)
        .await
        .context("connect to the project-environment database")?;
    let connection = tokio::spawn(connection);
    let heads = client
        .query(SELECT_HEADS, &[])
        .await
        .context("read the release head")?;
    drop(client);
    let _ = connection.await;
    let heads: Vec<String> = heads.iter().map(|row| row.get(0)).collect();
    match heads.as_slice() {
        [head] if head == release => Ok(()),
        [head] => bail!("release {release} is not the head {head} of this environment"),
        [] => bail!("the database has no release head; select-release sets it"),
        _ => bail!("the database has more than one release head: {heads:?}"),
    }
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
    existing: ExistingObject,
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
                existing,
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
            existing,
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
    existing: ExistingObject,
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
        Ok(_) => Ok(()),
        Err(object_store::Error::AlreadyExists { .. })
            if existing == ExistingObject::AcceptIdentical =>
        {
            let stored = store
                .get(&key)
                .await
                .with_context(|| format!("read the existing {key}"))?
                .bytes()
                .await
                .with_context(|| format!("read the existing {key}"))?;
            if stored[..] == bytes[..] {
                Ok(())
            } else {
                bail!("write {key}; an existing object with other bytes is never replaced")
            }
        }
        Err(error) => Err(anyhow::Error::new(error)
            .context(format!("write {key}; an existing object is never replaced"))),
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

    use super::{ExistingObject, Scheme, Sink, collect, require_head, write_files};

    const HEAD: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OTHER: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const SCOPE: &[u8] = br#"{"org":"acme","project":"receiving"}"#;

    #[tokio::test]
    async fn a_stale_release_is_refused_before_the_build() {
        let database = wamn_catalog::test_database::tenant();
        // The digest check of the release table holds, so the head row names
        // the digest of these bytes.
        let bytes = "{}";
        let head = format!(
            "sha256:{}",
            database
                .execute(&[&format!("SELECT encode(sha256('{bytes}'::bytea), 'hex')")])
                .expect("hash the snapshot bytes")
                .trim()
        );
        database
            .execute(&[&format!(
                "INSERT INTO catalog.releases (tenant_id, manifest_digest, canonical_bytes) \
                 VALUES ('t', '{head}', '{bytes}'::bytea); \
                 INSERT INTO catalog.effective_release_heads (tenant_id, environment, manifest_digest) \
                 VALUES ('t', 'dev', '{head}');"
            )])
            .expect("seed one head");
        require_head(database.url(), &head)
            .await
            .expect("the head is accepted");
        let error = require_head(database.url(), OTHER)
            .await
            .expect_err("a release that is not the head refuses");
        assert_eq!(
            error.to_string(),
            format!("release {OTHER} is not the head {head} of this environment")
        );
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
        write_files(
            &store,
            HEAD,
            &dist,
            &mut files,
            SCOPE,
            ExistingObject::Refuse,
        )
        .await
        .expect("the first upload writes");
        write_files(
            &store,
            HEAD,
            &dist,
            &mut files,
            SCOPE,
            ExistingObject::AcceptIdentical,
        )
        .await
        .expect("a resumed upload of the same files counts as done");

        std::fs::write(dist.join("index.html"), "second").unwrap();
        std::fs::write(dist.join("assets/app.js"), "second").unwrap();
        for existing in [ExistingObject::Refuse, ExistingObject::AcceptIdentical] {
            let error = write_files(&store, HEAD, &dist, &mut files, SCOPE, existing)
                .await
                .expect_err("the second upload refuses");
            assert!(
                error
                    .to_string()
                    .starts_with(&format!("write {HEAD}/assets/app.js")),
                "{error}"
            );
        }
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
