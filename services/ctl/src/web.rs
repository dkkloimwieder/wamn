//! Web client files in a bucket (docs/plan/web-deployment.md).

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail, ensure};
use clap::{Args, Subcommand};
use object_store::aws::AmazonS3Builder;
use object_store::{Attribute, Attributes, ObjectStore, PutOptions};
use tokio::process::Command;

/// A built file name carries its content hash, so a browser keeps it.
const ASSET_CACHE: &str = "public, max-age=31536000, immutable";
/// The index names the current files, so a browser asks again at every load.
const INDEX_CACHE: &str = "no-cache";

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
    /// The bucket and an optional prefix, `s3://<bucket>[/<prefix>]`. The
    /// `AWS_*` variables supply the endpoint and the credentials.
    #[arg(long)]
    pub bucket: String,
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
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(args.app.join("wamn.json"))
            .with_context(|| format!("read {}/wamn.json", args.app.display()))?,
    )?;
    let package = manifest
        .pointer("/package/id")
        .and_then(serde_json::Value::as_str)
        .context("wamn.json names its package id")?;
    let web = args.app.join("web");
    let status = Command::new("pnpm")
        .arg("--dir")
        .arg(&web)
        .args(["run", "build"])
        .status()
        .await
        .context("run pnpm; the web client builds with Vite through pnpm")?;
    ensure!(status.success(), "the web build failed: {status}");
    let dist = web.join("dist");
    let (bucket, prefix) = args
        .bucket
        .strip_prefix("s3://")
        .map(|rest| rest.split_once('/').unwrap_or((rest, "")))
        .context("--bucket must be s3://<bucket>[/<prefix>]")?;
    let store = AmazonS3Builder::from_env()
        .with_bucket_name(bucket)
        .build()
        .context("configure the bucket from the AWS_* variables")?;
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
    // The index goes last, so no reader meets an index whose files are absent.
    files.sort_by_key(|file| file == &dist.join("index.html"));
    for file in &files {
        let relative = file
            .strip_prefix(&dist)?
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
                    attributes,
                    ..PutOptions::default()
                },
            )
            .await
            .with_context(|| format!("write {key}"))?;
    }
    println!("s3://{bucket}/{root}/ {} files", files.len());
    Ok(())
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
