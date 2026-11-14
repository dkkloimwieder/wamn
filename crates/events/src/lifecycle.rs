//! Provision and discard CDC state through authorized connections.
use anyhow::Context as _;
use sqlx::Row as _;
use std::fmt::Write as _;
use std::time::Duration;
use wamn_control_provision::{project_env_cdc_secret_name, sql};
use wamn_registry::Triple;

/// Restore the CDC slot, maps and grants through the project database administrator.
pub async fn apply_cdc_sql(
    admin_database_url: &str,
    capture_database_url: &str,
    schemas: &[String],
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
        wamn_control_provision::sql::create_failover_slot_sql(cdc_name),
    ))
    .execute(&mut capture)
    .await
    .map(drop)
    .context("restore the capture slot")
}

/// Record the CDC reader registration of `triple` in the registry
/// (idempotent + refreshing: recording it again leaves one row). The caller
/// records it last, once the role SQL, the CDC SQL, the publication and the
/// Secret are in place, so a registration names a reader that can start.
/// The supplied credential holds the event-reader registration grants. The capture declarations
/// share `cdc_name`, and `schemas` is the set that the publication covers;
/// the stream and the Secret are named from the coordinate.
///
/// # Errors
///
/// When the system database refuses the connection or the write.
pub async fn record_event_reader(
    system_url: &str,
    triple: &Triple,
    schemas: &[String],
    cdc_name: &str,
    secret_namespace: Option<&str>,
) -> anyhow::Result<()> {
    let schemas = schemas
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    let env = triple.env.as_str();
    let stream = wamn_event_wire::stream_name(&triple.org, &triple.project, env);
    let secret_name = project_env_cdc_secret_name(&triple.org, &triple.project, env);
    let mut client = wamn_postgres::connect(system_url)
        .await
        .context("system db connect")?;

    let result = async {
        sqlx::query(sqlx::AssertSqlSafe((wamn_registry::sql::upsert_event_reader_sql()).to_string())).bind(&triple.org).bind(&triple.project).bind(env).bind(cdc_name).bind(cdc_name).bind(&stream).bind(&secret_name).bind(secret_namespace).bind(true).bind(&schemas).execute(&mut client).await.map(|result| result.rows_affected())
            .context(
                "upsert registry.event_readers row (is the project-env provisioned? \
                 enable-cdc-project-env overlays an existing env — run provision-project-env first)",
            )?;
        Ok(())
    }
    .await;
    drop(client);
    result
}

/// Discard unread changes while the reader and writers are stopped.
pub async fn discard_backlog(admin_url: &str, slot: &str) -> anyhow::Result<()> {
    let mut admin = wamn_postgres::connect(admin_url).await?;
    let boundary: String = sqlx::query_scalar(
        "SELECT pg_logical_emit_message(false, 'wamn.schema.reset', '', true)::text",
    )
    .fetch_one(&mut admin)
    .await?;
    let discarded: bool = sqlx::query_scalar(
        "SELECT end_lsn >= $2::pg_lsn FROM pg_replication_slot_advance($1::name, $2::pg_lsn)",
    )
    .bind(slot)
    .bind(&boundary)
    .fetch_one(&mut admin)
    .await?;
    anyhow::ensure!(
        discarded,
        "CDC slot {slot} did not reach the reset boundary"
    );
    // Slot advancement becomes durable at the next checkpoint, before broker disposal.
    sqlx::raw_sql("SET ROLE pg_checkpoint")
        .execute(&mut admin)
        .await?;
    sqlx::raw_sql("CHECKPOINT").execute(&mut admin).await?;
    sqlx::raw_sql("RESET ROLE").execute(&mut admin).await?;
    Ok(())
}

fn cdc_schema_sql_bundle(schemas: &[String], cdc_name: &str, db_name: &str) -> String {
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
            "\n{schema_guard};\n{entity_map};\n{exclusion_map};\n{grants}",
            schema_guard = sql::ensure_schema_sql(schema),
            entity_map = sql::ensure_entity_map_sql(schema),
            exclusion_map = sql::ensure_cdc_exclusion_map_sql(schema),
            grants = sql::grant_replication_access_sql(db_name, cdc_name, schema),
        )
        .expect("write CDC SQL into a string");
    }
    bundle
}

/// Configuration for a reader whose connection is mounted from its own ConfigMap.
pub fn reader_configuration(
    triple: &Triple,
    cdc_name: &str,
    nats_url: &str,
) -> crate::reader::ReaderConfig {
    crate::reader::ReaderConfig {
        org: triple.org.clone(),
        project: triple.project.clone(),
        env: triple.env.to_string(),
        publication: cdc_name.into(),
        slot: cdc_name.into(),
        stream: wamn_event_wire::stream_name(&triple.org, &triple.project, &triple.env),
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

/// Render a reader with its replication configuration and broker certificate.
pub fn reader_deployment(triple: &Triple, namespace: &str, image: &str) -> serde_json::Value {
    use serde_json::json;
    let name = project_env_cdc_secret_name(&triple.org, &triple.project, &triple.env);
    let project_env_key = wamn_registry::project_env_key(&triple.org, &triple.project, &triple.env);
    let labels = json!({"app.kubernetes.io/name":"wamn-cdc-reader", "app.kubernetes.io/instance":project_env_key});
    json!({
        "apiVersion":"apps/v1", "kind":"Deployment",
        "metadata":{"name":name,"namespace":namespace,"annotations":{"wamn.environment":triple.to_string()}},
        "spec":{"replicas":1,"strategy":{"type":"Recreate"},"selector":{"matchLabels":labels},
            "template":{"metadata":{"labels":labels},"spec":{
                "automountServiceAccountToken":true,
                "serviceAccountName":wamn_registry::service_account_name(&triple.org,&triple.project,&triple.env),
                "containers":[{"name":"reader","image":image,"command":["/usr/local/bin/wamn-cdc-reader"],
                    "args":["--config","/etc/wamn/cdc/reader.json"],
                    "volumeMounts":[{"name":"cdc","mountPath":"/etc/wamn/cdc","readOnly":true},
                        {"name":"nats","mountPath":"/etc/wamn/nats","readOnly":true}]
                }],"volumes":[{"name":"cdc","configMap":{"name":name}},
                    {"name":"nats","secret":{"secretName":"wamn-cdc-reader-nats"}}]
            }}
        }
    })
}

/// Stop the target reader without touching another environment's process or broker state.
pub async fn stop_reader(
    kubeconfig: &std::path::Path,
    context: &str,
    namespace: &str,
    triple: &Triple,
) -> anyhow::Result<()> {
    use wamn_kubernetes::kubectl::kubectl;
    let name = project_env_cdc_secret_name(&triple.org, &triple.project, &triple.env);
    let deployment: Option<serde_json::Value> =
        wamn_kubernetes::kubectl::get(kubeconfig, context, namespace, "deployment", &name).await?;
    if deployment.is_none() {
        return Ok(());
    }
    kubectl(
        kubeconfig,
        context,
        namespace,
        &["scale", "deployment", &name, "--replicas=0"],
        None,
    )
    .await?;
    wait_reader_stopped(kubeconfig, context, namespace, triple).await
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
        wamn_control_provision::project_env_key(&triple.org, &triple.project, &triple.env);
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

/// Start the reader and wait until its Deployment is ready.
pub async fn start_reader(
    kubeconfig: &std::path::Path,
    context: &str,
    namespace: &str,
    triple: &Triple,
) -> anyhow::Result<()> {
    use wamn_kubernetes::kubectl::kubectl;
    let name = project_env_cdc_secret_name(&triple.org, &triple.project, &triple.env);
    kubectl(
        kubeconfig,
        context,
        namespace,
        &["scale", "deployment", &name, "--replicas=1"],
        None,
    )
    .await?;
    kubectl(
        kubeconfig,
        context,
        namespace,
        &[
            "rollout",
            "status",
            &format!("deployment/{name}"),
            "--timeout=120s",
        ],
        None,
    )
    .await?;
    Ok(())
}
