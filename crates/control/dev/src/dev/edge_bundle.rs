//! Write edge bundles from built packages and admitted component bytes.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context as _, ensure};
use wamn_catalog::AdmittedComponent;
use wamn_catalog::edge_bundle::{
    BUNDLE_FILE_NAME, BUNDLE_FORMAT, COMPONENTS_FILE_NAME, EdgeBundle, EdgeGrants,
    GRANTS_FILE_NAME, INGRESS_FILE_NAME,
};
use wamn_catalog::{PackageCoordinate, RELEASE_MANIFEST_FILE_NAME, ServingManifest};
use wamn_digest::bytes_sha256;
use wamn_engine::artifact_source::local_component_path;
use wamn_identity_protocol::ADMIN_ROLE;
use wamn_registry::project_env_key;

use wamn_publish::component_admission::{AdmitComponentRequest, admit_component};
use wamn_publish::component_declaration::{authored_base_digests, render_package_declaration};
use wamn_publish::publish_release::{
    ComposedApplication, PublishReleaseRequest, application_member, assemble_release,
    compose_application,
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

/// Write the bundle of `package`, whose component `component` is built at
/// `bytes`, into the new directory `out`, and return the digest of
/// `edge-release.json`.
///
/// It admits the bytes and assembles the release without a database or session.
pub fn write_package(
    package: &Path,
    component: &str,
    bytes: &Path,
    ingress: &Path,
    target: &EdgeTarget<'_>,
    out: &Path,
) -> anyhow::Result<String> {
    let project_env_key = target.project_env_key();
    let document = render_package_declaration(package, component, &project_env_key)?;
    let declaration = TemporaryFile::write(&serde_json::to_vec(&document)?)?;
    let admission = admit_component(AdmitComponentRequest {
        package: package.to_owned(),
        component_bytes: bytes.to_owned(),
        declaration: declaration.path().to_owned(),
        admitted_platform_packages: vec!["wamn:node".to_owned(), "wamn:postgres".to_owned()],
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
    let assembled = assemble_release(
        &request,
        &[package.to_owned()],
        std::slice::from_ref(&admission),
        &[],
    )?;
    let (member, application_bytes) = application_member(
        &assembled.published,
        &[(admission.facts(), admission.bytes())],
        &request.applications,
        None,
    )?;
    let entry = compose_application(member, application_bytes, &fs::read(ingress)?)?;
    write_bundle(
        &assembled.published.canonical_bytes,
        &assembled.components,
        &entry,
        |digest| {
            ensure!(
                digest == admission.component_digest(),
                "the release names component {digest}, which is not the built one"
            );
            fs::read(bytes).with_context(|| format!("read {}", bytes.display()))
        },
        out,
    )
}

fn write_bundle(
    manifest_bytes: &[u8],
    facts: &[AdmittedComponent],
    entry: &ComposedApplication,
    read_component: impl Fn(&str) -> anyhow::Result<Vec<u8>>,
    out: &Path,
) -> anyhow::Result<String> {
    let (manifest, _) = ServingManifest::from_canonical_bytes(manifest_bytes)
        .context("the local release manifest is not canonical")?;

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

    let components = serde_json::to_vec(facts).context("encode the facts")?;
    let grants = serde_json::to_vec(&grants).context("encode the grants")?;
    let mut bodies = Vec::with_capacity(facts.len());
    for component in facts {
        let digest = &component.component_digest;
        let bytes = read_component(digest)?;
        ensure!(
            bytes_sha256(&bytes) == *digest,
            "component {digest} changed after admission"
        );
        bodies.push(bytes);
    }
    let ingress_bytes = entry.bytes();

    fs::create_dir(out).with_context(|| format!("create {}", out.display()))?;
    for (component, bytes) in facts.iter().zip(bodies) {
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

struct TemporaryFile(std::path::PathBuf);

impl TemporaryFile {
    fn write(bytes: &[u8]) -> anyhow::Result<Self> {
        use std::io::Write as _;
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "wamn-edge-{}-{}.json",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let temporary = Self(path);
        file.write_all(bytes)?;
        Ok(temporary)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
