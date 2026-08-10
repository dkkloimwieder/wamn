//! The `provision-project-env` subcommand (wamn-q3n.7): stand up one
//! per-project-env Postgres **database** on an org's appropriate cluster (or the
//! T3 trials pool) and record it in the T1 control-plane registry.
//!
//! The four-tier counterpart of `provision-project`: identity is the `(org,
//! project, env)` [`Triple`], and the database lives on the cluster **derived** by
//! [`cluster_of`](wamn_control_registry::cluster_of) (D18) from the org's placement + the
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
//! 4. `kubectl apply -f` the emitted **credential Secret** and any independently
//!    requested management-author / route-caller PAT Secrets.
//!
//! What this tool does directly (given `--system-database-url`): read the org's
//! placement to pick the target cluster, and record `registry.projects` +
//! `registry.project_envs` (as the `wamn_system` owner); when requested, resolve
//! or create stable service principals, assign project roles, and issue then
//! authenticate PATs. Kubernetes artifacts are only emitted (no K8s client, no
//! target-cluster connection — the `provision-org` shape).
//!
//! **RLS floor** at provision time: there are no tables yet, so wamn-q3n.7
//! establishes the RLS-**enforceable substrate** only — `wamn_app` is
//! `NOSUPERUSER NOCREATEDB NOBYPASSRLS` (the role SQL) and `CONNECT` is confined
//! (the privilege SQL). The per-table `FORCE ROW LEVEL SECURITY` floor is applied
//! at catalog-publish (2.4/2.5), where the tables are created.

use std::ffi::OsString;
use std::fs::{File, OpenOptions, Permissions};
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Context as _;
use clap::Args;
use serde_json::{Value, json};
use tokio_postgres::NoTls;

use wamn_control_provision::{
    APP_ROLE, compose_url, project_env_database_name, project_env_secret_name,
    render_project_env_database, render_project_env_secret_manifest, sql, validate_project_env,
};
use wamn_control_registry::{Org, Placement, Triple, cluster_of};
use wamn_platform_identity::{
    IdentityErrorKind, Principal, PrincipalKind, PrincipalStatus, assign_project_role,
    authenticate_pat, create_service, issue_pat, resolve_subject, revoke_pat,
};

use crate::env_policies::read_env_policy;

#[derive(Debug, Args)]
pub struct ProvisionProjectEnvArgs {
    /// Org id (must already be registered — `provision-org`, or the T3 pool for a
    /// trials org). Names the target cluster and the `wamn-db-<org>--…` database.
    #[arg(long, required_unless_present = "revoke_pat_prefix")]
    pub org: Option<String>,

    /// Project id: a lowercase slug `[a-z0-9-]` (start/end alphanumeric). The
    /// reserved `wamn` prefix is rejected.
    #[arg(long, required_unless_present = "revoke_pat_prefix")]
    pub project: Option<String>,

    /// Environment slug: any policy in the ORG's `registry.env_policies` set
    /// (stamped from its template — `dev`/`prod`, plus `canary` on the dedicated
    /// templates; others are addable per org). Derives the target cluster via
    /// `cluster_of` — a dedicated org's `<org>-<owner(env)>`, or the shared pool.
    #[arg(long, required_unless_present = "revoke_pat_prefix")]
    pub env: Option<String>,

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

    /// Write the database credential `Secret` (JSON) here. Required for
    /// provisioning and must name a file; credentials are never written to stdout.
    #[arg(
        long,
        value_name = "PATH",
        value_parser = parse_secret_path,
        required_unless_present = "revoke_pat_prefix"
    )]
    pub emit_secret: Option<PathBuf>,

    /// Issue a management-author PAT and write its Kubernetes `Secret` JSON here.
    #[arg(
        long,
        value_name = "PATH",
        value_parser = parse_secret_path,
        conflicts_with = "revoke_pat_prefix"
    )]
    pub emit_management_author_pat_secret: Option<PathBuf>,

    /// Issue a route-caller PAT and write its Kubernetes `Secret` JSON here.
    #[arg(
        long,
        value_name = "PATH",
        value_parser = parse_secret_path,
        conflicts_with = "revoke_pat_prefix"
    )]
    pub emit_route_caller_pat_secret: Option<PathBuf>,

    /// Revoke one PAT by its non-secret 16-lowercase-hex lookup prefix. This is
    /// a separate invocation and performs no provisioning or Kubernetes work.
    #[arg(
        long,
        value_name = "16-LOWERCASE-HEX",
        value_parser = parse_pat_prefix,
        conflicts_with_all = [
            "emit_management_author_pat_secret",
            "emit_route_caller_pat_secret"
        ]
    )]
    pub revoke_pat_prefix: Option<String>,
}

pub async fn run(args: ProvisionProjectEnvArgs) -> anyhow::Result<()> {
    if let Some(prefix) = args.revoke_pat_prefix.as_deref() {
        let system_url = args
            .system_database_url
            .as_deref()
            .context("--revoke-pat-prefix requires --system-database-url")?;
        revoke_provisioning_pat(system_url, prefix).await?;
        println!("revoked PAT prefix {prefix}");
        return Ok(());
    }

    let db_secret_path = args
        .emit_secret
        .as_deref()
        .context("--emit-secret PATH is required and must not be '-'")?;
    ensure_secret_path(db_secret_path, "--emit-secret")?;
    if let Some(path) = args.emit_management_author_pat_secret.as_deref() {
        ensure_secret_path(path, "--emit-management-author-pat-secret")?;
    }
    if let Some(path) = args.emit_route_caller_pat_secret.as_deref() {
        ensure_secret_path(path, "--emit-route-caller-pat-secret")?;
    }
    ensure_distinct_secret_paths([
        ("--emit-secret", Some(db_secret_path)),
        (
            "--emit-management-author-pat-secret",
            args.emit_management_author_pat_secret.as_deref(),
        ),
        (
            "--emit-route-caller-pat-secret",
            args.emit_route_caller_pat_secret.as_deref(),
        ),
    ])?;
    let issues_pat = args.emit_management_author_pat_secret.is_some()
        || args.emit_route_caller_pat_secret.is_some();
    if issues_pat && args.system_database_url.is_none() {
        anyhow::bail!(
            "PAT issuance requires --system-database-url to resolve the stable service principal"
        );
    }

    let org = args
        .org
        .as_deref()
        .context("--org is required for provisioning")?;
    let project = args
        .project
        .as_deref()
        .context("--project is required for provisioning")?;
    let env = args
        .env
        .as_deref()
        .context("--env is required for provisioning")?;
    let triple = Triple::new(org, project, env);

    // Validate the project id + the assembled `wamn-db-<org>--<project>--<env>`
    // name length before any effect.
    validate_project_env(org, project, env)
        .map_err(|e| anyhow::anyhow!("project-env names: {e}"))?;

    // Pick the target cluster: an explicit `--cluster` wins (render-only / manual);
    // otherwise derive it from the org's placement + the env policy (`cluster_of`).
    let cluster = match &args.cluster {
        Some(c) => c.clone(),
        None => {
            let url = args.system_database_url.as_deref().context(
                "pass --cluster, or --system-database-url to resolve the target cluster from the registry",
            )?;
            resolve_cluster(url, org, env).await?
        }
    };

    let db_name = project_env_database_name(org, project, env);
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

    println!("{}", provision_summary(&triple, &db_name, &cluster));

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
    write_secret_json(db_secret_path, &secret_doc)?;
    println!(
        "wrote {} (database credential Secret; kubectl apply)",
        db_secret_path.display()
    );

    // Record the project + project-env in the registry (idempotent), when a system
    // DB URL is given. The Secret reference is what a triple resolves to.
    match &args.system_database_url {
        Some(url) => {
            let secret_name = project_env_secret_name(org, project, env);
            record_project_env(url, &triple, &secret_name, args.secret_namespace.as_deref())
                .await?;
            println!(
                "recorded project {:?} + project-env {} in the registry (wamn_system)",
                project, triple
            );

            if issues_pat {
                issue_pat_secrets(
                    url,
                    &triple,
                    &args.namespace,
                    args.emit_management_author_pat_secret.as_deref(),
                    args.emit_route_caller_pat_secret.as_deref(),
                )
                .await?;
            }
        }
        None => println!("(no --system-database-url: rendered artifacts only; not recorded)"),
    }

    Ok(())
}

const PAT_TTL: Duration = Duration::from_secs(2_592_000);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PatPurpose {
    purpose: &'static str,
    subject_stem: &'static str,
    display_stem: &'static str,
    role: &'static str,
    secret_stem: &'static str,
}

const MANAGEMENT_AUTHOR: PatPurpose = PatPurpose {
    purpose: "management-author",
    subject_stem: "wamn-management-author",
    display_stem: "WAMN management author",
    role: "project-author",
    secret_stem: "wamn-pat-management-author",
};

const ROUTE_CALLER: PatPurpose = PatPurpose {
    purpose: "route-caller",
    subject_stem: "wamn-route-caller",
    display_stem: "WAMN route caller",
    role: "route-caller",
    secret_stem: "wamn-pat-route-caller",
};

impl PatPurpose {
    fn subject(self, triple: &Triple) -> String {
        format!(
            "{}-{}--{}--{}",
            self.subject_stem, triple.org, triple.project, triple.env
        )
    }

    fn display_name(self, triple: &Triple) -> String {
        format!(
            "{} {}/{}/{}",
            self.display_stem, triple.org, triple.project, triple.env
        )
    }

    fn secret_name(self, triple: &Triple) -> String {
        format!(
            "{}-{}--{}--{}",
            self.secret_stem, triple.org, triple.project, triple.env
        )
    }
}

async fn issue_pat_secrets(
    system_url: &str,
    triple: &Triple,
    namespace: &str,
    management_author_path: Option<&Path>,
    route_caller_path: Option<&Path>,
) -> anyhow::Result<()> {
    let (client, connection) = tokio_postgres::connect(system_url, NoTls)
        .await
        .context("system db connect for PAT issuance")?;
    let connection_task = tokio::spawn(connection);
    let result = async {
        client
            .batch_execute("SET ROLE wamn_system")
            .await
            .context("SET ROLE wamn_system for PAT issuance")?;
        if let Some(path) = management_author_path {
            issue_pat_secret(&client, triple, namespace, MANAGEMENT_AUTHOR, path).await?;
        }
        if let Some(path) = route_caller_path {
            issue_pat_secret(&client, triple, namespace, ROUTE_CALLER, path).await?;
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;
    drop(client);
    let _ = connection_task.await;
    result
}

async fn issue_pat_secret(
    client: &tokio_postgres::Client,
    triple: &Triple,
    namespace: &str,
    purpose: PatPurpose,
    path: &Path,
) -> anyhow::Result<()> {
    let subject = purpose.subject(triple);
    let display_name = purpose.display_name(triple);
    let principal = resolve_or_create_service(client, &subject, &display_name).await?;
    anyhow::ensure!(
        principal.status() == PrincipalStatus::Active,
        "service principal {subject:?} is disabled"
    );
    assign_project_role(
        client,
        principal.id(),
        &triple.org,
        &triple.project,
        purpose.role,
    )
    .await
    .with_context(|| format!("assign {} role", purpose.role))?;

    let issued = issue_pat(client, principal.id(), purpose.purpose, PAT_TTL)
        .await
        .with_context(|| format!("issue {} PAT", purpose.purpose))?;
    let authenticated = authenticate_pat(client, issued.token())
        .await
        .with_context(|| format!("authenticate newly issued {} PAT", purpose.purpose))?
        .with_context(|| format!("newly issued {} PAT did not authenticate", purpose.purpose))?;
    anyhow::ensure!(
        authenticated.principal() == &principal,
        "newly issued {} PAT authenticated as an unexpected principal",
        purpose.purpose
    );

    let secret = render_pat_secret(
        triple,
        namespace,
        purpose,
        principal.id().as_str(),
        issued.token(),
        issued.record().prefix(),
        issued.record().expires_at(),
    );
    write_secret_json(path, &secret)?;
    println!(
        "wrote {} ({} PAT Secret; kubectl apply)",
        path.display(),
        purpose.purpose
    );
    Ok(())
}

async fn resolve_or_create_service(
    client: &tokio_postgres::Client,
    subject: &str,
    display_name: &str,
) -> anyhow::Result<Principal> {
    if let Some(principal) = resolve_subject(client, PrincipalKind::Service, subject)
        .await
        .context("resolve service principal")?
    {
        return Ok(principal);
    }

    match create_service(client, subject, display_name).await {
        Ok(principal) => Ok(principal),
        Err(error) if error.kind() == IdentityErrorKind::Conflict => {
            resolve_subject(client, PrincipalKind::Service, subject)
                .await
                .context("resolve concurrently created service principal")?
                .context("service principal conflict was not resolvable")
        }
        Err(error) => Err(error).context("create service principal"),
    }
}

async fn revoke_provisioning_pat(system_url: &str, prefix: &str) -> anyhow::Result<()> {
    let (client, connection) = tokio_postgres::connect(system_url, NoTls)
        .await
        .context("system db connect for PAT revocation")?;
    let connection_task = tokio::spawn(connection);
    let result = async {
        client
            .batch_execute("SET ROLE wamn_system")
            .await
            .context("SET ROLE wamn_system for PAT revocation")?;
        revoke_pat(&client, prefix)
            .await
            .context("revoke PAT by prefix")?;
        Ok::<(), anyhow::Error>(())
    }
    .await;
    drop(client);
    let _ = connection_task.await;
    result
}

fn render_pat_secret(
    triple: &Triple,
    namespace: &str,
    purpose: PatPurpose,
    principal_id: &str,
    token: &str,
    prefix: &str,
    expires_at: &str,
) -> Value {
    let subject = purpose.subject(triple);
    json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": purpose.secret_name(triple),
            "namespace": namespace,
            "labels": {
                "app.kubernetes.io/managed-by": "wamn",
                "app.kubernetes.io/component": "project-env-pat",
                "wamn.org": triple.org,
                "wamn.project": triple.project,
                "wamn.env": triple.env.as_str(),
            },
            "annotations": {
                "wamn.io/credential-purpose": purpose.purpose,
                "wamn.io/principal-id": principal_id,
                "wamn.io/principal-kind": "service",
                "wamn.io/principal-subject": subject,
                "wamn.io/project-role": purpose.role,
                "wamn.io/pat-prefix": prefix,
                "wamn.io/pat-expires-at": expires_at,
            },
        },
        "type": "Opaque",
        "stringData": {
            "token": token,
        },
    })
}

fn provision_summary(triple: &Triple, database: &str, cluster: &str) -> String {
    format!("project-env {triple}: database {database:?} on cluster {cluster:?} (owner {APP_ROLE})")
}

fn parse_secret_path(value: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    ensure_secret_path(&path, "secret output").map_err(|error| error.to_string())?;
    Ok(path)
}

fn ensure_secret_path(path: &Path, flag: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        path.as_os_str() != "-",
        "{flag} must name a file; '-' and stdout are forbidden for credentials"
    );
    Ok(())
}

fn ensure_distinct_secret_paths<const N: usize>(
    paths: [(&str, Option<&Path>); N],
) -> anyhow::Result<()> {
    let mut seen = Vec::with_capacity(N);
    for (flag, path) in paths {
        let Some(path) = path else {
            continue;
        };
        let absolute = std::path::absolute(path)
            .with_context(|| format!("resolve credential output path for {flag}"))?;
        let parent = absolute
            .parent()
            .context("credential output path has no parent directory")?;
        let file_name = absolute
            .file_name()
            .context("credential output path has no file name")?;
        let comparable = std::fs::canonicalize(parent)
            .with_context(|| format!("resolve credential output parent for {flag}"))?
            .join(file_name);
        if let Some((other_flag, _)) = seen
            .iter()
            .find(|(_, other_path)| other_path == &comparable)
        {
            anyhow::bail!("{flag} and {other_flag} must name distinct credential output files");
        }
        seen.push((flag, comparable));
    }
    Ok(())
}

fn parse_pat_prefix(value: &str) -> Result<String, String> {
    let valid = value.len() == 16
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'));
    if !valid {
        return Err("PAT prefix must be 16 lowercase hex digits".to_owned());
    }
    Ok(value.to_owned())
}

static SECRET_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn create_secret_temp(path: &Path) -> anyhow::Result<(PathBuf, File)> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .context("credential output path has no file name")?;

    for _ in 0..128 {
        let sequence = SECRET_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let mut temp_name = OsString::from(".");
        temp_name.push(file_name);
        temp_name.push(format!(".wamn-tmp-{}-{sequence}", std::process::id()));
        let temp_path = parent.join(temp_name);
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(&temp_path)
        {
            Ok(file) => return Ok((temp_path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("create credential output beside {}", path.display())
                });
            }
        }
    }
    anyhow::bail!(
        "could not allocate a temporary credential output beside {}",
        path.display()
    )
}

fn write_secret_json(path: &Path, doc: &Value) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(doc).context("serialize Secret JSON")?;
    bytes.push(b'\n');
    let (temp_path, mut file) = create_secret_temp(path)?;
    let result = (|| -> anyhow::Result<()> {
        file.set_permissions(Permissions::from_mode(0o600))
            .with_context(|| format!("set credential output mode on {}", temp_path.display()))?;
        file.write_all(&bytes)
            .with_context(|| format!("write credential output {}", temp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("sync credential output {}", temp_path.display()))?;
        drop(file);
        std::fs::rename(&temp_path, path)
            .with_context(|| format!("install credential output {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    result
}

/// Read the org's placement + the env's policy from the registry and **derive**
/// the target cluster via [`cluster_of`] (D18): a pooled org collapses onto its
/// pool; a dedicated org owns `<org>-<owner(env)>`. Connects as the `wamn_system`
/// owner (`SET ROLE`). Shared with the `enable-cdc-project-env` overlay
/// (wamn-l5i9.9), which targets the same derived cluster.
pub(crate) async fn resolve_cluster(
    system_url: &str,
    org: &str,
    env: &str,
) -> anyhow::Result<String> {
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
        .query_opt(
            wamn_control_registry::sql::select_org_placement_sql(),
            &[&org],
        )
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
/// wamn-q3n.3 apply pattern), then runs the pure `wamn-control-registry` builders.
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
            wamn_control_registry::sql::upsert_project_sql(),
            &[&triple.org, &triple.project],
        )
        .await
        .context("upsert registry.projects row")?;
    let env = triple.env.as_str();
    client
        .execute(
            wamn_control_registry::sql::upsert_project_env_sql(),
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
    use clap::Parser;

    #[derive(Debug, Parser)]
    struct TestCli {
        #[command(flatten)]
        args: ProvisionProjectEnvArgs,
    }

    fn parse_args(extra: &[&str]) -> Result<ProvisionProjectEnvArgs, clap::Error> {
        let mut argv = vec![
            "test",
            "--org",
            "acme",
            "--project",
            "billing",
            "--env",
            "dev",
        ];
        argv.extend_from_slice(extra);
        TestCli::try_parse_from(argv).map(|cli| cli.args)
    }

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
        use wamn_control_registry::EnvPolicy;
        let ded = Org::dedicated("acme");
        assert_eq!(cluster_of(&ded, &EnvPolicy::dev()).name, "acme-dev");
        assert_eq!(cluster_of(&ded, &EnvPolicy::prod()).name, "acme-prod");
        let pooled = Org::pooled("try", "wamn-pg");
        assert_eq!(cluster_of(&pooled, &EnvPolicy::prod()).name, "wamn-pg");
    }

    #[test]
    fn pat_literals_and_secret_documents_are_exact() {
        let triple = Triple::new("acme", "billing", "dev");
        assert_eq!(PAT_TTL, Duration::from_secs(2_592_000));
        assert_eq!(
            MANAGEMENT_AUTHOR.subject(&triple),
            "wamn-management-author-acme--billing--dev"
        );
        assert_eq!(
            MANAGEMENT_AUTHOR.display_name(&triple),
            "WAMN management author acme/billing/dev"
        );
        assert_eq!(
            ROUTE_CALLER.subject(&triple),
            "wamn-route-caller-acme--billing--dev"
        );
        assert_eq!(
            ROUTE_CALLER.display_name(&triple),
            "WAMN route caller acme/billing/dev"
        );

        let secret = render_pat_secret(
            &triple,
            "wamn-system",
            MANAGEMENT_AUTHOR,
            "6d3f2d1c-0000-4000-8000-00000000abcd",
            "wamn_pat_token-material",
            "0123456789abcdef",
            "2026-09-09T12:34:56Z",
        );
        assert_eq!(
            secret,
            json!({
                "apiVersion": "v1",
                "kind": "Secret",
                "metadata": {
                    "name": "wamn-pat-management-author-acme--billing--dev",
                    "namespace": "wamn-system",
                    "labels": {
                        "app.kubernetes.io/managed-by": "wamn",
                        "app.kubernetes.io/component": "project-env-pat",
                        "wamn.org": "acme",
                        "wamn.project": "billing",
                        "wamn.env": "dev",
                    },
                    "annotations": {
                        "wamn.io/credential-purpose": "management-author",
                        "wamn.io/principal-id": "6d3f2d1c-0000-4000-8000-00000000abcd",
                        "wamn.io/principal-kind": "service",
                        "wamn.io/principal-subject": "wamn-management-author-acme--billing--dev",
                        "wamn.io/project-role": "project-author",
                        "wamn.io/pat-prefix": "0123456789abcdef",
                        "wamn.io/pat-expires-at": "2026-09-09T12:34:56Z",
                    },
                },
                "type": "Opaque",
                "stringData": {
                    "token": "wamn_pat_token-material",
                },
            })
        );
        assert_eq!(
            secret["stringData"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            ["token"]
        );

        let route_secret = render_pat_secret(
            &triple,
            "wamn-system",
            ROUTE_CALLER,
            "6d3f2d1c-0000-4000-8000-00000000abcd",
            "wamn_pat_other-material",
            "fedcba9876543210",
            "2026-09-09T12:34:56Z",
        );
        assert_eq!(
            route_secret["metadata"]["name"],
            "wamn-pat-route-caller-acme--billing--dev"
        );
        assert_eq!(
            route_secret["metadata"]["annotations"]["wamn.io/project-role"],
            "route-caller"
        );
    }

    #[test]
    fn pat_issue_flags_select_independently_and_revoke_conflicts() {
        let management = parse_args(&[
            "--emit-secret",
            "/tmp/db.json",
            "--emit-management-author-pat-secret",
            "/tmp/management.json",
        ])
        .unwrap();
        assert!(management.emit_management_author_pat_secret.is_some());
        assert!(management.emit_route_caller_pat_secret.is_none());

        let route = parse_args(&[
            "--emit-secret",
            "/tmp/db.json",
            "--emit-route-caller-pat-secret",
            "/tmp/route.json",
        ])
        .unwrap();
        assert!(route.emit_management_author_pat_secret.is_none());
        assert!(route.emit_route_caller_pat_secret.is_some());

        let both = parse_args(&[
            "--emit-secret",
            "/tmp/db.json",
            "--emit-management-author-pat-secret",
            "/tmp/management.json",
            "--emit-route-caller-pat-secret",
            "/tmp/route.json",
        ])
        .unwrap();
        assert!(both.emit_management_author_pat_secret.is_some());
        assert!(both.emit_route_caller_pat_secret.is_some());

        let revoke = TestCli::try_parse_from([
            "test",
            "--system-database-url",
            "postgresql://postgres@localhost/postgres",
            "--revoke-pat-prefix",
            "0123456789abcdef",
        ])
        .unwrap()
        .args;
        assert_eq!(
            revoke.revoke_pat_prefix.as_deref(),
            Some("0123456789abcdef")
        );
        assert!(revoke.emit_secret.is_none());
        assert!(revoke.org.is_none());
        assert!(revoke.project.is_none());
        assert!(revoke.env.is_none());

        for issue_flag in [
            "--emit-management-author-pat-secret",
            "--emit-route-caller-pat-secret",
        ] {
            assert!(
                parse_args(&[
                    "--revoke-pat-prefix",
                    "0123456789abcdef",
                    issue_flag,
                    "/tmp/pat.json",
                ])
                .is_err(),
                "revoke accepted conflicting {issue_flag}"
            );
        }
    }

    #[test]
    fn every_secret_output_rejects_stdout_and_prefix_is_strict() {
        assert!(
            parse_args(&[]).is_err(),
            "database Secret path became optional"
        );
        assert!(parse_args(&["--emit-secret", "-"]).is_err());
        for issue_flag in [
            "--emit-management-author-pat-secret",
            "--emit-route-caller-pat-secret",
        ] {
            assert!(
                parse_args(&["--emit-secret", "/tmp/db.json", issue_flag, "-"]).is_err(),
                "{issue_flag} accepted stdout"
            );
        }
        for invalid in ["0123456789abcde", "0123456789ABCDEF", "0123456789abcdeg"] {
            assert!(
                parse_args(&["--revoke-pat-prefix", invalid]).is_err(),
                "accepted invalid PAT prefix {invalid:?}"
            );
        }
    }

    #[test]
    fn parsed_credential_outputs_must_name_distinct_files() {
        for argv in [
            vec![
                "--emit-secret",
                "/tmp/shared-credential.json",
                "--emit-management-author-pat-secret",
                "/tmp/shared-credential.json",
            ],
            vec![
                "--emit-secret",
                "/tmp/shared-credential.json",
                "--emit-route-caller-pat-secret",
                "/tmp/shared-credential.json",
            ],
            vec![
                "--emit-secret",
                "/tmp/shared-credential.json",
                "--emit-management-author-pat-secret",
                "/tmp/./shared-credential.json",
            ],
            vec![
                "--emit-secret",
                "/tmp/db.json",
                "--emit-management-author-pat-secret",
                "/tmp/shared-credential.json",
                "--emit-route-caller-pat-secret",
                "/tmp/shared-credential.json",
            ],
        ] {
            let args = parse_args(&argv).unwrap();
            let error = ensure_distinct_secret_paths([
                ("--emit-secret", args.emit_secret.as_deref()),
                (
                    "--emit-management-author-pat-secret",
                    args.emit_management_author_pat_secret.as_deref(),
                ),
                (
                    "--emit-route-caller-pat-secret",
                    args.emit_route_caller_pat_secret.as_deref(),
                ),
            ])
            .expect_err("duplicate credential output was accepted");
            assert!(error.to_string().contains("must name distinct"));
        }

        let args = parse_args(&[
            "--emit-secret",
            "/tmp/db.json",
            "--emit-management-author-pat-secret",
            "/tmp/management.json",
            "--emit-route-caller-pat-secret",
            "/tmp/route.json",
        ])
        .unwrap();
        ensure_distinct_secret_paths([
            ("--emit-secret", args.emit_secret.as_deref()),
            (
                "--emit-management-author-pat-secret",
                args.emit_management_author_pat_secret.as_deref(),
            ),
            (
                "--emit-route-caller-pat-secret",
                args.emit_route_caller_pat_secret.as_deref(),
            ),
        ])
        .unwrap();
    }

    #[test]
    fn secret_files_are_plain_json_and_mode_0600() {
        let path = std::env::temp_dir().join(format!(
            "wamn-ctl-pat-secret-mode-{}.json",
            std::process::id()
        ));
        std::fs::write(&path, b"old credential material").unwrap();
        std::fs::set_permissions(&path, Permissions::from_mode(0o644)).unwrap();

        let document = json!({"stringData": {"token": "test-token"}});
        write_secret_json(&path, &document).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(stored, document);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn secret_writes_replace_links_without_following_or_sharing_them() {
        let sequence = SECRET_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "wamn-ctl-pat-secret-links-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();

        let db_path = root.join("db.json");
        let management_path = root.join("management.json");
        std::os::unix::fs::symlink(&db_path, &management_path).unwrap();
        let db_document = json!({"stringData": {"url": "db-secret"}});
        let management_document = json!({"stringData": {"token": "management-secret"}});
        write_secret_json(&db_path, &db_document).unwrap();
        write_secret_json(&management_path, &management_document).unwrap();
        assert!(
            !std::fs::symlink_metadata(&management_path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let stored_db: Value = serde_json::from_slice(&std::fs::read(&db_path).unwrap()).unwrap();
        let stored_management: Value =
            serde_json::from_slice(&std::fs::read(&management_path).unwrap()).unwrap();
        assert_eq!(stored_db, db_document);
        assert_eq!(stored_management, management_document);

        let route_path = root.join("route.json");
        std::fs::hard_link(&management_path, &route_path).unwrap();
        let route_document = json!({"stringData": {"token": "route-secret"}});
        write_secret_json(&route_path, &route_document).unwrap();
        let stored_management: Value =
            serde_json::from_slice(&std::fs::read(&management_path).unwrap()).unwrap();
        let stored_route: Value =
            serde_json::from_slice(&std::fs::read(&route_path).unwrap()).unwrap();
        assert_eq!(stored_management, management_document);
        assert_eq!(stored_route, route_document);

        std::fs::remove_file(db_path).unwrap();
        std::fs::remove_file(management_path).unwrap();
        std::fs::remove_file(route_path).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn provisioning_summary_contains_no_database_credentials() {
        let triple = Triple::new("acme", "billing", "dev");
        let summary = provision_summary(&triple, "wamn-db-acme--billing--dev", "acme-dev");
        assert_eq!(
            summary,
            "project-env acme/billing/dev: database \"wamn-db-acme--billing--dev\" on cluster \"acme-dev\" (owner wamn_app)"
        );
        assert!(!summary.contains("postgres://"));
        assert!(!summary.contains("password"));
        assert!(!summary.contains("app url"));
    }
}
