//! Package SQLx history and schema lifecycle inputs.

use super::super::{
    Platform, analyse,
    document::{DeclaredRelease, EnvironmentDocument, SchemaMode},
    stage,
};
use crate::apply_package::{self, PackageRoot};
use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use sqlx::Row as _;
use std::collections::{BTreeMap, BTreeSet};
use wamn_catalog::package_schema::{AppliedPackage, PackageDirectory};

#[derive(Debug)]
pub struct Candidate {
    pub root: PackageRoot,
    pub directory: PackageDirectory,
    _artifact: Option<wamn_publish::package_artifact::OpenedPackage>,
}
impl Candidate {
    pub fn source(root: PackageRoot) -> anyhow::Result<Self> {
        Ok(Self {
            directory: apply_package::read_package_root(&root)?,
            root,
            _artifact: None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaSource {
    pub version: wamn_catalog::package_version::PackageVersion,
    pub artifact: Option<wamn_digest::Digest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackageDiff {
    pub installed: Option<wamn_catalog::package_version::PackageVersion>,
    pub candidate: wamn_catalog::package_version::PackageVersion,
    pub source_artifact: Option<wamn_digest::Digest>,
    pub migrations: crate::migrations::SchemaReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diff {
    pub application: wamn_catalog::ApplicationName,
    pub release: Option<wamn_digest::Digest>,
    pub mode: SchemaMode,
    pub disposable: Option<bool>,
    pub packages: BTreeMap<wamn_catalog::PackageName, PackageDiff>,
    pub reset_sources: BTreeMap<wamn_catalog::PackageName, SchemaSource>,
    pub affected_applications: BTreeSet<wamn_catalog::ApplicationName>,
    pub missing_reset_sources: BTreeSet<wamn_catalog::PackageName>,
    pub unknowns: Vec<String>,
}

#[derive(Debug)]
pub struct State {
    pub disposable: Option<bool>,
    pub packages: BTreeMap<wamn_catalog::PackageName, AppliedPackage>,
    pub unknowns: Vec<String>,
    database_url: Option<String>,
    project_env_key: wamn_registry::ProjectEnvKey,
}

pub async fn state(platform: &Platform, document: &EnvironmentDocument) -> anyhow::Result<State> {
    let mut system = wamn_postgres::connect(&platform.system_database_url).await?;
    let mut system = crate::provision_project_env::provisioning_transaction(&mut system).await?;
    let Some(row) = analyse::read_row(&mut system, document).await? else {
        return Ok(State {
            disposable: None,
            packages: BTreeMap::new(),
            unknowns: Vec::new(),
            database_url: None,
            project_env_key: wamn_registry::project_env_key(
                &document.org,
                &document.project,
                &document.env,
            ),
        });
    };
    system.commit().await?;
    let url = analyse::project_url(platform, document, &row).await?;
    let mut state = database_state(
        &url,
        &wamn_registry::project_env_key(&document.org, &document.project, &document.env),
    )
    .await?;
    state.disposable = Some(row.disposable);
    Ok(state)
}

pub async fn database_state(
    url: &str,
    project_env_key: &wamn_registry::ProjectEnvKey,
) -> anyhow::Result<State> {
    let mut client = analyse::connect_project(url).await?;
    let mut packages = BTreeMap::new();
    for row in sqlx::query("SELECT package_id, package_version, artifact_digest FROM catalog.packages WHERE project_env_key=$1")
        .bind(project_env_key).fetch_all(&mut client).await? {
        let id: wamn_catalog::PackageName = row.try_get(0)?;
        packages.insert(id.clone(), AppliedPackage {
            coordinate: wamn_catalog::PackageCoordinate::new(id, row.try_get::<wamn_catalog::package_version::PackageVersion, _>(1)?)?,
            artifact_digest: row.get(2),
        });
    }
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
    id: &wamn_catalog::PackageName,
    version: &wamn_catalog::package_version::PackageVersion,
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
    let candidate = Candidate::source(root)?;
    let manifest =
        wamn_catalog::manifest::PackageManifest::from_slice(&candidate.directory.manifest_bytes)?;
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
    document: &EnvironmentDocument,
    application: &wamn_catalog::ApplicationName,
    release: Option<wamn_digest::Digest>,
    candidates: &[Candidate],
    state: &State,
) -> anyhow::Result<Diff> {
    analyse::select_application(document, Some(application))?;
    let mut client = match &state.database_url {
        Some(url) => Some(analyse::connect_project(url).await?),
        None => None,
    };
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
        let metadata = wamn_catalog::package_schema::package_metadata(&candidate.directory)?;
        let id = metadata.coordinate.package_id();
        let migrator = wamn_codegen::migrations::migrator(
            &wamn_publish::package_source::migration_root(&candidate.root.path),
        )
        .await?;
        let migrations = if let Some(client) = &mut client {
            crate::migrations::observe(
                client,
                &wamn_provision::migration::history_table(&state.project_env_key, id),
                &migrator.migrations,
            )
            .await?
        } else {
            crate::migrations::SchemaReport {
                pending: migrator
                    .iter()
                    .filter(|source| source.migration_type.is_up_migration())
                    .map(|source| crate::migrations::MigrationFile {
                        version: source.version,
                        checksum: hex::encode(&source.checksum),
                    })
                    .collect(),
                ..Default::default()
            }
        };
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
    sources: &BTreeMap<wamn_catalog::PackageName, SchemaSource>,
) -> anyhow::Result<(Vec<wamn_catalog::manifest::PackageManifest>, Vec<usize>)> {
    let mut packages = Vec::new();
    let mut found = BTreeSet::new();
    let mut objects = BTreeSet::new();
    for candidate in candidates {
        let manifest = wamn_catalog::manifest::PackageManifest::from_slice(
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
        let manifest = wamn_publish::test_support::source_manifest();
        std::fs::create_dir_all(scratch.path().join("generated"))?;
        std::fs::write(
            scratch.path().join("generated/wamn.json"),
            serde_json::to_vec(&manifest)?,
        )?;
        let candidate = Candidate::source(PackageRoot {
            path: scratch.path().to_owned(),
            artifact_digest: None,
        })?;
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
