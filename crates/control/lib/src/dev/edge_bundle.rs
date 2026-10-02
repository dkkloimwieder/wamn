//! Write the edge release bundle of the local release that the loop published.
//!
//! The loop's release stage publishes the serving manifest and admits each
//! component into its local artifacts directory. This writer copies those exact
//! bytes into a new directory, adds the grants and the ingress guest, and pins
//! every file in `edge-release.json` (docs/plan/edge.md section 4.6). The
//! grants give the built-in role `admin` the permissions of every operation
//! that an attachment serves, as `admin` holds every served operation in the
//! cloud.
//!
//! [`write_package`] writes the same bundle from one package and its built
//! component without a session, for the edge tests.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context as _, ensure};
use wamn_catalog::edge_bundle::{
    BUNDLE_FILE_NAME, BUNDLE_FORMAT, COMPONENTS_FILE_NAME, EdgeBundle, EdgeGrants,
    GRANTS_FILE_NAME, INGRESS_FILE_NAME, file_digest,
};
use wamn_catalog::{PackageCoordinate, RELEASE_MANIFEST_FILE_NAME, ServingManifest};
use wamn_engine::artifact_source::local_component_path;
use wamn_project_state::ADMIN_ROLE;
use wamn_runtime::local_application::{LOCAL_FACTS_FILE, LocalApplicationFacts};

use super::coordinator::{
    NODE_CAPABILITY, PACKAGE_ATTACHMENTS, PACKAGE_COMPONENTS, POSTGRES_CAPABILITY, TemporaryFile,
};
use crate::component_declaration::{authored_base_digests, render_declaration_document};
use crate::publish_release::{PublishReleaseRequest, assemble_local_release};
use crate::push_component::{AdmitComponentRequest, admit_component};

/// Write the bundle of the local release in `release` into the new directory
/// `out`, with the ingress guest at `ingress`, and return the digest of
/// `edge-release.json`.
///
/// Refuses an existing `out`, a release with a wiring, and component bytes
/// that do not match their admitted digest.
pub fn write(release: &Path, ingress: &Path, out: &Path) -> anyhow::Result<String> {
    let manifest_bytes = fs::read(release.join(RELEASE_MANIFEST_FILE_NAME))
        .with_context(|| format!("read the release manifest in {}", release.display()))?;
    let facts: LocalApplicationFacts = serde_json::from_slice(
        &fs::read(release.join(LOCAL_FACTS_FILE)).context("read the local admission facts")?,
    )
    .context("parse the local admission facts")?;
    write_bundle(
        &manifest_bytes,
        &facts,
        |digest| {
            fs::read(local_component_path(release, digest)?)
                .with_context(|| format!("read component {digest}"))
        },
        ingress,
        out,
    )
}

/// The release identity of a bundle that [`write_package`] writes.
#[derive(Debug)]
pub struct PackageRelease<'a> {
    pub tenant: &'a str,
    pub environment: &'a str,
    pub publisher: &'a str,
    pub route_host: &'a str,
}

/// Write the bundle of `package`, whose component `component` is built at
/// `bytes`, into the new directory `out`, and return the digest of
/// `edge-release.json`.
///
/// It admits the bytes and assembles the release as the loop's admit and
/// release stages do, with no session and no database, then writes the
/// bundle as [`write`] does.
pub fn write_package(
    package: &Path,
    component: &str,
    bytes: &Path,
    ingress: &Path,
    release: &PackageRelease<'_>,
    out: &Path,
) -> anyhow::Result<String> {
    let template = package
        .join(PACKAGE_COMPONENTS)
        .join(format!("{component}.json.in"));
    let document =
        render_declaration_document(&template, release.tenant, &authored_base_digests(package)?)?;
    let declaration = TemporaryFile::write(&serde_json::to_vec(&document)?)?;
    let admission = admit_component(AdmitComponentRequest {
        package: package.to_owned(),
        component_bytes: bytes.to_owned(),
        declaration: declaration.path().to_owned(),
        admitted_platform_packages: vec![
            NODE_CAPABILITY.to_owned(),
            POSTGRES_CAPABILITY.to_owned(),
        ],
    })?;
    let request = PublishReleaseRequest {
        database_url: String::new(),
        control_database_url: String::new(),
        org: String::new(),
        project: String::new(),
        tenant: release.tenant.to_owned(),
        effective_release_id: 1,
        environment: release.environment.to_owned(),
        verified_publisher_principal: release.publisher.to_owned(),
        run_schema: String::new(),
        packages: vec![PackageCoordinate::new(
            admission.package_id(),
            admission.package_version(),
        )?],
        wirings: Vec::new(),
        attachments: vec![package.join(PACKAGE_ATTACHMENTS)],
        route_host: Some(release.route_host.to_owned()),
        package_manifests: vec![wamn_schema_generator::package_manifest_path(package)],
    };
    let assembled = assemble_local_release(&request, std::slice::from_ref(&admission), Vec::new())?;
    write_bundle(
        &assembled.published.canonical_bytes,
        &assembled.facts,
        |digest| {
            ensure!(
                digest == admission.component_digest(),
                "the release names component {digest}, which is not the built one"
            );
            fs::read(bytes).with_context(|| format!("read {}", bytes.display()))
        },
        ingress,
        out,
    )
}

fn write_bundle(
    manifest_bytes: &[u8],
    facts: &LocalApplicationFacts,
    read_component: impl Fn(&str) -> anyhow::Result<Vec<u8>>,
    ingress: &Path,
    out: &Path,
) -> anyhow::Result<String> {
    let (manifest, _) = ServingManifest::from_canonical_bytes(manifest_bytes)
        .context("the local release manifest is not canonical")?;
    ensure!(
        manifest.workflow.is_empty(),
        "the release carries wirings, and an edge box runs no wiring"
    );
    wamn_runtime::local_application::validate_local_facts(facts, &manifest)?;

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
    grants.roles.insert(ADMIN_ROLE.to_owned(), permissions);

    let components = serde_json::to_vec(&facts.components).context("encode the facts")?;
    let grants = serde_json::to_vec(&grants).context("encode the grants")?;
    let ingress_bytes = fs::read(ingress).with_context(|| format!("read {}", ingress.display()))?;

    fs::create_dir(out).with_context(|| format!("create {}", out.display()))?;
    for component in &facts.components {
        let digest = &component.component_digest;
        let bytes = read_component(digest)?;
        ensure!(
            file_digest(&bytes) == *digest,
            "component {digest} changed after admission"
        );
        fs::write(local_component_path(out, digest)?, bytes)?;
    }
    fs::write(out.join(RELEASE_MANIFEST_FILE_NAME), manifest_bytes)?;
    fs::write(out.join(COMPONENTS_FILE_NAME), &components)?;
    fs::write(out.join(GRANTS_FILE_NAME), &grants)?;
    fs::write(out.join(INGRESS_FILE_NAME), &ingress_bytes)?;
    let bundle = serde_json::to_vec(&EdgeBundle {
        format: BUNDLE_FORMAT,
        manifest: file_digest(manifest_bytes),
        components: file_digest(&components),
        grants: file_digest(&grants),
        ingress: file_digest(&ingress_bytes),
    })
    .context("encode the bundle")?;
    fs::write(out.join(BUNDLE_FILE_NAME), &bundle)?;
    Ok(file_digest(&bundle))
}
