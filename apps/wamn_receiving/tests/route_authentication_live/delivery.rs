//! Supplied release artifacts for the existing Receiving application cases.

use anyhow::{Context as _, ensure};
use wamn_catalog::ServingManifest;
use wamn_ctl::delivery::Candidate;

pub(super) fn candidate() -> anyhow::Result<Option<(Candidate, ServingManifest)>> {
    let Some(candidate) = Candidate::from_env()? else {
        return Ok(None);
    };
    let (manifest, _) = candidate.manifest()?;
    let packages = super::JOURNEY_PACKAGES
        .iter()
        .map(|package| wamn_catalog::PackageCoordinate::new(package.id, package.version))
        .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
    ensure!(
        manifest.release.tenant_id == super::TENANT
            && manifest.release.environment == super::ENVIRONMENT
            && manifest.release.effective_release_id.get() == super::RELEASE_ID
            && manifest.release.packages == packages,
        "supplied Receiving artifacts require the receiving-route-auth/dev release 1 with the exact Receiving and Acme packages"
    );
    let result = std::env::var_os("WAMN_DELIVERY_RESULT")
        .context("WAMN_DELIVERY_RESULT is required for supplied-artifact execution")?;
    let result = std::path::Path::new(&result);
    ensure!(
        result.is_absolute() && !result.exists(),
        "the supplied-artifact result must be a new absolute path"
    );
    Ok(Some((candidate, manifest)))
}

pub(super) fn image_reference(image: &str) -> anyhow::Result<String> {
    let (repository, digest) = image
        .rsplit_once('@')
        .context("the supplied image has a digest")?;
    if repository
        .rsplit('/')
        .next()
        .is_some_and(|name| name.contains(':'))
    {
        Ok(image.to_owned())
    } else {
        Ok(format!("{repository}:latest@{digest}"))
    }
}

pub(super) fn host_values(base: &str, image: &str) -> anyhow::Result<String> {
    let (name, digest) = image
        .rsplit_once('@')
        .context("the supplied host image has a digest")?;
    let (repository, tag) = name
        .rsplit_once(':')
        .context("the supplied host image has a tag")?;
    let mut base: serde_yaml::Value = serde_yaml::from_str(base)?;
    base["runtime"]["image"]["registry"] = "".into();
    base["runtime"]["image"]["repository"] = repository.into();
    base["runtime"]["image"]["tag"] = format!("{tag}@{digest}").into();
    serde_yaml::to_string(&base).context("render the digest-pinned host image")
}

pub(super) fn registry_files(candidate: &Candidate, work: &std::path::Path) -> anyhow::Result<()> {
    let Some(endpoint) = &candidate.native_registry_endpoint else {
        return Ok(());
    };
    let (authority, _) = candidate
        .host_image
        .split_once('/')
        .context("the mapped native image has a registry authority")?;
    let scheme = if candidate.native_registry_insecure {
        "http"
    } else {
        "https"
    };
    let endpoint = serde_json::to_string(&format!("{scheme}://{endpoint}"))?;
    std::fs::write(work.join("native-registry-authority"), authority)?;
    std::fs::write(
        work.join("native-registry-hosts.toml"),
        format!(
            "server = {endpoint}\n[host.{endpoint}]\n  capabilities = [\"pull\", \"resolve\"]\n"
        ),
    )?;
    Ok(())
}

pub(super) fn executor_launcher(
    work: &std::path::Path,
    image: &str,
    container: &str,
) -> anyhow::Result<std::path::PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
    let path = work.join("executor-launcher");
    let script = format!(
        r#"#!/bin/sh
exec docker run --rm --name {} --network host --pull never \
  --read-only --tmpfs /tmp --volume {} \
  --env WAMN_PG_URL --env WAMN_EXECUTOR_PLATFORM_PG_URL --env WAMN_HTTP_ADMITTER_PG_URL \
  --env WAMN_EVT_NATS_URL --env WAMN_EVT_ORG --env WAMN_EVT_PROJECT --env WAMN_EVT_ENV \
  --env WAMN_EVT_NATS_USERNAME --env WAMN_EVT_NATS_PASSWORD_FILE \
  --env WAMN_EVT_STREAM_REPLICAS --env WAMN_EVT_DUP_WINDOW_SECS {} "$@"
"#,
        quote(container),
        quote(&format!("{}:{}:ro", work.display(), work.display())),
        quote(image),
    );
    std::fs::write(&path, script)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    #[test]
    fn host_image_digest_survives_chart_values() {
        let digest = "a".repeat(64);
        for repository in [
            "registry.test:5443/wamn-host",
            "registry.test:5443/wamn-host:release",
        ] {
            let image = super::image_reference(&format!("{repository}@sha256:{digest}")).unwrap();
            let base = super::host_values(
                "runtime:\n  image:\n    registry: old\n    repository: old\n    tag: old\n",
                &image,
            )
            .unwrap();
            let base: serde_yaml::Value = serde_yaml::from_str(&base).unwrap();
            let actual = format!(
                "{}:{}",
                base["runtime"]["image"]["repository"].as_str().unwrap(),
                base["runtime"]["image"]["tag"].as_str().unwrap()
            );
            assert_eq!(actual, image);
            assert!(actual.starts_with("registry.test:5443/wamn-host:"));
            assert!(actual.ends_with(&format!("@sha256:{digest}")));
            assert_eq!(base["runtime"]["image"]["registry"].as_str(), Some(""));
        }
    }
}
