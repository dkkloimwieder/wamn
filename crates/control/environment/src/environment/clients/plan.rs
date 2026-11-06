use super::{BTreeMap, EnvironmentDocument, WebManifest, ensure};
use anyhow::Context as _;

pub(super) fn verify<'a>(
    document: &EnvironmentDocument,
    web: &WebManifest,
    packages: impl Iterator<Item = (&'a str, &'a str)>,
) -> anyhow::Result<()> {
    let client = document.legacy_client().expect("a pinned client");
    let (package, version) = web
        .package
        .split_once('@')
        .context("web.json package must be <id>@<version>")?;
    ensure!(
        package == client.package,
        "web artifact package {package}@{version} differs from client.package {} for release {}",
        client.package,
        *document.legacy_release()
    );
    ensure!(
        packages
            .into_iter()
            .any(|coordinate| coordinate == (package, version)),
        "web artifact package {package}@{version} is not in release {}",
        *document.legacy_release()
    );
    Ok(())
}

pub(in super::super) fn resolve<'a>(
    document: &EnvironmentDocument,
    packages: impl Iterator<Item = &'a str>,
) -> anyhow::Result<BTreeMap<String, String>> {
    let selected = match document.legacy_client() {
        Some(client) => {
            let name = client.package.as_str();
            ensure!(
                packages.into_iter().any(|package| package == name),
                "host {} names client package {name}, which the release does not provide",
                document.legacy_hostname()
            );
            Some(name)
        }
        None => None,
    };
    Ok(selected
        .map(|name| (document.legacy_hostname().to_owned(), name.to_owned()))
        .into_iter()
        .collect())
}
