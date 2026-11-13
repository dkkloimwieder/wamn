//! Exclusive ownership and explicit recreation of disposable development targets.
//!
//! Local sessions use the shared schema decision, also across process restarts. Recreation clones
//! the stamped template and restores its captured database ACL. The lease
//! prevents another session or reset command from replacing a serving target.

use sqlx::PgConnection as Client;
use sqlx::Row as _;
use sqlx::postgres::PgConnectOptions as PostgresConfig;
pub(super) mod plan;
pub use plan::template_fingerprint;

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

use crate::identifier::Identifier;

use wamn_schema_introspection::ir::CatalogIr;

use super::activation::DevActivationIdentity;
use super::config::{DevConfig, POSTGRES_SYSTEM_DATABASES};

const MAINTENANCE_DATABASE: &str = "postgres";
const SCHEMA_RECORD_FILE: &str = "target-schema.json";
const STALE_STANDUP_REMEDY: &str =
    "run wamn dev up to provision the environment and emit its privilege SQL";

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
    use wamn_environment_control::environment::sql::schema;
    use wamn_package_control::apply_package::PackageRoot;

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
        diffs.push(schema::plan(
            document,
            application,
            None,
            &selected,
            &state,
        )?);
    }
    ensure!(
        remaining.is_empty(),
        "local schema source has no declared application"
    );
    Ok(diffs)
}

/// Refuse incomplete reconstruction before the coordinator stops a serving application.
pub(crate) fn validate_schema_reset(
    document: &wamn_environment_control::environment::document::EnvironmentDocument,
    roots: &[std::path::PathBuf],
    diffs: &[wamn_environment_control::environment::sql::schema::Diff],
) -> anyhow::Result<()> {
    use anyhow::{Context as _, ensure};
    use wamn_environment_control::environment::sql::schema;
    ensure!(
        diffs.iter().all(|diff| diff.disposable == Some(true)),
        "local environment is not recorded as disposable"
    );
    let all = roots
        .iter()
        .map(|root| {
            schema::Candidate::source(wamn_package_control::apply_package::PackageRoot {
                path: root.to_owned(),
                artifact_digest: None,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let mut sources = diffs
        .first()
        .context("local reset has no schema candidate")?
        .reset_sources
        .clone();
    for candidate in &all {
        let manifest = wamn_compiler::manifest::PackageManifest::from_slice(
            &candidate.directory.manifest_bytes,
        )?;
        let source = sources
            .get_mut(&manifest.package.id)
            .context("local schema source is missing")?;
        source.version = manifest.package.version;
        source.artifact = None;
        source.fingerprint.clone_from(&candidate.fingerprint);
    }
    ensure!(
        diffs
            .iter()
            .flat_map(|diff| &diff.missing_reset_sources)
            .all(|id| sources.contains_key(id)),
        "local reset is missing an application's schema source"
    );
    schema::validate_candidates(document, &all, &sources)?;
    Ok(())
}

/// Assert configured local sources without executing their SQL or selecting code.
pub async fn adopt_schema(
    configuration: &Path,
    environment: &Path,
) -> anyhow::Result<BTreeMap<String, Vec<String>>> {
    use anyhow::Context as _;
    use wamn_package_control::apply_package::{self, PackageRoot};
    let config = super::config::parse_config(&std::fs::read(configuration)?)?;
    let document = super::session::read_environment(environment)?;
    recorded_disposable(&config, &document).await?;
    let identity = config.activation_identity();
    let state = wamn_environment_control::environment::sql::schema::database_state(
        config.target_database_url(),
        &identity.project_env_key(),
    )
    .await?;
    let mut lease = acquire(&config).await?;
    lease.instance(&config).await?;
    let mut roots = BTreeMap::new();
    for source in config.package_sources() {
        let manifest = wamn_compiler::manifest::PackageManifest::from_slice(
            &apply_package::read_package_directory(source)?.manifest_bytes,
        )?;
        if !document
            .applications
            .values()
            .any(|application| application.packages.contains(&manifest.package.id))
        {
            continue;
        }
        let (_, sources) = crate::source_deploy::sources(&document, source)?;
        for path in sources {
            let root = PackageRoot {
                path,
                artifact_digest: None,
            };
            let directory = apply_package::read_package_root(&root)?;
            let manifest =
                wamn_compiler::manifest::PackageManifest::from_slice(&directory.manifest_bytes)?;
            roots.insert(manifest.package.id, root);
        }
    }
    anyhow::ensure!(
        state.packages.keys().all(|id| roots.contains_key(id)),
        "local adoption requires the retained sources of every installed package"
    );
    apply_package::adopt_package_schemas(
        config.target_database_url(),
        &identity.project_env_key(),
        state
            .initialization_identity
            .as_deref()
            .context("adoption requires a known initialization identity")?,
        &roots.values().collect::<Vec<_>>(),
        Some(&identity.environment),
    )
    .await
}

/// Inspect or explicitly apply local source SQL without selecting application code.
#[cfg(target_os = "linux")]
pub async fn schema_operation(
    configuration: &Path,
    environment: &Path,
    root: &Path,
    apply: bool,
    resolution: Option<&wamn_package_control::apply_package::MigrationResolution>,
) -> anyhow::Result<Vec<wamn_environment_control::environment::sql::schema::Diff>> {
    use anyhow::{Context as _, ensure};
    use wamn_package_control::apply_package::{self, ApplyPackageRequest, PackageRoot};
    let config = super::config::parse_config(&std::fs::read(configuration)?)?;
    let document = super::session::read_environment(environment)?;
    let (_, mut roots) = crate::source_deploy::sources(&document, root)?;
    let diffs = schema_changes(&config, &document, &roots).await?;
    if !apply {
        ensure!(resolution.is_none(), "resolution requires schema apply");
        return Ok(diffs);
    }
    let changes = wamn_broker::Publisher::new(config.nats().connect().await?);
    let mut lease = acquire(&config).await?;
    lease.instance(&config).await?;
    let initial_state = wamn_environment_control::environment::sql::schema::database_state(
        config.target_database_url(),
        &config.activation_identity().project_env_key(),
    )
    .await?;
    apply_package::resolution_package(
        resolution,
        diffs
            .iter()
            .flat_map(|diff| diff.packages.keys())
            .map(|id| (id.as_str(), initial_state.migration_records(id))),
    )?;

    if resolution.is_none()
        && diffs
            .iter()
            .any(wamn_environment_control::environment::sql::schema::Diff::requires_reset)
    {
        let output = wamn_compiler::build::default_output_root(root);
        for diff in &diffs {
            for (id, source) in &diff.reset_sources {
                if diff.packages.contains_key(id) {
                    continue;
                }
                let root = wamn_compiler::build_record::source_root(&output.join(id))?;
                let candidate =
                    wamn_environment_control::environment::sql::schema::Candidate::source(
                        PackageRoot {
                            path: root.clone(),
                            artifact_digest: None,
                        },
                    )?;
                ensure!(
                    candidate.fingerprint == source.fingerprint,
                    "retained package {id} differs from its installed local schema source"
                );
                roots.push(root);
            }
        }
        let diffs = schema_changes(&config, &document, &roots).await?;
        validate_schema_reset(&document, &roots, &diffs)?;
        tracing::warn!(affected = ?diffs.iter().flat_map(|diff| &diff.affected_applications).collect::<std::collections::BTreeSet<_>>(),
            "reset development database; all application data is discarded");
        lease.recreate(&config).await?;
    } else {
        for diff in &diffs {
            for (id, package) in &diff.packages {
                ensure!(
                    package.edited_applied.is_empty(),
                    "package {id} edits applied SQL: {:?}",
                    package.edited_applied
                );
                ensure!(
                    package.migration_error.is_none(),
                    "package {id}: {:?}",
                    package.migration_error
                );
            }
        }
    }
    let identity = config.activation_identity();
    // Resolve dependency order through the same validator used for reconstruction.
    let candidates = roots
        .iter()
        .map(|path| {
            wamn_environment_control::environment::sql::schema::Candidate::source(PackageRoot {
                path: path.clone(),
                artifact_digest: None,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let manifests = candidates
        .iter()
        .map(|candidate| {
            wamn_compiler::manifest::PackageManifest::from_slice(
                &candidate.directory.manifest_bytes,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let order = wamn_environment_control::environment::stage::apply_order(
        &manifests
            .iter()
            .map(|manifest| {
                (
                    manifest.package.id.as_str(),
                    manifest
                        .base_dependencies
                        .values()
                        .map(|base| base.package.as_str())
                        .collect(),
                )
            })
            .collect::<Vec<_>>(),
    )?;
    let state = wamn_environment_control::environment::sql::schema::database_state(
        config.target_database_url(),
        &identity.project_env_key(),
    )
    .await?;
    let resolution_id = apply_package::resolution_package(
        resolution,
        manifests.iter().map(|manifest| {
            (
                manifest.package.id.as_str(),
                state.migration_records(&manifest.package.id),
            )
        }),
    )?;
    let mut roots = roots.into_iter().map(Some).collect::<Vec<_>>();
    for index in order {
        // A resolution changes only the named package, even if another is uncertain.
        if resolution.is_some() && resolution_id != Some(manifests[index].package.id.as_str()) {
            continue;
        }

        apply_package::apply_local_package_schema(
            ApplyPackageRequest {
                component_source: apply_package::component_source_from_env(None)?,
                package: PackageRoot {
                    path: roots[index].take().context("package applied twice")?,
                    artifact_digest: None,
                },
                database_url: config.target_database_url().to_owned(),
                changes: changes.clone(),
                org: identity.org.clone(),
                project: identity.project.clone(),
                environment: identity.environment.clone(),
            },
            &identity.environment,
            resolution.filter(|_| resolution_id == Some(manifests[index].package.id.as_str())),
        )
        .await?;
    }
    let roots = candidates
        .into_iter()
        .map(|candidate| candidate.root.path)
        .collect::<Vec<_>>();
    let current = wamn_environment_control::environment::sql::schema::database_state(
        config.target_database_url(),
        &identity.project_env_key(),
    )
    .await?;
    let mut grant_roots = roots
        .iter()
        .zip(&manifests)
        .filter(|(_, manifest)| current.packages.contains_key(&manifest.package.id))
        .map(|(root, _)| root.clone())
        .collect::<Vec<_>>();
    let output = wamn_compiler::build::default_output_root(root);
    for (id, installed) in &current.packages {
        if manifests.iter().any(|manifest| manifest.package.id == *id) {
            continue;
        }
        let retained = wamn_compiler::build_record::source_root(&output.join(id))?;
        let candidate =
            wamn_environment_control::environment::sql::schema::Candidate::source(PackageRoot {
                path: retained.clone(),
                artifact_digest: None,
            })?;
        ensure!(
            candidate.fingerprint == installed.physical_schema_fingerprint,
            "retained package {id} differs from its installed local schema source"
        );
        grant_roots.push(retained);
    }
    let grants =
        wamn_environment_control::reconcile_package_data_access::prepare_local(&grant_roots)?;
    wamn_environment_control::reconcile_package_data_access::reconcile_local(
        &grants,
        config.target_database_url(),
        &identity.project_env_key(),
        &identity.environment,
        true,
    )
    .await?;
    schema_changes(&config, &document, &roots).await
}

/// Rotate an idle owned target and its environment instance together.
#[cfg(target_os = "linux")]
pub async fn reset(configuration: &Path) -> anyhow::Result<String> {
    use anyhow::Context as _;

    let bytes = std::fs::read(configuration).context("read development reset configuration")?;
    let config =
        super::config::parse_config(&bytes).context("validate development reset configuration")?;
    let document =
        super::session::read_environment(&configuration.with_file_name("environment.k"))?;
    anyhow::ensure!(
        recorded_disposable(&config, &document).await? == Some(true),
        "local environment is not recorded as disposable"
    );
    let mut lease = acquire(&config).await?;
    lease.instance(&config).await?;
    let instance = lease.recreate(&config).await?;
    Ok(instance)
}

/// Stable category of a target-database lifecycle failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetDatabaseErrorType {
    InvalidConfiguration,
    StaleStandup,
    LeaseUnavailable,
    LeaseFailed,
    DropFailed,
    CreateFailed,
    TemplateMissing,
    TemplateForeign,
    AclFailed,
}

impl TargetDatabaseErrorType {
    /// Stable diagnostic code for this error category.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidConfiguration => "dev-target-database-invalid-configuration",
            Self::StaleStandup => "dev-target-database-stale-standup",
            Self::LeaseUnavailable => "dev-target-database-lease-unavailable",
            Self::LeaseFailed => "dev-target-database-lease-failed",
            Self::DropFailed => "dev-target-database-drop-failed",
            Self::CreateFailed => "dev-target-database-create-failed",
            Self::TemplateMissing => "dev-target-database-template-missing",
            Self::TemplateForeign => "dev-target-database-template-foreign",
            Self::AclFailed => "dev-target-database-acl-failed",
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

/// The target database and the maintenance connection that can recreate it.
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

/// The database name inside a PostgreSQL URL, for fingerprinting.
pub fn database_name(url: &str) -> Option<String> {
    PostgresConfig::from_str(url)
        .ok()?
        .get_database()
        .map(str::to_owned)
}

/// Read the database-level ACL `wamn dev up` captured from the healthy target.
///
/// A missing file is a stale standup, not a lifecycle failure: the environment
/// was never provisioned, or it was provisioned somewhere this `dev.json` no
/// longer names.
fn read_database_acl(path: &Path) -> Result<String, TargetDatabaseError> {
    std::fs::read_to_string(path).map_err(|source| {
        TargetDatabaseError::new(TargetDatabaseErrorType::StaleStandup, STALE_STANDUP_REMEDY)
            .with_source(source)
    })
}

/// Exact target SQL captured before candidate cutover.
#[derive(Debug)]
pub(crate) struct PreparedConfiguration {
    database: Identifier,
    privileges: String,
    acl: String,
}

pub(crate) fn prepare_configuration(config: &DevConfig) -> anyhow::Result<PreparedConfiguration> {
    use anyhow::Context as _;
    Ok(PreparedConfiguration {
        database: TargetSpec::from_config(config)?.database,
        privileges: std::fs::read_to_string(config.target_privileges_file())
            .context("read emitted target privileges")?,
        acl: read_database_acl(config.target_database_acl_file())?,
    })
}

/// Record the build inputs that one target creation took and the catalogs a
/// successful Introspect read from it.
///
/// The record names the database instance, so a record left beside a target
/// that was since recreated or reset matches nothing.
pub(crate) fn record_target_schema(
    config: &DevConfig,
    instance: &str,
    build_inputs: &[String],
    catalogs: &BTreeMap<String, CatalogIr>,
) -> anyhow::Result<()> {
    use anyhow::Context as _;
    let directory = &config.local_artifacts().directory;
    std::fs::create_dir_all(directory).context("create the local artifact directory")?;
    let record = serde_json::json!({
        "database-instance": instance,
        "build-inputs": build_inputs,
        "catalogs": catalogs,
    });
    std::fs::write(
        directory.join(SCHEMA_RECORD_FILE),
        serde_json::to_vec(&record)?,
    )
    .context("record the target schema")
}

#[derive(Debug)]
pub(crate) struct ResetInputs {
    template: Identifier,
    fingerprint: String,
    acl: String,
    substrate: Vec<wamn_schema_control::MigrationSource>,
}

/// Exclusive ownership held while a development session can serve this target.
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

    /// Validate or converge captured database privileges without retaining obsolete grants.
    pub(crate) async fn reconcile_configuration(
        &mut self,
        prepared: &PreparedConfiguration,
        apply: bool,
    ) -> anyhow::Result<()> {
        use anyhow::Context as _;
        self.require_current_lease().await?;
        anyhow::ensure!(
            prepared.database == self.spec.database,
            "prepared privileges name another local target"
        );
        sqlx::raw_sql("BEGIN")
            .execute(&mut self.client)
            .await
            .map(drop)?;
        let result = async {
            let rows = sqlx::query("SELECT DISTINCT grant_row.grantee, pg_catalog.pg_get_userbyid(grant_row.grantee)::text
                 FROM pg_catalog.pg_database AS database,
                      LATERAL pg_catalog.aclexplode(COALESCE(database.datacl, pg_catalog.acldefault('d', database.datdba))) AS grant_row
                 WHERE database.datname = $1").bind(self.spec.database.as_str()).fetch_all(&mut self.client).await?;
            for row in rows {
                let grantee: u32 = row.get::<sqlx::postgres::types::Oid, _>(0).0;
                let role = if grantee == 0 { "PUBLIC".to_owned() } else { Identifier::new(row.get::<String, _>(1))?.quoted() };
                sqlx::raw_sql(sqlx::AssertSqlSafe(format!("REVOKE ALL ON DATABASE {} FROM {role}", self.spec.database.quoted()))).execute(&mut self.client).await.map(drop)?;
            }
            sqlx::raw_sql(sqlx::AssertSqlSafe(prepared.privileges.clone())).execute(&mut self.client).await.map(drop)?;
            sqlx::raw_sql(sqlx::AssertSqlSafe(prepared.acl.clone())).execute(&mut self.client).await.map(drop)?;
            Ok::<_, anyhow::Error>(())
        }.await;
        match result {
            Ok(()) => {
                sqlx::raw_sql(sqlx::AssertSqlSafe(
                    (if apply { "COMMIT" } else { "ROLLBACK" }).to_string(),
                ))
                .execute(&mut self.client)
                .await
                .map(drop)?;
            }
            Err(error) => {
                sqlx::raw_sql("ROLLBACK")
                    .execute(&mut self.client)
                    .await
                    .map(drop)
                    .context("roll back target privilege validation")?;
                return Err(error);
            }
        }
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

    /// The instance of this target, its recorded build inputs, and its saved
    /// catalogs.
    ///
    /// Returns `None` when the record is absent or unreadable, holds no
    /// catalogs, names another creation of the database, or when the local
    /// target marker no longer matches. The caller then reads the catalogs again.
    pub(crate) async fn retained_target(
        &mut self,
        config: &DevConfig,
    ) -> Option<(String, Vec<String>, BTreeMap<String, CatalogIr>)> {
        let bytes =
            std::fs::read(config.local_artifacts().directory.join(SCHEMA_RECORD_FILE)).ok()?;
        let mut record: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        let build_inputs = serde_json::from_value(record["build-inputs"].take()).ok()?;
        let catalogs = serde_json::from_value(record["catalogs"].take()).ok()?;
        let instance: i64 =
            sqlx::query("SELECT oid::bigint FROM pg_catalog.pg_database WHERE datname = $1")
                .bind(self.spec.database.as_str())
                .fetch_one(&mut self.client)
                .await
                .ok()?
                .get(0);
        let instance = instance.to_string();
        if record["database-instance"] != instance.as_str() {
            return None;
        }
        let identity = config.activation_identity();
        wamn_runtime::local_application::require_local_target(
            config.target_database_url(),
            &identity.project_env_key(),
            &identity.environment,
        )
        .await
        .ok()?;
        Some((instance, build_inputs, catalogs))
    }

    /// Resolve and validate template inputs before interrupting a serving application.
    pub(crate) async fn prepare_reset(
        &mut self,
        config: &DevConfig,
    ) -> Result<ResetInputs, TargetDatabaseError> {
        let selected = TargetSpec::from_config(config)?;
        if selected.database != self.spec.database {
            return Err(TargetDatabaseError::new(
                TargetDatabaseErrorType::InvalidConfiguration,
                "the target configuration must name the database held by this session",
            ));
        }
        self.require_current_lease().await?;
        let template =
            Identifier::new(config.target_template_database().to_owned()).map_err(|source| {
                TargetDatabaseError::new(
                    TargetDatabaseErrorType::InvalidConfiguration,
                    "set target_template_database to a database name PostgreSQL can quote",
                )
                .with_source(source)
            })?;
        let acl = read_database_acl(config.target_database_acl_file())?;
        let system_database = database_name(config.system_database_url()).ok_or_else(|| {
            TargetDatabaseError::new(
                TargetDatabaseErrorType::InvalidConfiguration,
                "set system_database_url to a PostgreSQL URL with an explicit database",
            )
        })?;
        let fingerprint = template_fingerprint(
            self.spec.database.as_str(),
            &system_database,
            config.activation_identity(),
        );
        validate_template(&mut self.client, &template, &fingerprint).await?;
        let substrate = async {
            use wamn_environment_control::platform_migrations::{self, ProjectReset};
            use wamn_control_provision::schema_migrations::MigrationDatabase;
            let mut template_url = PostgresConfig::from_str(config.target_database_url())?;
            template_url = template_url.database(template.as_str());
            let mut template_client = wamn_postgres::connect(template_url.clone()).await?;

            let result = async {
                let image = platform_migrations::embedded_sources(MigrationDatabase::Project);
                let exists: bool = sqlx::query("SELECT EXISTS(SELECT FROM pg_database WHERE datname=$1)").bind(self.spec.database.as_str()).fetch_one(&mut self.client)
                    .await?
                    .get(0);
                let mut target = if exists {
                    Some(wamn_postgres::connect(config.target_database_url()).await?)
                } else {
                    None
                };
                let mut recorded_client = &mut template_client;
                if let Some(target) = &mut target {
                    let initialized: bool = sqlx::query("SELECT to_regclass('platform.schema_baseline') IS NOT NULL").fetch_one(&mut *target)
                        .await?
                        .get(0);
                    if initialized {
                        recorded_client = target;
                    }
                }
                let recorded = ProjectReset::read(recorded_client, &image).await?;
                let template_state = ProjectReset::read(&mut template_client, &image).await?;
                anyhow::ensure!(
                    template_state.applied.len() <= recorded.applied.len()
                        && recorded.applied.starts_with(&template_state.applied),
                    "local reset template contains SQL outside the recorded target state; run wamn dev up with matching substrate"
                );
                recorded.sources(image)
            }
            .await;
            drop(template_client);
            result
        }
        .await
        .map_err(|source: anyhow::Error| TargetDatabaseError {
            source: Some(source.into_boxed_dyn_error()),
            ..TargetDatabaseError::new(
                TargetDatabaseErrorType::TemplateForeign,
                "resolve uncertain project SQL and use an image with every recorded file before reset",
            )
        })?;
        Ok(ResetInputs {
            template,
            fingerprint,
            acl,
            substrate,
        })
    }

    /// Recreate only the target named by this still-held lease.
    pub async fn recreate(&mut self, config: &DevConfig) -> Result<String, TargetDatabaseError> {
        let inputs = self.prepare_reset(config).await?;
        self.recreate_prepared(config, &inputs).await
    }

    pub(crate) async fn recreate_prepared(
        &mut self,
        config: &DevConfig,
        inputs: &ResetInputs,
    ) -> Result<String, TargetDatabaseError> {
        let ResetInputs {
            template,
            fingerprint,
            acl,
            substrate,
        } = inputs;
        let instance =
            replace_database(&mut self.client, &self.spec, template, fingerprint, acl).await?;
        let identity = config.activation_identity();
        let marker = wamn_runtime::local_application::local_target_marker(
            &identity.project_env_key(),
            &identity.environment,
            instance
                .parse()
                .expect("database instance is a PostgreSQL oid"),
        );
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "COMMENT ON DATABASE {} IS '{}'",
            self.spec.database.quoted(),
            marker,
        )))
        .execute(&mut self.client)
        .await
        .map(drop)
        .map_err(|source| {
            TargetDatabaseError::new(
                TargetDatabaseErrorType::CreateFailed,
                "stamp the owned local target before serving the application",
            )
            .with_source(std::io::Error::other(source))
        })?;
        let mut target = wamn_postgres::connect(config.target_database_url())
            .await
            .map_err(|source| TargetDatabaseError {
                source: Some(source.into_boxed_dyn_error()),
                ..TargetDatabaseError::new(
                    TargetDatabaseErrorType::CreateFailed,
                    "connect to the recreated local database",
                )
            })?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            ("UPDATE platform.schema_baseline SET installed_at=clock_timestamp() WHERE singleton")
                .to_string(),
        ))
        .execute(&mut target)
        .await
        .map(drop)
        .map_err(|source| {
            TargetDatabaseError::new(
                TargetDatabaseErrorType::CreateFailed,
                "record the recreated database initialization identity",
            )
            .with_source(std::io::Error::other(source))
        })?;
        wamn_environment_control::platform_migrations::restore_project(&mut target, substrate)
            .await
            .map_err(|source| TargetDatabaseError {
                source: Some(source.into_boxed_dyn_error()),
                ..TargetDatabaseError::new(
                    TargetDatabaseErrorType::CreateFailed,
                    "resume the recorded project substrate through the shared migration executor",
                )
            })?;
        Ok(instance)
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

async fn validate_template(
    client: &mut Client,
    template: &Identifier,
    fingerprint: &str,
) -> Result<(), TargetDatabaseError> {
    // A missing template is a stale standup and not a lifecycle failure. It is
    // validated before the drop, so a run that cannot restore the database never
    // destroys it.
    let present: bool =
        sqlx::query("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_database WHERE datname = $1)")
            .bind(template.as_str())
            .fetch_one(&mut *client)
            .await
            .map(|row| row.get(0))
            .map_err(|source| {
                TargetDatabaseError::new(
                    TargetDatabaseErrorType::StaleStandup,
                    STALE_STANDUP_REMEDY,
                )
                .with_source(source)
            })?;
    if !present {
        return Err(TargetDatabaseError::new(
            TargetDatabaseErrorType::TemplateMissing,
            STALE_STANDUP_REMEDY,
        ));
    }

    // The template is pristine only for the standup it was taken from. Both
    // tests run BEFORE the drop, so a run that cannot restore the database
    // never destroys it.
    let stamped: Option<String> = sqlx::query(
        "SELECT pg_catalog.shobj_description(oid, 'pg_database')
               FROM pg_catalog.pg_database WHERE datname = $1",
    )
    .bind(template.as_str())
    .fetch_one(&mut *client)
    .await
    .map(|row| row.get(0))
    .map_err(|source| {
        TargetDatabaseError::new(TargetDatabaseErrorType::StaleStandup, STALE_STANDUP_REMEDY)
            .with_source(source)
    })?;
    if stamped.as_deref() != Some(fingerprint) {
        return Err(TargetDatabaseError::new(
            TargetDatabaseErrorType::TemplateForeign,
            STALE_STANDUP_REMEDY,
        ));
    }

    Ok(())
}

/// Returns the INSTANCE identity of the database this call created.
///
/// A recreated database is a different target, and anything the control plane
/// keys to this environment has to say which creation it means (wamn-10yt.51).
/// PostgreSQL already mints exactly that: `CREATE DATABASE` assigns a fresh oid,
/// so the server is the authority and nothing here invents a value. A timestamp
/// would have been a guess about identity; this is the identity itself.
async fn replace_database(
    client: &mut Client,
    spec: &TargetSpec,
    template: &Identifier,
    fingerprint: &str,
    acl: &str,
) -> Result<String, TargetDatabaseError> {
    validate_template(client, template, fingerprint).await?;

    // A replication slot blocks the drop, and the loop makes a new slot on
    // the new database. Its CDC reader stopped before this point.
    sqlx::query(
        "SELECT pg_catalog.pg_drop_replication_slot(slot_name)
               FROM pg_catalog.pg_replication_slots WHERE database = $1",
    )
    .bind(spec.database.as_str())
    .execute(&mut *client)
    .await
    .map(|result| result.rows_affected())
    .map_err(|source| {
        TargetDatabaseError::new(
            TargetDatabaseErrorType::DropFailed,
            "stop the CDC reader of the disposable database before it is replaced",
        )
        .with_source(source)
    })?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {} WITH (FORCE)",
        spec.database.quoted()
    )))
    .execute(&mut *client)
    .await
    .map(drop)
    .map_err(|source| {
        TargetDatabaseError::new(
            TargetDatabaseErrorType::DropFailed,
            "grant the target credential authority to drop its disposable database",
        )
        .with_source(source)
    })?;
    // The whole provisioned state arrives here: schemas, tables, functions and
    // every object-level privilege. A clone of a pristine template is pristine,
    // so freshness is a property of the copy rather than of a replay this
    // module got right.
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {} TEMPLATE {}",
        spec.database.quoted(),
        template.quoted()
    )))
    .execute(&mut *client)
    .await
    .map(drop)
    .map_err(|source| {
        TargetDatabaseError::new(
            TargetDatabaseErrorType::CreateFailed,
            "grant the target credential CREATEDB authority for its disposable database",
        )
        .with_source(source)
    })?;
    // CREATE DATABASE copies objects and not the ACL of the database itself, so
    // ownership, the PUBLIC revoke and the workload CONNECT grants are the one
    // thing to re-issue.
    sqlx::raw_sql(sqlx::AssertSqlSafe((acl).to_string()))
        .execute(&mut *client)
        .await
        .map(drop)
        .map_err(|source| {
            TargetDatabaseError::new(
                TargetDatabaseErrorType::AclFailed,
                "re-emit the environment database ACL with wamn dev up",
            )
            .with_source(std::io::Error::other(source))
        })?;
    let instance: i64 =
        sqlx::query("SELECT oid::bigint FROM pg_catalog.pg_database WHERE datname = $1")
            .bind(spec.database.as_str())
            .fetch_one(&mut *client)
            .await
            .map(|row| row.get::<i64, _>(0))
            .map_err(|source| {
                TargetDatabaseError::new(
                    TargetDatabaseErrorType::CreateFailed,
                    "grant the target credential CREATEDB authority for its disposable database",
                )
                .with_source(std::io::Error::other(source))
            })?;
    Ok(instance.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[tokio::test]
    async fn local_schema_reads_shared_records_and_preserves_code_query_and_mode_edits()
    -> anyhow::Result<()> {
        use wamn_environment_control::environment::document::{EnvironmentDocument, SchemaMode};
        use wamn_package_control::apply_package::{self, ApplyPackageRequest, PackageRoot};
        use wamn_test_infrastructure::{fixture_digest, scratch::ScratchRoot};
        let _lock = wamn_test_postgres::lock();
        let system = wamn_control_provision::test_database::system();
        let target = wamn_catalog::test_database::project();
        let mut system_client = wamn_postgres::connect(system.url()).await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&format!(
                "{} UPDATE registry.environments SET disposable=true;",
                wamn_control_provision::test_database::seed_org_project_env_sql(
                    "acme",
                    "fixture",
                    "fixture-dev"
                ),
            ))
                .to_string(),
        ))
        .execute(&mut system_client)
        .await
        .map(drop)?;
        let mut target_client = wamn_postgres::connect(target.url()).await?;
        sqlx::raw_sql(wamn_control_provision::APP_SCHEMA_SQL)
            .execute(&mut target_client)
            .await
            .map(drop)?;
        wamn_package_control::schema_baseline::record_project(&mut target_client).await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (wamn_control_provision::sql::ensure_db_owner_role_sql()).to_string(),
        ))
        .execute(&mut target_client)
        .await
        .map(drop)?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&format!(
                "GRANT CREATE ON DATABASE {} TO wamn_db_owner",
                Identifier::new(target.name())?.quoted()
            ))
                .to_string(),
        ))
        .execute(&mut target_client)
        .await
        .map(drop)?;
        let endpoint = url::Url::parse(target.url())?.socket_addrs(|| None)?[0];
        let mut config = crate::dev::config::tests::complete_document(&[endpoint; 10]);
        config["target_database_url"] = json!(target.url());
        config["system_database_url"] = json!(system.url());
        let config_bytes = serde_json::to_vec(&config)?;
        let config = super::super::config::parse_config(&config_bytes)?;
        let instance: i64 = sqlx::query(
            "SELECT oid::bigint FROM pg_catalog.pg_database WHERE datname=current_database()",
        )
        .fetch_one(&mut target_client)
        .await?
        .get(0);
        let marker = wamn_runtime::local_application::local_target_marker(
            &config.activation_identity().project_env_key(),
            "fixture-dev",
            u32::try_from(instance)?,
        );
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&format!(
                "COMMENT ON DATABASE {} IS {}",
                Identifier::new(target.name())?.quoted(),
                postgres_protocol::escape::escape_literal(&marker)
            ))
                .to_string(),
        ))
        .execute(&mut target_client)
        .await
        .map(drop)?;
        let scratch = ScratchRoot::create()?;
        let root = scratch.path().join("package");
        std::fs::create_dir_all(root.join("migrations"))?;
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../package/tests/fixtures/component_package");
        std::fs::copy(source.join("wamn.json"), root.join("wamn.json"))?;
        std::fs::write(
            root.join("migrations/0001_item.sql"),
            "CREATE TABLE inventory.item (id uuid NOT NULL, CONSTRAINT item_pkey PRIMARY KEY (id));",
        )?;
        let columns = BTreeMap::from([(
            "item".into(),
            vec![wamn_schema_control::physical_schema::RelationColumn {
                name: "id".into(),
                primary_key: true,
                timestamptz: false,
            }],
        )]);
        fixture_digest::write_physical_schema(&root, columns)?;
        fixture_digest::write_build_record(&root);
        apply_package::apply_local_package_schema(
            ApplyPackageRequest {
                component_source: None,
                changes: wamn_broker::Publisher::default(),
                package: PackageRoot {
                    path: root.clone(),
                    artifact_digest: None,
                },
                database_url: target.url().to_owned(),
                org: "acme".into(),
                project: "fixture".into(),
                environment: "fixture-dev".into(),
            },
            "fixture-dev",
            None,
        )
        .await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            ("INSERT INTO inventory.item VALUES ('00000000-0000-4000-8000-000000000001')")
                .to_string(),
        ))
        .execute(&mut target_client)
        .await
        .map(drop)?;
        let mut document = EnvironmentDocument::from_json(br#"{
            "org":"acme","project":"fixture","env":"fixture-dev","target":"cloud","policy":"dev",
            "applications":{"alpha":{"current":"none","hostname":"alpha.invalid","packages":["source_fixture"]}}
        }"#)?;
        let roots = [root.clone()];
        assert!(!schema_changes(&config, &document, &roots).await?[0].requires_reset());
        for file in ["component.rs", "query.sql"] {
            std::fs::write(root.join(file), "changed")?;
            fixture_digest::write_build_record(&root);
            assert!(!schema_changes(&config, &document, &roots).await?[0].requires_reset());
        }
        let manifest = std::fs::read_to_string(root.join("wamn.json"))?.replace("1.0.0", "9.0.0");
        std::fs::write(root.join("wamn.json"), manifest)?;
        fixture_digest::write_build_record(&root);
        assert!(!schema_changes(&config, &document, &roots).await?[0].requires_reset());
        document.schema.mode = Some(SchemaMode::Migrate);
        assert!(!schema_changes(&config, &document, &roots).await?[0].requires_reset());
        document.schema.mode = Some(SchemaMode::Reset);
        assert!(!schema_changes(&config, &document, &roots).await?[0].requires_reset());
        let configuration = scratch.path().join("dev.json");
        let environment = scratch.path().join("environment.json");
        let mut local: serde_json::Value = serde_json::from_slice(&config_bytes)?;
        local["package_sources"] = json!([root]);
        std::fs::write(&configuration, serde_json::to_vec(&local)?)?;
        document.schema.mode = Some(SchemaMode::Migrate);
        std::fs::write(&environment, document.canonical_json()?)?;
        let adopted = adopt_schema(&configuration, &environment).await?;
        assert!(adopted.values().all(Vec::is_empty));
        document.schema.mode = Some(SchemaMode::Reset);
        assert!(!schema_changes(&config, &document, &roots).await?[0].requires_reset());
        std::fs::write(
            root.join("migrations/0002_note.sql"),
            "ALTER TABLE inventory.item ADD COLUMN note text;",
        )?;
        let physical: wamn_schema_control::physical_schema::PhysicalSchema =
            serde_json::from_slice(&std::fs::read(
                wamn_compiler::output_root(&root).join("physical-schema.json"),
            )?)?;
        fixture_digest::write_physical_schema(&root, physical.columns)?;
        fixture_digest::write_build_record(&root);
        let changed = schema_changes(&config, &document, &roots).await?;
        assert!(changed[0].requires_reset());
        validate_schema_reset(&document, &roots, &changed)?;
        document.schema.mode = Some(SchemaMode::Migrate);
        assert!(!schema_changes(&config, &document, &roots).await?[0].requires_reset());
        document.schema.mode = Some(SchemaMode::Reset);
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            (&format!(
                "BEGIN; {} UPDATE registry.environments SET disposable=false; COMMIT;",
                wamn_control_provision::bind_platform_principal_sql(
                    wamn_control_provision::PlatformComponent::Provisioning
                ),
            ))
                .to_string(),
        ))
        .execute(&mut system_client)
        .await
        .map(drop)?;
        let durable = schema_changes(&config, &document, &roots).await?;
        assert!(validate_schema_reset(&document, &roots, &durable).is_err());
        assert_eq!(
            sqlx::query("SELECT id::text FROM inventory.item")
                .fetch_one(&mut target_client)
                .await?
                .get::<String, _>(0),
            "00000000-0000-4000-8000-000000000001"
        );
        Ok(())
    }

    #[tokio::test]
    async fn local_lease_retains_data_and_reset_reapplies_exact_configuration() -> anyhow::Result<()>
    {
        let mut server = wamn_test_infrastructure::postgres::start(&[])?;
        let admin_database = server.database("postgres")?;
        let template_database = server.create_database("target_template")?;
        let target_database = server.create_database("target")?;
        let directory =
            std::env::temp_dir().join(format!("wamn-local-target-test-{}", std::process::id()));
        std::fs::create_dir(&directory)?;
        let privileges = directory.join("privileges.sql");
        let acl = directory.join("database-acl.sql");
        std::fs::write(&privileges, "REVOKE ALL ON DATABASE target FROM PUBLIC;")?;
        std::fs::write(&acl, "GRANT CONNECT ON DATABASE target TO fixture_reader;")?;
        let endpoint: std::net::SocketAddr =
            url::Url::parse(admin_database.url())?.socket_addrs(|| None)?[0];
        let mut document = crate::dev::config::tests::complete_document(&[endpoint; 10]);
        document["target_database_url"] = json!(target_database.url());
        document["target_template_database"] = json!("target_template");
        document["target_privileges_file"] = json!(privileges);
        document["target_database_acl_file"] = json!(acl);
        document["local_artifacts"] = json!({"directory": directory, "flow_http_component": directory.join("http.wasm"),
                "materializer_component": directory.join("materializer.wasm")});
        let config = super::super::config::parse_config(&serde_json::to_vec(&document)?)?;
        let mut admin = wamn_postgres::connect(admin_database.url()).await?;

        sqlx::raw_sql("CREATE ROLE wamn_app; CREATE ROLE fixture_reader;")
            .execute(&mut admin)
            .await
            .map(drop)?;
        let fingerprint = template_fingerprint("target", "system", config.activation_identity());
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "COMMENT ON DATABASE \"target_template\" IS '{fingerprint}'"
        )))
        .execute(&mut admin)
        .await
        .map(drop)?;
        let mut template = wamn_postgres::connect(template_database.url()).await?;

        sqlx::raw_sql(wamn_catalog::CATALOG_SCHEMA_SQL)
            .execute(&mut template)
            .await
            .map(drop)?;
        sqlx::raw_sql(wamn_control_provision::APP_SCHEMA_SQL)
            .execute(&mut template)
            .await
            .map(drop)?;
        wamn_package_control::schema_baseline::record_project(&mut template).await?;
        sqlx::raw_sql("CREATE TABLE saved_data(value text);")
            .execute(&mut template)
            .await
            .map(drop)?;
        drop(template);

        let mut lease = acquire(&config).await?;
        assert_eq!(
            acquire(&config).await.unwrap_err().error_type(),
            TargetDatabaseErrorType::LeaseUnavailable
        );
        let first = lease.recreate(&config).await?;
        let first_initialization =
            wamn_environment_control::environment::sql::schema::database_state(
                target_database.url(),
                &config.activation_identity().project_env_key(),
            )
            .await?
            .initialization_identity;
        let identity = config.activation_identity();
        wamn_runtime::local_application::require_local_target(
            target_database.url(),
            &identity.project_env_key(),
            &identity.environment,
        )
        .await?;
        assert!(
            wamn_runtime::local_application::require_local_target(
                target_database.url(),
                "another-project",
                &identity.environment
            )
            .await
            .is_err()
        );
        let mut target = wamn_postgres::connect(target_database.url()).await?;

        sqlx::raw_sql("INSERT INTO saved_data VALUES ('retained');")
            .execute(&mut target)
            .await
            .map(drop)?;
        lease.require_current_lease().await?;
        assert_eq!(
            sqlx::query("SELECT value FROM saved_data")
                .fetch_one(&mut target)
                .await?
                .get::<String, _>(0),
            "retained"
        );
        std::fs::write(&acl, "REVOKE ALL ON DATABASE target FROM PUBLIC;")?;
        let prepared_configuration = prepare_configuration(&config)?;
        lease
            .reconcile_configuration(&prepared_configuration, false)
            .await?;
        assert!(
            sqlx::query("SELECT has_database_privilege('fixture_reader', 'target', 'CONNECT')")
                .fetch_one(&mut admin)
                .await?
                .get::<bool, _>(0)
        );
        std::fs::remove_file(&privileges)?;
        std::fs::write(&acl, "GRANT ALL ON DATABASE target TO PUBLIC;")?;
        lease
            .reconcile_configuration(&prepared_configuration, true)
            .await?;
        std::fs::write(&privileges, "REVOKE ALL ON DATABASE target FROM PUBLIC;")?;
        assert!(
            !sqlx::query("SELECT has_database_privilege('fixture_reader', 'target', 'CONNECT')")
                .fetch_one(&mut admin)
                .await?
                .get::<bool, _>(0)
        );
        std::fs::write(&acl, "this is invalid SQL;")?;
        assert!(
            lease
                .reconcile_configuration(&prepare_configuration(&config)?, false)
                .await
                .is_err()
        );
        assert_eq!(
            sqlx::query("SELECT count(*) FROM saved_data")
                .fetch_one(&mut target)
                .await?
                .get::<i64, _>(0),
            1
        );
        std::fs::write(&acl, "REVOKE ALL ON DATABASE target FROM PUBLIC;")?;

        let projection = "\
INSERT INTO platform.external_system (id, name) VALUES \
    ('10000000-0000-4000-8000-000000000001', 'objects-account') ON CONFLICT DO NOTHING; \
INSERT INTO platform.integration (id, integration_type, external_system_id, name, status) VALUES \
    ('20000000-0000-4000-8000-000000000001', 's3', \
     '10000000-0000-4000-8000-000000000001', 'objects', 'active') ON CONFLICT DO NOTHING; \
INSERT INTO platform.connection (id, integration_id, integration_type, status) VALUES \
    ('30000000-0000-4000-8000-000000000001', \
     '20000000-0000-4000-8000-000000000001', 's3', 'active') ON CONFLICT DO NOTHING; \
INSERT INTO catalog.releases (project_env_key, manifest_digest, canonical_bytes) VALUES \
    ('local-target', 'sha256:' || encode(sha256('{}'::bytea), 'hex'), '{}') ON CONFLICT DO NOTHING; \
INSERT INTO catalog.release_connection (project_env_key, manifest_digest, connection_id, version) VALUES \
    ('local-target', 'sha256:' || encode(sha256('{}'::bytea), 'hex'), \
     '30000000-0000-4000-8000-000000000001', 1) ON CONFLICT DO NOTHING;";
        let read_projection = "\
SELECT jsonb_build_array(external.name, integration.name, integration.integration_type, \
                        integration.status, connection.integration_type, connection.status)::text \
FROM platform.connection AS connection \
JOIN platform.integration AS integration ON integration.id = connection.integration_id \
JOIN platform.external_system AS external ON external.id = integration.external_system_id";
        let actor = wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::ApplyPackage,
        );
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("BEGIN; {actor} {projection}")))
            .execute(&mut target)
            .await
            .map(drop)?;
        let selected: String = sqlx::query(sqlx::AssertSqlSafe((read_projection).to_string()))
            .fetch_one(&mut target)
            .await?
            .get(0);
        sqlx::raw_sql("ROLLBACK")
            .execute(&mut target)
            .await
            .map(drop)?;
        assert!(
            sqlx::query(sqlx::AssertSqlSafe((read_projection).to_string()))
                .fetch_all(&mut target)
                .await?
                .is_empty()
        );
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "BEGIN; {actor} {projection} COMMIT;"
        )))
        .execute(&mut target)
        .await
        .map(drop)?;
        let stamped: bool = sqlx::query("SELECT created_by = $1::text::uuid AND updated_by = $1::text::uuid AND created_at = updated_at FROM catalog.release_connection").bind(&wamn_control_provision::PlatformComponent::ApplyPackage.principal_id().to_string()).fetch_one(&mut target).await?.get(0);
        assert!(stamped, "release connections stamp wamn:apply-package");
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "BEGIN; {actor} {projection} COMMIT;"
        )))
        .execute(&mut target)
        .await
        .map(drop)?;
        assert_eq!(
            selected,
            sqlx::query(sqlx::AssertSqlSafe((read_projection).to_string()))
                .fetch_one(&mut target)
                .await?
                .get::<String, _>(0)
        );
        assert_eq!(
            sqlx::query("SELECT count(*) FROM catalog.release_connection_history")
                .fetch_one(&mut target)
                .await?
                .get::<i64, _>(0),
            1,
            "replaying the release connection leaves its history unchanged"
        );
        drop(target);

        let second = lease.recreate(&config).await?;
        assert_ne!(first, second);
        let second_initialization =
            wamn_environment_control::environment::sql::schema::database_state(
                target_database.url(),
                &config.activation_identity().project_env_key(),
            )
            .await?
            .initialization_identity;
        assert_ne!(first_initialization, second_initialization);
        let mut target = wamn_postgres::connect(target_database.url()).await?;

        assert_eq!(
            sqlx::query("SELECT count(*) FROM saved_data")
                .fetch_one(&mut target)
                .await?
                .get::<i64, _>(0),
            0
        );
        assert!(
            sqlx::query(sqlx::AssertSqlSafe((read_projection).to_string()))
                .fetch_all(&mut target)
                .await?
                .is_empty()
        );
        assert_eq!(
            sqlx::query("SELECT count(*) FROM catalog.release_connection")
                .fetch_one(&mut target)
                .await?
                .get::<i64, _>(0),
            0
        );
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "BEGIN; {actor} {projection} COMMIT;"
        )))
        .execute(&mut target)
        .await
        .map(drop)?;
        let stamped: bool = sqlx::query("SELECT created_by = $1::text::uuid AND updated_by = $1::text::uuid AND created_at = updated_at FROM catalog.release_connection").bind(&wamn_control_provision::PlatformComponent::ApplyPackage.principal_id().to_string()).fetch_one(&mut target).await?.get(0);
        assert!(stamped, "release connections stamp wamn:apply-package");
        assert_eq!(
            selected,
            sqlx::query(sqlx::AssertSqlSafe((read_projection).to_string()))
                .fetch_one(&mut target)
                .await?
                .get::<String, _>(0)
        );
        drop(target);
        drop(lease);
        drop(admin);
        std::fs::remove_dir_all(directory)?;
        server.stop()?;
        Ok(())
    }
}
