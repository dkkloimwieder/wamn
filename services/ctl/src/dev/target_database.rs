//! Fresh, run-scoped PostgreSQL target database for the development loop.
//!
//! The loop drops and recreates the target database before every Apply. That is
//! what makes a development run reproducible from saved bytes: a package that
//! applied once cannot leave a fact behind that the next run has to argue with.
//! It is also what removes the reason for the committed-source refusal in a
//! development session, because nothing this database holds is durable.
//!
//! WHY THIS IS NOT THE VERIFICATION LIFECYCLE WITH A RENAME. A verification
//! database is usable the moment it exists. A target database is not: Apply
//! needs the roles and the privileges the environment was provisioned with.
//! Roles are cluster-level and survive the drop. Per-database privileges do
//! not, so the loop replays the privilege SQL `wamn dev up` emitted. It replays
//! that file rather than deriving a privilege set of its own, because a set the
//! loop computed would not be the set the environment was provisioned with.
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
    PrivilegesFailed,
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
            Self::PrivilegesFailed => "dev-target-database-privileges-failed",
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

/// Read the privilege SQL `wamn dev up` emitted for the target database.
///
/// A missing file is a stale standup, not a lifecycle failure: the environment
/// was never provisioned, or it was provisioned somewhere this `dev.json` no
/// longer names.
fn read_privileges(path: &Path) -> Result<String, TargetDatabaseError> {
    std::fs::read_to_string(path).map_err(|source| {
        TargetDatabaseError::new(TargetDatabaseErrorKind::StaleStandup, STALE_STANDUP_REMEDY)
            .with_source(source)
    })
}

/// Drop the target database, create it empty, and replay its privileges.
///
/// Every statement is issued on a maintenance connection to `postgres`, because
/// a session connected to the target cannot drop it.
pub async fn recreate(config: &DevConfig) -> Result<(), TargetDatabaseError> {
    let spec = TargetSpec::from_config(config)?;
    let privileges = read_privileges(config.target_privileges_file())?;

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

    let result = recreate_with_client(&client, &spec, &privileges).await;
    drop(client);
    handle.abort();
    result
}

async fn recreate_with_client(
    client: &Client,
    spec: &TargetSpec,
    privileges: &str,
) -> Result<(), TargetDatabaseError> {
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

    let outcome = replace_database(client, spec, privileges).await;

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
    outcome?;
    if !released {
        return Err(TargetDatabaseError::new(
            TargetDatabaseErrorKind::LeaseFailed,
            "ensure the target credential keeps its maintenance session through the recreate",
        ));
    }
    Ok(())
}

async fn replace_database(
    client: &Client,
    spec: &TargetSpec,
    privileges: &str,
) -> Result<(), TargetDatabaseError> {
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
    client
        .batch_execute(&format!("CREATE DATABASE {}", spec.database.quoted()))
        .await
        .map_err(|source| {
            TargetDatabaseError::new(
                TargetDatabaseErrorKind::CreateFailed,
                "grant the target credential CREATEDB authority for its disposable database",
            )
            .with_source(source)
        })?;
    client
        .batch_execute(&format!(
            "REVOKE CONNECT ON DATABASE {} FROM PUBLIC",
            spec.database.quoted()
        ))
        .await
        .map_err(|source| {
            TargetDatabaseError::new(
                TargetDatabaseErrorKind::CreateFailed,
                "grant the target credential authority to revoke PUBLIC CONNECT on its disposable database",
            )
            .with_source(source)
        })?;
    // Privileges are per database and went with the drop. Roles are
    // cluster-level and did not, so this replays privileges only.
    let target = {
        let mut target = spec.maintenance.clone();
        target.dbname(spec.database.as_str());
        target
    };
    let (target_client, connection) = target.connect(NoTls).await.map_err(|source| {
        TargetDatabaseError::new(
            TargetDatabaseErrorKind::PrivilegesFailed,
            "make the recreated target database reachable with the target credential",
        )
        .with_source(source)
    })?;
    let handle = tokio::spawn(async move {
        let _ = connection.await;
    });
    let applied = target_client
        .batch_execute(privileges)
        .await
        .map_err(|source| {
            TargetDatabaseError::new(
                TargetDatabaseErrorKind::PrivilegesFailed,
                "re-emit the environment privilege SQL with wamn dev up",
            )
            .with_source(source)
        });
    drop(target_client);
    handle.abort();
    applied
}
