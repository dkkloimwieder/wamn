//! Fresh, run-scoped PostgreSQL target database for the development loop.
//!
//! The loop drops and recreates the target database before every Apply. That is
//! what makes a development run reproducible from saved bytes: a package that
//! applied once cannot leave a fact behind that the next run has to argue with.
//! It is also what removes the reason for the committed-source refusal in a
//! development session, because nothing this database holds is durable.
//!
//! WHY THIS IS NOT THE VERIFICATION LIFECYCLE WITH A RENAME. A verification
//! database is usable the moment it exists. A target database is not. Apply
//! needs the environment the standup built, and a drop takes all of it: the
//! platform floor, the run plane and the workload grants.
//!
//! SO THE RUN DOES NOT REBUILD IT, IT CLONES IT. `wamn dev up` leaves a pristine
//! TEMPLATE database behind, and a run is `CREATE DATABASE target TEMPLATE
//! <name>`. Freshness is then a property of the copy rather than of a replay
//! this module got right, and there is no second provisioning path to drift
//! from the first.
//!
//! THE ONE THING A CLONE DOES NOT CARRY is the ACL of the database itself.
//! `CREATE DATABASE` copies every object and every object-level privilege, and
//! no database-level grant. So ownership, the PUBLIC revoke and the workload
//! CONNECT grants are re-issued from SQL `wamn dev up` captured off the healthy
//! database. Captured, never derived: a set this module computed would be its
//! opinion of what the standup granted rather than what it granted.
//!
//! THE DATABASE KEEPS ITS NAME. The environment row in the system database
//! points at one database name, so recreating under the same name keeps the
//! durable registry honest and stops a watch session from minting a row per
//! save.
//!
//! THERE IS NO TRAILING DROP, and that is deliberate. The run's product is a
//! served release, and `--hold` keeps serving it after the loop returns.
//! Dropping at the end of the run would destroy what the run just produced. The
//! next run's leading drop is what removes it, so a crash leaves one corpse and
//! the run after it clears the corpse.

use std::error::Error;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

use tokio_postgres::{Client, Config as PostgresConfig, NoTls};
use wamn_pg_core::Identifier;

use super::activation::DevActivationIdentity;
use super::config::{DevConfig, POSTGRES_SYSTEM_DATABASES};

const MAINTENANCE_DATABASE: &str = "postgres";
const STALE_STANDUP_REMEDY: &str =
    "run wamn dev up to provision the environment and emit its privilege SQL";

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
        // by another path, exactly as the verification lifecycle does.
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

/// Drop the target database and clone it back from the pristine template.
///
/// Every statement is issued on a maintenance connection to `postgres`, because
/// a session connected to the target cannot drop it, and `CREATE DATABASE`
/// cannot run inside the database it creates.
pub async fn recreate(config: &DevConfig) -> Result<String, TargetDatabaseError> {
    let spec = TargetSpec::from_config(config)?;
    let template =
        Identifier::new(config.target_template_database().to_owned()).map_err(|source| {
            TargetDatabaseError::new(
                TargetDatabaseErrorKind::InvalidConfiguration,
                "set target_template_database to a database name PostgreSQL can quote",
            )
            .with_source(source)
        })?;
    let acl = read_database_acl(config.target_database_acl_file())?;

    let (client, connection) = spec.maintenance.connect(NoTls).await.map_err(|source| {
        TargetDatabaseError::new(
            TargetDatabaseErrorKind::InvalidConfiguration,
            "make the PostgreSQL maintenance database reachable with the target credential",
        )
        .with_source(source)
    })?;
    let handle = tokio::spawn(async move {
        let _ = connection.await;
    });

    let system_database = database_name(config.system_database_url()).ok_or_else(|| {
        TargetDatabaseError::new(
            TargetDatabaseErrorKind::InvalidConfiguration,
            "set system_database_url to a PostgreSQL URL with an explicit database",
        )
    })?;
    let fingerprint = template_fingerprint(
        spec.database.as_str(),
        &system_database,
        config.activation_identity(),
    );

    let result = recreate_with_client(&client, &spec, &template, &fingerprint, &acl).await;
    drop(client);
    handle.abort();
    result
}

async fn recreate_with_client(
    client: &Client,
    spec: &TargetSpec,
    template: &Identifier,
    fingerprint: &str,
    acl: &str,
) -> Result<String, TargetDatabaseError> {
    // The lease is session-scoped and keyed by the database name, so two dev
    // loops pointed at one target refuse rather than drop each other's database
    // mid-run.
    let acquired: bool = client
        .query_one(
            "SELECT pg_catalog.pg_try_advisory_lock(pg_catalog.hashtextextended($1, 0))",
            &[&spec.database.as_str()],
        )
        .await
        .map(|row| row.get(0))
        .map_err(|source| {
            TargetDatabaseError::new(
                TargetDatabaseErrorKind::LeaseFailed,
                "ensure the target credential can acquire session advisory locks on the postgres maintenance database",
            )
            .with_source(source)
        })?;
    if !acquired {
        return Err(TargetDatabaseError::new(
            TargetDatabaseErrorKind::LeaseUnavailable,
            "wait for the active wamn dev run using this target database to finish",
        ));
    }

    let outcome = replace_database(client, spec, template, fingerprint, acl).await;

    let released: bool = client
        .query_one(
            "SELECT pg_catalog.pg_advisory_unlock(pg_catalog.hashtextextended($1, 0))",
            &[&spec.database.as_str()],
        )
        .await
        .map(|row| row.get(0))
        .map_err(|source| {
            TargetDatabaseError::new(
                TargetDatabaseErrorKind::LeaseFailed,
                "ensure the target credential keeps its maintenance session through the recreate",
            )
            .with_source(source)
        })?;
    let instance = outcome?;
    if !released {
        return Err(TargetDatabaseError::new(
            TargetDatabaseErrorKind::LeaseFailed,
            "ensure the target credential keeps its maintenance session through the recreate",
        ));
    }
    Ok(instance)
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
