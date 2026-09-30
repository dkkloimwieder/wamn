//! The `provision-system` subcommand: install the control store into an empty
//! system database and record the platform domain of the deployment.
//!
//! The database and its owner role `wamn_system` exist before this runs. On
//! CloudNativePG, the `initdb` bootstrap of the cluster creates both. The
//! verb connects as the superuser, installs [`CONTROL_BOOTSTRAP_SQL`] as
//! `wamn_system`, closes the PUBLIC floors, and writes
//! `registry.meta.platform_domain`. It refuses a database that already has
//! the schema `registry`, so a second run changes nothing.

use anyhow::{Context as _, ensure};
use tokio_postgres::{Client, NoTls};
use wamn_control_provision::schema_migrations::MigrationTarget;
use wamn_control_provision::{CONTROL_BOOTSTRAP_SQL, sql, validate_platform_domain};

/// Inputs of one `provision-system` run.
#[derive(Debug)]
pub struct ProvisionSystemRequest {
    /// Superuser URL of the system database.
    pub system_database_url: String,
    /// The email domain of the platform principal rows.
    pub platform_domain: String,
}

/// Install the control store and record the platform domain.
pub async fn provision_system(request: &ProvisionSystemRequest) -> anyhow::Result<()> {
    validate_platform_domain(&request.platform_domain).context("check the platform domain")?;
    let (client, connection) = tokio_postgres::connect(&request.system_database_url, NoTls)
        .await
        .context("connect to the system database")?;
    let connection = tokio::spawn(connection);
    let result = async {
        install_control_store(&client).await?;
        client
            .execute(
                "UPDATE registry.meta SET platform_domain = $1",
                &[&request.platform_domain],
            )
            .await
            .context("record the platform domain")?;
        Ok(())
    }
    .await;
    drop(client);
    let _ = connection.await;
    result
}

/// Install [`CONTROL_BOOTSTRAP_SQL`] as `wamn_system` into the database of
/// `admin`, a superuser connection, and close the PUBLIC floors.
///
/// It refuses when the schema `registry` exists.
pub async fn install_control_store(admin: &Client) -> anyhow::Result<()> {
    let installed: bool = admin
        .query_one(
            "SELECT EXISTS (SELECT FROM pg_catalog.pg_namespace WHERE nspname = 'registry')",
            &[],
        )
        .await
        .context("look for the schema registry")?
        .get(0);
    ensure!(
        !installed,
        "the system database already has the schema registry; provision-system runs once"
    );
    admin
        .batch_execute(&sql::ensure_control_author_acl_role_sql())
        .await
        .context("ensure the portable store's control-author ACL role")?;
    admin
        .batch_execute(sql::ensure_db_owner_role_sql())
        .await
        .context("ensure the database-owner role that the record history grants name")?;
    admin
        .batch_execute(
            "DO $$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_system', \
                                        current_database()); END $$;",
        )
        .await
        .context("give wamn_system the system database")?;
    admin
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("assume the control owner")?;
    for stage in CONTROL_BOOTSTRAP_SQL {
        admin
            .batch_execute(stage)
            .await
            .context("install the control store")?;
    }
    crate::upgrade_schema::record_fresh_install(admin, MigrationTarget::System)
        .await
        .context("record the system migrations that the full schema holds")?;
    admin
        .batch_execute("RESET ROLE")
        .await
        .context("release the control owner before cluster ACL convergence")?;
    admin
        .batch_execute(sql::revoke_public_connect_floor_sql())
        .await
        .context("converge the cluster PUBLIC CONNECT floor")?;
    admin
        .batch_execute(
            "DO $$ BEGIN EXECUTE format(\
               'REVOKE TEMPORARY ON DATABASE %I FROM PUBLIC', current_database()); END $$;",
        )
        .await
        .context("converge the system database PUBLIC TEMPORARY floor")?;
    Ok(())
}
