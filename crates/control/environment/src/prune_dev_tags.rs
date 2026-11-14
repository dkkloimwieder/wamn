//! Expire unpinned development package tags after seven days.

use std::collections::BTreeSet;

use anyhow::Context as _;
use chrono::{DateTime, Utc};
use wamn_catalog::ServingManifest;
use wamn_package_artifact::{
    CREATED_ANNOTATION, PACKAGE_ARTIFACT_TYPE, PackageRegistry, Repository, ensure_digest,
    is_dev_tag,
};

pub(crate) async fn prune_published(platform: &crate::environment::Platform) -> anyhow::Result<()> {
    let packages = platform
        .registry(
            platform
                .package_artifact_base
                .as_deref()
                .context("package artifact base is absent")?,
        )
        .await?;
    let releases = platform.registry(&platform.release_artifact_base).await?;
    let repository = Repository::open(&releases, "latest")?;
    let mut manifests = Vec::new();
    for tag in repository.tags().await? {
        let digest = format!("sha256:{tag}");
        if ensure_digest(&digest).is_ok() {
            manifests
                .push(crate::environment::analyse::pull_release_manifest(platform, &digest).await?);
        }
    }
    for tag in prune(&packages, &manifests, Utc::now()).await? {
        tracing::info!(tag, "removed expired development package tag");
    }
    Ok(())
}

/// Delete expired dev tags except coordinates pinned by any published release.
/// Missing publication times leave old data untouched. Registry errors stop pruning.
pub async fn prune(
    registry: &PackageRegistry,
    releases: &[ServingManifest],
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<String>> {
    let pinned: BTreeSet<_> = releases
        .iter()
        .flat_map(|release| &release.release.packages)
        .map(|package| format!("{}-{}", package.package_id(), package.package_version()))
        .collect();
    let repository = Repository::open(registry, "latest")?;
    let mut expired = Vec::new();
    for tag in repository.tags().await? {
        if !is_dev_tag(&tag) || pinned.contains(&tag) {
            continue;
        }
        let Some((bytes, _)) = Repository::open(registry, &tag)?.manifest().await? else {
            continue;
        };
        let manifest: oci_client::manifest::OciImageManifest = serde_json::from_slice(&bytes)?;
        if manifest.artifact_type.as_deref() != Some(PACKAGE_ARTIFACT_TYPE) {
            continue;
        }
        let Some(created) = manifest
            .annotations
            .as_ref()
            .and_then(|annotations| annotations.get(CREATED_ANNOTATION))
        else {
            continue;
        };
        let created =
            DateTime::parse_from_rfc3339(created).context("invalid dev publication time")?;
        if created < now - chrono::Duration::days(7) {
            expired.push(tag);
        }
    }
    for tag in &expired {
        repository.delete_tag(tag).await?;
    }
    Ok(expired)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    #[tokio::test]
    async fn pruning_keeps_release_pins_and_deletes_only_expired_dev_tags() -> anyhow::Result<()> {
        let now = Utc::now();
        let pinned_version = "1.0.0-dev.h0123456789ab";
        let pinned = format!("orders-{pinned_version}");
        let expired = "orders-1.0.0-dev.h123456789abc";
        let fresh = "orders-1.0.0-dev.h23456789abcd";
        let stable = "orders-1.0.0";
        let manifests: BTreeMap<_, _> = [
            (pinned.as_str(), Some(now - chrono::Duration::days(8))),
            (expired, Some(now - chrono::Duration::days(8))),
            (fresh, Some(now - chrono::Duration::days(7))),
            (stable, None),
        ]
        .into_iter()
        .map(|(tag, created)| {
            let manifest = oci_client::manifest::OciImageManifest {
                artifact_type: Some(PACKAGE_ARTIFACT_TYPE.into()),
                annotations: created
                    .map(|created| [(CREATED_ANNOTATION.into(), created.to_rfc3339())].into()),
                ..Default::default()
            };
            (tag.to_owned(), serde_json::to_vec(&manifest).unwrap())
        })
        .collect();
        let manifests = Arc::new(Mutex::new(manifests));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let state = Arc::clone(&manifests);
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    socket.read_exact(&mut byte).await.unwrap();
                    request.push(byte[0]);
                }
                let request = String::from_utf8(request).unwrap();
                let mut words = request.split_whitespace();
                let method = words.next().unwrap();
                let path = words.next().unwrap();
                let (status, body) = {
                    let mut state = state.lock().unwrap();
                    if let Some(tag) = path.strip_prefix("/v2/packages/manifests/") {
                        if method == "DELETE" {
                            state.remove(tag);
                            (202, Vec::new())
                        } else {
                            (200, state[tag].clone())
                        }
                    } else if path.starts_with("/v2/packages/tags/list") {
                        let last = path.split_once('?').and_then(|(_, query)| {
                            url::form_urlencoded::parse(query.as_bytes())
                                .find(|(name, _)| name == "last")
                                .map(|(_, value)| value.into_owned())
                        });
                        let tags: Vec<_> = state
                            .keys()
                            .filter(|tag| last.as_ref().is_none_or(|last| *tag > last))
                            .collect();
                        (
                            200,
                            serde_json::to_vec(&serde_json::json!({"name":"packages","tags":tags}))
                                .unwrap(),
                        )
                    } else {
                        (200, Vec::new())
                    }
                };
                socket.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nContent-Type: application/vnd.oci.image.manifest.v1+json\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
                socket.write_all(&body).await.unwrap();
            }
        });
        let directory = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
        let auth = directory.path().join("auth.json");
        std::fs::write(
            &auth,
            serde_json::to_vec(&serde_json::json!({"auths":{address.to_string():{
                "username":"fixture", "password":"fixture"
            }}}))?,
        )?;
        let registry = PackageRegistry {
            artifact_base: format!("{address}/packages"),
            credentials: None,
            registry_auth_file: auth,
            insecure_registry: true,
            oci_ca_paths: Vec::new(),
        };
        let release = ServingManifest {
            format_version: wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,
            release: wamn_catalog::ServingRelease {
                packages: [wamn_catalog::PackageCoordinate::new(
                    "orders",
                    pinned_version,
                )?]
                .into(),
            },
            composed_entry: None,
            components: BTreeSet::new(),
            routes: BTreeSet::new(),
            attachments: BTreeMap::new(),
            host_routes: BTreeSet::new(),
        };
        let result = prune(&registry, &[release], now).await;
        server.abort();
        result?;
        let state = manifests.lock().unwrap();
        assert!(state.contains_key(&pinned));
        assert!(!state.contains_key(expired));
        assert!(state.contains_key(fresh));
        assert!(state.contains_key(stable));
        Ok(())
    }
}
