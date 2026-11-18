//! Content-derived development tags.

use anyhow::Context as _;

/// Derive a development tag from the authored version and source digest.
pub fn version(
    authored: &wamn_catalog::package_version::PackageVersion,
    digest: &wamn_digest::Digest,
) -> anyhow::Result<wamn_catalog::package_version::PackageVersion> {
    let base = authored
        .as_str()
        .split(['-', '+'])
        .next()
        .context("authored package version")?;
    let version = format!("{base}-dev.h{}", &digest.hex()[..12]);
    let tag = wamn_oci::package_artifact::tag::OciTag::try_from(version)?;
    Ok(String::from(tag).try_into()?)
}

#[cfg(test)]
mod tests {
    #[test]
    fn development_versions_require_a_content_digest() {
        let digest = wamn_digest::bytes_sha256(b"source");
        let version = super::version(&"1.0.0".parse().unwrap(), &digest).unwrap();
        assert!(wamn_oci::package_artifact::is_dev_tag(
            &version.to_string().parse().unwrap()
        ));
    }
}
