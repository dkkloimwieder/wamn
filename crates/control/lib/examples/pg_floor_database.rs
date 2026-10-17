//! Install Receiving through the local package applier, without starting a host.
use anyhow::Context as _;
use wamn_control::apply_package::{ApplyPackageRequest, PackageRoot, apply_local_package};
use wamn_control::dev::environment::{ENVIRONMENT, TENANT};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let url = std::env::args().nth(1).context("database URL")?;
    let package =
        std::path::PathBuf::from(std::env::args().nth(2).context("Receiving package root")?);
    let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls).await?;
    tokio::spawn(connection);
    client
        .batch_execute("CREATE ROLE wamn_app NOLOGIN; CREATE ROLE wamn_scenario_author NOLOGIN;")
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
    let marker =
        wamn_runtime::local_application::local_target_marker(TENANT, ENVIRONMENT, row.get(0));
    client
        .batch_execute(&format!(
            "COMMENT ON DATABASE {} IS {}",
            wamn_pg_core::quote_ident(row.get(1)),
            wamn_pg_core::quote_literal(&marker),
        ))
        .await?;
    apply_local_package(
        ApplyPackageRequest {
            package: PackageRoot {
                path: package.clone(),
                artifact_digest: None,
            },
            database_url: url.clone(),
            tenant: TENANT.to_owned(),
        },
        ENVIRONMENT,
    )
    .await?;
    let access = wamn_control::reconcile_package_data_access::prepare_local(&[package])?;
    wamn_control::reconcile_package_data_access::reconcile_local(
        &access,
        &url,
        TENANT,
        ENVIRONMENT,
        true,
    )
    .await?;
    Ok(())
}
