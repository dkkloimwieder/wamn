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
    pub version: String,
    pub artifact: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackageDiff {
    pub installed: Option<String>,
    pub candidate: String,
    pub source_artifact: Option<String>,
    pub migrations: crate::migrations::SchemaReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diff {
    pub application: String,
    pub release: Option<String>,
    pub mode: SchemaMode,
    pub disposable: Option<bool>,
    pub packages: BTreeMap<String, PackageDiff>,
    pub reset_sources: BTreeMap<String, SchemaSource>,
    pub affected_applications: BTreeSet<String>,
    pub missing_reset_sources: BTreeSet<String>,
    pub unknowns: Vec<String>,
}

#[derive(Debug, Default)]
pub struct State {
    pub disposable: Option<bool>,
    pub packages: BTreeMap<String, AppliedPackage>,
    pub unknowns: Vec<String>,
    database_url: Option<String>,
    project_env_key: String,
}

pub async fn state(platform: &Platform, document: &EnvironmentDocument) -> anyhow::Result<State> {
    let mut system = wamn_postgres::connect(&platform.system_database_url).await?;
    let Some(row) = analyse::read_row(&mut system, document).await? else {
        return Ok(State::default());
    };
    let url = analyse::project_url(platform, document, &row).await?;
    let mut state = database_state(
        &url,
        &wamn_registry::project_env_key(&document.org, &document.project, &document.env),
    )
    .await?;
    state.disposable = Some(row.disposable);
    Ok(state)
}

pub async fn database_state(url: &str, project_env_key: &str) -> anyhow::Result<State> {
    let mut client = analyse::connect_project(url).await?;
    let mut packages = BTreeMap::new();
    for row in sqlx::query("SELECT package_id, package_version, artifact_digest FROM catalog.packages WHERE project_env_key=$1")
        .bind(project_env_key).fetch_all(&mut client).await? {
        let id: String = row.get(0);
        packages.insert(id.clone(), AppliedPackage {
            coordinate: wamn_catalog::PackageCoordinate::new(&id, row.get::<&str, _>(1))?,
            artifact_digest: row.get(2),
        });
    }
    Ok(State {
        packages,
        database_url: Some(url.to_owned()),
        project_env_key: project_env_key.to_owned(),
        ..State::default()
    })
}

/// Resolve an explicit schema release without editing the application's code selection.
pub async fn release_candidates(
    platform: &Platform,
    document: &EnvironmentDocument,
    application: &str,
    release: Option<&str>,
) -> anyhow::Result<(String, Vec<Candidate>)> {
    ensure!(
        document.applications.contains_key(application),
        "application {application} is not declared"
    );
    let revision;
    let release = if let Some(digest) = release {
        digest
    } else {
        let name = analyse::application_release_name(document, application)?;
        revision = analyse::read_revision(platform, &name)
            .await?
            .context("application has no current schema source")?;
        revision
            .manifest_digest()
            .context("application revision has no release digest")?
    };
    let manifest = analyse::pull_release_manifest(platform, release).await?;
    let mut candidates = Vec::new();
    for package in &manifest.release.packages {
        candidates.push(
            open(
                platform,
                package.package_id(),
                package.package_version(),
                None,
            )
            .await?,
        );
    }
    Ok((release.to_owned(), candidates))
}

pub(super) async fn open(
    platform: &Platform,
    id: &str,
    version: &str,
    digest: Option<&str>,
) -> anyhow::Result<Candidate> {
    let artifact = stage::open(platform, id, version).await?;
    let root = artifact.package_root();
    if let Some(digest) = digest {
        ensure!(
            root.artifact_digest.as_deref() == Some(digest),
            "installed schema source for {id}@{version} differs from recorded artifact {digest}"
        );
    }
    let candidate = Candidate::source(root)?;
    let manifest =
        wamn_compiler::manifest::PackageManifest::from_slice(&candidate.directory.manifest_bytes)?;
    ensure!(
        manifest.package.id == id && manifest.package.version == version,
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
    application: &str,
    release: Option<String>,
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
                    artifact: applied.artifact_digest.clone(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    for candidate in candidates {
        let metadata = wamn_catalog::package_schema::package_metadata(&candidate.directory)?;
        let id = metadata.coordinate.package_id();
        let migrator = wamn_compiler::migrations::migrator(
            &wamn_publish::package_source::migration_root(&candidate.root.path),
        )
        .await?;
        let migrations = if let Some(client) = &mut client {
            crate::migrations::observe(
                client,
                &wamn_control_provision::migration::history_table(&state.project_env_key, id),
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
                source_artifact: candidate.root.artifact_digest.clone(),
                migrations,
            },
        );
        reset_sources.insert(
            id.to_owned(),
            SchemaSource {
                version: metadata.coordinate.package_version().to_owned(),
                artifact: candidate.root.artifact_digest.clone(),
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
        application: application.into(),
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
            .as_deref()
            .with_context(|| format!("retained package {id} has no published source"))?;
        selected.push(open(platform, id, &source.version, Some(digest)).await?);
    }
    Ok(selected)
}

/// Validate all replacement inputs before stopping users.
pub fn validate_candidates(
    candidates: &[Candidate],
    sources: &BTreeMap<String, SchemaSource>,
) -> anyhow::Result<(Vec<wamn_compiler::manifest::PackageManifest>, Vec<usize>)> {
    let mut packages = Vec::new();
    let mut found = BTreeSet::new();
    let mut objects = BTreeSet::new();
    for candidate in candidates {
        let manifest = wamn_compiler::manifest::PackageManifest::from_slice(
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
                    p.package.id.as_str(),
                    p.base_dependencies
                        .values()
                        .map(|d| d.package.as_str())
                        .collect(),
                )
            })
            .collect::<Vec<_>>(),
    )?;
    Ok((packages, order))
}

pub async fn apply(
    platform: &Platform,
    document: &EnvironmentDocument,
    application: &str,
    release: &str,
) -> anyhow::Result<()> {
    let (_, candidates) =
        release_candidates(platform, document, application, Some(release)).await?;
    let state = state(platform, document).await?;
    ensure!(
        document.schema_mode(state.disposable) == SchemaMode::Migrate,
        "reset mode requires the environment reset operation"
    );
    let mut system = wamn_postgres::connect(&platform.system_database_url).await?;
    let row = analyse::read_row(&mut system, document)
        .await?
        .context("environment database is absent")?;
    let url = analyse::project_url(platform, document, &row).await?;
    apply_candidates(platform, document, candidates, &url).await
}

pub(super) async fn apply_candidates(
    platform: &Platform,
    document: &EnvironmentDocument,
    candidates: Vec<Candidate>,
    url: &str,
) -> anyhow::Result<()> {
    let manifests = candidates
        .iter()
        .map(|c| wamn_compiler::manifest::PackageManifest::from_slice(&c.directory.manifest_bytes))
        .collect::<Result<Vec<_>, _>>()?;
    let order = stage::apply_order(
        &manifests
            .iter()
            .map(|p| {
                (
                    p.package.id.as_str(),
                    p.base_dependencies
                        .values()
                        .map(|d| d.package.as_str())
                        .collect(),
                )
            })
            .collect::<Vec<_>>(),
    )?;
    let mut candidates = candidates.into_iter().map(Some).collect::<Vec<_>>();
    let publisher = platform.auth_changes().await?;
    for index in order {
        let candidate = candidates[index].take().expect("each package applies once");
        apply_package::apply_package_schema(apply_package::ApplyPackageRequest {
            migration_database_url: wamn_control_provision::migration::configuration(
                url,
                &document.org,
                &document.project,
                &document.env,
            )?,
            component_source: platform.component_source()?,
            package: candidate.root,
            changes: publisher.clone(),
            database_url: url.to_owned(),
            org: document.org.clone(),
            project: document.project.clone(),
            environment: document.env.clone(),
        })
        .await?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Status {
    pub mode: SchemaMode,
    pub disposable: Option<bool>,
    pub installed: BTreeMap<String, SchemaSource>,
    pub applications: BTreeMap<String, Diff>,
    pub unknowns: Vec<String>,
}

pub async fn status(
    platform: &mut Platform,
    document: &EnvironmentDocument,
) -> anyhow::Result<Status> {
    super::super::runtime::resolve_registry(platform, &document.org).await?;
    let state = state(platform, document).await?;
    let mut applications = BTreeMap::new();
    let mut unknowns = state.unknowns.clone();
    for application in document.applications.keys() {
        match release_candidates(platform, document, application, None).await {
            Ok((release, candidates)) => {
                applications.insert(
                    application.clone(),
                    plan(document, application, Some(release), &candidates, &state).await?,
                );
            }
            Err(error) => unknowns.push(format!(
                "application {application} candidate unknown: {error:#}"
            )),
        }
    }
    Ok(Status {
        mode: document.schema_mode(state.disposable),
        disposable: state.disposable,
        installed: state
            .packages
            .into_iter()
            .map(|(id, p)| {
                (
                    id,
                    SchemaSource {
                        version: p.coordinate.package_version().to_owned(),
                        artifact: p.artifact_digest,
                    },
                )
            })
            .collect(),
        applications,
        unknowns,
    })
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
                version: manifest.package.version.clone(),
                artifact: None,
            },
        )]);
        validate_candidates(std::slice::from_ref(&candidate), &sources)?;
        sources
            .values_mut()
            .next()
            .unwrap()
            .version
            .push_str("-different");
        assert!(validate_candidates(&[candidate], &sources).is_err());
        Ok(())
    }
}
