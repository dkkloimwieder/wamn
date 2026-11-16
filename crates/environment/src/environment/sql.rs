//! Independent environment provisioning and absent-application initialization.

pub mod reset;
pub mod schema;

use anyhow::ensure;

use super::document::EnvironmentDocument;
use super::{Platform, ensure};

/// Database identities resolved before a Job starts, compared under its own lock.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ExpectedDatabase {
    active: Option<wamn_registry::instance_suffix::InstanceSuffix>,
}

impl ExpectedDatabase {
    pub async fn read(platform: &Platform, document: &EnvironmentDocument) -> anyhow::Result<Self> {
        let mut client = wamn_postgres::connect(&platform.system_database_url).await?;
        Ok(Self {
            active: wamn_registry::deployment::instance_suffix(&mut client, &document.triple())
                .await?,
        })
    }

    /// The Job owns this session until SQL finishes. An expired CLI cannot release it.
    pub async fn lock(
        &self,
        platform: &Platform,
        document: &EnvironmentDocument,
        exclusive: bool,
    ) -> anyhow::Result<wamn_catalog::lifecycle_lock::LifecycleLock> {
        let lock = if exclusive {
            wamn_catalog::lifecycle_lock::LifecycleLock::acquire_wait(
                wamn_postgres::connect(&platform.system_database_url).await?,
                &document.triple(),
            )
            .await?
        } else {
            wamn_catalog::lifecycle_lock::LifecycleLock::acquire_shared(
                wamn_postgres::connect(&platform.system_database_url).await?,
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
