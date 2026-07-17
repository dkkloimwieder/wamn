//! The `provision-project-env` subcommand (wamn-q3n.7): stand up one
//! per-project-env Postgres **database** on an org's appropriate cluster (or the
//! T3 trials pool) and record it in the T1 control-plane registry.
//!
//! The four-tier counterpart of `provision-project`: identity is the `(org,
//! project, env)` [`Triple`], and the database lives on the cluster **derived** by
//! [`cluster_of`](wamn_registry::cluster_of) (D18) from the org's placement + the
//! env's policy — a dedicated org's `<org>-<owner(env)>` (so `canary` sharing prod
//! lands on `<org>-prod`, `canary` own on `<org>-canary`), or the shared pool for a
//! pooled org. One derivation path serves every placement.
//!
//! An imperative CLI (the `provision-org` precedent). It **renders + records**;
//! the runbook/Job applies the emitted artifacts, in this order:
//!
//! 1. the shared `wamn_app` role must exist **before** the `Database` CR (its
//!    `owner`): apply the emitted **role SQL** to the target cluster's superuser;
//! 2. `kubectl apply -f` the emitted **`Database` CR** and wait it applied — the
//!    CNPG operator declaratively creates the database owned by `wamn_app`;
//! 3. apply the emitted **privilege SQL** (`REVOKE CONNECT FROM PUBLIC` / `GRANT
//!    wamn_app`) — the thin imperative step the `Database` CRD does not cover
//!    (topology fact 3), run **after** the database exists;
//! 4. `kubectl apply -f` the emitted **credential Secret**.
//!
//! What this tool does directly (given `--system-database-url`): read the org's
//! placement to pick the target cluster, and record `registry.projects` +
//! `registry.project_envs` (as the `wamn_system` owner). Everything else is
//! emitted (no K8s client, no target-cluster connection — the `provision-org`
//! shape).
//!
//! **RLS floor** at provision time: there are no tables yet, so wamn-q3n.7
//! establishes the RLS-**enforceable substrate** only — `wamn_app` is
//! `NOSUPERUSER NOCREATEDB NOBYPASSRLS` (the role SQL) and `CONNECT` is confined
//! (the privilege SQL). The per-table `FORCE ROW LEVEL SECURITY` floor is applied
//! at catalog-publish (2.4/2.5), where the tables are created.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::Args;
use tokio_postgres::NoTls;

use wamn_provision::{
    APP_ROLE, compose_url, project_env_database_name, project_env_secret_name,
    render_project_env_database, render_project_env_secret_manifest, sql, validate_project_env,
};
use wamn_registry::{Org, Placement, Triple, cluster_of};

use crate::env_policies::read_env_policy;

#[derive(Debug, Args)]
pub struct ProvisionProjectEnvArgs {
    /// Org id (must already be registered — `provision-org`, or the T3 pool for a
    /// trials org). Names the target cluster and the `wamn-db-<org>--…` database.
    #[arg(long)]
    pub org: String,

    /// Project id: a lowercase slug `[a-z0-9-]` (start/end alphanumeric). The
    /// reserved `wamn` prefix is rejected.
    #[arg(long)]
    pub project: String,

    /// Environment slug: any policy in the ORG's `registry.env_policies` set
    /// (stamped from its template — `dev`/`prod`, plus `canary` on the dedicated
    /// templates; others are addable per org). Derives the target cluster via
    /// `cluster_of` — a dedicated org's `<org>-<owner(env)>`, or the shared pool.
    #[arg(long)]
    pub env: String,

    /// Superuser Postgres URL to the T1 system DB (`wamn_system`): read the org's
    /// placement (pick the target cluster) and record the project + project-env.
    /// Env `WAMN_SYSTEM_ADMIN_URL`. Omit (and pass `--cluster`) to render only.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: Option<String>,

    /// Override the target CNPG `Cluster` name. When omitted, it is read from the
    /// org's placement in the registry. Required if `--system-database-url` is not
    /// given (render-only mode).
    #[arg(long)]
    pub cluster: Option<String>,

    /// Per-project-env `CONNECTION LIMIT` (noisy-neighbour governance within a
    /// cluster). Default: no limit (`-1`).
    #[arg(long)]
    pub connection_limit: Option<i64>,

    /// Password for the shared `wamn_app` role (embedded in the emitted URL + the
    /// role SQL). Env `WAMN_APP_PASSWORD`.
    #[arg(long, env = "WAMN_APP_PASSWORD", default_value = "wamn_app")]
    pub app_password: String,

    /// Host the runtime reaches the project-env database at. Defaults to the
    /// target cluster's read-write service `<cluster>-rw`.
    #[arg(long)]
    pub app_host: Option<String>,

    /// Port the runtime reaches the database at.
    #[arg(long, default_value_t = 5432)]
    pub app_port: u16,

    /// Namespace the emitted `Database` CR + `Secret` are applied to.
    #[arg(long, env = "WAMN_NAMESPACE", default_value = "wamn-system")]
    pub namespace: String,

    /// Secret namespace to RECORD in the registry `SecretRef`. Omit to record
    /// `NULL` (the resolving component's own namespace).
    #[arg(long)]
    pub secret_namespace: Option<String>,

    /// Write the CNPG `Database` CR (JSON) here; `-` = stdout. Absent ⇒ printed
    /// with a labeled header.
    #[arg(long)]
    pub emit_database: Option<PathBuf>,

    /// Write the role-ensure SQL (apply to the target cluster BEFORE the `Database`
    /// CR — the CR's `owner` must exist) here; `-` = stdout.
    #[arg(long)]
    pub emit_role_sql: Option<PathBuf>,

    /// Write the privilege SQL (`REVOKE CONNECT FROM PUBLIC` / `GRANT wamn_app`;
    /// apply AFTER the database is ready) here; `-` = stdout.
    #[arg(long)]
    pub emit_privilege_sql: Option<PathBuf>,

    /// Write the credential `Secret` (JSON) here; `-` = stdout.
    #[arg(long)]
    pub emit_secret: Option<PathBuf>,
}

pub async fn run(args: ProvisionProjectEnvArgs) -> anyhow::Result<()> {
    let triple = Triple::new(&args.org, &args.project, args.env.as_str());

    // Validate the project id + the assembled `wamn-db-<org>--<project>--<env>`
    // name length before any effect.
    validate_project_env(&args.org, &args.project, &args.env)
        .map_err(|e| anyhow::anyhow!("project-env names: {e}"))?;

    // Pick the target cluster: an explicit `--cluster` wins (render-only / manual);
    // otherwise derive it from the org's placement + the env policy (`cluster_of`).
    let cluster = match &args.cluster {
        Some(c) => c.clone(),
        None => {
            let url = args.system_database_url.as_deref().context(
                "pass --cluster, or --system-database-url to resolve the target cluster from the registry",
            )?;
            resolve_cluster(url, &args.org, &args.env).await?
        }
    };

    let db_name = project_env_database_name(&args.org, &args.project, &args.env);
    let app_host = args
        .app_host
        .clone()
        .unwrap_or_else(|| format!("{cluster}-rw"));
    let app_url = compose_url(
        APP_ROLE,
        &args.app_password,
        &app_host,
        args.app_port,
        &db_name,
    );

    // Render the artifacts the runbook applies.
    let db_cr = render_project_env_database(&triple, &cluster, args.connection_limit);
    let role_sql = sql::ensure_app_role_sql(&args.app_password);
    let privilege_sql = sql::grant_connect_on_database_sql(&db_name);
    let secret_doc = render_project_env_secret_manifest(&triple, &args.namespace, &app_url);

    println!(
        "project-env {triple}: database {db_name:?} on cluster {cluster:?} (owner {APP_ROLE}); \
         app url {app_url}"
    );

    emit_json(&args.emit_database, "Database CR (kubectl apply)", &db_cr)?;
    emit_text(
        &args.emit_role_sql,
        "role SQL (psql the TARGET cluster BEFORE the Database CR)",
        &role_sql,
    )?;
    emit_text(
        &args.emit_privilege_sql,
        "privilege SQL (psql the TARGET cluster AFTER the Database is ready)",
        &privilege_sql,
    )?;
    emit_json(
        &args.emit_secret,
        "credential Secret (kubectl apply)",
        &secret_doc,
    )?;

    // Record the project + project-env in the registry (idempotent), when a system
    // DB URL is given. The Secret reference is what a triple resolves to.
    match &args.system_database_url {
        Some(url) => {
            let secret_name = project_env_secret_name(&args.org, &args.project, &args.env);
            record_project_env(url, &triple, &secret_name, args.secret_namespace.as_deref())
                .await?;
            println!(
                "recorded project {:?} + project-env {} in the registry (wamn_system)",
                args.project, triple
            );
        }
        None => println!("(no --system-database-url: rendered artifacts only; not recorded)"),
    }

    Ok(())
}

/// Read the org's placement + the env's policy from the registry and **derive**
/// the target cluster via [`cluster_of`] (D18): a pooled org collapses onto its
/// pool; a dedicated org owns `<org>-<owner(env)>`. Connects as the `wamn_system`
/// owner (`SET ROLE`).
async fn resolve_cluster(system_url: &str, org: &str, env: &str) -> anyhow::Result<String> {
    let (client, conn) = tokio_postgres::connect(system_url, NoTls)
        .await
        .context("system db connect")?;
    let conn_task = tokio::spawn(conn);
    let result = do_resolve_cluster(&client, org, env).await;
    drop(client);
    let _ = conn_task.await;
    result
}

async fn do_resolve_cluster(
    client: &tokio_postgres::Client,
    org: &str,
    env: &str,
) -> anyhow::Result<String> {
    client
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("SET ROLE wamn_system")?;
    let row = client
        .query_opt(wamn_registry::sql::select_org_placement_sql(), &[&org])
        .await
        .context("read org placement")?
        .with_context(|| {
            format!(
                "org {org:?} is not registered: run provision-org before provisioning a project-env"
            )
        })?;
    let placement_kind: String = row.get("placement_kind");
    let pool: Option<String> = row.get("pool_cluster");
    let placement = match placement_kind.as_str() {
        "pooled" => Placement::Pooled {
            pool: pool.context("pooled org row is missing its pool_cluster")?,
        },
        "dedicated" => Placement::Dedicated,
        other => anyhow::bail!("unknown placement_kind {other:?} for org {org:?}"),
    };
    let org_obj = Org {
        id: org.to_string(),
        placement,
    };
    // The env must name a policy in the ORG's own set (8df.4 — its recovery
    // domain drives the derivation); a pooled org ignores the policy but the env
    // must still resolve.
    let policy = read_env_policy(client, org, env).await?.with_context(|| {
        format!(
            "env {env:?} names none of org {org:?}'s env policies — provision-org stamps them \
             from a template; customize/add rows in registry.env_policies"
        )
    })?;
    Ok(cluster_of(&org_obj, &policy).name)
}

/// Record the project and the provisioned project-env in the registry (idempotent).
/// Connects as superuser and `SET ROLE wamn_system` (the registry owner — the
/// wamn-q3n.3 apply pattern), then runs the pure `wamn-registry` builders.
async fn record_project_env(
    system_url: &str,
    triple: &Triple,
    secret_name: &str,
    secret_namespace: Option<&str>,
) -> anyhow::Result<()> {
    let (client, conn) = tokio_postgres::connect(system_url, NoTls)
        .await
        .context("system db connect")?;
    let conn_task = tokio::spawn(conn);
    let result = do_record_project_env(&client, triple, secret_name, secret_namespace).await;
    drop(client);
    let _ = conn_task.await;
    result
}

async fn do_record_project_env(
    client: &tokio_postgres::Client,
    triple: &Triple,
    secret_name: &str,
    secret_namespace: Option<&str>,
) -> anyhow::Result<()> {
    client
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("SET ROLE wamn_system")?;
    client
        .execute(
            wamn_registry::sql::upsert_project_sql(),
            &[&triple.org, &triple.project],
        )
        .await
        .context("upsert registry.projects row")?;
    let env = triple.env.as_str();
    client
        .execute(
            wamn_registry::sql::upsert_project_env_sql(),
            &[
                &triple.org,
                &triple.project,
                &env,
                &secret_name,
                &secret_namespace,
            ],
        )
        .await
        .context("upsert registry.project_envs row")?;
    Ok(())
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

    #[test]
    fn a_reserved_or_bad_project_id_is_rejected_before_any_effect() {
        // The name validation runs first — a reserved / non-slug project id fails
        // without touching the registry or emitting a CR.
        assert!(validate_project_env("acme", "wamn-x", "dev").is_err());
        assert!(validate_project_env("acme", "Bad", "prod").is_err());
        assert!(validate_project_env("acme", "billing", "prod").is_ok());
    }

    /// The target cluster is DERIVED (D18 `cluster_of`) from the org's placement +
    /// the env's policy: a dedicated org owns `<org>-<owner(env)>`, a pooled org
    /// collapses every env onto its pool. (The live routing through the DB is
    /// proven by the in-cluster gate; here we pin the pure derivation the
    /// subcommand calls.)
    #[test]
    fn cluster_is_derived_by_placement_and_policy() {
        use wamn_registry::EnvPolicy;
        let ded = Org::dedicated("acme");
        assert_eq!(cluster_of(&ded, &EnvPolicy::dev()).name, "acme-dev");
        assert_eq!(cluster_of(&ded, &EnvPolicy::prod()).name, "acme-prod");
        let pooled = Org::pooled("try", "wamn-pg");
        assert_eq!(cluster_of(&pooled, &EnvPolicy::prod()).name, "wamn-pg");
    }
}
