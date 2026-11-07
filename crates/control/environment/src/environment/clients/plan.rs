use super::{ApplicationSelection, WebManifest, ensure};
use anyhow::Context as _;

pub(super) fn verify<'a>(
    selection: &ApplicationSelection,
    web: &WebManifest,
    packages: impl Iterator<Item = (&'a str, &'a str)>,
) -> anyhow::Result<()> {
    let client = selection.client.as_ref().expect("a pinned client");
    let (package, version) = web
        .package
        .split_once('@')
        .context("web.json package must be <id>@<version>")?;
    ensure!(
        package == client.package,
        "web artifact package {package}@{version} differs from client.package {} for release {}",
        client.package,
        selection.current
    );
    let selected_version = packages
        .into_iter()
        .find_map(|(id, version)| (id == package).then_some(version))
        .with_context(|| {
            format!(
                "web artifact package {package}@{version} is not in release {}",
                selection.current
            )
        })?;
    if selected_version != version {
        tracing::warn!(%package, web_version = version, release_version = selected_version,
            "selected web artifact uses a different package version");
    }
    Ok(())
}

pub(in super::super) fn verify_client_package<'a>(
    selection: &ApplicationSelection,
    packages: impl Iterator<Item = &'a str>,
) -> anyhow::Result<()> {
    if let Some(client) = &selection.client {
        ensure!(
            packages
                .into_iter()
                .any(|package| package == client.package),
            "host {} names client package {}, which the release does not provide",
            selection.hostname,
            client.package
        );
    }
    Ok(())
}
