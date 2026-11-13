use super::{
    ASSET_CACHE, BuildRecord, INDEX_CACHE, Path, WEB_MANIFEST_FILE, WebManifest, bail,
    bytes_sha256, ensure, ensure_digest,
};
use anyhow::Context as _;

pub(super) fn web_manifest(
    record: &BuildRecord,
    compiled: &[u8],
    version: String,
) -> anyhow::Result<WebManifest> {
    let digest = bytes_sha256(compiled);
    ensure!(
        digest == record.outputs.manifest,
        "package {}@{} wamn.json hashes to {digest}, but build.json records {}",
        record.package.id,
        record.package.version,
        record.outputs.manifest
    );
    let manifest = wamn_compiler::manifest::PackageManifest::from_slice(compiled)?;
    ensure!(
        manifest.package.id == record.package.id
            && manifest.package.version == record.package.version,
        "wamn.json identity differs from build.json for package {}@{}",
        record.package.id,
        record.package.version
    );
    let client = manifest.client_package.with_context(|| {
        format!(
            "package {}@{} has no client_package and no web artifact",
            record.package.id, record.package.version
        )
    })?;
    Ok(WebManifest {
        client_package: client.name,
        version,
        package: format!("{}@{}", record.package.id, record.package.version),
        contract_digest: record.outputs.manifest.clone(),
    })
}

pub(super) fn read_web_manifest(tar: &[u8]) -> anyhow::Result<WebManifest> {
    use std::io::Read as _;

    let mut archive = tar::Archive::new(tar);
    let mut metadata = None;
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.path()?.as_ref() != Path::new(WEB_MANIFEST_FILE) {
            continue;
        }
        ensure!(metadata.is_none(), "web artifact repeats web.json");
        ensure!(
            entry.header().entry_type().is_file(),
            "web.json must be a regular file"
        );
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        let web: WebManifest = serde_json::from_slice(&bytes)?;
        ensure!(bytes == web.to_bytes(), "web.json is not canonical JSON");
        ensure_digest(&web.contract_digest)?;
        ensure!(
            !web.client_package.is_empty(),
            "web.json has no client_package"
        );
        ensure!(!web.version.is_empty(), "web.json has no version");
        let (package, version) = web
            .package
            .split_once('@')
            .context("web.json package must be <id>@<version>")?;
        ensure!(
            !package.is_empty() && !version.is_empty(),
            "web.json package must be <id>@<version>"
        );
        metadata = Some(web);
    }
    metadata.context("web artifact has no web.json")
}

pub(super) fn web_dist_files(
    tar: &[u8],
) -> anyhow::Result<std::collections::BTreeMap<String, Vec<u8>>> {
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
        cache_control(name)?;
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

/// The project that a client package `@wamn/<project>-client` names.
pub fn client_project(client: &str) -> anyhow::Result<&str> {
    client
        .strip_prefix("@wamn/")
        .and_then(|rest| rest.strip_suffix("-client"))
        .filter(|project| !project.is_empty())
        .with_context(|| format!("the client package {client} is not @wamn/<project>-client"))
}

pub(super) fn cache_control(relative: &str) -> anyhow::Result<&'static str> {
    if relative == "index.html" || relative == "sw.js" {
        Ok(INDEX_CACHE)
    } else if relative.starts_with("assets/") {
        Ok(ASSET_CACHE)
    } else {
        bail!("the build wrote {relative}, which has no declared cache rule")
    }
}

pub(super) fn content_type(name: &str) -> anyhow::Result<&'static str> {
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
