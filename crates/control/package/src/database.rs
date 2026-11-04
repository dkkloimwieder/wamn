//! Shared PostgreSQL connection adapter.

use anyhow::Context as _;
use tokio_postgres::{Client, NoTls};

pub async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .context("connect to the database")?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok(client)
}
