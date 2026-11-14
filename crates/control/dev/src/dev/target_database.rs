//! Shared schema lifecycle with an exclusive lease for each development target.

use super::config::{DevConfig, POSTGRES_SYSTEM_DATABASES};
use crate::identifier::Identifier;
use sqlx::PgConnection as Client;
use sqlx::Row as _;
use sqlx::postgres::PgConnectOptions as PostgresConfig;
use std::{
    error::Error,
    fmt,
    path::{Path, PathBuf},
    str::FromStr,
};
use wamn_environment_control::{
    apply_package::{self, PackageRoot},
    environment::{
        document::{EnvironmentDocument, SchemaMode},
        sql::schema,
    },
};
const MAINTENANCE_DATABASE: &str = "postgres";

async fn recorded_disposable(
    config: &DevConfig,
    document: &wamn_environment_control::environment::document::EnvironmentDocument,
) -> anyhow::Result<Option<bool>> {
    let identity = config.activation_identity();
    anyhow::ensure!(
        document.org == identity.org
            && document.project == identity.project
            && document.env == identity.environment,
        "local environment differs from dev.json identity"
    );
    let mut system = wamn_postgres::connect(config.system_database_url()).await?;
    Ok(
        sqlx::query(wamn_registry::sql::CONTROL_DEV_SRC_DEV_TARGET_DATABASE_RECORDED_DISPOSABLE)
            .bind(&document.org)
            .bind(&document.project)
            .bind(&document.env)
            .fetch_optional(&mut system)
            .await?
            .map(|row| row.get(0)),
    )
}

/// Compare local source recipes with the same records and decision used by deployment.
pub async fn schema_changes(
    config: &DevConfig,
    document: &wamn_environment_control::environment::document::EnvironmentDocument,
    roots: &[std::path::PathBuf],
) -> anyhow::Result<Vec<wamn_environment_control::environment::sql::schema::Diff>> {
    use anyhow::ensure;

    let identity = config.activation_identity();
    let disposable = recorded_disposable(config, document).await?;
    let mut state =
        schema::database_state(config.target_database_url(), &identity.project_env_key()).await?;
    state.disposable = disposable;
    let candidates = roots
        .iter()
        .map(|root| {
            schema::Candidate::source(PackageRoot {
                path: root.to_owned(),
                artifact_digest: None,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let mut remaining = candidates;
    let mut diffs = Vec::new();
    for (application, selection) in &document.applications {
        let (selected, rest): (Vec<_>, Vec<_>) = remaining.into_iter().partition(|candidate| {
            wamn_compiler::manifest::PackageManifest::from_slice(
                &candidate.directory.manifest_bytes,
            )
            .is_ok_and(|manifest| selection.packages.contains(&manifest.package.id))
        });
        remaining = rest;
        if selected.is_empty() {
            continue;
        }
        diffs.push(schema::plan(document, application, None, &selected, &state).await?);
    }
    ensure!(
        remaining.is_empty(),
        "local schema source has no declared application"
    );
    Ok(diffs)
}

/// Resolve every local package before a destructive schema apply.
pub(crate) fn validate_schema_reset(
    document: &EnvironmentDocument,
    roots: &[PathBuf],
    diffs: &[schema::Diff],
) -> anyhow::Result<()> {
    anyhow::ensure!(
        diffs.iter().all(|diff| diff.disposable == Some(true)),
        "local environment is not recorded as disposable"
    );
    let candidates = roots
        .iter()
        .map(|root| {
            schema::Candidate::source(PackageRoot {
                path: root.clone(),
                artifact_digest: None,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let sources = candidates
        .iter()
        .map(|candidate| {
            let metadata = wamn_catalog::package_schema::package_metadata(&candidate.directory)?;
            Ok((
                metadata.coordinate.package_id().to_owned(),
                schema::SchemaSource {
                    version: metadata.coordinate.package_version().to_owned(),
                    artifact: None,
                },
            ))
        })
        .collect::<anyhow::Result<std::collections::BTreeMap<_, _>>>()?;
    anyhow::ensure!(
        diffs
            .iter()
            .flat_map(|diff| diff.reset_sources.keys())
            .all(|id| sources.contains_key(id)),
        "local reset is missing an installed source"
    );
    schema::validate_candidates(document, &candidates, &sources)?;
    Ok(())
}

fn all_sources(config: &DevConfig, document: &EnvironmentDocument) -> anyhow::Result<Vec<PathBuf>> {
    let mut roots = std::collections::BTreeSet::new();
    for source in config.package_sources() {
        roots.extend(crate::source_deploy::sources(document, source)?.1);
    }
    Ok(roots.into_iter().collect())
}

#[cfg(target_os = "linux")]
pub async fn schema_operation(
    configuration: &Path,
    environment: &Path,
    root: &Path,
    apply: bool,
) -> anyhow::Result<Vec<schema::Diff>> {
    let config = super::config::parse_config(&std::fs::read(configuration)?)?;
    let document = super::session::read_environment(environment)?;
    let roots = crate::source_deploy::sources(&document, root)?.1;
    let diffs = schema_changes(&config, &document, &roots).await?;
    if !apply {
        return Ok(diffs);
    }
    let mut lease = acquire(&config).await?;
    lease.instance(&config).await?;
    let roots = if diffs.iter().any(|diff| diff.mode == SchemaMode::Reset) {
        let roots = all_sources(&config, &document)?;
        let diffs = schema_changes(&config, &document, &roots).await?;
        validate_schema_reset(&document, &roots, &diffs)?;
        lease.reset(&config).await?;
        roots
    } else {
        roots
    };
    apply_sources(&config, &roots).await?;
    schema_changes(&config, &document, &roots).await
}

async fn apply_sources(config: &DevConfig, roots: &[PathBuf]) -> anyhow::Result<()> {
    let manifests = roots
        .iter()
        .map(|path| {
            let directory = apply_package::read_package_directory(path)?;
            wamn_compiler::manifest::PackageManifest::from_slice(&directory.manifest_bytes)
                .map_err(Into::into)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let order = wamn_environment_control::environment::stage::apply_order(
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
    let identity = config.activation_identity();
    let changes = wamn_broker::Publisher::new(config.nats().connect().await?);
    for index in order {
        apply_package::apply_local_package_schema(
            apply_package::ApplyPackageRequest {
                migration_database_url: config.migration_database_url().to_owned(),
                component_source: apply_package::component_source_from_env(None)?,
                package: PackageRoot {
                    path: roots[index].clone(),
                    artifact_digest: None,
                },
                database_url: config.target_database_url().to_owned(),
                changes: changes.clone(),
                org: identity.org.clone(),
                project: identity.project.clone(),
                environment: identity.environment.clone(),
            },
            &identity.environment,
        )
        .await?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub async fn reset(configuration: &Path) -> anyhow::Result<String> {
    let config = super::config::parse_config(&std::fs::read(configuration)?)?;
    let document =
        super::session::read_environment(&configuration.with_file_name("environment.k"))?;
    let roots = all_sources(&config, &document)?;
    let diffs = schema_changes(&config, &document, &roots).await?;
    validate_schema_reset(&document, &roots, &diffs)?;
    let mut lease = acquire(&config).await?;
    lease.instance(&config).await?;
    let instance = lease.reset(&config).await?;
    apply_sources(&config, &roots).await?;
    Ok(instance)
}

/// Stable category of a target-database lifecycle failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetDatabaseErrorType {
    InvalidConfiguration,
    LeaseUnavailable,
    LeaseFailed,
    ResetFailed,
}

impl TargetDatabaseErrorType {
    /// Stable diagnostic code for this error category.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "dev-target-database-invalid-configuration",
            Self::LeaseUnavailable => "dev-target-database-lease-unavailable",
            Self::LeaseFailed => "dev-target-database-lease-failed",
            Self::ResetFailed => "dev-target-database-reset-failed",
        }
    }
}

/// One refusal from the run-targeted target-database lifecycle.
pub struct TargetDatabaseError {
    type_: TargetDatabaseErrorType,
    remedy: &'static str,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl fmt::Debug for TargetDatabaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TargetDatabaseError")
            .field("type", &self.type_)
            .field("remedy", &self.remedy)
            .finish_non_exhaustive()
    }
}

impl TargetDatabaseError {
    fn new(type_: TargetDatabaseErrorType, remedy: &'static str) -> Self {
        Self {
            type_,
            remedy,
            source: None,
        }
    }

    fn with_source(mut self, source: impl Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// Stable refusal category.
    pub const fn error_type(&self) -> TargetDatabaseErrorType {
        self.type_
    }

    /// Operator action that clears this refusal.
    pub const fn remedy(&self) -> &'static str {
        self.remedy
    }
}

impl fmt::Display for TargetDatabaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.type_.as_str(), self.remedy)
    }
}

impl Error for TargetDatabaseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

struct TargetSpec {
    maintenance: PostgresConfig,
    database: Identifier,
}

impl TargetSpec {
    fn from_config(config: &DevConfig) -> Result<Self, TargetDatabaseError> {
        let mut maintenance =
            PostgresConfig::from_str(config.target_database_url()).map_err(|source| {
                TargetDatabaseError::new(
                    TargetDatabaseErrorType::InvalidConfiguration,
                    "set target_database_url to a valid PostgreSQL URL with an explicit database",
                )
                .with_source(source)
            })?;
        let database = maintenance.get_database().ok_or_else(|| {
            TargetDatabaseError::new(
                TargetDatabaseErrorType::InvalidConfiguration,
                "set target_database_url to a valid PostgreSQL URL with an explicit database",
            )
        })?;
        // The parse-time guard in `config` already refuses these. Repeating the
        // require_current_lease here keeps the drop safe for any caller that reaches this module
        // by another path.
        if POSTGRES_SYSTEM_DATABASES.contains(&database) {
            return Err(TargetDatabaseError::new(
                TargetDatabaseErrorType::InvalidConfiguration,
                "set target_database_url to a disposable database, never a PostgreSQL system database",
            ));
        }
        let database = Identifier::new(database.to_owned()).map_err(|source| {
            TargetDatabaseError::new(
                TargetDatabaseErrorType::InvalidConfiguration,
                "set target_database_url to a database name PostgreSQL can quote",
            )
            .with_source(std::io::Error::other(source))
        })?;
        maintenance = maintenance.database(MAINTENANCE_DATABASE);
        Ok(Self {
            maintenance,
            database,
        })
    }
}

/// The database name inside a PostgreSQL URL, for database identity.
pub fn database_name(url: &str) -> Option<String> {
    PostgresConfig::from_str(url)
        .ok()?
        .get_database()
        .map(str::to_owned)
}

/// The lease stays held for the whole local session or schema operation.
pub struct TargetLease {
    client: Client,
    spec: TargetSpec,
}
impl fmt::Debug for TargetLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TargetLease")
            .field("database", &self.spec.database.as_str())
            .finish_non_exhaustive()
    }
}
impl TargetLease {
    /// Verify the retained maintenance connection before serving another candidate.
    pub async fn require_current_lease(&mut self) -> Result<(), TargetDatabaseError> {
        sqlx::query("SELECT 1")
            .fetch_one(&mut self.client)
            .await
            .map_err(|source| {
                TargetDatabaseError::new(
                    TargetDatabaseErrorType::LeaseFailed,
                    "restart the development session after its exclusive target lease was lost",
                )
                .with_source(source)
            })?;
        Ok(())
    }

    /// Read the owned database identity independently of the optional catalog cache.
    pub(crate) async fn instance(&mut self, config: &DevConfig) -> anyhow::Result<String> {
        let identity = config.activation_identity();
        wamn_runtime::local_application::require_local_target(
            config.target_database_url(),
            &identity.project_env_key(),
            &identity.environment,
        )
        .await?;
        let instance: i64 =
            sqlx::query("SELECT oid::bigint FROM pg_catalog.pg_database WHERE datname=$1")
                .bind(self.spec.database.as_str())
                .fetch_one(&mut self.client)
                .await?
                .get::<i64, _>(0);
        Ok(instance.to_string())
    }

    /// Reset application objects after the caller stops its local users.
    pub async fn reset(&mut self, config: &DevConfig) -> Result<String, TargetDatabaseError> {
        self.require_current_lease().await?;
        let result = async {
            let instance = self.instance(config).await?;
            let identity = config.activation_identity();
            let triple = wamn_registry::Triple::new(
                &identity.org,
                identity.environment.as_str(),
                &identity.project,
            );
            wamn_environment_control::environment::sql::reset::reset_database(
                config.system_database_url(),
                config.target_database_url(),
                &triple,
            )
            .await?;
            wamn_broker::streams::discard(
                &async_nats::jetstream::new(config.nats().connect().await?),
                &triple,
            )
            .await?;
            Ok(instance)
        }
        .await;
        result.map_err(|source: anyhow::Error| TargetDatabaseError {
            source: Some(source.into_boxed_dyn_error()),
            ..TargetDatabaseError::new(
                TargetDatabaseErrorType::ResetFailed,
                "stop users and reapply the local schema sources",
            )
        })
    }
}

/// Refuse concurrent sessions or reset commands before any target mutation.
pub async fn acquire(config: &DevConfig) -> Result<TargetLease, TargetDatabaseError> {
    let spec = TargetSpec::from_config(config)?;
    let client = wamn_postgres::connect(spec.maintenance.clone())
        .await
        .map_err(|source| {
            TargetDatabaseError::new(
                TargetDatabaseErrorType::InvalidConfiguration,
                "make the PostgreSQL maintenance database reachable with the target credential",
            )
            .with_source(std::io::Error::other(source))
        })?;

    let mut lease = TargetLease { client, spec };
    let acquired: bool = sqlx::query("SELECT pg_catalog.pg_try_advisory_lock(pg_catalog.hashtextextended($1, 0))").bind(lease.spec.database.as_str()).fetch_one(&mut lease.client).await.map(|row| row.get(0)).map_err(|source| {
        TargetDatabaseError::new(TargetDatabaseErrorType::LeaseFailed,
            "ensure the target credential can acquire session advisory locks on the postgres maintenance database")
            .with_source(source)
    })?;
    if !acquired {
        return Err(TargetDatabaseError::new(
            TargetDatabaseErrorType::LeaseUnavailable,
            "stop the active wamn dev session before resetting or reusing this target",
        ));
    }
    Ok(lease)
}
