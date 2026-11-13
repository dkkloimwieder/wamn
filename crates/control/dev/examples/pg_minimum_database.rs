//! Install Receiving through the local package applier, without starting a host.
use anyhow::Context as _;
use wamn_dev_control::dev::environment::{ENVIRONMENT, PROJECT_ENV_KEY};
use wamn_package_control::apply_package::{ApplyPackageRequest, PackageRoot, apply_local_package};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let url = std::env::args().nth(1).context("database URL")?;
    let package =
        std::path::PathBuf::from(std::env::args().nth(2).context("Receiving package root")?);
    let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls).await?;
    tokio::spawn(connection);
    // A shared server, such as the GCP minimum, already has both roles.
    client
        .batch_execute(
            "DO $roles$ BEGIN
               IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_app') THEN
                 CREATE ROLE wamn_app NOLOGIN;
               END IF;

             END $roles$;",
        )
        .await?;
    client
        .batch_execute(wamn_control_provision::sql::ensure_db_owner_role_sql())
        .await?;
    client.batch_execute("DO $$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()); END $$;").await?;
    client
        .batch_execute(wamn_catalog::CATALOG_SCHEMA_SQL)
        .await?;
    client
        .batch_execute(wamn_control_provision::APP_SCHEMA_SQL)
        .await?;
    let row = client
        .query_one(
            "SELECT oid, datname FROM pg_database WHERE datname = current_database()",
            &[],
        )
        .await?;
    let marker = wamn_runtime::local_application::local_target_marker(
        PROJECT_ENV_KEY,
        ENVIRONMENT,
        row.get(0),
    );
    client
        .batch_execute(&format!(
            "COMMENT ON DATABASE {} IS {}",
            postgres_protocol::escape::escape_identifier(row.get(1)),
            postgres_protocol::escape::escape_literal(&marker),
        ))
        .await?;
    apply_local_package(
        ApplyPackageRequest {
            component_source: wamn_package_control::apply_package::component_source_from_env(None)
                .unwrap(),
            changes: wamn_broker::Publisher::default(),
            package: PackageRoot {
                path: package.clone(),
                artifact_digest: None,
            },
            database_url: url.clone(),
            org: wamn_dev_control::dev::environment::ORG.to_owned(),
            project: wamn_dev_control::dev::environment::PROJECT.to_owned(),
            environment: ENVIRONMENT.to_owned(),
        },
        ENVIRONMENT,
    )
    .await?;
    Ok(())
}
