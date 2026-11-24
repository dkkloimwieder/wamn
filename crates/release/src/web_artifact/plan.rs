use super::{Path, bail, ensure};
use anyhow::Context as _;

/// The project that a client package `@wamn/<project>-client` names.
pub fn client_project(client: &str) -> anyhow::Result<&str> {
    client
        .strip_prefix("@wamn/")
        .and_then(|rest| rest.strip_suffix("-client"))
        .filter(|project| !project.is_empty())
        .with_context(|| format!("the client package {client} is not @wamn/<project>-client"))
}

pub fn validate_path(relative: &str) -> anyhow::Result<()> {
    ensure!(
        relative == "index.html" || relative == "sw.js" || relative.starts_with("assets/"),
        "unsupported web artifact path {relative}"
    );
    Ok(())
}

pub fn content_type(name: &str) -> anyhow::Result<&'static str> {
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

/// The object store that a bucket value names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    /// `s3://`, an S3 API endpoint, as type uses.
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
}
