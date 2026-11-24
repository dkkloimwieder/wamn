//! System and project databases installed by the product installers.

use crate::containers::Postgres;

/// Install the control store in a new `wamn_system` database and return its administrator URL.
pub async fn system(postgres: &Postgres) -> url::Url {
    postgres.create_system_role().await.unwrap();
    let url = postgres.create_database("wamn_system").await.unwrap();
    let mut admin = wamn_postgres::connect(url.as_str()).await.unwrap();
    wamn_platform_install::provision_system::install_control_store(&mut admin)
        .await
        .unwrap();
    url
}

/// Install the project schemas in a new database `name` and return its administrator URL.
pub async fn project(postgres: &Postgres, name: &str) -> url::Url {
    let migration = postgres.create_project_database(name).await.unwrap();
    let mut owner = wamn_postgres::connect(migration.as_str()).await.unwrap();
    wamn_environment::platform_migrations::install_project(&mut owner)
        .await
        .unwrap();
    let mut url = url::Url::parse(postgres.admin_url()).unwrap();
    url.set_path(name);
    url
}

/// Begin a fixture write under its principal and operation.
pub async fn transaction<'connection>(
    connection: &'connection mut sqlx::PgConnection,
    principal: &wamn_identity_protocol::PrincipalId,
    operation: &str,
) -> Result<sqlx::Transaction<'connection, sqlx::Postgres>, sqlx::Error> {
    let mut transaction = sqlx::Connection::begin(connection).await?;
    wamn_postgres::bind_transaction_identity(
        &mut transaction,
        principal,
        "",
        "fixture",
        Some(operation),
        None,
    )
    .await?;
    Ok(transaction)
}
