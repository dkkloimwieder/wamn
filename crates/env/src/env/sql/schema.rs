//! Package SQLx history and schema lifecycle inputs.

use super::super::{
    Platform, analyse,
    document::{DeclaredRelease, EnvDocument, SchemaMode},
    stage,
};
use crate::env_sql_effects;
use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use wamn_package::package_schema::{AppliedPackage, PackageDirectory};
use wamn_release::{
    package_source::{PackageRoot, migration_root, read_package_root},
    release_package::OpenedPackage,
};

#[derive(Debug)]
pub struct Candidate {
    pub root: PackageRoot,
    pub directory: PackageDirectory,
    _artifact: Option<OpenedPackage>,
    migration_root: std::path::PathBuf,
}
impl Candidate {
    pub fn source(root: PackageRoot) -> anyhow::Result<Self> {
        Ok(Self {
            directory: read_package_root(&root)?,
            migration_root: migration_root(&root.path),
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
    pub app: wamn_registry::AppName,
    pub release: Option<wamn_digest::Digest>,
    pub mode: SchemaMode,
    pub disposable: Option<bool>,
    pub packages: BTreeMap<wamn_package::PackageName, PackageDiff>,
    pub reset_sources: BTreeMap<wamn_package::PackageName, SchemaSource>,
    pub affected_apps: BTreeSet<wamn_registry::AppName>,
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

pub async fn state(platform: &Platform, document: &EnvDocument) -> anyhow::Result<State> {
    let Some(row) =
        env_sql_effects::env_row(&platform.system_database_url, &document.triple()).await?
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
    url: &str,
    project_env_key: &wamn_registry::ProjectEnvKey,
) -> anyhow::Result<State> {
    let packages = env_sql_effects::package_baselines(url).await?;
    Ok(State {
        packages,
        database_url: Some(url.to_owned()),
        project_env_key: project_env_key.to_owned(),
        disposable: None,
        unknowns: Vec::new(),
    })
}

/// Compare selected files with SQLx versions and checksums.
pub async fn plan(
    document: &EnvDocument,
    app: &wamn_registry::AppName,
    release: Option<wamn_digest::Digest>,
    candidates: &[Candidate],
    state: &State,
) -> anyhow::Result<Diff> {
    analyse::select_app(document, Some(app))?;
    let mut migration_reader =
        env_sql_effects::package_migrations(state.database_url.as_deref()).await?;
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
        let migrations = env_sql_effects::observe_package_migrations(
            &mut migration_reader,
            &candidate.migration_root,
            &state.project_env_key,
            id,
        )
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
    let affected_apps = document.apps.keys().cloned().collect();
    let missing_reset_sources = document
        .apps
        .values()
        .filter(|selection| selection.current != DeclaredRelease::None)
        .flat_map(|selection| &selection.packages)
        .filter(|id| !reset_sources.contains_key(*id))
        .cloned()
        .collect();
    Ok(Diff {
        app: app.clone(),
        release,
        mode: document.schema_mode(state.disposable),
        disposable: state.disposable,
        packages,
        reset_sources,
        affected_apps,
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
    let retained = diff
        .reset_sources
        .iter()
        .filter(|(id, _)| !diff.packages.contains_key(*id))
        .map(|(id, source)| (id.clone(), source.artifact));
    for (id, artifact, manifest) in stage::open(platform, retained).await? {
        let version = &diff.reset_sources[&id].version;
        ensure!(
            &manifest.package.version == version,
            "package artifact does not declare {id}@{version}"
        );
        let candidate = Candidate::source(artifact.package_root())?;
        selected.push(Candidate {
            _artifact: Some(artifact),
            ..candidate
        });
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
