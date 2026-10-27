//! Resolve application connection names against the system authority.

use std::collections::BTreeMap;
use std::fmt;

use tokio_postgres::GenericClient;
use wamn_control_provision::WorkloadRoleFamily;
use wamn_schema_generator::{ConnectionDeclaration, ConnectionMode, PackageManifest};

use super::document::EnvironmentDocument;

/// An apply refusal with the binding that caused it.
#[derive(Debug)]
pub struct BindingError {
    pub code: &'static str,
    pub context: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl fmt::Display for BindingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.context)
    }
}
impl std::error::Error for BindingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}
impl From<tokio_postgres::Error> for BindingError {
    fn from(source: tokio_postgres::Error) -> Self {
        Self {
            code: "connection_database_error",
            context: "read connection authority".into(),
            source: Some(Box::new(source)),
        }
    }
}
fn refuse(code: &'static str, context: impl Into<String>) -> BindingError {
    BindingError {
        code,
        context: context.into(),
        source: None,
    }
}

/// One resolved binding. Values come only from the org connection generation.
#[derive(Debug, Clone)]
pub struct Binding {
    pub application: String,
    pub alias: String,
    pub connection_id: String,
    pub generation: i64,
    pub integration: String,
    pub mode: ConnectionMode,
    pub exclusive: bool,
    pub resolved: crate::org_connection::ResolvedConnection,
}

/// Check target identity, declarations, project enablement and all three scopes.
/// This function writes nothing. The reserving transaction repeats it under a lock.
async fn requests(
    system: &(impl GenericClient + Sync),
    document: &EnvironmentDocument,
    packages: &[PackageManifest],
) -> Result<serde_json::Value, BindingError> {
    let mut requests = Vec::new();
    for (application, definition) in &document.applications {
        let mut declarations: BTreeMap<String, ConnectionDeclaration> = BTreeMap::new();
        for package in packages
            .iter()
            .filter(|package| definition.packages.contains(&package.package.id))
        {
            for (alias, declaration) in &package.connections {
                if let Some(previous) = declarations.insert(alias.clone(), declaration.clone())
                    && previous != *declaration
                {
                    return Err(refuse(
                        "connection_declaration_conflict",
                        format!("{application}.{alias}"),
                    ));
                }
            }
        }
        if document.release == super::document::DeclaredRelease::None && packages.is_empty() {
            for row in system.query("SELECT alias, integration, mode FROM registry.connection_binding_rows($1,$2,$3) WHERE application=$4", &[&document.org,&document.project,&document.env,application]).await? {
                declarations.insert(row.get(0), ConnectionDeclaration { integration: row.get(1), mode: if row.get::<_, String>(2) == "read" { ConnectionMode::Read } else { ConnectionMode::Write } });
            }
        }
        for alias in definition.connections.keys() {
            if !declarations.contains_key(alias) {
                return Err(refuse(
                    "connection_undeclared",
                    format!("{application}.{alias}"),
                ));
            }
        }
        for (alias, declaration) in declarations {
            let at = format!("{application}.{alias}");
            let name = definition
                .connections
                .get(&alias)
                .ok_or_else(|| refuse("connection_unbound", &at))?;
            requests.push(serde_json::json!({
                "application": application, "alias": alias, "name": name,
                "integration": declaration.integration, "mode": declaration.mode,
            }));
        }
    }
    Ok(serde_json::Value::Array(requests))
}

async fn resolve(
    system: &(impl GenericClient + Sync),
    document: &EnvironmentDocument,
    requests: &serde_json::Value,
    reserve: bool,
) -> Result<Vec<Binding>, BindingError> {
    let sql = if reserve {
        "SELECT registry.reserve_connection_bindings($1,$2,$3,$4,$5::text::jsonb)::text"
    } else {
        "SELECT registry.analyse_connection_bindings($1,$2,$3,$4,$5::text::jsonb)::text"
    };
    let row = system
        .query_one(
            sql,
            &[
                &document.org,
                &document.project,
                &document.env,
                &document.target,
                &requests.to_string(),
            ],
        )
        .await?;
    let result: serde_json::Value = serde_json::from_str(row.get(0)).expect("PostgreSQL JSON");
    if let Some(code) = result["error"].as_str() {
        let code = match code {
            "target_not_found" => "target_not_found",
            "environment_target_changed" => "environment_target_changed",
            "invalid_input" => "invalid_input",
            "not_found" => "not_found",
            "connection_declaration_conflict" => "connection_declaration_conflict",
            "integration_not_found" => "integration_not_found",
            "connection_not_enabled" => "connection_not_enabled",
            "connection_integration_mismatch" => "connection_integration_mismatch",
            "integration_not_allowed" => "integration_not_allowed",
            "exclusive_connection_bound" => "exclusive_connection_bound",
            _ => "connection_database_error",
        };
        return Err(refuse(
            code,
            result["context"].as_str().unwrap_or("bindings"),
        ));
    }
    result["bindings"]
        .as_array()
        .expect("database returns bindings")
        .iter()
        .map(|row| {
            let integration = row["integration"].as_str().expect("integration");
            let requirement_type = wamn_catalog::RequirementType::for_integration(integration)
                .ok_or_else(|| refuse("integration_unavailable", integration))?;
            let parameters = row["parameters"].clone();
            requirement_type
                .check_definition(&parameters)
                .map_err(|error| refuse("connection_definition_invalid", error))?;
            let generation = row["generation"].as_i64().expect("generation");
            let name = row["name"].as_str().expect("connection name");
            Ok(Binding {
                application: row["application"].as_str().expect("application").into(),
                alias: row["alias"].as_str().expect("alias").into(),
                connection_id: row["connection_id"].as_str().expect("connection id").into(),
                generation,
                integration: integration.into(),
                mode: if row["mode"] == "read" {
                    ConnectionMode::Read
                } else {
                    ConnectionMode::Write
                },
                exclusive: row["exclusive"].as_bool().expect("exclusive"),
                resolved: crate::org_connection::ResolvedConnection {
                    requirement_type,
                    definition: parameters,
                    credential_handle: (integration != "gcs")
                        .then(|| format!("org:{}:{name}@{generation}", document.org)),
                    org_connection_name: Some(name.into()),
                },
            })
        })
        .collect()
}

/// Check binding authority through the control function without writing rows.
pub async fn analyse(
    system: &(impl GenericClient + Sync),
    document: &EnvironmentDocument,
    packages: &[PackageManifest],
) -> Result<Vec<Binding>, BindingError> {
    let requests = requests(system, document, packages).await?;
    resolve(system, document, &requests, false).await
}

/// Reserve bindings through the control function in the caller's control transaction.
pub async fn reserve(
    tx: &tokio_postgres::Transaction<'_>,
    document: &EnvironmentDocument,
    packages: &[PackageManifest],
) -> Result<Vec<Binding>, BindingError> {
    let requests = requests(tx, document, packages).await?;
    // Validate the compiled connector before the reserving function writes.
    resolve(tx, document, &requests, false).await?;
    resolve(tx, document, &requests, true).await
}

/// The project catalog identity keeps equal aliases in separate applications apart.
pub fn key(application: &str, alias: &str) -> String {
    format!("{application}/{alias}")
}

/// Refresh the read-only project projection through the administration role.
pub async fn project_authority(
    system_url: &str,
    project_url: &str,
    document: &EnvironmentDocument,
) -> anyhow::Result<wamn_platform_identity::auth_changes::AuthDrops> {
    let mut system = super::analyse::connect(system_url).await?;
    let source = system.transaction().await?;
    crate::connection_configuration::use_role(&source, WorkloadRoleFamily::Control).await?;
    let mut project = super::analyse::connect(project_url).await?;
    let target = project.transaction().await?;
    crate::connection_configuration::use_role(&target, WorkloadRoleFamily::Administration).await?;
    target
        .batch_execute(&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
        .await?;
    let drops = wamn_platform_identity::connections::project(
        &source,
        &target,
        &document.org,
        &document.project,
        &document.env,
    )
    .await?;
    target.commit().await?;
    source.commit().await?;
    Ok(drops)
}
