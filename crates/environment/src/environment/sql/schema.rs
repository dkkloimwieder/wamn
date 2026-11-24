//! Package SQLx history and schema lifecycle inputs.

use super::super::{
    Platform, analyse,
    document::{DeclaredRelease, EnvironmentDocument, SchemaMode},
    stage,
};
use crate::apply_package::PackageRoot;
use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use wamn_package::package_schema::{AppliedPackage, PackageDirectory};
use wamn_release::package_source::{OpenPackage, PackageSource};

#[derive(Debug)]
pub struct Candidate {
    pub root: PackageRoot,
    pub directory: PackageDirectory,
    _artifact: Option<Box<dyn OpenPackage>>,
    migration_root: std::path::PathBuf,
}
impl Candidate {
    pub fn source(source: &dyn PackageSource, root: PackageRoot) -> anyhow::Result<Self> {
        Ok(Self {
            directory: source.read_package_root(&root)?,
            migration_root: source.migration_root(&root.path),
            root,
            _artifact: None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaSource {
    pub version: wamn_package::package_version::PackageVersion,
    pub artifact: Option<wamn_digest::Digest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackageDiff {
    pub installed: Option<wamn_package::package_version::PackageVersion>,
    pub candidate: wamn_package::package_version::PackageVersion,
    pub source_artifact: Option<wamn_digest::Digest>,
    pub migrations: crate::migrations::SchemaReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diff {
    pub application: wamn_registry::ApplicationName,
    pub release: Option<wamn_digest::Digest>,
    pub mode: SchemaMode,
    pub disposable: Option<bool>,
    pub packages: BTreeMap<wamn_package::PackageName, PackageDiff>,
    pub reset_sources: BTreeMap<wamn_package::PackageName, SchemaSource>,
    pub affected_applications: BTreeSet<wamn_registry::ApplicationName>,
    pub missing_reset_sources: BTreeSet<wamn_package::PackageName>,
    pub unknowns: Vec<String>,
}

#[derive(Debug)]
pub struct State {
    pub disposable: Option<bool>,
    pub packages: BTreeMap<wamn_package::PackageName, AppliedPackage>,
    pub unknowns: Vec<String>,
    database_url: Option<String>,
    project_env_key: wamn_registry::ProjectEnvKey,
}

pub async fn state(platform: &Platform, document: &EnvironmentDocument) -> anyhow::Result<State> {
    let Some(row) = platform
        .database
        .environment_row(&platform.system_database_url, &document.triple())
        .await?
    else {
        return Ok(State {
            disposable: None,
            packages: BTreeMap::new(),
            unknowns: Vec::new(),
            database_url: None,
            project_env_key: wamn_registry::project_env_key(
                &document.settings.org,
                &document.settings.project,
                &document.settings.env,
            ),
        });
    };
    let url = analyse::project_url(document, &row)?;
    let mut state = database_state(
        &*platform.database,
        &url,
        &wamn_registry::project_env_key(
            &document.settings.org,
            &document.settings.project,
            &document.settings.env,
        ),
    )
    .await?;
    state.disposable = Some(row.disposable);
    Ok(state)
}

pub async fn database_state(
    database: &dyn super::super::database::Database,
    url: &str,
    project_env_key: &wamn_registry::ProjectEnvKey,
) -> anyhow::Result<State> {
    let packages = database.package_baselines(url).await?;
    Ok(State {
        packages,
        database_url: Some(url.to_owned()),
        project_env_key: project_env_key.to_owned(),
        disposable: None,
        unknowns: Vec::new(),
    })
}

pub(super) async fn open(
    platform: &Platform,
    id: &wamn_package::PackageName,
    version: &wamn_package::package_version::PackageVersion,
    digest: Option<&wamn_digest::Digest>,
) -> anyhow::Result<Candidate> {
    let artifact = stage::open(platform, id, version).await?;
    let root = artifact.package_root();
    if let Some(digest) = digest {
        ensure!(
            root.artifact_digest.as_ref() == Some(digest),
            "installed schema source for {id}@{version} differs from recorded artifact {digest}"
        );
    }
    let candidate = Candidate::source(&*platform.packages, root)?;
    let manifest =
        wamn_component_policy::PackageManifest::from_slice(&candidate.directory.manifest_bytes)?;
    ensure!(
        &manifest.package.id == id && &manifest.package.version == version,
        "package artifact does not declare {id}@{version}"
    );
    Ok(Candidate {
        _artifact: Some(artifact),
        ..candidate
    })
}

/// Compare selected files with SQLx versions and checksums.
pub async fn plan(
    database: &dyn super::super::database::Database,
    document: &EnvironmentDocument,
    application: &wamn_registry::ApplicationName,
    release: Option<wamn_digest::Digest>,
    candidates: &[Candidate],
    state: &State,
) -> anyhow::Result<Diff> {
    analyse::select_application(document, Some(application))?;
    let mut migration_reader = database
        .package_migrations(state.database_url.as_deref())
        .await?;
    let mut packages = BTreeMap::new();
    let mut reset_sources = state
        .packages
        .iter()
        .map(|(id, applied)| {
            (
                id.clone(),
                SchemaSource {
                    version: applied.coordinate.package_version().to_owned(),
                    artifact: applied.artifact_digest,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    for candidate in candidates {
        let metadata = wamn_package::package_schema::package_metadata::<
            wamn_component_policy::ComponentDescriptor,
        >(&candidate.directory)?;
        let id = metadata.coordinate.package_id();
        let migrations = migration_reader
            .observe(&candidate.migration_root, &state.project_env_key, id)
            .await?;
        ensure!(
            !packages.contains_key(id),
            "schema candidate repeats package {id}"
        );
        packages.insert(
            id.to_owned(),
            PackageDiff {
                installed: state
                    .packages
                    .get(id)
                    .map(|p| p.coordinate.package_version().to_owned()),
                candidate: metadata.coordinate.package_version().to_owned(),
                source_artifact: candidate.root.artifact_digest,
                migrations,
            },
        );
        reset_sources.insert(
            id.to_owned(),
            SchemaSource {
                version: metadata.coordinate.package_version().to_owned(),
                artifact: candidate.root.artifact_digest,
            },
        );
    }
    let affected_applications = document.applications.keys().cloned().collect();
    let missing_reset_sources = document
        .applications
        .values()
        .filter(|selection| selection.current != DeclaredRelease::None)
        .flat_map(|selection| &selection.packages)
        .filter(|id| !reset_sources.contains_key(*id))
        .cloned()
        .collect();
    Ok(Diff {
        application: application.clone(),
        release,
        mode: document.schema_mode(state.disposable),
        disposable: state.disposable,
        packages,
        reset_sources,
        affected_applications,
        missing_reset_sources,
        unknowns: state.unknowns.clone(),
    })
}

pub async fn reset_candidates(
    platform: &Platform,
    diff: &Diff,
    mut selected: Vec<Candidate>,
) -> anyhow::Result<Vec<Candidate>> {
    ensure!(
        diff.missing_reset_sources.is_empty(),
        "reset is missing schema sources: {:?}",
        diff.missing_reset_sources
    );
    for (id, source) in &diff.reset_sources {
        if diff.packages.contains_key(id) {
            continue;
        }
        let digest = source
            .artifact
            .as_ref()
            .with_context(|| format!("retained package {id} has no published source"))?;
        selected.push(open(platform, id, &source.version, Some(digest)).await?);
    }
    Ok(selected)
}

/// Validate all replacement inputs before stopping users.
pub fn validate_candidates(
    candidates: &[Candidate],
    sources: &BTreeMap<wamn_package::PackageName, SchemaSource>,
) -> anyhow::Result<(Vec<wamn_component_policy::PackageManifest>, Vec<usize>)> {
    let mut packages = Vec::new();
    let mut found = BTreeSet::new();
    let mut objects = BTreeSet::new();
    for candidate in candidates {
        let manifest = wamn_component_policy::PackageManifest::from_slice(
            &candidate.directory.manifest_bytes,
        )?;
        let id = &manifest.package.id;
        ensure!(found.insert(id.clone()), "duplicate reset package {id}");
        let source = sources
            .get(id)
            .with_context(|| format!("unexpected reset package {id}"))?;
        ensure!(
            source.version == manifest.package.version
                && source.artifact == candidate.root.artifact_digest,
            "reset package {id} differs from selected source"
        );
        for dependency in manifest.base_dependencies.values() {
            ensure!(
                sources.contains_key(&dependency.package),
                "reset package {id} is missing dependency {}",
                dependency.package
            );
        }
        for model in manifest.models.values().filter(|model| model.owner == *id) {
            ensure!(
                objects.insert((model.schema.clone(), model.table.clone())),
                "conflicting reset definitions for {}.{}",
                model.schema,
                model.table
            );
        }
        packages.push(manifest);
    }
    ensure!(
        found.len() == sources.len(),
        "incomplete reset package inputs"
    );
    let order = stage::apply_order(
        &packages
            .iter()
            .map(|p| {
                (
                    &p.package.id,
                    p.base_dependencies.values().map(|d| &d.package).collect(),
                )
            })
            .collect::<Vec<_>>(),
    )?;
    Ok((packages, order))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reset_validates_selected_sources_without_document_membership() -> anyhow::Result<()> {
        let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create()?;
        let manifest = wamn_test_infrastructure::publish::source_manifest();
        std::fs::create_dir_all(scratch.path().join("generated"))?;
        std::fs::write(
            scratch.path().join("generated/wamn.json"),
            serde_json::to_vec(&manifest)?,
        )?;
        let reader = wamn_oci::deploy_packages::DeployPackages {
            registry_token_metadata: false,
            registry_auth_file: std::path::PathBuf::new(),
            insecure_registry: false,
            oci_ca_paths: Vec::new(),
        };
        let candidate = Candidate::source(
            &reader,
            PackageRoot {
                path: scratch.path().to_owned(),
                artifact_digest: None,
            },
        )?;
        let mut sources = BTreeMap::from([(
            manifest.package.id.clone(),
            SchemaSource {
                version: manifest.package.version,
                artifact: None,
            },
        )]);
        validate_candidates(std::slice::from_ref(&candidate), &sources)?;
        let source = sources.values_mut().next().unwrap();
        source.version = format!("{}-different", source.version).parse().unwrap();
        assert!(validate_candidates(&[candidate], &sources).is_err());
        Ok(())
    }
}
