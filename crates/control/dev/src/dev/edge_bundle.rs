//! Write the edge release bundle of the local release that the loop published.
//!
//! The loop's release stage publishes the serving manifest and admits each
//! component into its local artifacts directory. This writer copies those exact
//! bytes into a new directory, adds the grants, composes the pinned http-route
//! guest with the release's application into the entry as cloud publish does,
//! and pins every file in `edge-release.json` (docs/history/edge.md section
//! 4.6). The edge loads the entry and composes nothing. The
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
    GRANTS_FILE_NAME, INGRESS_FILE_NAME,
};
use wamn_catalog::{PackageCoordinate, RELEASE_MANIFEST_FILE_NAME, ServingManifest};
use wamn_digest::bytes_sha256;
use wamn_engine::artifact_source::local_component_path;
use wamn_identity_protocol::ADMIN_ROLE;
use wamn_registry::project_env_key;
use wamn_runtime::local_application::{LOCAL_FACTS_FILE, LocalApplicationFacts};

use super::coordinator::{NODE_CAPABILITY, PACKAGE_COMPONENTS, POSTGRES_CAPABILITY, TemporaryFile};
use wamn_package_control::component_admission::{AdmitComponentRequest, admit_component};
use wamn_package_control::component_declaration::{
    authored_base_digests, render_declaration_document,
};
use wamn_release_control::publish_release::{
    PublishReleaseRequest, assemble_local_release, compose_pinned_application,
    compose_release_entry,
};

/// The environment coordinate of the box: the edge configuration's `[target]`.
/// The `project_env_key` of the release is [`project_env_key`] of the three.
#[derive(Debug, Clone, Copy)]
pub struct EdgeTarget<'a> {
    pub org: &'a str,
    pub project: &'a str,
    pub environment: &'a str,
}

impl EdgeTarget<'_> {
    fn project_env_key(&self) -> String {
        project_env_key(self.org, self.project, self.environment)
    }
}

/// Write the bundle of the local release in `release` into the new directory
/// `out`, with the entry composed from the http-route guest at `ingress`, and
/// return the digest of `edge-release.json`. `target` is the coordinate the edge
/// configuration names.
///
/// Refuses an existing `out`, a release with a wiring, component bytes that do
/// not match their admitted digest, and a route guest that differs from its
/// pin.
pub fn write(
    release: &Path,
    ingress: &Path,
    out: &Path,
    target: EdgeTarget<'_>,
) -> anyhow::Result<String> {
    let manifest_bytes = fs::read(release.join(RELEASE_MANIFEST_FILE_NAME))
        .with_context(|| format!("read the release manifest in {}", release.display()))?;
    let facts: LocalApplicationFacts = serde_json::from_slice(
        &fs::read(release.join(LOCAL_FACTS_FILE)).context("read the local admission facts")?,
    )
    .context("parse the local admission facts")?;
    let (manifest, _) = ServingManifest::from_canonical_bytes(&manifest_bytes)?;
    let application = &manifest
        .composed_entry
        .as_ref()
        .context("local release has no composed entry")?
        .application;
    write_bundle(
        &manifest_bytes,
        &facts,
        &target.project_env_key(),
        application,
        |digest| {
            fs::read(local_component_path(release, digest)?)
                .with_context(|| format!("read component {digest}"))
        },
        ingress,
        out,
    )
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
    target: &EdgeTarget<'_>,
    out: &Path,
) -> anyhow::Result<String> {
    let template = package
        .join(PACKAGE_COMPONENTS)
        .join(format!("{component}.json.in"));
    let project_env_key = target.project_env_key();
    let document = render_declaration_document(&template, &project_env_key)?;
    let declaration = TemporaryFile::write(&serde_json::to_vec(&document)?)?;
    let admission = admit_component(AdmitComponentRequest {
        package: package.to_owned(),
        component_bytes: bytes.to_owned(),
        declaration: declaration.path().to_owned(),
        admitted_platform_packages: vec![
            NODE_CAPABILITY.to_owned(),
            POSTGRES_CAPABILITY.to_owned(),
        ],
        base_digests: authored_base_digests(package)?
            .into_values()
            .map(String::from)
            .collect(),
    })?;
    let request = PublishReleaseRequest {
        applications: [(
            admission.package_id().to_owned(),
            wamn_catalog::ApplicationDefinition {
                connections: std::collections::BTreeMap::new(),
                packages: [admission.package_id().to_owned()].into(),
            },
        )]
        .into(),
        application: None,
        org: target.org.to_owned(),
        project: target.project.to_owned(),
        environment: target.environment.to_owned(),
        packages: vec![PackageCoordinate::new(
            admission.package_id(),
            admission.package_version(),
        )?],
    };
    let assembled = assemble_local_release(
        &request,
        &[package.to_owned()],
        std::slice::from_ref(&admission),
    )?;
    let entry = compose_release_entry(
        &assembled.published,
        std::slice::from_ref(&admission),
        &fs::read(ingress)?,
        &request.applications,
        None,
    )?;
    write_bundle(
        &assembled.published.canonical_bytes,
        &assembled.facts,
        &project_env_key,
        entry.application(),
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
    project_env_key: &str,
    application: &wamn_catalog::ServingEntryMember,
    read_component: impl Fn(&str) -> anyhow::Result<Vec<u8>>,
    ingress: &Path,
    out: &Path,
) -> anyhow::Result<String> {
    let (manifest, _) = ServingManifest::from_canonical_bytes(manifest_bytes)
        .context("the local release manifest is not canonical")?;
    wamn_runtime::local_application::validate_local_facts(facts, &manifest, project_env_key)?;

    let mut permissions = BTreeSet::new();
    for (id, attachment) in &manifest.attachments {
        let operation = manifest
            .components
            .iter()
            .find(|served| {
                served.package_id == attachment.package_id
                    && served.descriptor.component == attachment.component
            })
            .and_then(|served| served.descriptor.operations.get(&attachment.operation))
            .with_context(|| format!("attachment {id} names no released operation"))?;
        permissions.extend(operation.registered_operation.iter().cloned());
    }
    let mut grants = EdgeGrants::default();
    grants.roles.insert(ADMIN_ROLE.to_owned(), permissions);

    let components = serde_json::to_vec(&facts.components).context("encode the facts")?;
    let grants = serde_json::to_vec(&grants).context("encode the grants")?;
    let route = fs::read(ingress).with_context(|| format!("read {}", ingress.display()))?;
    let mut bodies = Vec::with_capacity(facts.components.len());
    for component in &facts.components {
        let digest = &component.component_digest;
        let bytes = read_component(digest)?;
        ensure!(
            bytes_sha256(&bytes) == *digest,
            "component {digest} changed after admission"
        );
        bodies.push(bytes);
    }
    let (application, bytes) = facts
        .components
        .iter()
        .zip(&bodies)
        .find(|(fact, _)| {
            fact.target.package_id == application.package_id
                && fact.component == application.component
                && fact.component_digest == application.digest
        })
        .context("edge release has no named application member")?;
    let entry =
        compose_pinned_application(application, bytes, &route).context("compose the edge entry")?;
    let ingress_bytes = entry.bytes();

    fs::create_dir(out).with_context(|| format!("create {}", out.display()))?;
    for (component, bytes) in facts.components.iter().zip(bodies) {
        fs::write(
            local_component_path(out, &component.component_digest)?,
            bytes,
        )?;
    }
    fs::write(out.join(RELEASE_MANIFEST_FILE_NAME), manifest_bytes)?;
    fs::write(out.join(COMPONENTS_FILE_NAME), &components)?;
    fs::write(out.join(GRANTS_FILE_NAME), &grants)?;
    fs::write(out.join(INGRESS_FILE_NAME), ingress_bytes)?;
    let bundle = serde_json::to_vec(&EdgeBundle {
        format: BUNDLE_FORMAT,
        manifest: bytes_sha256(manifest_bytes),
        components: bytes_sha256(&components),
        grants: bytes_sha256(&grants),
        ingress: bytes_sha256(ingress_bytes),
    })
    .context("encode the bundle")?;
    fs::write(out.join(BUNDLE_FILE_NAME), &bundle)?;
    Ok(bytes_sha256(&bundle))
}
