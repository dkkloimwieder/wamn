//! The `enable-cdc-project-env` subcommand (wamn-l5i9.9, D19 v3 §4): overlay
//! CDC **capture** onto an ALREADY-provisioned project-env. CDC is opt-in and
//! may be enabled long after provisioning, so it is its own overlay rather than
//! a `provision-project-env` flag.
//!
//! Provisioning creates the declared broker objects and records the CDC reader.
//! The runbook applies the emitted PostgreSQL and Kubernetes files in this order:
//!
//! 1. apply the emitted **replication-role SQL** to the target cluster's
//!    superuser (any database — roles are cluster-global);
//! 2. apply the emitted **CDC SQL** connected to the PROJECT-ENV database
//!    (publications and logical slots are database-bound): the eager schema
//!    guard, the publication (`FOR TABLES IN SCHEMA` — auto-includes tables
//!    catalog-publish creates later), the **failover-enabled slot** (WAL is
//!    pinned from here — capture starts at CDC-enable, bounded by
//!    `max_slot_wal_keep_size`), and the role's grants;
//! 3. `kubectl apply -f` the emitted **replication-credential Secret**.
//!
//! This command also creates the declared source and advisory streams and
//! materializer consumers with explicit event provisioning credentials.
//! Existing broker objects must match their complete declarations.
//!
//! What this tool does directly (given `--system-database-url`): derive the
//! target cluster from the org's placement, and record the
//! `registry.event_readers` registration (as the `wamn_system` owner). The
//! registration's project-env FK makes the overlay ordering structural: an
//! unprovisioned env is rejected.
//!
//! One shared name serves the publication, the slot, and the role
//! (`wamn_cdc_<org>__<project>__<env>__<instance>` — underscored, a slot admits only
//! `[a-z0-9_]`); the Secret keeps the hyphenated convention
//! (`wamn-cdc-<org>--<project>--<env>`). The replication credential is its own
//! R8b tier — distinct from the `wamn_app` query credential and the dispatch
//! role. NOTE Postgres `REPLICATION` is cluster-wide: on a shared pool,
//! input-side isolation rests on handing each reader only its own
//! slot/publication/credentials. Separate environment streams and broker
//! credentials limit event access. Regulated tiers use dedicated clusters.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context as _;
use serde_json::Value;
use tokio_postgres::NoTls;

use wamn_control_provision::{
    cdc_object_name, compose_url, event_stream_name, project_env_cdc_secret_name,
    project_env_database_name, render_project_env_cdc_secret_manifest, sql,
    validate_project_env_cdc,
};
use wamn_control_registry::Triple;

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

    /// The application DATA schema the publication covers (not the
    /// `app_system` auth schema).
    pub schema: String,

    /// Superuser Postgres URL to the T1 system DB (`wamn_system`): derive the
    /// target cluster, resolve the stored project-env instance suffix, and record
    /// the `registry.event_readers` registration.
    pub system_database_url: Option<String>,

    /// Target CNPG `Cluster` name. When absent, it is derived from the org's
    /// placement in the registry.
    pub cluster: Option<String>,

    /// Password for the per-project-env replication role (embedded in the
    /// rendered URL + role SQL).
    pub replication_password: String,

    /// Host the reader reaches the project-env database at. Defaults to the
    /// target cluster's read-write service `<cluster>-rw`.
    pub db_host: Option<String>,

    /// Port the reader reaches the database at.
    pub db_port: u16,

    /// Namespace the rendered `Secret` is applied to.
    pub namespace: String,

    /// Secret namespace to RECORD in the registration's replication `SecretRef`.
    /// Absent records `NULL` (the resolving service's own namespace).
    pub secret_namespace: Option<String>,

    /// Exact environment source stream. Must match the declared coordinates.
    pub stream: Option<String>,

    /// Event broker managed by this environment's provisioning credential.
    pub nats_url: String,

    /// Provisioning username. Runtime uses a separate restricted credential.
    pub nats_username: String,

    /// Private file containing the provisioning password.
    pub nats_password_file: PathBuf,

    /// NATS stream copies, separate from workload instances.
    pub stream_replicas: usize,

    /// Declared duplicate detection window in seconds.
    pub dup_window_secs: u64,

    /// Native NATS pull consumer configurations as JSON.
    pub consumer_config: Vec<String>,

    /// Write the replication-role SQL here.
    pub emit_role_sql: Option<PathBuf>,

    /// Write the CDC SQL here.
    pub emit_cdc_sql: Option<PathBuf>,

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
    pub role_sql: String,
    pub cdc_sql: String,
    pub secret: Value,
}

/// Provision the declared broker objects, write the requested CDC artifacts, and
/// record the CDC reader registration.
pub async fn enable_cdc_project_env(
    args: &EnableCdcProjectEnvRequest,
) -> anyhow::Result<EnableCdcProjectEnvOutcome> {
    let triple = Triple::new(&args.org, &args.project, args.env.as_str());
    let stream = event_stream_name(&args.org, &args.project, &args.env);
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
    if !crate::ident::is_bare_ident(&args.schema) {
        anyhow::bail!(
            "--schema must be a bare lowercase identifier, got {:?}",
            args.schema
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
    crate::event_streams::validate_inputs(
        &triple,
        args.stream_replicas,
        duplicate_window,
        &consumers,
    )?;
    let broker_options =
        crate::event_streams::connection_options(&args.nats_username, &args.nats_password_file)?;

    let system_url = args
        .system_database_url
        .as_deref()
        .context("--system-database-url is required to resolve the stored instance suffix")?;
    let instance =
        crate::provision_project_env::read_project_env_instance(system_url, &triple).await?;

    // Pick the target cluster: an explicit `--cluster` wins; otherwise derive it
    // from the org's placement (`cluster_of`).
    let cluster = match &args.cluster {
        Some(c) => c.clone(),
        None => {
            let url = args.system_database_url.as_deref().context(
                "pass --cluster, or --system-database-url to resolve the target cluster from the registry",
            )?;
            crate::provision_project_env::resolve_cluster(url, &args.org, &args.env).await?
        }
    };

    let db_name = project_env_database_name(&args.org, &args.project, &args.env, &instance);
    let cdc_name = cdc_object_name(&args.org, &args.project, &args.env, &instance);
    let secret_name = project_env_cdc_secret_name(&args.org, &args.project, &args.env);
    let db_host = args
        .db_host
        .clone()
        .unwrap_or_else(|| format!("{cluster}-rw"));
    let cdc_url = compose_url(
        &cdc_name,
        &args.replication_password,
        &db_host,
        args.db_port,
        &db_name,
    );

    let broker = async_nats::jetstream::new(
        broker_options
            .connect(&args.nats_url)
            .await
            .context("connect the event provisioning credential")?,
    );
    crate::event_streams::provision(
        &broker,
        &triple,
        args.stream_replicas,
        duplicate_window,
        &consumers,
    )
    .await?;

    // Render the artifacts the runbook applies.
    let role_sql = sql::ensure_replication_role_sql(&cdc_name, &args.replication_password);
    let cdc_sql = cdc_sql_bundle(&args.schema, &cdc_name, &db_name);
    let secret_doc =
        render_project_env_cdc_secret_manifest(&triple, &instance, &args.namespace, &cdc_url);

    write_output(args.emit_role_sql.as_deref(), &role_sql)?;
    write_output(args.emit_cdc_sql.as_deref(), &cdc_sql)?;
    write_output(
        args.emit_secret.as_deref(),
        &serde_json::to_string_pretty(&secret_doc)?,
    )?;

    record_event_reader(
        system_url,
        &triple,
        &cdc_name,
        &stream,
        &secret_name,
        args.secret_namespace.as_deref(),
    )
    .await?;

    Ok(EnableCdcProjectEnvOutcome {
        triple,
        cdc_name,
        cluster,
        stream,
        secret_name,
        role_sql,
        cdc_sql,
        secret: secret_doc,
    })
}

/// The CDC SQL the runbook applies connected to the PROJECT-ENV database, in
/// dependency order: the eager schema guard (F2 — `FOR TABLES IN SCHEMA`
/// auto-includes tables created later, so the publication may precede package
/// apply), the publication, the failover slot (WAL pinned from here),
/// the decode-time entity and exclusion maps (created BEFORE the grants so the
/// role's exact SELECT grants cover them; package apply owns their rows),
/// then the replication role's grants (the role SQL must have been applied to
/// the cluster first). Every statement is idempotent — re-applying is a no-op.
fn cdc_sql_bundle(schema: &str, cdc_name: &str, db_name: &str) -> String {
    format!(
        "{schema_guard};\n{publication}\n{slot}\n{entity_map};\n{exclusion_map};\n{grants}\n",
        schema_guard = sql::ensure_schema_sql(schema),
        publication = sql::create_publication_sql(cdc_name, schema),
        slot = sql::create_failover_slot_sql(cdc_name),
        entity_map = sql::ensure_entity_map_sql(schema),
        exclusion_map = sql::ensure_cdc_exclusion_map_sql(schema),
        grants = sql::grant_replication_access_sql(db_name, cdc_name, schema),
    )
}

/// Record the CDC reader registration in the registry (idempotent + refreshing).
/// Connects as superuser and `SET ROLE wamn_system` (the registry owner), then
/// runs the pure `wamn-control-registry` builder. The publication and slot share
/// `cdc_name`.
async fn record_event_reader(
    system_url: &str,
    triple: &Triple,
    cdc_name: &str,
    stream: &str,
    secret_name: &str,
    secret_namespace: Option<&str>,
) -> anyhow::Result<()> {
    let (client, conn) = tokio_postgres::connect(system_url, NoTls)
        .await
        .context("system db connect")?;
    let conn_task = tokio::spawn(conn);
    let result = async {
        client
            .batch_execute("SET ROLE wamn_system")
            .await
            .context("SET ROLE wamn_system")?;
        let env = triple.env.as_str();
        client
            .execute(
                wamn_control_registry::sql::upsert_event_reader_sql(),
                &[
                    &triple.org,
                    &triple.project,
                    &env,
                    &cdc_name,
                    &cdc_name,
                    &stream,
                    &secret_name,
                    &secret_namespace,
                    &true,
                ],
            )
            .await
            .context(
                "upsert registry.event_readers row (is the project-env provisioned? \
                 enable-cdc-project-env overlays an existing env — run provision-project-env first)",
            )?;
        Ok(())
    }
    .await;
    drop(client);
    let _ = conn_task.await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CDC bundle's statements land in dependency order: the schema guard
    /// before the publication (FOR TABLES IN SCHEMA needs the schema), the
    /// publication before the slot (nothing decodes before there is something
    /// published), the entity map before the grants (so `SELECT ON ALL TABLES`
    /// covers it — the reader's decode-time lookup needs it), the grants last.
    #[test]
    fn cdc_sql_bundle_orders_schema_publication_slot_map_grants() {
        let bundle = cdc_sql_bundle(
            "app",
            "wamn_cdc_acme__billing__dev",
            "wamn-db-acme--billing--dev",
        );
        let schema = bundle
            .find("CREATE SCHEMA IF NOT EXISTS \"app\"")
            .expect("schema guard");
        let publication = bundle
            .find("CREATE PUBLICATION \"wamn_cdc_acme__billing__dev\" FOR TABLES IN SCHEMA \"app\"")
            .expect("publication");
        let slot = bundle
            .find("pg_create_logical_replication_slot('wamn_cdc_acme__billing__dev', 'pgoutput', false, false, true)")
            .expect("failover slot");
        let entity_map = bundle
            .find("CREATE TABLE IF NOT EXISTS \"app\".wamn_entities")
            .expect("entity map");
        let exclusion_map = bundle
            .find("CREATE TABLE IF NOT EXISTS \"app\".wamn_cdc_exclusions")
            .expect("CDC exclusion map");
        let grants = bundle
            .find("GRANT CONNECT ON DATABASE \"wamn-db-acme--billing--dev\"")
            .expect("grants");
        assert!(schema < publication && publication < slot && slot < entity_map);
        assert!(entity_map < exclusion_map && exclusion_map < grants);
    }

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
