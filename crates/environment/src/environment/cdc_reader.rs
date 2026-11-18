//! Provision and discard CDC state through authorized connections.
use anyhow::Context as _;
use sqlx::Row as _;
use std::fmt::Write as _;
use std::time::Duration;
use wamn_event::reader::ReaderConfig;
use wamn_provision::{project_env_cdc_secret_name, sql};
use wamn_registry::Triple;

/// Restore the CDC slot, maps and grants after package SQL creates the model schemas.
pub async fn apply_cdc_sql(
    admin_database_url: &str,
    capture_database_url: &str,
    schemas: &[wamn_catalog::canonical_name::CanonicalName],
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
    sqlx::raw_sql(sqlx::AssertSqlSafe(cdc_schema_sql_bundle(
        schemas, cdc_name, database,
    )))
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
    schemas: &[wamn_catalog::canonical_name::CanonicalName],
    cdc_name: &str,
    db_name: &str,
) -> String {
    let mut bundle = String::new();
    if schemas.is_empty() {
        writeln!(
            bundle,
            "\nGRANT CONNECT ON DATABASE {} TO {};",
            postgres_protocol::escape::escape_identifier(db_name),
            postgres_protocol::escape::escape_identifier(cdc_name)
        )
        .expect("write the empty capture grant");
    }
    for schema in schemas {
        writeln!(
            bundle,
            "\n{entity_map};\n{exclusion_map};\n{grants}",
            entity_map = sql::ensure_entity_map_sql(schema.as_str()),
            exclusion_map = sql::ensure_cdc_exclusion_map_sql(schema.as_str()),
            grants = sql::grant_replication_access_sql(db_name, cdc_name, schema.as_str()),
        )
        .expect("write CDC SQL into a string");
    }
    bundle
}

/// Configuration for a reader whose connection is mounted from its own ConfigMap.
pub fn reader_configuration(triple: &Triple, cdc_name: &str, nats_url: &str) -> ReaderConfig {
    ReaderConfig {
        org: triple.org.clone(),
        project: triple.project.clone(),
        env: triple.env.clone(),
        publication: cdc_name.into(),
        slot: cdc_name.into(),
        stream: wamn_event::stream_name(&triple.org, &triple.project, &triple.env),
        cdc_url: "/etc/wamn/cdc/connection.json".into(),
        nats_url: nats_url.into(),
        nats_tls_ca: Some("/etc/wamn/nats/ca.crt".into()),
        nats_tls_cert: Some("/etc/wamn/nats/tls.crt".into()),
        nats_tls_key: Some("/etc/wamn/nats/tls.key".into()),
        feedback_secs: 5,
        stall_threshold_secs: 30,
        slot_poll_secs: 30,
        slot_safe_wal_warn_bytes: 268_435_456,
    }
}

/// Wait for reader pods, including pods left by an interrupted Deployment delete.
pub async fn wait_reader_stopped(
    kubeconfig: &std::path::Path,
    context: &str,
    namespace: &str,
    triple: &Triple,
) -> anyhow::Result<()> {
    use wamn_kubernetes::kubectl::kubectl;
    let project_env_key =
        wamn_provision::project_env_key(&triple.org, &triple.project, &triple.env);
    let selector = format!(
        "app.kubernetes.io/name=wamn-cdc-reader,app.kubernetes.io/instance={project_env_key}"
    );
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let pods: serde_json::Value = serde_json::from_str(
                &kubectl(
                    kubeconfig,
                    context,
                    namespace,
                    &["get", "pods", "-l", &selector, "-o", "json"],
                    None,
                )
                .await?,
            )?;
            if pods["items"].as_array().context("reader pods")?.is_empty() {
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
    let name = project_env_cdc_secret_name(&triple.org, &triple.project, &triple.env);
    wamn_kubernetes::kubectl::wait_deployment_ready(target, &name, Duration::from_secs(120)).await
}
