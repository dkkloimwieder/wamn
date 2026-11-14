//! Development package retention through the shared registry:2 fixture.

#[path = "../../environment/tests/support/local_registry.rs"]
mod local_registry;

use chrono::{Duration, Utc};
use oci_client::client::{Config, ImageLayer};
use oci_client::manifest::{OCI_IMAGE_MEDIA_TYPE, OciImageManifest};
use std::collections::{BTreeMap, BTreeSet};
use wamn_catalog::{PackageCoordinate, ServingManifest, ServingRelease};
use wamn_package_artifact::{
    CREATED_ANNOTATION, PACKAGE_ARTIFACT_TYPE, PackageRegistry, Repository,
};

#[tokio::test]
async fn prune_preserves_pins_and_shared_digests_and_removes_expired_dev_tags() -> anyhow::Result<()>
{
    let local = local_registry::LocalRegistry::start("dev-retention");
    local.ready().await;
    let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
    let auth = scratch.path().join("auth.json");
    std::fs::write(
        &auth,
        serde_json::to_vec(&serde_json::json!({"auths": {
            local.address.clone(): {"username":"fixture", "password":"fixture"}
        }}))?,
    )?;
    let registry = PackageRegistry {
        artifact_base: format!("{}/packages", local.address),
        credentials: None,
        registry_auth_file: auth,
        insecure_registry: true,
        oci_ca_paths: Vec::new(),
    };
    let now = Utc::now();
    let pinned_version = "1.0.0-dev.h0123456789ab";
    let pinned = format!("orders-{pinned_version}");
    let expired = "orders-1.0.0-dev.h123456789abc";
    let shared = "orders-1.0.0-dev.h23456789abcd";
    let stable = "orders-1.0.0";
    let shared_pin_version = "1.0.0-dev.h3456789abcde";
    let shared_pin = format!("billing-{shared_pin_version}");
    let pinned_alias = "orders-1.0.0-dev.h456789abcdef";
    for (tag, alias) in [
        (pinned.as_str(), None),
        (expired, None),
        (shared, Some(stable)),
        (shared_pin.as_str(), Some(pinned_alias)),
    ] {
        let repository = Repository::open(&registry, tag)?;
        let layer = ImageLayer::new(
            tag.as_bytes().to_vec(),
            "application/octet-stream".into(),
            None,
        );
        let config = Config::new(
            b"{}".to_vec(),
            "application/vnd.oci.empty.v1+json".into(),
            None,
        );
        let mut manifest = OciImageManifest::build(std::slice::from_ref(&layer), &config, None);
        manifest.artifact_type = Some(PACKAGE_ARTIFACT_TYPE.into());
        manifest.annotations = Some(
            [(
                CREATED_ANNOTATION.into(),
                (now - Duration::days(8)).to_rfc3339(),
            )]
            .into(),
        );
        repository
            .client
            .push(
                &repository.reference,
                &[layer],
                config,
                &repository.auth,
                Some(manifest),
            )
            .await?;
        if let Some(alias) = alias {
            let (bytes, _) = repository.manifest().await?.unwrap();
            let alias = Repository::open(&registry, alias)?;
            alias
                .client
                .push_manifest_raw(
                    &alias.reference,
                    bytes,
                    reqwest::header::HeaderValue::from_static(OCI_IMAGE_MEDIA_TYPE),
                )
                .await?;
        }
    }
    let release = ServingManifest {
        format_version: wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,
        release: ServingRelease {
            packages: [
                PackageCoordinate::new("orders", pinned_version)?,
                PackageCoordinate::new("billing", shared_pin_version)?,
            ]
            .into(),
        },
        composed_entry: None,
        components: BTreeSet::new(),
        routes: BTreeSet::new(),
        attachments: BTreeMap::new(),
        host_routes: BTreeSet::new(),
    };
    wamn_publish::development::prune(&registry, &[release], now).await?;
    assert!(
        Repository::open(&registry, &pinned)?
            .manifest()
            .await?
            .is_some()
    );
    assert!(
        Repository::open(&registry, expired)?
            .manifest()
            .await?
            .is_none()
    );
    for tag in [shared, stable, &shared_pin, pinned_alias] {
        assert!(
            Repository::open(&registry, tag)?
                .manifest()
                .await?
                .is_some()
        );
    }
    Ok(())
}
