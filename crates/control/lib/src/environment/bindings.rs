//! Resolve application connection names against the system authority.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use tokio_postgres::GenericClient;
use wamn_schema_generator::{ConnectionDeclaration, ConnectionMode, PackageManifest};

use super::document::EnvironmentDocument;

/// An apply refusal with the binding that caused it.
#[derive(Debug)]
pub struct BindingError {
    pub kind: &'static str,
    pub context: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl fmt::Display for BindingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.context)
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
            kind: "connection_database_error",
            context: "read connection authority".into(),
            source: Some(Box::new(source)),
        }
    }
}
fn refuse(kind: &'static str, context: impl Into<String>) -> BindingError {
    BindingError {
        kind,
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
pub async fn analyse(
    system: &(impl GenericClient + Sync),
    document: &EnvironmentDocument,
    packages: &[PackageManifest],
) -> Result<Vec<Binding>, BindingError> {
    let target = system
        .query_opt(
            "SELECT id::text FROM registry.target WHERE name = $1 AND status = 'active'",
            &[&document.target],
        )
        .await?
        .ok_or_else(|| refuse("target_not_found", &document.target))?;
    let target: String = target.get(0);
    if let Some(row) = system.query_opt(
        "SELECT target_id::text FROM registry.project_envs WHERE org = $1 AND project = $2 AND env = $3",
        &[&document.org, &document.project, &document.env],
    ).await? && let Some(existing) = row.get::<_, Option<String>>(0) && existing != target {
        return Err(refuse("environment_target_changed", &document.target));
    }
    let mut result = Vec::new();
    let mut exclusive = BTreeSet::new();
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
            for row in system.query("SELECT b.alias, c.integration, b.mode FROM registry.environment_connection_binding b JOIN registry.connection c ON c.id=b.connection_id WHERE b.org=$1 AND b.project=$2 AND b.env=$3 AND b.application=$4", &[&document.org,&document.project,&document.env,application]).await? {
                let alias: String = row.get(0);
                declarations.insert(alias, ConnectionDeclaration {integration:row.get(1),mode:if row.get::<_,String>(2) == "read" {ConnectionMode::Read} else {ConnectionMode::Write}});
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
            if system
                .query_opt(
                    "SELECT code FROM registry.integrations WHERE code = $1",
                    &[&declaration.integration],
                )
                .await?
                .is_none()
            {
                return Err(refuse("integration_not_found", &declaration.integration));
            }
            let row = system.query_opt(
                "SELECT c.id::text, c.integration, i.exclusive, c.current_generation, g.parameters::text, EXISTS (SELECT FROM registry.project_connection p WHERE p.org = c.org AND p.project = $3 AND p.connection_id = c.id) FROM registry.connection c JOIN registry.integrations i ON i.code = c.integration JOIN registry.connection_generation g ON (g.connection_id, g.generation) = (c.id, c.current_generation) WHERE c.org = $1 AND c.name = $2 AND c.status = 'active'",
                &[&document.org, name, &document.project],
            ).await?.ok_or_else(|| refuse("connection_not_enabled", format!("{at}: {name}")))?;
            if !row.get::<_, bool>(5) {
                return Err(refuse("connection_not_enabled", format!("{at}: {name}")));
            }
            let integration: String = row.get(1);
            if integration != declaration.integration {
                return Err(refuse("connection_integration_mismatch", &at));
            }
            let environment = format!("{}/{}", document.project, document.env);
            for (scope, id) in [
                ("org", &document.org),
                ("project", &document.project),
                ("environment", &environment),
            ] {
                if system.query_opt("SELECT 1 FROM registry.integration_allow WHERE org = $1 AND scope_type = $2 AND scope_id = $3 AND integration = $4", &[&document.org, &scope, id, &integration]).await?.is_none() {
                    return Err(refuse("integration_not_allowed", format!("{at}: {scope} {id}")));
                }
            }
            let id: String = row.get(0);
            let is_exclusive: bool = row.get(2);
            if is_exclusive {
                if !exclusive.insert(id.clone()) {
                    return Err(refuse("exclusive_connection_bound", &at));
                }
                if system.query_opt("SELECT 1 FROM registry.environment_connection_binding WHERE target_id = $1::text::uuid AND connection_id = $2::text::uuid AND (org, project, env) <> ($3, $4, $5)", &[&target, &id, &document.org, &document.project, &document.env]).await?.is_some() {
                    return Err(refuse("exclusive_connection_bound", &at));
                }
            }
            let requirement_type = match integration.as_str() {
                "http" => wamn_catalog::RequirementType::Http,
                "gcs" | "s3" => wamn_catalog::RequirementType::Blobstore,
                _ => return Err(refuse("integration_unavailable", &integration)),
            };
            let parameters = serde_json::from_str(row.get(4)).expect("PostgreSQL JSON");
            requirement_type
                .check_definition(&parameters)
                .map_err(|error| refuse("connection_definition_invalid", error))?;
            let generation: i64 = row.get(3);
            let credential_handle =
                (integration != "gcs").then(|| format!("org:{}:{name}@{generation}", document.org));
            result.push(Binding {
                application: application.clone(),
                alias,
                connection_id: id,
                generation,
                integration,
                mode: declaration.mode,
                exclusive: is_exclusive,
                resolved: crate::org_connection::ResolvedConnection {
                    requirement_type,
                    definition: parameters,
                    credential_handle,
                    org_connection_name: Some(name.clone()),
                },
            });
        }
    }
    Ok(result)
}

/// Reserve a target's bindings atomically, including concurrent environment applies.
/// The caller holds the environment lifecycle lock and uses the system role.
pub async fn reserve(
    tx: &tokio_postgres::Transaction<'_>,
    document: &EnvironmentDocument,
    packages: &[PackageManifest],
) -> Result<Vec<Binding>, BindingError> {
    tx.query_one(
        "SELECT id FROM registry.target WHERE name = $1 FOR UPDATE",
        &[&document.target],
    )
    .await?;
    let bindings = analyse(tx, document, packages).await?;
    tx.execute("UPDATE registry.project_envs SET target_id = (SELECT id FROM registry.target WHERE name = $4) WHERE org = $1 AND project = $2 AND env = $3", &[&document.org, &document.project, &document.env, &document.target]).await?;
    tx.execute("DELETE FROM registry.environment_connection_binding WHERE org = $1 AND project = $2 AND env = $3", &[&document.org, &document.project, &document.env]).await?;
    for binding in &bindings {
        let mode = match binding.mode {
            ConnectionMode::Read => "read",
            ConnectionMode::Write => "write",
        };
        tx.execute("INSERT INTO registry.environment_connection_binding(org, project, env, application, alias, target_id, connection_id, generation, mode, exclusive) SELECT $1, $2, $3, $4, $5, id, $7::text::uuid, $8, $9, $10 FROM registry.target WHERE name = $6", &[&document.org, &document.project, &document.env, &binding.application, &binding.alias, &document.target, &binding.connection_id, &binding.generation, &mode, &binding.exclusive]).await?;
    }
    Ok(bindings)
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
    source.batch_execute("SET LOCAL ROLE wamn_system").await?;
    let mut project = super::analyse::connect(project_url).await?;
    let target = project.transaction().await?;
    target
        .batch_execute("SET LOCAL ROLE wamn_administration")
        .await?;
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
