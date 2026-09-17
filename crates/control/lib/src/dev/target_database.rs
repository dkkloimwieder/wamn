//! Exclusive ownership and explicit recreation of disposable development targets.
//!
//! Local sessions retain the target until its structure or an applied migration
//! changes, also across restarts of the developer process. Recreation clones
//! the stamped template and restores its captured database ACL. The lease
//! prevents another session or reset command from replacing a serving target.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

use tokio_postgres::{Client, Config as PostgresConfig, NoTls};
use wamn_pg_core::Identifier;
use wamn_schema_introspection::ir::CatalogIr;

use super::activation::DevActivationIdentity;
use super::config::{DevConfig, POSTGRES_SYSTEM_DATABASES};

const MAINTENANCE_DATABASE: &str = "postgres";
const SCHEMA_RECORD_FILE: &str = "target-schema.json";
const STALE_STANDUP_REMEDY: &str =
    "run wamn dev up to provision the environment and emit its privilege SQL";

/// Rotate an idle owned target and its environment instance together.
#[cfg(target_os = "linux")]
pub async fn reset(configuration: &Path) -> anyhow::Result<String> {
    use anyhow::Context as _;

    let bytes = std::fs::read(configuration).context("read development reset configuration")?;
    let config =
        super::config::parse_config(&bytes).context("validate development reset configuration")?;
    let lease = acquire(&config).await?;
    let instance = lease.recreate(&config).await?;
    super::coordinator::claim_environment_instance(
        config.system_database_url(),
        &config.activation_identity().tenant,
        &instance,
    )
    .await?;
    Ok(instance)
}

/// Stable category of a target-database lifecycle failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetDatabaseErrorKind {
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

impl TargetDatabaseErrorKind {
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

/// One refusal from the run-scoped target-database lifecycle.
pub struct TargetDatabaseError {
    kind: TargetDatabaseErrorKind,
    remedy: &'static str,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl fmt::Debug for TargetDatabaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TargetDatabaseError")
            .field("kind", &self.kind)
            .field("remedy", &self.remedy)
            .finish_non_exhaustive()
    }
}

impl TargetDatabaseError {
    fn new(kind: TargetDatabaseErrorKind, remedy: &'static str) -> Self {
        Self {
            kind,
            remedy,
            source: None,
        }
    }

    fn with_source(mut self, source: impl Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// Stable refusal category.
    pub const fn kind(&self) -> TargetDatabaseErrorKind {
        self.kind
    }

    /// Operator action that clears this refusal.
    pub const fn remedy(&self) -> &'static str {
        self.remedy
    }
}

impl fmt::Display for TargetDatabaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.kind.as_str(), self.remedy)
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
                    TargetDatabaseErrorKind::InvalidConfiguration,
                    "set target_database_url to a valid PostgreSQL URL with an explicit database",
                )
                .with_source(source)
            })?;
        let database = maintenance.get_dbname().ok_or_else(|| {
            TargetDatabaseError::new(
                TargetDatabaseErrorKind::InvalidConfiguration,
                "set target_database_url to a valid PostgreSQL URL with an explicit database",
            )
        })?;
        // The parse-time guard in `config` already refuses these. Repeating the
        // check here keeps the drop safe for any caller that reaches this module
        // by another path.
        if POSTGRES_SYSTEM_DATABASES.contains(&database) {
            return Err(TargetDatabaseError::new(
                TargetDatabaseErrorKind::InvalidConfiguration,
                "set target_database_url to a disposable database, never a PostgreSQL system database",
            ));
        }
        let database = Identifier::new(database.to_owned()).map_err(|source| {
            TargetDatabaseError::new(
                TargetDatabaseErrorKind::InvalidConfiguration,
                "set target_database_url to a database name PostgreSQL can quote",
            )
            .with_source(source)
        })?;
        maintenance.dbname(MAINTENANCE_DATABASE);
        Ok(Self {
            maintenance,
            database,
        })
    }
}

/// Fingerprint of the standup a template belongs to.
///
/// A template is only pristine FOR THE ENVIRONMENT IT WAS TAKEN FROM. Point
/// `dev.json` at another database or another tenant and the clone would be
/// someone else's provisioned state, so the run refuses instead.
///
/// Database NAMES and identity, never the URLs: a rotated credential does not
/// make a template stale, and a hash of a URL would say it did.
pub fn template_fingerprint(
    target_database: &str,
    system_database: &str,
    identity: &DevActivationIdentity,
) -> String {
    let fields = serde_json::json!({
        "target_database": target_database,
        "system_database": system_database,
        "org": identity.org,
        "project": identity.project,
        "environment": identity.environment,
        "tenant": identity.tenant,
        "catalog": identity.catalog,
        "schema": identity.schema,
    });
    wamn_execution_contract::canonical_json_sha256(&fields)
}

/// The database name inside a PostgreSQL URL, for fingerprinting.
pub fn database_name(url: &str) -> Option<String> {
    PostgresConfig::from_str(url)
        .ok()?
        .get_dbname()
        .map(str::to_owned)
}

/// Read the database-level ACL `wamn dev up` captured from the healthy target.
///
/// A missing file is a stale standup, not a lifecycle failure: the environment
/// was never provisioned, or it was provisioned somewhere this `dev.json` no
/// longer names.
fn read_database_acl(path: &Path) -> Result<String, TargetDatabaseError> {
    std::fs::read_to_string(path).map_err(|source| {
        TargetDatabaseError::new(TargetDatabaseErrorKind::StaleStandup, STALE_STANDUP_REMEDY)
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

/// Record the schema inputs and structure of one target creation and the catalogs
/// a successful Introspect read from it.
///
/// The record names the database instance, so a record left beside a target
/// that was since recreated or reset matches nothing.
pub(crate) fn record_target_schema(
    config: &DevConfig,
    instance: &str,
    schema_digest: &str,
    structure_digest: &str,
    catalogs: &BTreeMap<String, CatalogIr>,
) -> anyhow::Result<()> {
    use anyhow::Context as _;
    let directory = &config.local_artifacts().directory;
    std::fs::create_dir_all(directory).context("create the local artifact directory")?;
    let record = serde_json::json!({
        "database-instance": instance,
        "schema-input-digest": schema_digest,
        "target-structure-digest": structure_digest,
        "catalogs": catalogs,
    });
    std::fs::write(
        directory.join(SCHEMA_RECORD_FILE),
        serde_json::to_vec(&record)?,
    )
    .context("record the target schema")
}

/// Exclusive ownership held while a development session can serve this target.
pub struct TargetLease {
    client: Client,
    spec: TargetSpec,
    connection: tokio::task::JoinHandle<()>,
}

impl fmt::Debug for TargetLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TargetLease")
            .field("database", &self.spec.database.as_str())
            .finish_non_exhaustive()
    }
}

impl Drop for TargetLease {
    fn drop(&mut self) {
        // Closing this connection releases PostgreSQL's session advisory lock.
        self.connection.abort();
    }
}

impl TargetLease {
    /// Verify the retained maintenance connection before serving another candidate.
    pub async fn check(&self) -> Result<(), TargetDatabaseError> {
        self.client
            .simple_query("SELECT 1")
            .await
            .map_err(|source| {
                TargetDatabaseError::new(
                    TargetDatabaseErrorKind::LeaseFailed,
                    "restart the development session after its exclusive target lease was lost",
                )
                .with_source(source)
            })?;
        Ok(())
    }

    /// Validate or converge captured database privileges without retaining obsolete grants.
    pub(crate) async fn reconcile_configuration(
        &self,
        prepared: &PreparedConfiguration,
        apply: bool,
    ) -> anyhow::Result<()> {
        use anyhow::Context as _;
        self.check().await?;
        anyhow::ensure!(
            prepared.database == self.spec.database,
            "prepared privileges name another local target"
        );
        self.client.batch_execute("BEGIN").await?;
        let result = async {
            let rows = self.client.query(
                "SELECT DISTINCT grant_row.grantee, pg_catalog.pg_get_userbyid(grant_row.grantee)::text
                 FROM pg_catalog.pg_database AS database,
                      LATERAL pg_catalog.aclexplode(COALESCE(database.datacl, pg_catalog.acldefault('d', database.datdba))) AS grant_row
                 WHERE database.datname = $1", &[&self.spec.database.as_str()],
            ).await?;
            for row in rows {
                let grantee: u32 = row.get(0);
                let role = if grantee == 0 { "PUBLIC".to_owned() } else { Identifier::new(row.get::<_, String>(1))?.quoted() };
                self.client.batch_execute(&format!("REVOKE ALL ON DATABASE {} FROM {role}", self.spec.database.quoted())).await?;
            }
            self.client.batch_execute(&prepared.privileges).await?;
            self.client.batch_execute(&prepared.acl).await?;
            Ok::<_, anyhow::Error>(())
        }.await;
        match result {
            Ok(()) => {
                self.client
                    .batch_execute(if apply { "COMMIT" } else { "ROLLBACK" })
                    .await?;
            }
            Err(error) => {
                self.client
                    .batch_execute("ROLLBACK")
                    .await
                    .context("roll back target privilege validation")?;
                return Err(error);
            }
        }
        Ok(())
    }

    /// The instance of this target, its recorded schema digest, and its saved
    /// catalogs when the record names `structure_digest`.
    ///
    /// Returns `None` when the record is absent or unreadable, holds no
    /// catalogs, names another structure or another creation of the database,
    /// or when the local target marker no longer matches. The caller then
    /// recreates the target.
    pub(crate) async fn retained_target(
        &self,
        config: &DevConfig,
        structure_digest: &str,
    ) -> Option<(String, String, BTreeMap<String, CatalogIr>)> {
        let bytes =
            std::fs::read(config.local_artifacts().directory.join(SCHEMA_RECORD_FILE)).ok()?;
        let mut record: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        if record["target-structure-digest"] != structure_digest {
            return None;
        }
        let schema_digest = record["schema-input-digest"].as_str()?.to_owned();
        let catalogs = serde_json::from_value(record["catalogs"].take()).ok()?;
        let instance: u32 = self
            .client
            .query_one(
                "SELECT oid FROM pg_catalog.pg_database WHERE datname = $1",
                &[&self.spec.database.as_str()],
            )
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
            &identity.tenant,
            &identity.environment,
        )
        .await
        .ok()?;
        Some((instance, schema_digest, catalogs))
    }

    /// Recreate only the target named by this still-held lease.
    pub async fn recreate(&self, config: &DevConfig) -> Result<String, TargetDatabaseError> {
        let selected = TargetSpec::from_config(config)?;
        if selected.database != self.spec.database {
            return Err(TargetDatabaseError::new(
                TargetDatabaseErrorKind::InvalidConfiguration,
                "the target configuration must name the database held by this session",
            ));
        }
        self.check().await?;
        let template =
            Identifier::new(config.target_template_database().to_owned()).map_err(|source| {
                TargetDatabaseError::new(
                    TargetDatabaseErrorKind::InvalidConfiguration,
                    "set target_template_database to a database name PostgreSQL can quote",
                )
                .with_source(source)
            })?;
        let acl = read_database_acl(config.target_database_acl_file())?;
        let system_database = database_name(config.system_database_url()).ok_or_else(|| {
            TargetDatabaseError::new(
                TargetDatabaseErrorKind::InvalidConfiguration,
                "set system_database_url to a PostgreSQL URL with an explicit database",
            )
        })?;
        let fingerprint = template_fingerprint(
            self.spec.database.as_str(),
            &system_database,
            config.activation_identity(),
        );
        let instance =
            replace_database(&self.client, &self.spec, &template, &fingerprint, &acl).await?;
        let identity = config.activation_identity();
        let marker = wamn_runtime::local_application::local_target_marker(
            &identity.tenant,
            &identity.environment,
            instance
                .parse()
                .expect("database instance is a PostgreSQL oid"),
        );
        self.client
            .batch_execute(&format!(
                "COMMENT ON DATABASE {} IS '{}'",
                self.spec.database.quoted(),
                marker,
            ))
            .await
            .map_err(|source| {
                TargetDatabaseError::new(
                    TargetDatabaseErrorKind::CreateFailed,
                    "stamp the owned local target before serving the application",
                )
                .with_source(source)
            })?;
        Ok(instance)
    }
}

/// Refuse concurrent sessions or reset commands before any target mutation.
pub async fn acquire(config: &DevConfig) -> Result<TargetLease, TargetDatabaseError> {
    let spec = TargetSpec::from_config(config)?;
    let (client, connection) = spec.maintenance.connect(NoTls).await.map_err(|source| {
        TargetDatabaseError::new(
            TargetDatabaseErrorKind::InvalidConfiguration,
            "make the PostgreSQL maintenance database reachable with the target credential",
        )
        .with_source(source)
    })?;
    let connection = tokio::spawn(async move {
        let _ = connection.await;
    });
    let lease = TargetLease {
        client,
        spec,
        connection,
    };
    let acquired: bool = lease.client.query_one(
        "SELECT pg_catalog.pg_try_advisory_lock(pg_catalog.hashtextextended($1, 0))",
        &[&lease.spec.database.as_str()],
    ).await.map(|row| row.get(0)).map_err(|source| {
        TargetDatabaseError::new(TargetDatabaseErrorKind::LeaseFailed,
            "ensure the target credential can acquire session advisory locks on the postgres maintenance database")
            .with_source(source)
    })?;
    if !acquired {
        return Err(TargetDatabaseError::new(
            TargetDatabaseErrorKind::LeaseUnavailable,
            "stop the active wamn dev session before resetting or reusing this target",
        ));
    }
    Ok(lease)
}

/// Reset an idle owned target, refusing a target held by a serving session.
pub async fn recreate(config: &DevConfig) -> Result<String, TargetDatabaseError> {
    acquire(config).await?.recreate(config).await
}

/// Returns the INSTANCE identity of the database this call created.
///
/// A recreated database is a different target, and anything the control plane
/// keys to this environment has to say which creation it means (wamn-10yt.51).
/// PostgreSQL already mints exactly that: `CREATE DATABASE` assigns a fresh oid,
/// so the server is the authority and nothing here invents a value. A timestamp
/// would have been a guess about identity; this is the identity itself.
async fn replace_database(
    client: &Client,
    spec: &TargetSpec,
    template: &Identifier,
    fingerprint: &str,
    acl: &str,
) -> Result<String, TargetDatabaseError> {
    // A missing template is a stale standup and not a lifecycle failure. It is
    // checked before the drop, so a run that cannot restore the database never
    // destroys it.
    let present: bool = client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_database WHERE datname = $1)",
            &[&template.as_str()],
        )
        .await
        .map(|row| row.get(0))
        .map_err(|source| {
            TargetDatabaseError::new(TargetDatabaseErrorKind::StaleStandup, STALE_STANDUP_REMEDY)
                .with_source(source)
        })?;
    if !present {
        return Err(TargetDatabaseError::new(
            TargetDatabaseErrorKind::TemplateMissing,
            STALE_STANDUP_REMEDY,
        ));
    }

    // The template is pristine only for the standup it was taken from. Both
    // checks run BEFORE the drop, so a run that cannot restore the database
    // never destroys it.
    let stamped: Option<String> = client
        .query_one(
            "SELECT pg_catalog.shobj_description(oid, 'pg_database')
               FROM pg_catalog.pg_database WHERE datname = $1",
            &[&template.as_str()],
        )
        .await
        .map(|row| row.get(0))
        .map_err(|source| {
            TargetDatabaseError::new(TargetDatabaseErrorKind::StaleStandup, STALE_STANDUP_REMEDY)
                .with_source(source)
        })?;
    if stamped.as_deref() != Some(fingerprint) {
        return Err(TargetDatabaseError::new(
            TargetDatabaseErrorKind::TemplateForeign,
            STALE_STANDUP_REMEDY,
        ));
    }

    client
        .batch_execute(&format!(
            "DROP DATABASE IF EXISTS {} WITH (FORCE)",
            spec.database.quoted()
        ))
        .await
        .map_err(|source| {
            TargetDatabaseError::new(
                TargetDatabaseErrorKind::DropFailed,
                "grant the target credential authority to drop its disposable database",
            )
            .with_source(source)
        })?;
    // The whole provisioned state arrives here: schemas, tables, functions and
    // every object-level privilege. A clone of a pristine template is pristine,
    // so freshness is a property of the copy rather than of a replay this
    // module got right.
    client
        .batch_execute(&format!(
            "CREATE DATABASE {} TEMPLATE {}",
            spec.database.quoted(),
            template.quoted()
        ))
        .await
        .map_err(|source| {
            TargetDatabaseError::new(
                TargetDatabaseErrorKind::CreateFailed,
                "grant the target credential CREATEDB authority for its disposable database",
            )
            .with_source(source)
        })?;
    // CREATE DATABASE copies objects and not the ACL of the database itself, so
    // ownership, the PUBLIC revoke and the workload CONNECT grants are the one
    // thing to re-issue.
    client.batch_execute(acl).await.map_err(|source| {
        TargetDatabaseError::new(
            TargetDatabaseErrorKind::AclFailed,
            "re-emit the environment database ACL with wamn dev up",
        )
        .with_source(source)
    })?;
    let instance: u32 = client
        .query_one(
            "SELECT oid FROM pg_catalog.pg_database WHERE datname = $1",
            &[&spec.database.as_str()],
        )
        .await
        .map(|row| row.get(0))
        .map_err(|source| {
            TargetDatabaseError::new(
                TargetDatabaseErrorKind::CreateFailed,
                "grant the target credential CREATEDB authority for its disposable database",
            )
            .with_source(source)
        })?;
    Ok(instance.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wamn_schema_introspection::ir::{
        Column, ColumnDefault, ColumnGeneration, ColumnType, Constraint, Exclusion,
        ExclusionAccessMethod, ExclusionElement, ExclusionKey, ForeignKeyAction, ForeignKeyColumn,
        IdentityMode, Index, IndexColumn, IndexDirection, Table,
    };

    /// Catalogs that hold every tagged and flattened IR shape the record carries.
    fn record_catalogs() -> anyhow::Result<BTreeMap<String, CatalogIr>> {
        let order = Table::new(
            "receiving",
            "purchase_order",
            vec![
                Column::new(
                    "id",
                    ColumnType::Uuid,
                    false,
                    Some(ColumnDefault::GenRandomUuid),
                    None,
                ),
                Column::new(
                    "number",
                    ColumnType::Int64,
                    false,
                    None,
                    Some(ColumnGeneration::Identity {
                        mode: IdentityMode::Always,
                    }),
                ),
                Column::new(
                    "status",
                    ColumnType::Text,
                    false,
                    Some(ColumnDefault::text("open")),
                    None,
                ),
                Column::new("starts_at", ColumnType::Timestamptz, true, None, None),
                Column::new("ends_at", ColumnType::Timestamptz, true, None, None),
            ],
            vec![
                Constraint::primary_key("purchase_order_id_pkey", ["id"])?,
                Constraint::check("purchase_order_status_check", "status <> ''")?,
            ],
            vec![Index::new(
                "purchase_order_number_idx",
                vec![IndexColumn::new("number", IndexDirection::Desc)],
            )?],
        )
        .with_exclusions(vec![Exclusion::new(
            "purchase_order_no_overlap",
            ExclusionAccessMethod::Gist,
            vec![
                ExclusionKey::new(ExclusionElement::column("id"), "="),
                ExclusionKey::new(
                    ExclusionElement::expression("tstzrange(starts_at, ends_at)"),
                    "&&",
                ),
            ],
            ["id", "starts_at", "ends_at"],
        )?]);
        let line = Table::new(
            "receiving",
            "purchase_order_line",
            vec![Column::new("order_id", ColumnType::Uuid, false, None, None)],
            vec![Constraint::foreign_key(
                "purchase_order_line_order_id_fkey",
                vec![ForeignKeyColumn::new("order_id", "id")],
                "receiving",
                "purchase_order",
                ForeignKeyAction::NoAction,
                ForeignKeyAction::Cascade,
            )?],
            Vec::new(),
        );
        Ok(BTreeMap::from([(
            "wamn-receiving".to_owned(),
            CatalogIr::new(vec![order, line]),
        )]))
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
        let mut document = crate::dev::config::tests::complete_document(&[endpoint; 11]);
        document["target_database_url"] = json!(target_database.url());
        document["target_template_database"] = json!("target_template");
        document["target_privileges_file"] = json!(privileges);
        document["target_database_acl_file"] = json!(acl);
        document["local_artifacts"] =
            json!({"directory": directory, "flow_http_component": directory.join("http.wasm")});
        let config = super::super::config::parse_config(&serde_json::to_vec(&document)?)?;
        let (admin, admin_driver) = tokio_postgres::connect(admin_database.url(), NoTls).await?;
        let admin_driver = tokio::spawn(admin_driver);
        admin.batch_execute("CREATE ROLE wamn_app; CREATE ROLE wamn_scenario_author; CREATE ROLE fixture_reader;").await?;
        let fingerprint = template_fingerprint("target", "system", config.activation_identity());
        admin
            .batch_execute(&format!(
                "COMMENT ON DATABASE \"target_template\" IS '{fingerprint}'"
            ))
            .await?;
        let (template, driver) = tokio_postgres::connect(template_database.url(), NoTls).await?;
        let driver = tokio::spawn(driver);
        template
            .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
            .await?;
        template
            .batch_execute("CREATE TABLE saved_data(value text);")
            .await?;
        drop(template);
        driver.await??;

        let lease = acquire(&config).await?;
        assert_eq!(
            acquire(&config).await.unwrap_err().kind(),
            TargetDatabaseErrorKind::LeaseUnavailable
        );
        let first = lease.recreate(&config).await?;
        let identity = config.activation_identity();
        wamn_runtime::local_application::require_local_target(
            target_database.url(),
            &identity.tenant,
            &identity.environment,
        )
        .await?;
        assert!(
            wamn_runtime::local_application::require_local_target(
                target_database.url(),
                "another-tenant",
                &identity.environment
            )
            .await
            .is_err()
        );
        let (target, driver) = tokio_postgres::connect(target_database.url(), NoTls).await?;
        let driver = tokio::spawn(driver);
        target
            .batch_execute("INSERT INTO saved_data VALUES ('retained');")
            .await?;
        lease.check().await?;
        assert_eq!(
            target
                .query_one("SELECT value FROM saved_data", &[])
                .await?
                .get::<_, String>(0),
            "retained"
        );
        assert_eq!(
            recreate(&config).await.unwrap_err().kind(),
            TargetDatabaseErrorKind::LeaseUnavailable
        );
        std::fs::write(&acl, "REVOKE ALL ON DATABASE target FROM PUBLIC;")?;
        let prepared_configuration = prepare_configuration(&config)?;
        lease
            .reconcile_configuration(&prepared_configuration, false)
            .await?;
        assert!(
            admin
                .query_one(
                    "SELECT has_database_privilege('fixture_reader', 'target', 'CONNECT')",
                    &[]
                )
                .await?
                .get::<_, bool>(0)
        );
        std::fs::remove_file(&privileges)?;
        std::fs::write(&acl, "GRANT ALL ON DATABASE target TO PUBLIC;")?;
        lease
            .reconcile_configuration(&prepared_configuration, true)
            .await?;
        std::fs::write(&privileges, "REVOKE ALL ON DATABASE target FROM PUBLIC;")?;
        assert!(
            !admin
                .query_one(
                    "SELECT has_database_privilege('fixture_reader', 'target', 'CONNECT')",
                    &[]
                )
                .await?
                .get::<_, bool>(0)
        );
        std::fs::write(&acl, "this is invalid SQL;")?;
        assert!(
            lease
                .reconcile_configuration(&prepare_configuration(&config)?, false)
                .await
                .is_err()
        );
        assert_eq!(
            target
                .query_one("SELECT count(*) FROM saved_data", &[])
                .await?
                .get::<_, i64>(0),
            1
        );
        std::fs::write(&acl, "REVOKE ALL ON DATABASE target FROM PUBLIC;")?;

        let definition = directory.join("connection.json");
        std::fs::write(
            &definition,
            r#"{"endpoint":"https://objects.invalid","container":"fixture","prefix":"local/"}"#,
        )?;
        let mut input = crate::bind_connection::LocalInstanceInput {
            requirement_type: crate::bind_connection::RequirementType::Blobstore,
            definition,
            credential_handle: "fixture-vault-handle".to_owned(),
        };
        let prepared_instance = crate::bind_connection::read_local_instance(&input)?;
        let requirement = wamn_catalog::ComponentConnectionRequirement::new(
            format!("sha256:{}", "7".repeat(64)),
            "objects",
            wamn_catalog::ConnectionTypeDescriptor::blobstore_v1(),
        );
        target.batch_execute("BEGIN").await?;
        crate::bind_connection::prepare_local_instance(
            &target,
            &identity.tenant,
            &identity.environment,
            "objects-a",
            &prepared_instance,
        )
        .await?;
        let selected = wamn_runtime::local_application::read_local_binding(
            &target,
            &identity.tenant,
            &identity.environment,
            &requirement,
            "objects-a",
        )
        .await?;
        target.batch_execute("ROLLBACK").await?;
        assert!(
            wamn_runtime::local_application::read_local_binding(
                &target,
                &identity.tenant,
                &identity.environment,
                &requirement,
                "objects-a"
            )
            .await
            .is_err()
        );
        let definition_bytes = std::fs::read(&input.definition)?;
        std::fs::remove_file(&input.definition)?;
        assert!(crate::bind_connection::read_local_instance(&input).is_err());
        target.batch_execute("BEGIN").await?;
        crate::bind_connection::prepare_local_instance(
            &target,
            &identity.tenant,
            &identity.environment,
            "objects-a",
            &prepared_instance,
        )
        .await?;
        target.batch_execute("COMMIT").await?;
        crate::bind_connection::prepare_local_instance(
            &target,
            &identity.tenant,
            &identity.environment,
            "objects-a",
            &prepared_instance,
        )
        .await?;
        assert_eq!(
            selected,
            wamn_runtime::local_application::read_local_binding(
                &target,
                &identity.tenant,
                &identity.environment,
                &requirement,
                "objects-a"
            )
            .await?
        );
        std::fs::write(&input.definition, definition_bytes)?;
        input.credential_handle = "changed-handle".to_owned();
        assert!(
            crate::bind_connection::prepare_local_instance(
                &target,
                &identity.tenant,
                &identity.environment,
                "objects-a",
                &crate::bind_connection::read_local_instance(&input)?
            )
            .await
            .is_err()
        );
        assert_eq!(
            selected,
            wamn_runtime::local_application::read_local_binding(
                &target,
                &identity.tenant,
                &identity.environment,
                &requirement,
                "objects-a"
            )
            .await?
        );
        drop(target);
        driver.await??;

        // A restarted session keeps the target, its schema digest, and the
        // catalogs its last Introspect recorded, and a changed structure
        // recreates it.
        std::fs::write(
            directory.join(SCHEMA_RECORD_FILE),
            serde_json::to_vec(&json!({
                "database-instance": first,
                "schema-input-digest": "sha256:schema-one",
                "target-structure-digest": "sha256:structure-one",
            }))?,
        )?;
        assert_eq!(
            lease.retained_target(&config, "sha256:structure-one").await,
            None,
            "a record that holds no catalogs recreates the target"
        );
        let catalogs = record_catalogs()?;
        record_target_schema(
            &config,
            &first,
            "sha256:schema-one",
            "sha256:structure-one",
            &catalogs,
        )?;
        assert_eq!(
            lease.retained_target(&config, "sha256:structure-one").await,
            Some((first.clone(), "sha256:schema-one".to_owned(), catalogs))
        );
        assert_eq!(
            lease.retained_target(&config, "sha256:structure-two").await,
            None
        );

        let second = lease.recreate(&config).await?;
        assert_ne!(first, second);
        assert_eq!(
            lease.retained_target(&config, "sha256:structure-one").await,
            None,
            "a record of the previous creation matches no later one"
        );
        let (target, driver) = tokio_postgres::connect(target_database.url(), NoTls).await?;
        let driver = tokio::spawn(driver);
        assert_eq!(
            target
                .query_one("SELECT count(*) FROM saved_data", &[])
                .await?
                .get::<_, i64>(0),
            0
        );
        target.batch_execute("BEGIN").await?;
        crate::bind_connection::prepare_local_instance(
            &target,
            &identity.tenant,
            &identity.environment,
            "objects-a",
            &prepared_instance,
        )
        .await?;
        target.batch_execute("COMMIT").await?;
        wamn_runtime::local_application::read_local_binding(
            &target,
            &identity.tenant,
            &identity.environment,
            &requirement,
            "objects-a",
        )
        .await?;
        drop(target);
        driver.await??;
        drop(lease);
        drop(admin);
        admin_driver.await??;
        std::fs::remove_dir_all(directory)?;
        server.stop()?;
        Ok(())
    }
}
