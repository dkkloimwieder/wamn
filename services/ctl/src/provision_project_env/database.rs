//! PostgreSQL connection configuration for workload credentials.

use std::str::FromStr as _;

use anyhow::Context as _;

use super::{NoTls, PgConfig, Url};

pub(super) fn exact_project_database_config(admin_url: &str, database: &str) -> anyhow::Result<PgConfig> {
    let config = PgConfig::from_str(admin_url).context("parse target admin database URL")?;
    anyhow::ensure!(
        config.get_dbname() == Some(database),
        "--target-admin-database-url must name the exact project database"
    );
    Ok(config)
}

pub(super) fn named_database_config(admin_url: &str, purpose: &str) -> anyhow::Result<PgConfig> {
    let config = PgConfig::from_str(admin_url).with_context(|| format!("parse {purpose} URL"))?;
    anyhow::ensure!(
        config
            .get_dbname()
            .is_some_and(|database| !database.is_empty()),
        "{purpose} URL must name the exact database"
    );
    Ok(config)
}

pub(super) fn workload_config(admin: &PgConfig, role: &str, password: &str, database: &str) -> PgConfig {
    let mut config = admin.clone();
    config.user(role);
    config.password(password);
    config.dbname(database);
    config
}

pub(super) fn workload_url(
    admin_url: &str,
    role: &str,
    password: &str,
    database: &str,
) -> anyhow::Result<String> {
    let mut url = Url::parse(admin_url).context("parse target admin URL for credential")?;
    anyhow::ensure!(
        matches!(url.scheme(), "postgres" | "postgresql"),
        "target admin URL must use postgres or postgresql"
    );
    url.set_username(role)
        .map_err(|_| anyhow::anyhow!("set workload URL username"))?;
    url.set_password(Some(password))
        .map_err(|_| anyhow::anyhow!("set workload URL password"))?;
    url.set_path(&format!("/{database}"));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.into())
}

pub(super) async fn connect_config(
    config: &PgConfig,
    purpose: &str,
) -> anyhow::Result<(
    tokio_postgres::Client,
    tokio::task::JoinHandle<Result<(), tokio_postgres::Error>>,
)> {
    let (client, connection) = config
        .connect(NoTls)
        .await
        .with_context(|| format!("connect {purpose}"))?;
    Ok((client, tokio::spawn(connection)))
}
