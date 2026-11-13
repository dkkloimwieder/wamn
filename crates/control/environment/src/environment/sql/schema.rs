//! Read-only schema input preparation and one plan for source and published packages.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use wamn_compiler::manifest::PackageManifest;
use wamn_package_control::apply_package::{self, PackageRoot};
use wamn_schema_control::{AppliedPackage, PackageDirectory, plan_package_migrations};

use super::super::{
    Platform, analyse,
    document::{DeclaredRelease, EnvironmentDocument, SchemaMode},
    stage,
};

/// Prepared bytes stay alive until the caller finishes inspection or execution.
#[derive(Debug)]
pub struct Candidate {
    pub root: PackageRoot,
    pub directory: PackageDirectory,
    pub fingerprint: String,
    _artifact: Option<wamn_package_control::package_artifact::OpenedPackage>,
}

impl Candidate {
    /// Inspect an already built source package without building or publishing it.
    pub fn source(root: PackageRoot) -> anyhow::Result<Self> {
        let directory = apply_package::read_package_root(&root)?;
        let fingerprint = wamn_compiler::build_record::physical_schema_fingerprint(
            &wamn_compiler::output_root(&root.path),
        )?;
        Ok(Self {
            root,
            directory,
            fingerprint,
            _artifact: None,
        })
    }
}

/// A reported difference in an immutable applied migration file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EditedMigration {
    pub path: String,
    pub recorded: String,
    pub presented: Option<String>,
}

/// Cheap recipe and record comparisons do not inspect live table compatibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackageDiff {
    pub baseline: Option<String>,
    pub installed: Option<String>,
    pub candidate: String,
    pub source_artifact: Option<String>,
    pub pending: Vec<String>,
    pub edited_applied: Vec<EditedMigration>,
    pub migration_error: Option<String>,
    pub uncertain: Option<String>,
    pub adopted: Vec<String>,
}

/// Exact schema sources required to reconstruct the complete environment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaSource {
    pub version: String,
    pub artifact: Option<String>,
    pub fingerprint: String,
}

/// An observation of SQL state, independent of running code selections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diff {
    pub application: String,
    pub release: Option<String>,
    pub mode: SchemaMode,
    pub disposable: Option<bool>,
    pub database_instance: Option<String>,
    pub initialization_identity: Option<String>,
    pub installed_environment: Option<String>,
    pub candidate_environment: Option<String>,
    pub packages: BTreeMap<String, PackageDiff>,
    pub reset_sources: BTreeMap<String, SchemaSource>,
    pub affected_applications: BTreeSet<String>,
    pub missing_reset_sources: BTreeSet<String>,
    pub unknowns: Vec<String>,
}

impl Diff {
    /// Changed recipes in reset mode need replacement, subject to disposable authorization.
    pub fn requires_reset(&self) -> bool {
        self.mode == SchemaMode::Reset
            && self
                .candidate_environment
                .as_ref()
                .is_some_and(|candidate| self.installed_environment.as_ref() != Some(candidate))
    }
}

/// Database facts from one read-only transaction.
#[derive(Debug, Default)]
pub struct State {
    pub disposable: Option<bool>,
    pub database_instance: Option<String>,
    pub initialization_identity: Option<String>,
    pub project_baseline: Option<String>,
    pub installed_environment: Option<String>,
    pub packages: BTreeMap<String, AppliedPackage>,
    pub incomplete: BTreeMap<String, Vec<wamn_schema_control::RecordedMigration>>,
    pub unknowns: Vec<String>,
}

impl State {
    pub fn migration_records(&self, package: &str) -> &[wamn_schema_control::RecordedMigration] {
        self.packages
            .get(package)
            .map(|state| state.migrations.as_slice())
            .or_else(|| self.incomplete.get(package).map(Vec::as_slice))
            .unwrap_or_default()
    }
}

/// Read records without provisioning an absent target or writing schema state.
pub async fn state(platform: &Platform, document: &EnvironmentDocument) -> anyhow::Result<State> {
    let system = wamn_package_control::database::connect(&platform.system_database_url).await?;
    let Some(row) = analyse::read_row(&system, document).await? else {
        return Ok(State::default());
    };
    let url = analyse::project_url(platform, document, &row)?;
    let instance = url::Url::parse(&url)?
        .path()
        .trim_start_matches('/')
        .to_owned();
    let mut state = State {
        disposable: Some(row.disposable),
        database_instance: Some(instance),
        ..State::default()
    };
    let project_env_key =
        wamn_control_provision::project_env_key(&document.org, &document.project, &document.env);
    let read = database_state(&url, &project_env_key).await;
    match read {
        Ok(records) => {
            state.project_baseline = records.project_baseline;
            state.initialization_identity = records.initialization_identity;
            state.packages = records.packages;
            state.incomplete = records.incomplete;
            state.unknowns = records.unknowns;
            state.installed_environment = records.installed_environment;
        }
        Err(error) => state
            .unknowns
            .push(format!("database records unknown: {error:#}")),
    }
    Ok(state)
}

/// Read the same installed SQL records through a local or deployed connection.
pub async fn database_state(database_url: &str, project_env_key: &str) -> anyhow::Result<State> {
    let mut client = analyse::connect_project(database_url).await?;
    let tx = client
        .build_transaction()
        .read_only(true)
        .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
        .start()
        .await?;
    tx.query_one(
        "SELECT set_config('app.project_env_key', $1, true)",
        &[&project_env_key],
    )
    .await?;
    let baseline = tx
        .query_one(
            "SELECT sha256, installed_at::text, (SELECT oid::text FROM pg_catalog.pg_database WHERE datname=current_database()) FROM platform.schema_baseline WHERE singleton",
            &[],
        )
        .await?;
    let initialization_identity: String = baseline.get(1);
    let database_instance: String = baseline.get(2);
    let baseline: String = baseline.get(0);
    let mut packages = BTreeMap::new();
    let mut incomplete = BTreeMap::new();
    for row in tx
        .query(
            "SELECT package_id FROM catalog.packages WHERE project_env_key=$1 UNION SELECT package_id FROM catalog.package_migrations WHERE project_env_key=$1",
            &[&project_env_key],
        )
        .await?
    {
        let id: String = row.get(0);
        if let Some(applied) = apply_package::read_applied_package(&tx, project_env_key, &id).await? {
            packages.insert(id, applied);
        } else {
            let records = apply_package::read_package_migrations(&tx, project_env_key, &id).await?;
            incomplete.insert(id, records);
        }
    }
    let fingerprint =
        apply_package::installed_environment_fingerprint(&tx, project_env_key).await?;
    tx.commit().await?;
    let mut unknowns = Vec::new();
    for (id, files) in packages
        .iter()
        .map(|(id, state)| (id, &state.migrations))
        .chain(incomplete.iter())
    {
        for file in files
            .iter()
            .filter(|file| file.outcome == wamn_schema_control::MigrationOutcome::Started)
        {
            unknowns.push(format!("package {id}: migration {} has an uncertain outcome; inspect its effects and use --resolve {id}:{}=applied|retry", file.relative_path, file.relative_path));
        }
    }
    for id in incomplete.keys() {
        unknowns.push(format!(
            "package {id} has an unfinished schema installation"
        ));
    }
    Ok(State {
        project_baseline: Some(baseline),
        packages,
        incomplete,
        unknowns,
        installed_environment: fingerprint,
        initialization_identity: Some(initialization_identity),
        database_instance: Some(database_instance),
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
    let selected = analyse::select_application(document, Some(application))?;
    let release = match release {
        Some(digest) => digest,
        None => match &selected.applications[application].current {
            DeclaredRelease::Digest(digest) => digest,
            DeclaredRelease::None => anyhow::bail!(
                "application {application} has no current schema source; select --release or a built source package"
            ),
        },
    };
    let manifest = analyse::pull_release_manifest(platform, release).await?;
    let refusals = analyse::application_refusals(
        &selected.membership(),
        manifest
            .release
            .packages
            .iter()
            .map(wamn_catalog::PackageCoordinate::package_id),
    );
    ensure!(refusals.is_empty(), "{}", refusals.join("; "));
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

/// Compare a complete targeted candidate with records through the same package planner.
pub fn plan(
    document: &EnvironmentDocument,
    application: &str,
    release: Option<String>,
    candidates: &[Candidate],
    state: &State,
) -> anyhow::Result<Diff> {
    let selected = analyse::select_application(document, Some(application))?;
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
                    fingerprint: applied.physical_schema_fingerprint.clone(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    for candidate in candidates {
        let complete = plan_package_migrations(&candidate.directory, None, &[])?;
        let id = complete.coordinate.package_id();
        ensure!(
            !packages.contains_key(id),
            "schema candidate repeats package {id}"
        );
        let applied = state.packages.get(id);
        packages.insert(
            id.to_owned(),
            package_diff(candidate, applied, state.migration_records(id), &complete),
        );
        reset_sources.insert(
            id.to_owned(),
            SchemaSource {
                version: complete.coordinate.package_version().to_owned(),
                artifact: candidate.root.artifact_digest.clone(),
                fingerprint: candidate.fingerprint.clone(),
            },
        );
    }
    let refusals =
        analyse::application_refusals(&selected.membership(), packages.keys().map(String::as_str));
    ensure!(refusals.is_empty(), "{}", refusals.join("; "));
    let mut affected_applications = BTreeSet::from([application.to_owned()]);
    let mut missing_reset_sources = BTreeSet::new();
    missing_reset_sources.extend(
        state
            .incomplete
            .keys()
            .filter(|id| !packages.contains_key(*id))
            .cloned(),
    );
    for (owner, selection) in &document.applications {
        if selection.current != DeclaredRelease::None {
            affected_applications.insert(owner.clone());
            missing_reset_sources.extend(
                selection
                    .packages
                    .iter()
                    .filter(|id| !reset_sources.contains_key(*id))
                    .cloned(),
            );
        }
    }
    let mut unknowns = state.unknowns.clone();
    if packages.values().any(|package| !package.pending.is_empty()) {
        unknowns.push(format!("pending authored SQL for {application} can affect its consumers; running code compatibility is not inspected"));
    }
    for id in reset_sources.keys() {
        let owners = document
            .applications
            .iter()
            .filter(|(_, selection)| selection.packages.contains(id))
            .map(|(name, _)| name);
        let mut found = false;
        for owner in owners {
            found = true;
            affected_applications.insert(owner.to_owned());
        }
        if !found {
            unknowns.push(format!(
                "retained package {id} has no declared application for reconnection"
            ));
        }
    }
    let candidate_environment = state.project_baseline.as_ref().map(|project| {
        apply_package::environment_fingerprint(
            project,
            &reset_sources
                .iter()
                .map(|(id, source)| (id.as_str(), source.fingerprint.as_str()))
                .collect(),
        )
    });
    if state.project_baseline.is_none() {
        unknowns.push("project SQL baseline is unknown until the database is initialized".into());
    }
    unknowns.push("recipe fingerprints compare recorded inputs; live schema and executable compatibility are not inspected".into());
    Ok(Diff {
        application: application.to_owned(),
        release,
        mode: document.schema_mode(state.disposable),
        disposable: state.disposable,
        database_instance: state.database_instance.clone(),
        initialization_identity: state.initialization_identity.clone(),
        installed_environment: state.installed_environment.clone().filter(|_| {
            !state
                .packages
                .values()
                .flat_map(|package| &package.migrations)
                .chain(state.incomplete.values().flatten())
                .any(|file| file.outcome == wamn_schema_control::MigrationOutcome::Started)
        }),
        candidate_environment,
        packages,
        reset_sources,
        affected_applications,
        missing_reset_sources,
        unknowns,
    })
}

fn package_diff(
    candidate: &Candidate,
    applied: Option<&AppliedPackage>,
    recorded: &[wamn_schema_control::RecordedMigration],
    complete: &wamn_schema_control::PackageMigrationPlan,
) -> PackageDiff {
    let mut edited_applied = Vec::new();
    for file in recorded {
        let presented = complete.pending.iter().find(|presented| {
            presented.ordinal == file.ordinal && presented.relative_path == file.relative_path
        });
        if presented.map(|presented| &presented.sha256) != Some(&file.sha256) {
            edited_applied.push(EditedMigration {
                path: file.relative_path.clone(),
                recorded: file.sha256.clone(),
                presented: presented.map(|file| file.sha256.clone()),
            });
        }
    }
    let pending = complete
        .pending
        .iter()
        .filter(|file| {
            !recorded.iter().any(|record| {
                record.ordinal == file.ordinal
                    && record.outcome != wamn_schema_control::MigrationOutcome::Started
            })
        })
        .map(|file| file.relative_path.clone())
        .collect();
    let migration_error = plan_package_migrations(&candidate.directory, applied, recorded)
        .err()
        .map(|error| error.to_string());
    PackageDiff {
        baseline: applied.map(|applied| applied.baseline_fingerprint.clone()),
        installed: applied
            .filter(|_| {
                !recorded
                    .iter()
                    .any(|file| file.outcome == wamn_schema_control::MigrationOutcome::Started)
            })
            .map(|applied| applied.physical_schema_fingerprint.clone()),
        candidate: candidate.fingerprint.clone(),
        source_artifact: candidate.root.artifact_digest.clone(),
        pending,
        edited_applied,
        migration_error,
        uncertain: recorded
            .iter()
            .find(|file| file.outcome == wamn_schema_control::MigrationOutcome::Started)
            .map(|file| file.relative_path.clone()),
        adopted: recorded
            .iter()
            .filter(|file| file.outcome == wamn_schema_control::MigrationOutcome::Adopted)
            .map(|file| file.relative_path.clone())
            .collect(),
    }
}

/// Resolve retained SQL from recorded artifacts, including stopped applications.
/// A missing or changed source refuses preparation before any SQL operation.
pub async fn reset_candidates(
    platform: &Platform,
    diff: &Diff,
    mut selected: Vec<Candidate>,
) -> anyhow::Result<Vec<Candidate>> {
    ensure!(
        diff.missing_reset_sources.is_empty(),
        "running applications have packages without recorded schema sources: {:?}",
        diff.missing_reset_sources
    );
    for (id, source) in &diff.reset_sources {
        if diff.packages.contains_key(id) {
            continue;
        }
        let digest = source
            .artifact
            .as_deref()
            .with_context(|| format!("retained package {id} has no published schema source"))?;
        let candidate = open(platform, id, &source.version, Some(digest)).await?;
        ensure!(
            candidate.fingerprint == source.fingerprint,
            "retained package {id} schema fingerprint differs from its record"
        );
        selected.push(candidate);
    }
    Ok(selected)
}

/// Validate complete reconstruction inputs before either reset adapter changes a database.
pub fn validate_candidates(
    document: &EnvironmentDocument,
    candidates: &[Candidate],
    sources: &BTreeMap<String, SchemaSource>,
) -> anyhow::Result<(Vec<PackageManifest>, Vec<usize>)> {
    let mut packages = Vec::new();
    let mut found = std::collections::BTreeSet::new();
    let mut objects = std::collections::BTreeSet::new();
    for candidate in candidates {
        let manifest = PackageManifest::from_slice(&candidate.directory.manifest_bytes)?;
        let id = &manifest.package.id;
        ensure!(
            found.insert(id.as_str().to_owned()),
            "duplicate reset package {id}"
        );
        ensure!(
            document
                .applications
                .values()
                .any(|application| application.packages.contains(id)),
            "reset package {id} has no declared application"
        );
        for dependency in manifest.base_dependencies.values() {
            ensure!(
                sources.contains_key(&dependency.package),
                "reset package {id} is missing dependency {}",
                dependency.package
            );
        }
        let source = sources
            .get(id)
            .with_context(|| format!("unexpected reset package {id}"))?;
        ensure!(
            manifest.package.version == source.version
                && source.artifact == candidate.root.artifact_digest
                && source.fingerprint == candidate.fingerprint,
            "reset package {id} differs from its selected source"
        );
        for model in manifest.models.values().filter(|model| model.owner == *id) {
            ensure!(
                objects.insert((
                    model.schema.as_str().to_owned(),
                    model.table.as_str().to_owned()
                )),
                "conflicting reset definitions for {}.{}",
                model.schema,
                model.table
            );
        }
        // The package owner validates the complete policy, including deferred refusals.
        apply_package::validate_package_migration_policy(
            &candidate.root.path,
            &candidate.directory,
        )?;
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

/// Assert selected recipes and retain installed sources for stopped applications.
pub async fn adopt(
    platform: &Platform,
    document: &EnvironmentDocument,
    initialization_identity: &str,
) -> anyhow::Result<BTreeMap<String, Vec<String>>> {
    adopt_application(platform, document, initialization_identity, None).await
}

/// Adopt only the requested application's published packages, or the complete environment.
pub async fn adopt_application(
    platform: &Platform,
    document: &EnvironmentDocument,
    initialization_identity: &str,
    application: Option<&str>,
) -> anyhow::Result<BTreeMap<String, Vec<String>>> {
    let packages = application
        .map(|name| {
            document
                .applications
                .get(name)
                .with_context(|| format!("application {name} is not declared"))
        })
        .transpose()?
        .map(|selection| &selection.packages);
    let state = state(platform, document).await?;
    ensure!(
        state.initialization_identity.as_deref() == Some(initialization_identity),
        "database initialization changed before adoption"
    );
    let mut selected = BTreeMap::new();
    for (application_name, selection) in &document.applications {
        if application.is_some_and(|name| name != application_name)
            || matches!(selection.current, DeclaredRelease::None)
        {
            continue;
        }
        let (_, candidates) =
            release_candidates(platform, document, application_name, None).await?;
        for candidate in candidates {
            let manifest = PackageManifest::from_slice(&candidate.directory.manifest_bytes)?;
            if let Some(previous) = selected.get(&manifest.package.id) {
                let previous: &Candidate = previous;
                ensure!(
                    previous.fingerprint == candidate.fingerprint,
                    "conflicting adoption sources for {}",
                    manifest.package.id
                );
            } else {
                selected.insert(manifest.package.id, candidate);
            }
        }
    }
    for (id, installed) in &state.packages {
        if packages.is_some_and(|packages| !packages.contains(id)) {
            continue;
        }
        if !selected.contains_key(id) {
            let artifact = installed
                .artifact_digest
                .as_deref()
                .with_context(|| format!("retained package {id} has no published schema source"))?;
            let candidate = open(
                platform,
                id,
                installed.coordinate.package_version(),
                Some(artifact),
            )
            .await?;
            ensure!(
                candidate.fingerprint == installed.physical_schema_fingerprint,
                "retained package {id} differs from its installed source"
            );
            selected.insert(id.to_owned(), candidate);
        }
    }
    let system = wamn_package_control::database::connect(&platform.system_database_url).await?;
    let row = analyse::read_row(&system, document)
        .await?
        .context("adoption requires an existing database")?;
    let url = analyse::project_url(platform, document, &row)?;
    let roots = selected
        .values()
        .map(|candidate| &candidate.root)
        .collect::<Vec<_>>();
    apply_package::adopt_package_schemas(
        &url,
        &wamn_control_provision::project_env_key(&document.org, &document.project, &document.env),
        initialization_identity,
        &roots,
        None,
    )
    .await
}

/// Apply selected SQL through the package owner without caching or activating code.
pub async fn apply(
    platform: &Platform,
    document: &EnvironmentDocument,
    application: &str,
    release: &str,
    admin_url: &str,
    resolution: Option<&apply_package::MigrationResolution>,
) -> anyhow::Result<()> {
    let (release, candidates) =
        release_candidates(platform, document, application, Some(release)).await?;
    let state = state(platform, document).await?;
    let diff = plan(document, application, Some(release), &candidates, &state)?;
    ensure!(
        state.project_baseline.is_some(),
        "schema apply requires readable initialized project SQL for {:?}: {}",
        diff.database_instance,
        state.unknowns.join("; ")
    );
    let resolution_id = apply_package::resolution_package(
        resolution,
        diff.packages
            .keys()
            .map(|id| (id.as_str(), state.migration_records(id))),
    )?;
    if resolution.is_none() && diff.requires_reset() {
        ensure!(
            diff.disposable == Some(true),
            "database {:?} is not recorded as disposable",
            diff.database_instance
        );
        let _complete = reset_candidates(platform, &diff, candidates).await?;
        anyhow::bail!(
            "database {:?} requires whole-environment replacement for applications {:?}; reset execution is not available yet",
            diff.database_instance,
            diff.affected_applications
        );
    }
    for (id, package) in &diff.packages {
        ensure!(
            package.migration_error.is_none(),
            "package {id}: {}. Compare recorded and selected inputs with wamn schema diff; explicitly adopt the selected state with wamn schema adopt, repair the SQL, or reset an authorized disposable target",
            package.migration_error.as_deref().unwrap_or_default()
        );
    }
    let manifests = candidates
        .iter()
        .map(|candidate| {
            wamn_compiler::manifest::PackageManifest::from_slice(
                &candidate.directory.manifest_bytes,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let order = stage::apply_order(
        &manifests
            .iter()
            .map(|manifest| {
                (
                    manifest.package.id.as_str(),
                    manifest
                        .base_dependencies
                        .values()
                        .map(|dependency| dependency.package.as_str())
                        .collect(),
                )
            })
            .collect::<Vec<_>>(),
    )?;
    let system = wamn_package_control::database::connect(&platform.system_database_url).await?;
    let row = analyse::read_row(&system, document)
        .await?
        .context("environment database disappeared")?;
    let url = analyse::project_url(platform, document, &row)?;
    ensure!(
        url::Url::parse(&url)?.path().trim_start_matches('/')
            == state.database_instance.as_deref().unwrap_or_default(),
        "environment database changed during schema preparation"
    );
    if manifests
        .iter()
        .any(|manifest| !state.packages.contains_key(&manifest.package.id))
    {
        let admin = wamn_package_control::database::connect(admin_url).await?;
        super::super::ensure::ensure_package_roles(&admin, &manifests).await?;
        let selected = analyse::select_application(document, Some(application))?;
        let release = analyse::pull_release_manifest(
            platform,
            diff.release.as_deref().expect("selected schema release"),
        )
        .await?;
        let project_env_key = wamn_control_provision::project_env_key(
            &document.org,
            &document.project,
            &document.env,
        );
        stage::materialize_application_principals(
            &platform.system_database_url,
            &url,
            &project_env_key,
            &release,
            &manifests,
            &platform.auth_changes().await?,
        )
        .await?;
        stage::create_number_sequences(
            &url,
            &stage::sequence_defaults(&selected.membership(), manifests.iter())?,
        )
        .await?;
    }
    let publisher = platform.auth_changes().await?;
    let mut candidates = candidates.into_iter().map(Some).collect::<Vec<_>>();
    for index in order {
        // A resolution changes only the named package, even if another is uncertain.
        if resolution.is_some() && resolution_id != Some(manifests[index].package.id.as_str()) {
            continue;
        }

        let candidate = candidates[index].take().expect("each package applies once");
        let package = &diff.packages[&manifests[index].package.id];
        if package.installed.as_ref() == Some(&package.candidate) && package.pending.is_empty() {
            continue;
        }
        apply_package::apply_package_schema(
            apply_package::ApplyPackageRequest {
                component_source: platform.component_source()?,
                package: candidate.root,
                changes: publisher.clone(),
                database_url: url.clone(),
                org: document.org.clone(),
                project: document.project.clone(),
                environment: document.env.clone(),
            },
            resolution.filter(|_| resolution_id == Some(manifests[index].package.id.as_str())),
        )
        .await?;
    }
    stage::reconcile_installed_sql_grants(platform, document, &url).await?;
    Ok(())
}

/// Shared SQL status remains useful when a candidate or target is unavailable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Status {
    pub mode: SchemaMode,
    pub disposable: Option<bool>,
    pub database_instance: Option<String>,
    pub initialization_identity: Option<String>,
    pub pending_database: Option<serde_json::Value>,
    pub installed_environment: Option<String>,
    pub installed: BTreeMap<String, SchemaSource>,
    pub adopted: BTreeMap<String, Vec<String>>,
    pub applications: BTreeMap<String, Diff>,
    pub unknowns: Vec<String>,
}

/// Read every selected candidate and retain causes for unknown observations.
pub async fn status(
    platform: &mut Platform,
    document: &EnvironmentDocument,
) -> anyhow::Result<Status> {
    let registry = super::super::runtime::resolve_registry(platform, &document.org)
        .await
        .map(|_| ());
    let state = state(platform, document)
        .await
        .unwrap_or_else(|error| State {
            unknowns: vec![format!("database state unknown: {error:#}")],
            ..State::default()
        });
    let mut unknowns = state.unknowns.clone();
    let mut applications = BTreeMap::new();
    for application in document.applications.keys() {
        if let Err(error) = &registry {
            unknowns.push(format!(
                "application {application} candidate unknown: {error:#}"
            ));
            continue;
        }
        match release_candidates(platform, document, application, None).await {
            Ok((release, candidates)) => {
                applications.insert(
                    application.clone(),
                    plan(document, application, Some(release), &candidates, &state)?,
                );
            }
            Err(error) => unknowns.push(format!(
                "application {application} candidate unknown: {error:#}"
            )),
        }
    }
    Ok(Status {
        pending_database: if state.disposable.is_some() {
            crate::provision_project_env::registry::pending_database(
                &wamn_package_control::database::connect(&platform.system_database_url).await?,
                &document.triple(),
            )
            .await?
        } else {
            None
        },
        mode: document.schema_mode(state.disposable),
        disposable: state.disposable,
        database_instance: state.database_instance,
        initialization_identity: state.initialization_identity,
        installed_environment: state.installed_environment,
        adopted: state
            .packages
            .iter()
            .map(|(id, package)| {
                (
                    id.to_owned(),
                    package
                        .migrations
                        .iter()
                        .filter(|file| {
                            file.outcome == wamn_schema_control::MigrationOutcome::Adopted
                        })
                        .map(|file| file.relative_path.clone())
                        .collect(),
                )
            })
            .collect(),
        installed: state
            .packages
            .into_iter()
            .filter(|(_, package)| {
                !package
                    .migrations
                    .iter()
                    .any(|file| file.outcome == wamn_schema_control::MigrationOutcome::Started)
            })
            .map(|(id, applied)| {
                (
                    id,
                    SchemaSource {
                        version: applied.coordinate.package_version().to_owned(),
                        artifact: applied.artifact_digest,
                        fingerprint: applied.physical_schema_fingerprint,
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
    use wamn_schema_control::{MigrationSource, RecordedMigration};

    fn candidate(id: &str, version: &str) -> Candidate {
        let mut manifest = wamn_package_control::test_support::source_manifest();
        manifest.package.id = id.into();
        manifest.package.version = version.into();
        for model in manifest.models.values_mut() {
            model.owner = id.into();
        }
        Candidate {
            root: PackageRoot {
                path: "unused".into(),
                artifact_digest: Some(format!("sha256:{}", "a".repeat(64))),
            },
            directory: PackageDirectory {
                manifest_bytes: serde_json::to_vec(&manifest).unwrap(),
                migrations: vec![MigrationSource {
                    relative_path: "migrations/0001_initial.sql".into(),
                    bytes: b"SELECT 1;".to_vec(),
                }],
                artifact_digest: Some(format!("sha256:{}", "a".repeat(64))),
            },
            fingerprint: format!("sha256:{id}"),
            _artifact: None,
        }
    }

    fn applied(candidate: &Candidate) -> AppliedPackage {
        let plan = plan_package_migrations(&candidate.directory, None, &[]).unwrap();
        AppliedPackage {
            coordinate: plan.coordinate,
            artifact_digest: plan.artifact_digest,
            baseline_fingerprint: candidate.fingerprint.clone(),
            physical_schema_fingerprint: candidate.fingerprint.clone(),
            migrations: plan
                .pending
                .into_iter()
                .map(|file| RecordedMigration {
                    ordinal: file.ordinal,
                    relative_path: file.relative_path,
                    sha256: file.sha256,
                    outcome: wamn_schema_control::MigrationOutcome::Applied,
                })
                .collect(),
        }
    }

    fn document() -> EnvironmentDocument {
        EnvironmentDocument::from_json(
            br#"{
            "org":"acme", "project":"schema", "env":"dev", "target":"cloud", "policy":"dev","revocation_seconds":60,
            "schema":{"mode":"reset"},
            "applications": {
                "alpha":{"current":"none", "revocation_seconds":5,"hostname":"alpha.invalid", "packages":["alpha"]},
                "beta":{"current":"none", "revocation_seconds":5,"hostname":"beta.invalid", "packages":["beta"]}
            }
        }"#,
        )
        .unwrap()
    }

    #[test]
    fn unfinished_first_installation_reports_its_uncertain_file() {
        let mut candidate = candidate("alpha", "1.0.0");
        candidate.directory.migrations[0].bytes = b"-- wamn:no-transaction\nSELECT 1;".to_vec();
        let mut records = applied(&candidate).migrations;
        records[0].outcome = wamn_schema_control::MigrationOutcome::Started;
        let state = State {
            incomplete: BTreeMap::from([("alpha".into(), records)]),
            ..State::default()
        };
        let diff = plan(
            &document(),
            "alpha",
            None,
            std::slice::from_ref(&candidate),
            &state,
        )
        .unwrap();
        let package = &diff.packages["alpha"];
        assert!(package.installed.is_none());
        assert!(package.migration_error.is_none());
        assert_eq!(
            package.uncertain.as_deref(),
            Some(state.migration_records("alpha")[0].relative_path.as_str())
        );
        assert!(
            apply_package::resolution_package(None, [("alpha", state.migration_records("alpha"))])
                .is_err()
        );
        assert_eq!(
            package.pending,
            vec![state.migration_records("alpha")[0].relative_path.clone()]
        );
        let mut installed = applied(&candidate);
        installed.migrations = state.migration_records("alpha").to_vec();
        let state = State {
            installed_environment: Some("previous-complete-state".into()),
            packages: BTreeMap::from([("alpha".into(), installed)]),
            ..State::default()
        };
        let diff = plan(&document(), "alpha", None, &[candidate], &state).unwrap();
        assert!(diff.installed_environment.is_none());
        assert!(diff.packages["alpha"].installed.is_none());
        assert_eq!(
            diff.packages["alpha"].pending,
            vec![state.migration_records("alpha")[0].relative_path.clone()]
        );
    }

    #[test]
    fn reconstruction_accepts_inherited_models_and_refuses_competing_owners() {
        let mut candidates = [candidate("alpha", "1.0.0"), candidate("beta", "1.0.0")];
        for candidate in &mut candidates {
            let manifest =
                PackageManifest::from_slice(&candidate.directory.manifest_bytes).unwrap();
            candidate.directory.migrations[0].bytes = manifest
                .models
                .values()
                .map(|model| {
                    format!(
                        "CREATE TABLE {}.{} (id integer CONSTRAINT item_id_pkey PRIMARY KEY);",
                        model.schema, model.table
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
                .into_bytes();
        }
        let sources = candidates
            .iter()
            .map(|candidate| {
                let manifest =
                    PackageManifest::from_slice(&candidate.directory.manifest_bytes).unwrap();
                (
                    manifest.package.id,
                    SchemaSource {
                        version: manifest.package.version,
                        artifact: candidate.root.artifact_digest.clone(),
                        fingerprint: candidate.fingerprint.clone(),
                    },
                )
            })
            .collect();
        let document = document();
        assert!(validate_candidates(&document, &candidates, &sources).is_err());
        let mut overlay =
            PackageManifest::from_slice(&candidates[1].directory.manifest_bytes).unwrap();
        for model in overlay.models.values_mut() {
            model.owner = "alpha".into();
            model.audit_log = None;
        }
        overlay.base_dependencies.insert(
            "base".into(),
            wamn_compiler::manifest::BaseDependencyRequirement {
                package: "alpha".into(),
                version: "1.0.0".into(),
                digest: candidates[0].root.artifact_digest.as_ref().unwrap().clone(),
                operations: vec!["item.get".into()],
            },
        );
        candidates[1].directory.manifest_bytes = serde_json::to_vec(&overlay).unwrap();
        candidates[1].directory.migrations.clear();
        validate_candidates(&document, &candidates, &sources).unwrap();
    }

    #[test]
    fn targeted_schema_keeps_newer_installed_sources_for_stopped_applications() {
        let document = document();
        let beta = candidate("beta", "9.0.0");
        let beta = applied(&beta);
        let state = State {
            disposable: Some(true),
            project_baseline: Some("project".into()),
            packages: [("beta".into(), beta)].into(),
            ..State::default()
        };
        let beta = &state.packages["beta"];
        let selected = candidate("alpha", "2.0.0");
        let source = plan(&document, "alpha", None, &[selected], &state).unwrap();
        assert_eq!(
            source.reset_sources["beta"].version,
            beta.coordinate.package_version()
        );
        assert_eq!(source.reset_sources["beta"].artifact, beta.artifact_digest);
        assert_eq!(
            source.reset_sources["beta"].fingerprint,
            beta.physical_schema_fingerprint
        );
        assert_eq!(
            source.affected_applications,
            document.applications.keys().cloned().collect()
        );
        assert_eq!(source.mode, SchemaMode::Reset);
        let published = plan(
            &document,
            "alpha",
            Some("release".into()),
            &[candidate("alpha", "2.0.0")],
            &state,
        )
        .unwrap();
        assert_eq!(source.packages, published.packages);
        assert_eq!(
            source.candidate_environment,
            published.candidate_environment
        );
        assert!(published.requires_reset());
    }

    #[test]
    fn reset_identity_uses_the_complete_schema_set_without_code_coordinates() {
        let mut document = document();
        let alpha = candidate("alpha", "1.0.0");
        let beta = candidate("beta", "9.0.0");
        let mut state = State {
            disposable: Some(true),
            project_baseline: Some("project".into()),
            packages: [
                ("alpha".into(), applied(&alpha)),
                ("beta".into(), applied(&beta)),
            ]
            .into(),
            ..State::default()
        };
        state.installed_environment = Some(apply_package::environment_fingerprint(
            "project",
            &state
                .packages
                .iter()
                .map(|(id, package)| (id.as_str(), package.physical_schema_fingerprint.as_str()))
                .collect(),
        ));
        let mut next = candidate("alpha", "2.0.0");
        next.root.artifact_digest = Some(format!("sha256:{}", "b".repeat(64)));
        let code = plan(&document, "alpha", Some("new-code".into()), &[next], &state).unwrap();
        assert_eq!(code.candidate_environment, state.installed_environment);
        assert!(!code.requires_reset());

        let mut next = candidate("alpha", "2.0.0");
        next.fingerprint = "changed-schema".into();
        let installed = applied(&next);
        let changed = plan(&document, "alpha", None, &[next], &state).unwrap();
        assert_eq!(
            changed.candidate_environment,
            Some(apply_package::environment_fingerprint(
                "project",
                &state
                    .packages
                    .iter()
                    .map(|(id, package)| (
                        id.as_str(),
                        if id == "alpha" {
                            "changed-schema"
                        } else {
                            package.physical_schema_fingerprint.as_str()
                        }
                    ))
                    .collect(),
            ))
        );
        assert!(changed.requires_reset());
        document.schema.mode = Some(SchemaMode::Migrate);
        let mut next = candidate("alpha", "2.0.0");
        next.fingerprint = "changed-schema".into();
        let migrate = plan(&document, "alpha", None, &[next], &state).unwrap();
        assert_eq!(migrate.candidate_environment, changed.candidate_environment);
        assert!(!migrate.requires_reset());

        document.schema.mode = Some(SchemaMode::Reset);
        state.packages.insert("alpha".into(), installed);
        state.installed_environment = changed.candidate_environment;
        let mut next = candidate("alpha", "2.0.0");
        next.fingerprint = "changed-schema".into();
        // Changing mode in either direction only changes the plan. Matching
        // installed inputs need neither recreation nor another migration.
        for mode in [SchemaMode::Reset, SchemaMode::Migrate, SchemaMode::Reset] {
            document.schema.mode = Some(mode);
            let matched = plan(
                &document,
                "alpha",
                None,
                std::slice::from_ref(&next),
                &state,
            )
            .unwrap();
            assert_eq!(matched.mode, mode);
            assert_eq!(matched.candidate_environment, state.installed_environment);
            assert!(!matched.requires_reset());
            assert!(matched.packages["alpha"].pending.is_empty());
        }
        let fresh = plan(&document, "alpha", None, &[alpha], &State::default()).unwrap();
        assert!(!fresh.requires_reset());
    }

    #[test]
    fn edited_applied_files_report_both_checksums_and_pending_ids_across_versions() {
        let first = candidate("alpha", "1.0.0");
        let installed = applied(&first);
        let recorded = installed.migrations[0].sha256.clone();
        let mut next = candidate("alpha", "2.0.0");
        next.directory.migrations[0].bytes.push(b' ');
        next.directory.migrations.push(MigrationSource {
            relative_path: "migrations/0002_data.sql".into(),
            bytes: b"SELECT 2;".to_vec(),
        });
        let complete = plan_package_migrations(&next.directory, None, &[]).unwrap();
        let diff = package_diff(&next, Some(&installed), &installed.migrations, &complete);
        assert_eq!(
            diff.pending,
            complete.pending[installed.migrations.len()..]
                .iter()
                .map(|file| file.relative_path.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            diff.edited_applied[0],
            EditedMigration {
                path: installed.migrations[0].relative_path.clone(),
                recorded,
                presented: Some(complete.pending[0].sha256.clone())
            }
        );
        assert!(diff.migration_error.is_some());
    }

    #[test]
    fn uninstalled_candidate_does_not_need_a_serving_release_or_database() {
        let document = document();
        let diff = plan(
            &document,
            "alpha",
            Some("candidate".into()),
            &[candidate("alpha", "2.0.0")],
            &State::default(),
        )
        .unwrap();
        assert!(diff.packages["alpha"].installed.is_none());
        assert!(diff.database_instance.is_none());
        assert_eq!(diff.disposable, None);
        assert!(!diff.unknowns.is_empty());
        assert!(
            plan(
                &document,
                "beta",
                None,
                &[candidate("alpha", "2.0.0")],
                &State::default()
            )
            .is_err()
        );
        assert!(plan(&document, "alpha", None, &[], &State::default()).is_err());
    }
}
