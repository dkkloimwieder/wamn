//! Install Receiving through the local package applier, without starting a host.
use anyhow::Context as _;
use wamn_dev_control::dev::environment::{ENVIRONMENT, PROJECT_ENV_KEY};
use wamn_package_control::apply_package::{ApplyPackageRequest, PackageRoot, apply_local_package};

use sqlx::{Connection as _, Row as _, Statement as _};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let url = std::env::args().nth(1).context("database URL")?;
    let package =
        std::path::PathBuf::from(std::env::args().nth(2).context("Receiving package root")?);
    let mut client = wamn_postgres::connect(&url).await?;

    // A shared server, such as the GCP minimum, already has both roles.
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        ("DO $roles$ BEGIN
               IF NOT EXISTS (SELECT FROM pg_catalog.pg_roles WHERE rolname = 'wamn_app') THEN
                 CREATE ROLE wamn_app NOLOGIN;
               END IF;

             END $roles$;")
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (wamn_control_provision::sql::ensure_db_owner_role_sql()).to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)?;
    sqlx::raw_sql("DO $$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_db_owner', current_database()); END $$;").execute(&mut client).await.map(drop)?;
    sqlx::raw_sql(wamn_catalog::CATALOG_SCHEMA_SQL)
        .execute(&mut client)
        .await
        .map(drop)?;
    sqlx::raw_sql(wamn_control_provision::APP_SCHEMA_SQL)
        .execute(&mut client)
        .await
        .map(drop)?;
    let row =
        sqlx::query("SELECT oid, datname FROM pg_database WHERE datname = current_database()")
            .fetch_one(&mut client)
            .await?;
    let marker = wamn_runtime::local_application::local_target_marker(
        PROJECT_ENV_KEY,
        ENVIRONMENT,
        row.get::<sqlx::postgres::types::Oid, _>(0).0,
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        (&format!(
            "COMMENT ON DATABASE {} IS {}",
            postgres_protocol::escape::escape_identifier(row.get(1)),
            postgres_protocol::escape::escape_literal(&marker),
        ))
            .to_string(),
    ))
    .execute(&mut client)
    .await
    .map(drop)?;
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
    let access =
        wamn_environment_control::reconcile_package_data_access::prepare_local(&[package])?;
    wamn_environment_control::reconcile_package_data_access::reconcile_local(
        &access,
        &url,
        PROJECT_ENV_KEY,
        ENVIRONMENT,
        true,
    )
    .await?;
    Ok(())
}
