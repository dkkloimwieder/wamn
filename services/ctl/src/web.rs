//! Web client files in a bucket (docs/plan/web-deployment.md).

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail, ensure};
use clap::{Args, Subcommand};
use object_store::aws::AmazonS3Builder;
use object_store::gcp::GoogleCloudStorageBuilder;
use object_store::{Attribute, Attributes, ObjectStore, PutMode, PutOptions};
use tokio::process::Command;
use tokio_postgres::NoTls;

/// A built file name carries its content hash, so a browser keeps it.
const ASSET_CACHE: &str = "public, max-age=31536000, immutable";
/// The index names the current files, so a browser asks again at every load.
const INDEX_CACHE: &str = "no-cache";
/// The manifest digest of the release each environment head of the database
/// names. `select-release` and `promote` write the head.
const SELECT_HEADS: &str = "SELECT s.manifest_digest \
     FROM catalog.effective_release_heads AS h \
     JOIN catalog.release_manifest_v3_snapshots AS s USING (tenant_id, effective_release_id)";

/// Web client commands.
#[derive(Debug, Args)]
pub struct WebArgs {
    #[command(subcommand)]
    pub command: WebCommand,
}

#[derive(Debug, Subcommand)]
pub enum WebCommand {
    /// Build an application's web client and write it to a bucket.
    Upload(UploadArgs),
}

#[derive(Debug, Args)]
pub struct UploadArgs {
    /// The application directory, which holds `wamn.json` and `web/`.
    pub app: PathBuf,
    /// The release the files belong to: the manifest digest, `sha256:` and 64
    /// lowercase hexadecimal digits.
    #[arg(long)]
    pub release: String,
    /// The bucket and an optional prefix, `s3://<bucket>[/<prefix>]` or
    /// `gs://<bucket>[/<prefix>]`. For `s3://` the `AWS_*` variables supply
    /// the endpoint and the credentials. For `gs://` the `GOOGLE_*` variables
    /// and Application Default Credentials do.
    #[arg(long)]
    pub bucket: String,
    /// The org the client signs in to. The project comes from the client
    /// package name, `@wamn/<project>-client`.
    #[arg(long)]
    pub org: String,
    /// The project-environment database that holds the release head. The
    /// upload refuses a release that is not the head, before the build.
    #[arg(long)]
    pub database_url: String,
}

pub async fn run(args: WebArgs) -> anyhow::Result<()> {
    match args.command {
        WebCommand::Upload(args) => upload(args).await,
    }
}

async fn upload(args: UploadArgs) -> anyhow::Result<()> {
    let hex = args
        .release
        .strip_prefix("sha256:")
        .filter(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .context("--release must be sha256: and 64 lowercase hexadecimal digits")?;
    require_head(&args.database_url, &args.release).await?;
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(args.app.join("wamn.json"))
            .with_context(|| format!("read {}/wamn.json", args.app.display()))?,
    )?;
    let package = manifest
        .pointer("/package/id")
        .and_then(serde_json::Value::as_str)
        .context("wamn.json names its package id")?;
    let client = manifest
        .pointer("/client_package/name")
        .and_then(serde_json::Value::as_str)
        .context("wamn.json names its client package")?;
    let project = client_project(client)?;
    let web = args.app.join("web");
    let status = Command::new("pnpm")
        .arg("--dir")
        .arg(&web)
        .args(["run", "build"])
        .env("WAMN_ORG", &args.org)
        .env("WAMN_PROJECT", project)
        .status()
        .await
        .context("run pnpm; the web client builds with Vite through pnpm")?;
    ensure!(status.success(), "the web build failed: {status}");
    let dist = web.join("dist");
    let sink = Sink::parse(&args.bucket)?;
    let store = sink.store()?;
    let (bucket, prefix) = (sink.bucket, sink.prefix);
    let root = [prefix.trim_matches('/'), package, hex]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    let mut files = Vec::new();
    collect(&dist, &mut files)?;
    ensure!(
        files.iter().any(|file| file == &dist.join("index.html")),
        "the build wrote no index.html"
    );
    write_files(store.as_ref(), &root, &dist, &mut files).await?;
    println!(
        "{}{bucket}/{root}/ {} files",
        sink.scheme.prefix(),
        files.len()
    );
    Ok(())
}

/// Refuse a release that is not the head of the environment.
///
/// An existing release is not enough: an old client written over the current
/// one would serve the current release with stale files.
async fn require_head(database_url: &str, release: &str) -> anyhow::Result<()> {
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

/// Write every built file create-only, the index last, so no reader meets an
/// index whose files are absent. An existing object refuses the upload.
async fn write_files(
    store: &dyn ObjectStore,
    root: &str,
    dist: &Path,
    files: &mut [PathBuf],
) -> anyhow::Result<()> {
    files.sort_by_key(|file| file == &dist.join("index.html"));
    for file in files.iter() {
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
        let mut attributes = Attributes::new();
        attributes.insert(Attribute::CacheControl, cache.into());
        attributes.insert(Attribute::ContentType, content_type(relative)?.into());
        let key = object_store::path::Path::from(format!("{root}/{relative}"));
        store
            .put_opts(
                &key,
                std::fs::read(file)?.into(),
                PutOptions {
                    mode: PutMode::Create,
                    attributes,
                    ..PutOptions::default()
                },
            )
            .await
            .with_context(|| format!("write {key}; an existing object is never replaced"))?;
    }
    Ok(())
}

/// The project that a client package `@wamn/<project>-client` names.
fn client_project(client: &str) -> anyhow::Result<&str> {
    client
        .strip_prefix("@wamn/")
        .and_then(|rest| rest.strip_suffix("-client"))
        .filter(|project| !project.is_empty())
        .with_context(|| format!("the client package {client} is not @wamn/<project>-client"))
}

/// The object store that `--bucket` names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scheme {
    /// `s3://`, an S3 API endpoint, as kind uses.
    S3,
    /// `gs://`, Google Cloud Storage with Application Default Credentials.
    Gcs,
}

impl Scheme {
    fn prefix(self) -> &'static str {
        match self {
            Self::S3 => "s3://",
            Self::Gcs => "gs://",
        }
    }
}

/// A parsed `--bucket` value.
#[derive(Debug, PartialEq, Eq)]
struct Sink<'a> {
    scheme: Scheme,
    bucket: &'a str,
    prefix: &'a str,
}

impl<'a> Sink<'a> {
    fn parse(value: &'a str) -> anyhow::Result<Self> {
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

    fn store(&self) -> anyhow::Result<Box<dyn ObjectStore>> {
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

fn collect(directory: &Path, files: &mut Vec<PathBuf>) -> anyhow::Result<()> {
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

    use super::{Scheme, Sink, collect, require_head, write_files};

    const HEAD: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OTHER: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[tokio::test]
    async fn a_stale_release_is_refused_before_the_build() {
        let database = wamn_catalog::test_database::tenant();
        // The digest check of the snapshot table holds, so the head row names
        // the digest of these bytes.
        let bytes = "head";
        let head = format!(
            "sha256:{}",
            database
                .execute(&[&format!("SELECT encode(sha256('{bytes}'::bytea), 'hex')")])
                .expect("hash the snapshot bytes")
                .trim()
        );
        database
            .execute(&[&format!(
                "INSERT INTO catalog.effective_releases (tenant_id, effective_release_id, environment) \
                 VALUES ('t', 1, 'dev'); \
                 INSERT INTO catalog.release_manifest_v3_snapshots \
                 (tenant_id, effective_release_id, manifest_digest, canonical_bytes) \
                 VALUES ('t', 1, '{head}', '{bytes}'::bytea); \
                 INSERT INTO catalog.effective_release_heads (tenant_id, environment, effective_release_id) \
                 VALUES ('t', 'dev', 1);"
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
        write_files(&store, HEAD, &dist, &mut files)
            .await
            .expect("the first upload writes");

        std::fs::write(dist.join("index.html"), "second").unwrap();
        std::fs::write(dist.join("assets/app.js"), "second").unwrap();
        let error = write_files(&store, HEAD, &dist, &mut files)
            .await
            .expect_err("the second upload refuses");
        assert!(
            error
                .to_string()
                .starts_with(&format!("write {HEAD}/assets/app.js")),
            "{error}"
        );
        for name in ["index.html", "assets/app.js"] {
            let stored = store
                .get(&object_store::path::Path::from(format!("{HEAD}/{name}")))
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap();
            assert_eq!(&stored[..], b"first", "{name}");
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
