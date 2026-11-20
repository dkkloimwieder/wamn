//! Provision and discard CDC state through authorized connections.
use wamn_kubernetes::client::wait_named;
use anyhow::Context as _;
use k8s_openapi::api::{apps::v1::Deployment, core::v1::Pod};
use kube::{Api, api::ListParams};
use sqlx::Row as _;
use std::fmt::Write as _;
use std::time::Duration;
use wamn_provision::{cdc_reader_name, sql};
use wamn_registry::Triple;

/// Restore the CDC slot, maps and grants after package SQL creates the model schemas.
pub async fn apply_cdc_sql(
    admin_database_url: &str,
    capture_database_url: &str,
    schemas: &[wamn_package::canonical_name::CanonicalName],
    cdc_name: &str,
    database: &str,
) -> anyhow::Result<()> {
    let mut client = wamn_postgres::connect(admin_database_url).await?;
    let connected: String = sqlx::query("SELECT current_database()")
        .fetch_one(&mut client)
        .await?
        .get(0);
    anyhow::ensure!(
        connected == database,
        "CDC SQL targets a different database"
    );
    let login = wamn_role::CDC_LOGIN.parse()?;
    wamn_platform_install::credentials::wait_role(&mut client, &login).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(cdc_schema_sql_bundle(schemas, database)))
    .execute(&mut client)
    .await
    .map(drop)
    .context("restore CDC maps and grants")?;
    let mut capture = wamn_postgres::connect(capture_database_url).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        wamn_provision::sql::create_failover_slot_sql(cdc_name),
    ))
    .execute(&mut capture)
    .await
    .map(drop)
    .context("restore the capture slot")
}

fn cdc_schema_sql_bundle(
    schemas: &[wamn_package::canonical_name::CanonicalName],
    db_name: &str,
) -> String {
    let mut bundle = String::new();
    if schemas.is_empty() {
        writeln!(
            bundle,
            "\nGRANT CONNECT ON DATABASE {} TO {};",
            postgres_protocol::escape::escape_identifier(db_name),
            postgres_protocol::escape::escape_identifier(wamn_role::CDC_LOGIN)
        )
        .expect("write the empty capture grant");
    }
    for schema in schemas {
        writeln!(
            bundle,
            "\n{entity_map};\n{exclusion_map};\n{grants}",
            entity_map = sql::ensure_entity_map_sql(schema.as_str()),
            exclusion_map = sql::ensure_cdc_exclusion_map_sql(schema.as_str()),
            grants = sql::grant_replication_access_sql(db_name, wamn_role::CDC_LOGIN, schema.as_str()),
        )
        .expect("write CDC SQL into a string");
    }
    bundle
}

/// Wait for reader pods, including pods left by an interrupted Deployment delete.
pub async fn wait_reader_stopped(
    target: &wamn_kubernetes::Target,
    triple: &Triple,
) -> anyhow::Result<()> {
    let project_env_key =
        wamn_provision::project_env_key(&triple.org, &triple.project, &triple.env);
    let selector = format!(
        "app.kubernetes.io/name=wamn-cdc-reader,app.kubernetes.io/instance={project_env_key}"
    );
    let pods = Api::<Pod>::namespaced(target.client.client(), &target.namespace);
    let params = ListParams::default().labels(&selector);
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            if pods.list(&params).await?.items.is_empty() {
                break Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("wait for the old CDC reader to stop")??;
    Ok(())
}

pub(super) async fn wait_reader_ready(
    target: &wamn_kubernetes::Target,
    triple: &Triple,
) -> anyhow::Result<()> {
    let name = cdc_reader_name(&triple.org, &triple.project, &triple.env);
    let deployments = Api::<Deployment>::namespaced(target.client.client(), &target.namespace);
    wait_named(&deployments, &name, Duration::from_secs(120), |object| {
        Ok(object
            .filter(|deployment| {
                let Some(status) = &deployment.status else {
                    return false;
                };
                let desired = deployment
                    .spec
                    .as_ref()
                    .and_then(|spec| spec.replicas)
                    .unwrap_or(1);
                let updated = status.updated_replicas.unwrap_or(0);
                deployment
                    .metadata
                    .generation
                    .zip(status.observed_generation)
                    .is_some_and(|(generation, observed)| generation <= observed)
                    && updated >= desired
                    && status.replicas.unwrap_or(0) <= updated
                    && status.available_replicas.unwrap_or(0) >= updated
            })
            .map(drop))
    })
    .await
}
