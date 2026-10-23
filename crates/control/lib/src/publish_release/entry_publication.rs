//! Publish the composed workload through the runtime's OCI component publisher.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use wamn_engine::artifact_source::ArtifactSource as _;
use wamn_package_artifact::registry_credentials::read_registry_push_credentials;
use wamn_runtime::component_artifact_source::{
    ComponentArtifactSource, ComponentArtifactSourceConfig,
};

use super::{PackageRegistry, PublishedRelease, ReleaseComponents};

pub(super) async fn publish(
    published: &mut PublishedRelease,
    components: &ReleaseComponents,
    registry: &PackageRegistry,
) -> anyhow::Result<()> {
    if !published
        .manifest
        .attachments
        .values()
        .any(|attachment| attachment.type_ == wamn_catalog::AttachmentType::Http)
    {
        return Ok(());
    }
    let component_base = std::env::var("WAMN_COMPONENT_ARTIFACT_BASE")
        .context("set WAMN_COMPONENT_ARTIFACT_BASE for release entry publication")?;
    let source = ComponentArtifactSource::new(
        ComponentArtifactSourceConfig::new(
            &component_base,
            registry.insecure_registry,
            Duration::from_secs(30),
        )?
        .with_registry_auth_file(&registry.registry_auth_file)?
        .with_ca_paths(&registry.oci_ca_paths)?,
    )?;
    let route_path = std::env::var_os("WAMN_FLOW_HTTP_COMPONENT").map_or_else(
        || PathBuf::from("/usr/local/share/wamn/http_route.wasm"),
        PathBuf::from,
    );
    let route =
        std::fs::read(&route_path).context("read the publisher's pinned HTTP route guest")?;
    let pin = format!(
        "sha256:{}",
        include_str!("../../../../../apps/platform/ingress/http-route/http_route.wasm.sha256")
            .split_whitespace()
            .next()
            .expect("route pin exists")
    );
    let mut candidates = Vec::new();
    for application in components.facts.values().flatten() {
        if published.manifest.components.iter().any(|released| {
            released.package_id == application.scope.package_id
                && released.descriptor.component_digest == application.component_digest
        }) {
            candidates.push((application, source.pull_verified(application).await?));
        }
    }
    candidates.sort_by_key(|(_, bytes)| std::cmp::Reverse(bytes.len()));
    let mut selected = None;
    for (application, bytes) in candidates {
        if super::covers_routes(published, application, &bytes)? {
            selected = Some(super::compose_verified_application(
                application,
                &bytes,
                &route,
                &pin,
            )?);
            break;
        }
    }
    let entry =
        selected.context("release routes have no composed application covering their members")?;
    // The entry lives beside the members, under the component artifact base.
    let repository = format!(
        "{component_base}/{}",
        wamn_catalog::COMPOSED_ENTRY_REPOSITORY
    );
    let base = wamn_catalog::parse_component_artifact_base(&repository)?;
    let credentials =
        read_registry_push_credentials(&registry.registry_auth_file, base.registry())?;
    wash_runtime::oci::set_extra_ca_certificates(&registry.oci_ca_paths)?;
    let reference = format!(
        "{repository}:{}",
        entry.digest().trim_start_matches("sha256:")
    );
    let config = || wash_runtime::oci::OciConfig {
        credentials: credentials.as_ref().map(|credential| {
            (
                credential.username().to_owned(),
                credential.password().to_owned(),
            )
        }),
        insecure: registry.insecure_registry,
        timeout: Some(Duration::from_secs(30)),
        ..Default::default()
    };
    let digest =
        match wash_runtime::oci::push_component(&reference, entry.bytes(), config(), None).await {
            Ok(digest) => digest,
            // The tag is the entry's content digest, so a present tag already
            // holds these bytes. Each push writes a new config creation time, so
            // an immutable tag refuses the repeat. Reuse the present manifest.
            Err(push) => {
                let (bytes, digest) = wash_runtime::oci::pull_component(
                    &reference,
                    config(),
                    wash_runtime::oci::OciPullPolicy::Always,
                )
                .await
                .map_err(|_| push)
                .context("publish composed HTTP entry")?;
                ensure!(
                    bytes == entry.bytes(),
                    "composed HTTP entry tag {reference} holds other bytes"
                );
                digest
            }
        };
    // The manifest records only the path relative to the component artifact base.
    let path = format!("{}@{digest}", wamn_catalog::COMPOSED_ENTRY_REPOSITORY);
    ensure!(
        wamn_catalog::validate_composed_entry_path(&path).is_ok(),
        "component publisher returned an invalid manifest digest"
    );
    super::record_composed_entry(published, &entry, &path)
}
