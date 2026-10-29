//! Org connection authority and the rows projected into enabled projects.

pub mod administration;
pub mod references;

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_postgres::Transaction;
use wamn_catalog::RuntimeType;

use crate::auth_changes::AuthDrops;

/// A refused configuration write, or its database failure.
#[derive(Debug)]
pub struct ConnectionError {
    pub code: &'static str,
    pub field: &'static str,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.field)
    }
}
impl std::error::Error for ConnectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}
impl From<tokio_postgres::Error> for ConnectionError {
    fn from(source: tokio_postgres::Error) -> Self {
        Self {
            code: "database_error",
            field: "connection",
            source: Some(Box::new(source)),
        }
    }
}
fn refuse(kind: &'static str, field: &'static str) -> ConnectionError {
    ConnectionError {
        code: kind,
        field,
        source: None,
    }
}

/// Execute one connection configuration operation in the caller's transaction.
pub async fn write(
    tx: &Transaction<'_>,
    org: &str,
    operation: &str,
    payload: &str,
    drops: &mut AuthDrops,
) -> Result<Value, ConnectionError> {
    administration::execute(tx, org, operation, payload, drops).await
}

async fn configure(
    tx: &Transaction<'_>,
    org: &str,
    operation: &str,
    payload: &Value,
) -> Result<Value, ConnectionError> {
    let text: String = tx
        .query_one(
            "SELECT registry.configure_connection($1,$2,$3::text::jsonb)::text",
            &[&org, &operation, &payload.to_string()],
        )
        .await?
        .get(0);
    let result: Value = serde_json::from_str(&text).expect("PostgreSQL JSON");
    if let Some(kind) = result["error"].as_str() {
        let kind = match kind {
            "invalid_input" => "invalid_input",
            "integration_not_found" => "integration_not_found",
            "integration_mismatch" => "integration_mismatch",
            "integration_not_allowed" => "integration_not_allowed",
            "already_imported" => "already_imported",
            "integration_unavailable" => "integration_unavailable",
            "not_found" => "not_found",
            _ => "database_error",
        };
        let field = match result["field"].as_str() {
            Some("org") => "org",
            Some("id") => "id",
            Some("name") => "name",
            Some("integration") => "integration",
            Some("parameters") => "parameters",
            Some("external_system") => "external_system",
            Some("project") => "project",
            Some("scope_id") => "scope_id",
            Some("scope_type") => "scope_type",
            _ => "connection",
        };
        return Err(refuse(kind, field));
    }
    Ok(result)
}

/// The complete non-secret external-system row. The name selects or creates a row.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalSystemWrite {
    pub integration: String,
    pub name: String,
    pub parameters: Value,
    pub status: String,
}

/// The complete non-secret connection row. A new name creates a principal too.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionWrite {
    pub integration: String,
    pub external_system: Option<String>,
    pub name: String,
    pub transport: String,
    pub exchange: String,
    pub parameters: Value,
    pub status: String,
}

async fn lock_org(tx: &Transaction<'_>, org: &str) -> Result<(), ConnectionError> {
    if tx
        .query_one(
            "SELECT rolsuper OR rolbypassrls FROM pg_roles WHERE rolname=current_user",
            &[],
        )
        .await?
        .get::<_, bool>(0)
    {
        return Err(refuse("invalid_input", "database_role"));
    }

    tx.query_one(
        "SELECT pg_advisory_xact_lock(hashtextextended('connections:' || $1, 0))",
        &[&org],
    )
    .await?;
    if !tx
        .query_one("SELECT registry.connection_org_exists($1)", &[&org])
        .await?
        .get::<_, bool>(0)
    {
        return Err(refuse("invalid_input", "org"));
    }
    Ok(())
}

async fn check_row(
    tx: &Transaction<'_>,
    integration: &str,
    name: &str,
    parameters: &Value,
    status: &str,
) -> Result<(), ConnectionError> {
    if name.trim().is_empty() || !parameters.is_object() || !matches!(status, "active" | "disabled")
    {
        return Err(refuse("invalid_input", "connection"));
    }
    if !tx
        .query_one(
            "SELECT registry.connection_integration_exists($1)",
            &[&integration],
        )
        .await?
        .get::<_, bool>(0)
    {
        return Err(refuse("integration_not_found", "integration"));
    }
    Ok(())
}

/// Write a system by its immutable name, refusing an integration change.
pub async fn write_external_system(
    tx: &Transaction<'_>,
    org: &str,
    row: &ExternalSystemWrite,
) -> Result<String, ConnectionError> {
    lock_org(tx, org).await?;
    check_row(
        tx,
        &row.integration,
        &row.name,
        &row.parameters,
        &row.status,
    )
    .await?;
    let result = configure(
        tx,
        org,
        "external_system.write",
        &serde_json::to_value(row).expect("serializable row"),
    )
    .await?;
    Ok(result["name"]
        .as_str()
        .expect("database returns a name")
        .to_owned())
}

/// Write a connection and append a generation when its parameters change.
pub async fn write_connection(
    tx: &Transaction<'_>,
    org: &str,
    row: &ConnectionWrite,
    drops: &mut AuthDrops,
) -> Result<String, ConnectionError> {
    lock_org(tx, org).await?;
    check_row(
        tx,
        &row.integration,
        &row.name,
        &row.parameters,
        &row.status,
    )
    .await?;
    let runtime_type: String = tx
        .query_one(
            "SELECT runtime_type FROM registry.connection_integrations() WHERE code=$1",
            &[&row.integration],
        )
        .await?
        .get(0);
    let runtime_type: RuntimeType = serde_json::from_value(Value::String(runtime_type))
        .map_err(|_| refuse("integration_unavailable", "integration"))?;
    if runtime_type == RuntimeType::Host {
        let requirement = wamn_catalog::RequirementType::for_integration(&row.integration)
            .ok_or_else(|| refuse("integration_unavailable", "integration"))?;
        requirement
            .check_definition(&row.parameters)
            .map_err(|_| refuse("invalid_input", "parameters"))?;
        if requirement == wamn_catalog::RequirementType::Blobstore
            && row
                .parameters
                .get("provider")
                .and_then(Value::as_str)
                .unwrap_or("s3")
                != row.integration
        {
            return Err(refuse("integration_mismatch", "parameters"));
        }
    } else if runtime_type != RuntimeType::Wasm {
        return Err(refuse("integration_unavailable", "integration"));
    }
    if !matches!(row.exchange.as_str(), "request" | "event") || row.transport.is_empty() {
        return Err(refuse("invalid_input", "transport"));
    }
    let result = configure(
        tx,
        org,
        "connection.write",
        &serde_json::to_value(row).expect("serializable row"),
    )
    .await?;
    if let Some(principal) = result["principal_id"].as_str() {
        drops.identity(principal);
    }
    Ok(result["name"]
        .as_str()
        .expect("database returns a name")
        .to_owned())
}

/// Enable one active connection in an existing project.
pub async fn enable_connection(
    tx: &Transaction<'_>,
    org: &str,
    project: &str,
    name: &str,
) -> Result<(), ConnectionError> {
    set_project_connection(tx, org, project, name, "project_connection.enable").await
}

/// Remove one project's connection enablement.
pub async fn disable_connection(
    tx: &Transaction<'_>,
    org: &str,
    project: &str,
    name: &str,
) -> Result<(), ConnectionError> {
    set_project_connection(tx, org, project, name, "project_connection.disable").await
}

async fn set_project_connection(
    tx: &Transaction<'_>,
    org: &str,
    project: &str,
    name: &str,
    operation: &str,
) -> Result<(), ConnectionError> {
    lock_org(tx, org).await?;
    configure(
        tx,
        org,
        operation,
        &json!({"project": project, "connection": name}),
    )
    .await?;
    Ok(())
}

/// Add a permission at one scope, after checking its parent and its identity.
pub async fn allow_integration(
    tx: &Transaction<'_>,
    org: &str,
    scope: &str,
    id: &str,
    integration: &str,
) -> Result<(), ConnectionError> {
    lock_org(tx, org).await?;
    configure(
        tx,
        org,
        "integration_allow.allow",
        &json!({"scope_type":scope,"scope_id":id,"integration":integration}),
    )
    .await?;
    Ok(())
}
