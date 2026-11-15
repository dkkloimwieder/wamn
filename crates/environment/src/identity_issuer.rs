//! Provision the identity issuer through CloudNativePG's managed roles.

use anyhow::Context as _;
use serde_json::json;
use std::path::PathBuf;
use wamn_control_provision::identity_role::{
    grant_identity_issuer_surface_sql, identity_issuer_login,
};

pub struct IdentityIssuerRequest {
    pub issuer: String,
    pub system_database_url: String,
    pub db_host: Option<String>,
    pub db_port: u16,
    pub namespace: String,
    pub secret_name: String,
}

impl std::fmt::Debug for IdentityIssuerRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdentityIssuerRequest")
            .field("issuer", &self.issuer)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct IdentityIssuerOutcome {
    pub role: String,
    pub configuration: wamn_postgres::ConnectionConfig,
}

/// Create one role Secret and its public component configuration. CNPG owns the password.
pub async fn provision_identity_issuer(
    args: &IdentityIssuerRequest,
) -> anyhow::Result<IdentityIssuerOutcome> {
    let role = identity_issuer_login(&args.issuer)?;
    let options = wamn_postgres::connect_options(&args.system_database_url).await?;
    let database = options
        .get_database()
        .context("issuer database is absent")?
        .to_owned();
    anyhow::ensure!(
        database == wamn_control_provision::identity_role::IDENTITY_ISSUER_DATABASE,
        "issuer database is not the system database"
    );
    let host = args
        .db_host
        .as_deref()
        .context("issuer database host is absent")?;
    let cluster = host
        .split('.')
        .next()
        .context("issuer database host is absent")?
        .trim_end_matches("-rw");
    let mut connection = wamn_postgres::connect(&args.system_database_url).await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(grant_identity_issuer_surface_sql()))
        .execute(&mut connection)
        .await?;
    let kubeconfig = std::env::var_os("KUBECONFIG")
        .map(PathBuf::from)
        .unwrap_or_default();
    super::provision_project_env::workload::reconcile(
        &args.system_database_url,
        &kubeconfig,
        "",
        &args.namespace,
        cluster,
        &wamn_control_provision::connection::managed_role(
            role.clone(),
            wamn_control_provision::identity_role::IDENTITY_ISSUER_ROLE,
            args.secret_name.clone(),
        ),
    )
    .await?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
    while !sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT FROM pg_roles WHERE rolname=$1 AND rolcanlogin)",
    )
    .bind(&role)
    .fetch_one(&mut connection)
    .await?
    {
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "CNPG did not create the issuer login"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let statement = format!(
        "GRANT CONNECT ON DATABASE {} TO {}",
        postgres_protocol::escape::escape_identifier(&database),
        postgres_protocol::escape::escape_identifier(&role)
    );
    sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
        .execute(&mut connection)
        .await?;
    let configuration = super::provision_project_env::workload::configuration(
        &args.namespace,
        cluster,
        host.into(),
        args.db_port,
        database,
        role.clone(),
        args.secret_name.clone(),
    );
    let object = json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":format!("{}-configuration",args.secret_name),"namespace":args.namespace},"data":{"connection.json":configuration.to_json()?}});
    wamn_kubernetes::kubectl::apply(&kubeconfig, "", &args.namespace, &object).await?;
    Ok(IdentityIssuerOutcome {
        role,
        configuration,
    })
}
