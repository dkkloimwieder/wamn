//! Ordered installation of the system database.

use std::num::NonZeroU32;

use anyhow::Context as _;
use wamn_registry::platform_domain::PlatformDomain;

use wamn_postgres::database::ConnectionConfig;

/// Database containing the control store and identity issuer surface.
pub const SYSTEM_DATABASE: &str = "wamn_system";

/// Inputs of one `provision-system` run.
#[derive(Debug)]
pub struct ProvisionSystemRequest {
    /// Public connection settings for the system database's postgres certificate.
    pub database: ConnectionConfig,
    /// The email domain of the platform principal rows.
    pub platform_domain: PlatformDomain,
    /// The platform-wide project stream limit.
    pub max_projects: NonZeroU32,
}

/// Database operations performed by the platform SQL Job.
pub trait SystemBootstrap {
    type Connection: Send;

    fn connect(&self, input: &str)
    -> impl Future<Output = anyhow::Result<Self::Connection>> + Send;

    fn prepare_inputs(
        &self,
        client: &mut Self::Connection,
        database: &str,
        input: &str,
    ) -> impl Future<Output = anyhow::Result<(String, String)>> + Send;

    fn initialize(
        &self,
        client: &mut Self::Connection,
        migration_input: &str,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;

    fn install_pooler_auth(
        &self,
        pooler_input: &str,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;

    fn finalize(
        &self,
        client: &mut Self::Connection,
        platform_domain: &PlatformDomain,
        max_projects: NonZeroU32,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
}

/// Install the control store and record the platform domain and stream limit.
pub async fn provision_system(
    request: &ProvisionSystemRequest,
    bootstrap: &impl SystemBootstrap,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        request.database.username.as_str() == "postgres"
            && request.database.database == SYSTEM_DATABASE,
        "platform SQL requires the system database's postgres login"
    );
    let database = request.database.to_json()?;
    let mut client = bootstrap
        .connect(&database)
        .await
        .context("connect to the system database")?;
    let (migration_input, pooler_input) = bootstrap
        .prepare_inputs(&mut client, &request.database.database, &database)
        .await?;
    install_system(
        bootstrap,
        &mut client,
        &migration_input,
        &pooler_input,
        &request.platform_domain,
        request.max_projects,
    )
    .await
}

/// Install the control store and Pooler authentication, then grant its surfaces and record its limits.
pub async fn install_system<B: SystemBootstrap>(
    bootstrap: &B,
    client: &mut B::Connection,
    migration_input: &str,
    pooler_input: &str,
    platform_domain: &PlatformDomain,
    max_projects: NonZeroU32,
) -> anyhow::Result<()> {
    bootstrap.initialize(client, migration_input).await?;
    bootstrap.install_pooler_auth(pooler_input).await?;
    bootstrap
        .finalize(client, platform_domain, max_projects)
        .await
}
