//! Provision capture with one CNPG-managed replication login and native credentials.

use sqlx::Row as _;
pub(super) mod plan;
pub use plan::cdc_sql_bundle;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context as _;
use serde_json::Value;

use wamn_control_provision::{
    cdc_object_name, project_env_cdc_secret_name, project_env_database_name, sql,
    validate_project_env_cdc,
};
use wamn_registry::Triple;

use crate::provision_project_env::write_output;

/// Inputs of one CDC overlay onto a provisioned project-env.
///
/// An artifact path that is absent or `-` is not written; the outcome carries
/// every rendered artifact.
#[derive(Debug)]
pub struct EnableCdcProjectEnvRequest {
    /// Org id (the project-env must already be provisioned and recorded).
    pub org: String,

    /// Project id.
    pub project: String,

    /// Environment slug.
    pub env: String,

    /// The application data schemas that the publication covers.
    pub schemas: Vec<String>,

    /// Superuser Postgres URL to the T1 system DB (`wamn_system`): derive the
    /// target cluster, resolve the stored project-env instance suffix, and record
    /// the `registry.event_readers` registration.
    pub system_database_url: Option<String>,

    /// Target CNPG `Cluster` name. When absent, it is derived from the org's
    /// placement and the recorded project-env policy in the registry.
    pub cluster: Option<String>,

    /// Password for the per-project-env replication role (embedded in the
    /// rendered URL + role SQL).
    pub replication_password: String,

    /// Host the reader reaches the project-env database at. Defaults to the
    /// Service `<cluster>-rw.<cluster_namespace>.svc.cluster.local`.
    pub db_host: Option<String>,

    /// Port the reader reaches the database at.
    pub db_port: u16,

    /// Namespace the rendered `Secret` is applied to.
    pub namespace: String,

    /// Namespace of the CNPG `Cluster`, which the `Publication` CR shares.
    pub cluster_namespace: String,

    /// Secret namespace to RECORD in the registration's replication `SecretRef`.
    /// Absent records `NULL` (the resolving service's own namespace).
    pub secret_namespace: Option<String>,

    /// Exact environment source stream. Must match the declared coordinates.
    pub stream: Option<String>,

    /// The NATS of the event streams, with the provisioning certificate.
    pub nats: wamn_broker::nats::PlatformNats,

    /// NATS stream copies, separate from workload instances.
    pub stream_replicas: usize,

    /// Declared duplicate detection window in seconds.
    pub dup_window_secs: u64,

    /// Native NATS pull consumer configurations as JSON.
    pub consumer_config: Vec<String>,

    /// Write the CDC SQL here.
    pub emit_cdc_sql: Option<PathBuf>,

    /// Write the CNPG `Publication` CR (JSON) here.
    pub emit_publication: Option<PathBuf>,

    /// Write the replication-credential `Secret` (JSON) here.
    pub emit_secret: Option<PathBuf>,
}

/// The names and rendered artifacts of one CDC overlay.
#[derive(Debug)]
pub struct EnableCdcProjectEnvOutcome {
    pub triple: Triple,
    /// The shared publication, slot, and replication role name.
    pub cdc_name: String,
    pub cluster: String,
    pub stream: String,
    pub secret_name: String,
    pub cdc_sql: String,
    /// The CNPG `Publication` CR, which the operator turns into the publication.
    pub publication: Value,
    pub secret: Value,
}

/// Provision the declared broker objects and write the requested CDC
/// artifacts. The caller applies them, then records the CDC reader with
/// [`record_event_reader`].
pub async fn enable_cdc_project_env(
    args: &EnableCdcProjectEnvRequest,
) -> anyhow::Result<EnableCdcProjectEnvOutcome> {
    let triple = Triple::new(&args.org, args.env.as_str(), &args.project);
    let stream = wamn_event_wire::stream_name(&args.org, &args.project, &args.env);
    if args
        .stream
        .as_ref()
        .is_some_and(|requested| requested != &stream)
    {
        anyhow::bail!("--stream must match this environment's source stream {stream}");
    }

    // Validate the names (the base project-env rules + the assembled
    // `wamn_cdc_…` object name's 63-byte bound) before any effect.
    validate_project_env_cdc(&args.org, &args.project, &args.env)
        .map_err(|e| anyhow::anyhow!("cdc names: {e}"))?;
    if args
        .schemas
        .iter()
        .any(|schema| !crate::ident::is_bare_ident(schema))
    {
        anyhow::bail!(
            "--schema must be a bare lowercase identifier, got {:?}",
            args.schemas
        );
    }

    let consumers = args
        .consumer_config
        .iter()
        .map(|json| {
            serde_json::from_str::<async_nats::jetstream::consumer::pull::Config>(json)
                .context("parse the declared native consumer configuration")
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let duplicate_window = Duration::from_secs(args.dup_window_secs);
    wamn_broker::streams::validate_inputs(
        &triple,
        args.stream_replicas,
        duplicate_window,
        &consumers,
    )?;
    let system_url = args
        .system_database_url
        .as_deref()
        .context("--system-database-url is required to resolve the stored instance suffix")?;
    let instance =
        crate::provision_project_env::read_project_env_instance(system_url, &triple).await?;

    // Pick the target cluster: an explicit `--cluster` wins; otherwise derive it
    // from the org's placement and the recorded project-env policy (`cluster_of`).
    let cluster = if let Some(cluster) = &args.cluster {
        cluster.clone()
    } else {
        let policy =
            crate::provision_project_env::read_project_env_policy(system_url, &triple).await?;
        crate::provision_project_env::resolve_cluster(system_url, &args.org, &policy).await?
    };

    let db_name = project_env_database_name(&args.org, &args.project, &args.env, &instance);
    let cdc_name = cdc_object_name(&args.org, &args.project, &args.env, &instance);
    let secret_name = project_env_cdc_secret_name(&args.org, &args.project, &args.env);
    let _db_host = crate::provision_project_env::database_host(
        args.db_host.as_deref(),
        &cluster,
        &args.cluster_namespace,
    );

    let broker = async_nats::jetstream::new(
        args.nats
            .connect()
            .await
            .context("connect the event provisioning credential")?,
    );
    wamn_broker::streams::provision(
        &broker,
        &triple,
        args.stream_replicas,
        duplicate_window,
        &consumers,
    )
    .await?;

    // Render the artifacts the runbook applies.

    let cdc_sql = cdc_sql_bundle(&args.schemas, &cdc_name, &db_name);
    let publication = wamn_platform_install::environment::publication_values(
        &triple,
        &instance,
        &cluster,
        &cdc_name,
        &args.schemas,
    );
    let secret_doc = serde_json::to_value(wamn_kubernetes::secret::Secret::basic_auth(
        &args.org,
        &secret_name,
        &cdc_name,
        &args.replication_password,
    ))?;

    anyhow::ensure!(
        args.namespace == args.org && args.cluster_namespace == args.org,
        "database resources must share the org namespace"
    );
    let kubeconfig = std::env::var_os("KUBECONFIG")
        .map(PathBuf::from)
        .unwrap_or_default();
    let native_secret = wamn_kubernetes::secret::Secret::basic_auth(
        &args.org,
        &secret_name,
        &cdc_name,
        &args.replication_password,
    );
    wamn_kubernetes::secret::update(&kubeconfig, "", &args.org, &secret_name, |secret| {
        if secret.metadata.resource_version.is_none() {
            *secret = native_secret.clone();
        } else {
            anyhow::ensure!(
                secret.data == native_secret.data,
                "replication credential differs from its native Secret"
            );
        }
        anyhow::ensure!(
            secret.content_type == "kubernetes.io/basic-auth",
            "old database Secret shape refused"
        );
        Ok(())
    })
    .await?;
    crate::provision_project_env::workload::reconcile(
        system_url,
        &kubeconfig,
        "",
        &args.org,
        &cluster,
        &wamn_control_provision::connection::managed_replication_role(
            cdc_name.clone(),
            secret_name.clone(),
        ),
    )
    .await?;
    write_output(args.emit_cdc_sql.as_deref(), &cdc_sql)?;
    write_output(
        args.emit_publication.as_deref(),
        &serde_json::to_string_pretty(&publication)?,
    )?;
    write_output(
        args.emit_secret.as_deref(),
        &serde_json::to_string_pretty(&secret_doc)?,
    )?;

    Ok(EnableCdcProjectEnvOutcome {
        triple,
        cdc_name,
        cluster,
        stream,
        secret_name,
        cdc_sql,
        publication,
        secret: secret_doc,
    })
}

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
    sqlx::raw_sql(sqlx::AssertSqlSafe(plan::cdc_schema_sql_bundle(
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

/// Stop this environment's CDC publication before acquiring reset exclusion.
/// A reader tests this registration while holding the shared lifecycle key.
pub async fn pause_event_reader(system_url: &str, triple: &Triple) -> anyhow::Result<()> {
    let mut client = wamn_postgres::connect(system_url).await?;
    let mut tx = crate::provision_project_env::provisioning_transaction(&mut client).await?;
    sqlx::query(
        wamn_registry::sql::CONTROL_ENVIRONMENT_SRC_ENABLE_CDC_PROJECT_ENV_PAUSE_EVENT_READER,
    )
    .bind(&triple.org)
    .bind(&triple.project)
    .bind(triple.env.as_str())
    .execute(&mut *tx)
    .await
    .map(|result| result.rows_affected())?;
    tx.commit().await?;
    Ok(())
}

/// Render the existing CDC reader binary with this environment's owned credentials.
pub fn reader_deployment(
    triple: &Triple,
    namespace: &str,
    image: &str,
    replicas: usize,
    duplicate_window_secs: u64,
) -> serde_json::Value {
    use serde_json::json;

    let name = project_env_cdc_secret_name(&triple.org, &triple.project, &triple.env);
    let project_env_key =
        wamn_control_provision::project_env_key(&triple.org, &triple.project, &triple.env);
    let labels = json!({"app.kubernetes.io/name":"wamn-cdc-reader", "app.kubernetes.io/instance":project_env_key});
    json!({
        "apiVersion":"apps/v1", "kind":"Deployment",
        "metadata":{"name":name,"namespace":namespace,"annotations":{"wamn.environment":triple.to_string()}},
        "spec":{"replicas":1,"strategy":{"type":"Recreate"},"selector":{"matchLabels":labels},
            "template":{"metadata":{"labels":labels},"spec":{
                "automountServiceAccountToken":true,"serviceAccountName":wamn_registry::service_account_name(&triple.org,&triple.project,&triple.env),
                "containers":[{"name":"reader","image":image,"command":["/usr/local/bin/wamn-cdc-reader"],
                    "args":["--org",triple.org,"--project",triple.project,"--env",triple.env,
                        "--stream-replicas",replicas.to_string(),"--dup-window-secs",duplicate_window_secs.to_string()],
                    "env":[
                        {"name":"WAMN_SYSTEM_URL","valueFrom":{"configMapKeyRef":{"name":wamn_control_provision::project_env_secret_name(&triple.org,&triple.project,&triple.env),"key":"registry-reader.json"}}},
                        {"name":"WAMN_CDC_URL","valueFrom":{"configMapKeyRef":{"name":name,"key":"connection.json"}}},
                        {"name":"WAMN_NATS_URL","value":crate::platform::identity::NATS_URL},
                        {"name":"WAMN_NATS_TLS_CA","value":"/etc/wamn/nats/ca.crt"},
                        {"name":"WAMN_NATS_TLS_CERT","value":"/etc/wamn/nats/tls.crt"},
                        {"name":"WAMN_NATS_TLS_KEY","value":"/etc/wamn/nats/tls.key"}
                    ],"volumeMounts":[{"name":"nats","mountPath":"/etc/wamn/nats","readOnly":true}]
                }],"volumes":[{"name":"nats","secret":{"secretName":"wamn-cdc-reader-nats"}}]
            }}
        }
    })
}

/// Stop the target reader without touching another environment's process or broker state.
pub(crate) async fn stop_reader(
    platform: &crate::environment::Platform,
    triple: &Triple,
) -> anyhow::Result<()> {
    use crate::environment::ensure::kubectl;
    let name = project_env_cdc_secret_name(&triple.org, &triple.project, &triple.env);
    let deployment: Option<serde_json::Value> = wamn_kubernetes::kubectl::get(
        &platform.target.kubeconfig,
        &platform.target.context,
        &platform.target.namespace,
        "deployment",
        &name,
    )
    .await?;
    if deployment.is_none() {
        return Ok(());
    }
    kubectl(
        platform,
        &platform.target.namespace,
        &["scale", "deployment", &name, "--replicas=0"],
        None,
    )
    .await?;
    wait_reader_stopped(platform, triple).await
}

/// Discard unread changes while the environment's reader and writers are stopped.
pub(crate) async fn discard_backlog(
    system_url: &str,
    admin_url: &str,
    triple: &Triple,
) -> anyhow::Result<()> {
    let instance =
        crate::provision_project_env::read_project_env_instance(system_url, triple).await?;
    let slot = cdc_object_name(&triple.org, &triple.project, &triple.env, &instance);
    let mut admin = wamn_postgres::connect(admin_url).await?;
    let boundary: String = sqlx::query_scalar(
        "SELECT pg_logical_emit_message(false, 'wamn.schema.reset', '', true)::text",
    )
    .fetch_one(&mut admin)
    .await?;
    let discarded: bool = sqlx::query_scalar(
        "SELECT end_lsn >= $2::pg_lsn FROM pg_replication_slot_advance($1::name, $2::pg_lsn)",
    )
    .bind(&slot)
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

/// Wait for reader pods, including pods left by an interrupted Deployment delete.
pub(crate) async fn wait_reader_stopped(
    platform: &crate::environment::Platform,
    triple: &Triple,
) -> anyhow::Result<()> {
    use crate::environment::ensure::kubectl;
    let project_env_key =
        wamn_control_provision::project_env_key(&triple.org, &triple.project, &triple.env);
    let selector = format!(
        "app.kubernetes.io/name=wamn-cdc-reader,app.kubernetes.io/instance={project_env_key}"
    );
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            let pods: serde_json::Value = serde_json::from_str(
                &kubectl(
                    platform,
                    &platform.target.namespace,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cdc_names_are_validated_before_any_effect() {
        // A reserved / non-slug project id fails without touching the registry.
        assert!(validate_project_env_cdc("acme", "wamn-x", "dev").is_err());
        assert!(validate_project_env_cdc("acme", "Bad", "prod").is_err());
        assert!(validate_project_env_cdc("acme", "billing", "prod").is_ok());
        // The publication schema must be a bare identifier (defense-in-depth on
        // top of the builders' quoting).
        assert!(crate::ident::is_bare_ident("app_data"));
        assert!(!crate::ident::is_bare_ident("app;DROP"));
    }
}
