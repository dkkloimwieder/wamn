//! Write the edge release bundle of the local release that the loop published.
//!
//! The loop's release stage mints the serving manifest and admits each
//! component into its local artifacts directory. This writer copies those exact
//! bytes into a new directory, adds the grants and the ingress guest, and pins
//! every file in `edge-release.json` (docs/plan/edge.md section 4.6). The
//! grants give the platform's route-caller role the permissions of every
//! operation that an attachment serves, as publish grants them in the cloud.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context as _, ensure};
use wamn_catalog::edge_bundle::{
    BUNDLE_FILE_NAME, BUNDLE_FORMAT, COMPONENTS_FILE_NAME, EdgeBundle, EdgeGrants,
    GRANTS_FILE_NAME, INGRESS_FILE_NAME, file_digest,
};
use wamn_catalog::{RELEASE_MANIFEST_FILE_NAME, ServingManifest};
use wamn_control_provision::operation_grants::OPERATION_CALLER_ROLE;
use wamn_engine::artifact_source::local_component_path;
use wamn_runtime::local_application::{LOCAL_FACTS_FILE, LocalApplicationFacts};

/// Write the bundle of the local release in `release` into the new directory
/// `out`, with the ingress guest at `ingress`, and return the digest of
/// `edge-release.json`.
///
/// Refuses an existing `out`, a release with a wiring, and component bytes
/// that do not match their admitted digest.
pub fn write(release: &Path, ingress: &Path, out: &Path) -> anyhow::Result<String> {
    let manifest_bytes = fs::read(release.join(RELEASE_MANIFEST_FILE_NAME))
        .with_context(|| format!("read the release manifest in {}", release.display()))?;
    let (manifest, _) = ServingManifest::from_canonical_bytes(&manifest_bytes)
        .context("the local release manifest is not canonical")?;
    ensure!(
        manifest.workflow.is_empty(),
        "the release carries wirings, and an edge box runs no wiring"
    );
    let facts: LocalApplicationFacts = serde_json::from_slice(
        &fs::read(release.join(LOCAL_FACTS_FILE)).context("read the local admission facts")?,
    )
    .context("parse the local admission facts")?;
    wamn_runtime::local_application::validate_local_facts(&facts, &manifest)?;

    let mut permissions = BTreeSet::new();
    for (id, attachment) in &manifest.attachments {
        let operation = manifest
            .components
            .iter()
            .find(|served| {
                served.package_id == attachment.package_id
                    && served.component == attachment.component
            })
            .and_then(|served| served.operations.get(&attachment.operation))
            .with_context(|| format!("attachment {id} names no released operation"))?;
        permissions.extend(operation.permissions.iter().cloned());
    }
    let mut grants = EdgeGrants::default();
    grants
        .roles
        .insert(OPERATION_CALLER_ROLE.to_owned(), permissions);

    let components = serde_json::to_vec(&facts.components).context("encode the facts")?;
    let grants = serde_json::to_vec(&grants).context("encode the grants")?;
    let ingress_bytes = fs::read(ingress).with_context(|| format!("read {}", ingress.display()))?;

    fs::create_dir(out).with_context(|| format!("create {}", out.display()))?;
    for component in &facts.components {
        let digest = &component.component_digest;
        let bytes = fs::read(local_component_path(release, digest)?)
            .with_context(|| format!("read component {digest}"))?;
        ensure!(
            file_digest(&bytes) == *digest,
            "component {digest} changed after admission"
        );
        fs::write(local_component_path(out, digest)?, bytes)?;
    }
    fs::write(out.join(RELEASE_MANIFEST_FILE_NAME), &manifest_bytes)?;
    fs::write(out.join(COMPONENTS_FILE_NAME), &components)?;
    fs::write(out.join(GRANTS_FILE_NAME), &grants)?;
    fs::write(out.join(INGRESS_FILE_NAME), &ingress_bytes)?;
    let bundle = serde_json::to_vec(&EdgeBundle {
        format: BUNDLE_FORMAT,
        manifest: file_digest(&manifest_bytes),
        components: file_digest(&components),
        grants: file_digest(&grants),
        ingress: file_digest(&ingress_bytes),
    })
    .context("encode the bundle")?;
    fs::write(out.join(BUNDLE_FILE_NAME), &bundle)?;
    Ok(file_digest(&bundle))
}
