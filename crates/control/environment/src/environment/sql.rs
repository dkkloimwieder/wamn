//! Independent environment provisioning and absent-application initialization.

use sqlx::Row as _;
pub mod reset;
pub mod schema;

use anyhow::{Context as _, ensure};

use super::document::{DeclaredRelease, EnvironmentDocument};
use super::{Platform, analyse, ensure, stage};

/// Database identities resolved before a Job starts, compared under its own lock.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ExpectedDatabase {
    active: Option<String>,
}

impl ExpectedDatabase {
    pub async fn read(platform: &Platform, document: &EnvironmentDocument) -> anyhow::Result<Self> {
        let mut client = wamn_postgres::connect(&platform.system_database_url).await?;
        let row = sqlx::query(wamn_registry::sql::CONTROL_ENVIRONMENT_SRC_ENVIRONMENT_SQL_READ)
            .bind(&document.org)
            .bind(&document.project)
            .bind(&document.env)
            .fetch_optional(&mut client)
            .await?;
        Ok(match row {
            Some(row) => Self {
                active: Some(row.get(0)),
            },
            None => Self { active: None },
        })
    }

    /// The Job owns this session until SQL finishes. An expired CLI cannot release it.
    pub async fn lock(
        &self,
        platform: &Platform,
        document: &EnvironmentDocument,
        exclusive: bool,
    ) -> anyhow::Result<super::lock::LifecycleLock> {
        let lock = if exclusive {
            super::lock::LifecycleLock::acquire_wait(
                &platform.system_database_url,
                &document.triple(),
            )
            .await?
        } else {
            super::lock::LifecycleLock::acquire_shared(
                &platform.system_database_url,
                &document.triple(),
            )
            .await?
        };
        let actual = Self::read(platform, document).await?;
        ensure!(
            actual.active == self.active,
            "SQL Job database instance changed before execution"
        );
        if !exclusive {
            crate::provision_project_env::registry::require_connected(
                &mut *lock.client().await,
                &document.triple(),
            )
            .await?;
        }
        Ok(lock)
    }
}

/// Initialize project state and absent package baselines after CNPG creates the database.
/// Committed package records survive a later failure and prevent SQL replay.
pub async fn initialize(
    platform: &Platform,
    document: &EnvironmentDocument,
    admin_url: &str,
    application: Option<&str>,
) -> anyhow::Result<()> {
    let document = analyse::select_application(document, application)?;
    let DeclaredRelease::Digest(digest) = document.legacy_release() else {
        anyhow::bail!("initialization requires an application release");
    };
    let manifest = analyse::pull_release_manifest(platform, digest).await?;
    let packages = analyse::read_release_packages(platform, &manifest).await?;
    let project_url = ensure::project_url(platform, &document.triple()).await?;
    let database = wamn_postgres::identity(&project_url)?.1;
    let mut admin = wamn_postgres::connect(admin_url).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        crate::provision_project_env::privilege_sql(&database),
    ))
    .execute(&mut admin)
    .await
    .map(drop)
    .context("set the database base privileges after CNPG")?;
    ensure::ensure_project_database(platform, &document, &packages, &mut admin, admin_url).await?;
    let drops =
        super::bindings::project_authority(&platform.system_database_url, &project_url, &document)
            .await?;
    platform.auth_changes().await?.publish(drops).await;
    let installed = stage::installed(platform, &document).await?;
    stage::initialize(platform, &document, &installed, &manifest, &project_url).await?;
    Ok(())
}

/// Inputs for one project substrate operation in the selected image.
#[derive(Debug)]
pub struct ProjectSchemaRequest<'a> {
    pub expected: &'a ExpectedDatabase,
    pub migration_database_url: &'a str,
    pub sources: &'a [sqlx::migrate::Migration],
    pub apply: bool,
}

/// Resolve the registry target under the Job's exclusion and execute only substrate SQL.
pub async fn project_schema(
    platform: &Platform,
    document: &EnvironmentDocument,
    request: ProjectSchemaRequest<'_>,
    output: impl std::io::Write,
) -> anyhow::Result<()> {
    let ProjectSchemaRequest {
        expected,
        migration_database_url,
        sources,
        apply,
    } = request;
    let lock = expected.lock(platform, document, true).await?;
    crate::provision_project_env::require_connected(&mut *lock.client().await, &document.triple())
        .await?;
    let url = ensure::project_url(platform, &document.triple()).await?;
    let database = wamn_postgres::identity(&url)?.1;
    let mut client = wamn_postgres::connect(
        wamn_postgres::connect_options(migration_database_url)
            .await?
            .database(&database),
    )
    .await?;

    let result = lock
        .protect(async {
            crate::platform_migrations::run(
                &mut client,
                crate::platform_migrations::MigrationTarget::Project(&lock),
                sources,
                apply,
                output,
            )
            .await
        })
        .await;
    drop(client);
    lock.release().await;
    result
}
