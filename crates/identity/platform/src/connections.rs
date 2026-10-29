//! Org connection authority and the rows projected into enabled projects.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_postgres::Transaction;
use wamn_catalog::RuntimeType;

use crate::auth_changes::AuthDrops;
use crate::configuration::{
    Connection, ExternalSystem, IntegrationAllow, PlatformConfiguration, ProjectConnection,
};

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
    fn parse<T: serde::de::DeserializeOwned>(payload: &str) -> Result<T, ConnectionError> {
        serde_json::from_str(payload).map_err(|_| refuse("invalid_input", "value"))
    }
    match operation {
        "external_system.write" => {
            Ok(json!({"name": write_external_system(tx, org, &parse(payload)?).await?}))
        }
        "connection.write" => {
            Ok(json!({"name": write_connection(tx, org, &parse(payload)?, drops).await?}))
        }
        "project_connection.enable" | "project_connection.disable" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Request {
                project: String,
                connection: String,
            }
            let request: Request = parse(payload)?;
            set_project_connection(tx, org, &request.project, &request.connection, operation)
                .await?;
            Ok(json!({"connection": request.connection}))
        }
        "integration_allow.allow" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Request {
                scope_type: String,
                scope_id: String,
                integration: String,
            }
            let request: Request = parse(payload)?;
            allow_integration(
                tx,
                org,
                &request.scope_type,
                &request.scope_id,
                &request.integration,
            )
            .await?;
            Ok(json!({"integration": request.integration}))
        }
        _ => Err(refuse("invalid_input", "operation")),
    }
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

/// Read only the rows enabled in this project and applicable to this environment.
pub async fn projection(
    tx: &Transaction<'_>,
    org: &str,
    project: &str,
    env: &str,
) -> Result<PlatformConfiguration, ConnectionError> {
    let mut result = PlatformConfiguration::default();
    for row in tx.query("SELECT DISTINCT s.id::text, s.integration, s.name, s.parameters::text, CASE WHEN s.status = 'active' AND EXISTS (SELECT FROM registry.connection_rows($1) enabled JOIN registry.connection_project_rows($1) ep ON ep.connection_id = enabled.id WHERE ep.project = $2 AND enabled.external_system_id = s.id AND enabled.status = 'active') THEN 'active' ELSE 'disabled' END FROM registry.connection_system_rows($1) s JOIN registry.connection_rows($1) c ON c.external_system_id = s.id JOIN registry.connection_project_rows($1) p ON p.connection_id = c.id WHERE p.org = $1 AND p.project = $2 ORDER BY s.id::text", &[&org, &project]).await? {
        result.external_systems.push(ExternalSystem { id: row.get(0), integration: row.get(1), name: row.get(2), parameters: serde_json::from_str(row.get(3)).expect("PostgreSQL JSON"), status: row.get(4) });
    }
    for row in tx.query("SELECT c.id::text, c.integration, c.external_system_id::text, c.name, c.transport, c.exchange, c.parameters::text, c.status FROM registry.connection_rows($1) c JOIN registry.connection_project_rows($1) p ON p.connection_id = c.id WHERE p.org = $1 AND p.project = $2 ORDER BY c.id", &[&org, &project]).await? {
        let id: String = row.get(0);
        result.project_connections.push(ProjectConnection { project_id: project.to_owned(), connection_id: id.clone() });
        result.connections.push(Connection { id, integration: row.get(1), external_system_id: row.get(2), name: row.get(3), transport: row.get(4), exchange: row.get(5), parameters: serde_json::from_str(row.get(6)).expect("PostgreSQL JSON"), status: row.get(7) });
    }
    for row in tx.query("SELECT scope_type, scope_id, integration FROM registry.connection_allow_rows($1) WHERE org = $1 AND ((scope_type = 'org' AND scope_id = $1) OR (scope_type = 'project' AND scope_id = $2) OR (scope_type = 'environment' AND scope_id = $2 || '/' || $3)) AND integration IN (SELECT c.integration FROM registry.connection_rows($1) c JOIN registry.connection_project_rows($1) p ON p.connection_id = c.id WHERE p.org = $1 AND p.project = $2) ORDER BY scope_type, scope_id, integration", &[&org, &project, &env]).await? {
        result.integration_allows.push(IntegrationAllow { scope_type: row.get(0), scope_id: row.get(1), integration: row.get(2) });
    }
    Ok(result)
}

/// Reconcile the connection projections in an administration transaction.
/// The caller binds its actor and publishes the returned auth drops after commit.
pub async fn project(
    system: &Transaction<'_>,
    target: &Transaction<'_>,
    org: &str,
    project: &str,
    env: &str,
) -> Result<AuthDrops, ConnectionError> {
    if target
        .query_one(
            "SELECT rolsuper OR rolbypassrls FROM pg_roles WHERE rolname=current_user",
            &[],
        )
        .await?
        .get::<_, bool>(0)
    {
        return Err(refuse("invalid_input", "database_role"));
    }
    let rows = projection(system, org, project, env).await?;
    let enabled_ids: Vec<_> = rows
        .connections
        .iter()
        .filter(|row| row.status == "active")
        .map(|row| row.id.as_str())
        .collect();
    let disabled_ids: Vec<String> = target.query("SELECT id::text FROM platform.connection WHERE status='active' AND NOT (id::text=ANY($1::text[]))", &[&enabled_ids]).await?.into_iter().map(|row| row.get(0)).collect();
    let ids: Vec<String> = rows.connections.iter().map(|row| row.id.clone()).collect();
    let systems: Vec<String> = rows
        .external_systems
        .iter()
        .map(|row| row.id.clone())
        .collect();
    target.execute("DELETE FROM platform.project_connection WHERE NOT (connection_id::text = ANY($1::text[])) OR project_id <> $2", &[&ids, &project]).await?;
    target
        .execute(
            "UPDATE platform.connection SET status = 'disabled' WHERE status <> 'disabled' AND NOT (id::text = ANY($1::text[]))",
            &[&ids],
        )
        .await?;
    let scopes: Vec<_> = rows
        .integration_allows
        .iter()
        .map(|row| row.scope_type.as_str())
        .collect();
    let scope_ids: Vec<_> = rows
        .integration_allows
        .iter()
        .map(|row| row.scope_id.as_str())
        .collect();
    let integrations: Vec<_> = rows
        .integration_allows
        .iter()
        .map(|row| row.integration.as_str())
        .collect();
    target.execute("DELETE FROM platform.integration_allow a WHERE NOT EXISTS (SELECT FROM unnest($1::text[], $2::text[], $3::text[]) desired(scope_type,scope_id,integration) WHERE (a.scope_type,a.scope_id,a.integration) = (desired.scope_type,desired.scope_id,desired.integration))", &[&scopes, &scope_ids, &integrations]).await?;
    crate::configuration::write_configuration(target, &rows)
        .await
        .map_err(|source| ConnectionError {
            code: "database_error",
            field: "projection",
            source: Some(Box::new(source)),
        })?;
    target
        .execute(
            "UPDATE platform.external_system SET status = 'disabled' WHERE status <> 'disabled' AND NOT (id::text = ANY($1::text[]))",
            &[&systems],
        )
        .await?;
    let mut drops = AuthDrops::default();
    let env_key = format!("{org}--{project}--{env}");
    for row in system.query("SELECT service_principal_id::text FROM registry.connection_rows($1) WHERE id::text=ANY($2::text[])", &[&org, &disabled_ids]).await? {
        drops.principal(&env_key, &row.get::<_,String>(0));
    }
    for row in system.query("SELECT id::text, subject, display_name, status, platform_domain FROM registry.connection_principal_rows($1,$2)", &[&org, &project]).await? {
        let id: String = row.get(0);
        let domain: Option<String> = row.get(4);
        let domain = domain.ok_or_else(|| refuse("invalid_input", "platform_domain"))?;
        let email = format!("{}@{domain}", row.get::<_, String>(1));
        let changed = target.execute("INSERT INTO platform.\"user\"(id, type, email, display_name, status) VALUES ($1::text::uuid, 'service', $2, $3, $4) ON CONFLICT (id) DO UPDATE SET email = EXCLUDED.email, display_name = EXCLUDED.display_name, status = EXCLUDED.status WHERE (\"user\".email, \"user\".display_name, \"user\".status) IS DISTINCT FROM (EXCLUDED.email, EXCLUDED.display_name, EXCLUDED.status)", &[&id, &email, &row.get::<_, String>(2), &row.get::<_, String>(3)]).await?;
        if changed != 0 { drops.principal(&env_key, &id); }
    }
    Ok(drops)
}

/// Store new secret material and append a generation. Existing generations stay.
pub async fn rotate(
    tx: &Transaction<'_>,
    org: &str,
    name: &str,
    secret: &str,
) -> Result<i64, ConnectionError> {
    lock_org(tx, org).await?;
    let result = configure(
        tx,
        org,
        "connection.rotate",
        &json!({"name": name, "secret": secret}),
    )
    .await?;
    Ok(result["generation"]
        .as_i64()
        .expect("database returns a generation"))
}

/// Import credentials into explicitly named, enabled connections atomically.
/// Project entries use {"connection": "name", "secret": "material"}.
pub async fn import_credentials(
    tx: &Transaction<'_>,
    project_org: &str,
    text: &str,
) -> Result<(), ConnectionError> {
    let file: Value =
        serde_json::from_str(text).map_err(|_| refuse("invalid_input", "credentials_file"))?;
    lock_org(tx, project_org).await?;
    configure(tx, project_org, "connection.import_credentials", &file).await?;
    Ok(())
}
