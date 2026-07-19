//! The `enable-cdc-project-env` subcommand (wamn-l5i9.9, D19 v3 §4): overlay
//! CDC **capture** onto an ALREADY-provisioned project-env. CDC is opt-in and
//! may be enabled long after provisioning, so it is its own overlay rather than
//! a `provision-project-env` flag.
//!
//! Renders + records (the `provision-project-env` shape — no K8s client, no
//! target-cluster connection); the runbook applies the emitted artifacts, in
//! this order:
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
//! What this tool does directly (given `--system-database-url`): derive the
//! target cluster from the org's placement, and record the
//! `registry.event_readers` registration (as the `wamn_system` owner). The
//! registration's project-env FK makes the overlay ordering structural: an
//! unprovisioned env is rejected.
//!
//! One shared name serves the publication, the slot, and the role
//! (`wamn_cdc_<org>__<project>__<env>` — underscored, a slot name admits only
//! `[a-z0-9_]`); the Secret keeps the hyphenated convention
//! (`wamn-cdc-<org>--<project>--<env>`). The replication credential is its own
//! R8b tier — distinct from the `wamn_app` query credential and the dispatch
//! role. NOTE Postgres `REPLICATION` is cluster-wide: on a shared pool,
//! input-side isolation rests on handing each reader only its own
//! slot/publication/credentials (plus per-org NATS accounts on the output
//! side); regulated tiers use dedicated clusters.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::Args;
use tokio_postgres::NoTls;

use wamn_provision::{
    cdc_object_name, compose_url, event_stream_name, project_env_cdc_secret_name,
    project_env_database_name, render_project_env_cdc_secret_manifest, sql,
    validate_project_env_cdc,
};
use wamn_registry::Triple;

#[derive(Debug, Args)]
pub struct EnableCdcProjectEnvArgs {
    /// Org id (the project-env must already be provisioned and recorded).
    #[arg(long)]
    pub org: String,

    /// Project id.
    #[arg(long)]
    pub project: String,

    /// Environment slug.
    #[arg(long)]
    pub env: String,

    /// The app DATA schema the publication covers — the `--schema`
    /// catalog-publish/migrate-catalog use (NOT the `app_system` auth schema).
    #[arg(long, default_value = "public")]
    pub schema: String,

    /// Superuser Postgres URL to the T1 system DB (`wamn_system`): derive the
    /// target cluster + record the `registry.event_readers` registration. Env
    /// `WAMN_SYSTEM_ADMIN_URL`. Omit (and pass `--cluster`) to render only.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: Option<String>,

    /// Override the target CNPG `Cluster` name. When omitted, it is derived from
    /// the org's placement in the registry. Required if `--system-database-url`
    /// is not given (render-only mode).
    #[arg(long)]
    pub cluster: Option<String>,

    /// Password for the per-project-env replication role (embedded in the
    /// emitted URL + role SQL). Env `WAMN_REPLICATION_PASSWORD`.
    #[arg(long, env = "WAMN_REPLICATION_PASSWORD", default_value = "wamn_cdc")]
    pub replication_password: String,

    /// Host the reader reaches the project-env database at. Defaults to the
    /// target cluster's read-write service `<cluster>-rw`.
    #[arg(long)]
    pub db_host: Option<String>,

    /// Port the reader reaches the database at.
    #[arg(long, default_value_t = 5432)]
    pub db_port: u16,

    /// Namespace the emitted `Secret` is applied to.
    #[arg(long, env = "WAMN_NAMESPACE", default_value = "wamn-system")]
    pub namespace: String,

    /// Secret namespace to RECORD in the registration's replication `SecretRef`.
    /// Omit to record `NULL` (the resolving service's own namespace).
    #[arg(long)]
    pub secret_namespace: Option<String>,

    /// Override the JetStream stream recorded in the registration. Default:
    /// `EVT_<org>_<env>` (D19 v3 §5; e.g. a shared trials stream is a data
    /// override here, not a code change).
    #[arg(long)]
    pub stream: Option<String>,

    /// Write the replication-role SQL (psql the TARGET cluster first — roles are
    /// cluster-global) here; `-` = stdout.
    #[arg(long)]
    pub emit_role_sql: Option<PathBuf>,

    /// Write the CDC SQL (schema guard + publication + failover slot + grants;
    /// psql the PROJECT-ENV database) here; `-` = stdout.
    #[arg(long)]
    pub emit_cdc_sql: Option<PathBuf>,

    /// Write the replication-credential `Secret` (JSON) here; `-` = stdout.
    #[arg(long)]
    pub emit_secret: Option<PathBuf>,
}

pub async fn run(args: EnableCdcProjectEnvArgs) -> anyhow::Result<()> {
    let triple = Triple::new(&args.org, &args.project, args.env.as_str());

    // Validate the names (the base project-env rules + the assembled
    // `wamn_cdc_…` object name's 63-byte bound) before any effect.
    validate_project_env_cdc(&args.org, &args.project, &args.env)
        .map_err(|e| anyhow::anyhow!("cdc names: {e}"))?;
    if !crate::migrate_catalog::is_bare_ident(&args.schema) {
        anyhow::bail!(
            "--schema must be a bare lowercase identifier, got {:?}",
            args.schema
        );
    }

    // Pick the target cluster: an explicit `--cluster` wins (render-only /
    // manual); otherwise derive it from the org's placement (`cluster_of`).
    let cluster = match &args.cluster {
        Some(c) => c.clone(),
        None => {
            let url = args.system_database_url.as_deref().context(
                "pass --cluster, or --system-database-url to resolve the target cluster from the registry",
            )?;
            crate::provision_project_env::resolve_cluster(url, &args.org, &args.env).await?
        }
    };

    let db_name = project_env_database_name(&args.org, &args.project, &args.env);
    let cdc_name = cdc_object_name(&args.org, &args.project, &args.env);
    let secret_name = project_env_cdc_secret_name(&args.org, &args.project, &args.env);
    let stream = args
        .stream
        .clone()
        .unwrap_or_else(|| event_stream_name(&args.org, &args.env));
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

    // Render the artifacts the runbook applies.
    let role_sql = sql::ensure_replication_role_sql(&cdc_name, &args.replication_password);
    let cdc_sql = cdc_sql_bundle(&args.schema, &cdc_name, &db_name);
    let secret_doc = render_project_env_cdc_secret_manifest(&triple, &args.namespace, &cdc_url);

    println!(
        "cdc for project-env {triple}: publication/slot/role {cdc_name:?} over schema {:?} \
         on cluster {cluster:?}; stream {stream:?}; replication secret {secret_name:?}",
        args.schema,
    );

    emit_text(
        &args.emit_role_sql,
        "replication-role SQL (psql the TARGET cluster — roles are cluster-global)",
        &role_sql,
    )?;
    emit_text(
        &args.emit_cdc_sql,
        "CDC SQL (psql the PROJECT-ENV database — publication + slot are database-bound)",
        &cdc_sql,
    )?;
    emit_json(
        &args.emit_secret,
        "replication-credential Secret (kubectl apply)",
        &secret_doc,
    )?;

    // Record the reader registration (idempotent), when a system DB URL is
    // given. The project-env FK enforces the overlay ordering.
    match &args.system_database_url {
        Some(url) => {
            record_event_reader(
                url,
                &triple,
                &cdc_name,
                &stream,
                &secret_name,
                args.secret_namespace.as_deref(),
            )
            .await?;
            println!(
                "recorded event-reader registration for {triple} in the registry (wamn_system)"
            );
        }
        None => println!("(no --system-database-url: rendered artifacts only; not recorded)"),
    }

    Ok(())
}

/// The CDC SQL the runbook applies connected to the PROJECT-ENV database, in
/// dependency order: the eager schema guard (F2 — `FOR TABLES IN SCHEMA`
/// auto-includes tables created later, so the publication may precede
/// catalog-publish), the publication, the failover slot (WAL pinned from here),
/// then the replication role's grants (the role SQL must have been applied to
/// the cluster first). Every statement is idempotent — re-applying is a no-op.
fn cdc_sql_bundle(schema: &str, cdc_name: &str, db_name: &str) -> String {
    format!(
        "{schema_guard};\n{publication}\n{slot}\n{grants}\n",
        schema_guard = sql::ensure_schema_sql(schema),
        publication = sql::create_publication_sql(cdc_name, schema),
        slot = sql::create_failover_slot_sql(cdc_name),
        grants = sql::grant_replication_access_sql(db_name, cdc_name, schema),
    )
}

/// Record the CDC reader registration in the registry (idempotent + refreshing).
/// Connects as superuser and `SET ROLE wamn_system` (the registry owner), then
/// runs the pure `wamn-registry` builder. The publication and slot share
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
                wamn_registry::sql::upsert_event_reader_sql(),
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

/// Print a JSON document to a path, or to stdout with a labeled header when the
/// path is absent (`-` also means stdout).
fn emit_json(path: &Option<PathBuf>, label: &str, doc: &serde_json::Value) -> anyhow::Result<()> {
    emit_text(path, label, &serde_json::to_string_pretty(doc)?)
}

fn emit_text(path: &Option<PathBuf>, label: &str, text: &str) -> anyhow::Result<()> {
    match path {
        Some(p) if p.as_os_str() != "-" => {
            std::fs::write(p, text).with_context(|| format!("write {}", p.display()))?;
            println!("wrote {} ({label})", p.display());
        }
        _ => println!("--- {label} ---\n{text}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CDC bundle's statements land in dependency order: the schema guard
    /// before the publication (FOR TABLES IN SCHEMA needs the schema), the
    /// publication before the slot (nothing decodes before there is something
    /// published), the grants last.
    #[test]
    fn cdc_sql_bundle_orders_schema_publication_slot_grants() {
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
        let grants = bundle
            .find("GRANT CONNECT ON DATABASE \"wamn-db-acme--billing--dev\"")
            .expect("grants");
        assert!(schema < publication && publication < slot && slot < grants);
    }

    #[test]
    fn cdc_names_are_validated_before_any_effect() {
        // A reserved / non-slug project id fails without touching the registry.
        assert!(validate_project_env_cdc("acme", "wamn-x", "dev").is_err());
        assert!(validate_project_env_cdc("acme", "Bad", "prod").is_err());
        assert!(validate_project_env_cdc("acme", "billing", "prod").is_ok());
        // The publication schema must be a bare identifier (defense-in-depth on
        // top of the builders' quoting).
        assert!(crate::migrate_catalog::is_bare_ident("app_data"));
        assert!(!crate::migrate_catalog::is_bare_ident("app;DROP"));
    }
}
