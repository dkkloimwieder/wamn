use super::{ApplicationSelection, BTreeMap, WebManifest, ensure};
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
    ensure!(
        packages
            .into_iter()
            .any(|coordinate| coordinate == (package, version)),
        "web artifact package {package}@{version} is not in release {}",
        selection.current
    );
    Ok(())
}

pub(in super::super) fn resolve<'a>(
    selection: &ApplicationSelection,
    packages: impl Iterator<Item = &'a str>,
) -> anyhow::Result<BTreeMap<String, String>> {
    let selected = match selection.client.as_ref() {
        Some(client) => {
            let name = client.package.as_str();
            ensure!(
                packages.into_iter().any(|package| package == name),
                "host {} names client package {name}, which the release does not provide",
                selection.hostname
            );
            Some(name)
        }
        None => None,
    };
    Ok(selected
        .map(|name| (selection.hostname.clone(), name.to_owned()))
        .into_iter()
        .collect())
}
