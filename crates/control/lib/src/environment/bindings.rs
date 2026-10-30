//! Resolve project integrations and write release-scoped environment bindings.

use std::collections::BTreeMap;
use std::fmt;

use tokio_postgres::GenericClient;
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

/// One declared name resolved to an active environment connection and version.
#[derive(Debug, Clone)]
pub struct Binding {
    pub application: String,
    pub alias: String,
    pub connection_id: String,
    pub integration_type: String,
    pub version: i32,
    pub generation: i64,
    pub mode: ConnectionMode,
    pub digest: Option<String>,
    pub exports: Vec<String>,
    pub resolved: Option<crate::org_connection::ResolvedConnection>,
}

/// Read project authority without creating or changing a connection.
pub async fn analyse(
    system: &(impl GenericClient + Sync),
    document: &EnvironmentDocument,
    packages: &[PackageManifest],
) -> Result<Vec<Binding>, BindingError> {
    let mut selected = BTreeMap::new();
    for (name, version) in &document.integrations {
        let row = system
            .query_opt(
                "SELECT * FROM registry.resolve_environment_connection($1,$2,$3,$4,$5,$6)",
                &[
                    &document.org,
                    &document.project,
                    &document.env,
                    &document.target,
                    name,
                    &version.version,
                ],
            )
            .await?
            .ok_or_else(|| refuse("connection_unavailable", name))?;
        let runtime: String = row.get(2);
        let schema: Option<serde_json::Value> = row.get(4);
        let schema = schema.ok_or_else(|| refuse("integration_version_unpublished", name))?;
        let digest: Option<String> = row.get(5);
        if (runtime == "host" && version.version != 1)
            || (runtime == "wasm" && digest.is_none())
            || runtime == "native"
        {
            return Err(refuse("integration_version_unserved", name));
        }
        let mut parameters: serde_json::Value = row.get(3);
        let values = parameters
            .as_object_mut()
            .ok_or_else(|| refuse("connection_parameters_invalid", name))?;
        values.remove("auth");
        if !wamn_platform_identity::connections::administration::validates(&parameters, &schema) {
            return Err(refuse("connection_parameters_invalid", name));
        }
        selected.insert(name, row);
    }
    let mut bindings = Vec::new();
    for (application, definition) in &document.applications {
        let mut declarations: BTreeMap<String, ConnectionDeclaration> = BTreeMap::new();
        for package in packages
            .iter()
            .filter(|p| definition.packages.contains(&p.package.id))
        {
            for (name, declaration) in &package.connections {
                if let Some(previous) = declarations.insert(name.clone(), declaration.clone())
                    && previous != *declaration
                {
                    return Err(refuse(
                        "connection_declaration_conflict",
                        format!("{application}.{name}"),
                    ));
                }
            }
        }
        if document.release == super::document::DeclaredRelease::None {
            continue;
        }
        for name in definition.connections.keys() {
            if !declarations.contains_key(name) {
                return Err(refuse(
                    "connection_undeclared",
                    format!("{application}.{name}"),
                ));
            }
        }
        for (name, declaration) in declarations {
            let at = format!("{application}.{name}");
            let integration = definition
                .connections
                .get(&name)
                .ok_or_else(|| refuse("connection_unbound", &at))?;
            let row = selected
                .get(integration)
                .ok_or_else(|| refuse("integration_version_missing", &at))?;
            let integration_type: String = row.get(1);
            let exports: Vec<String> = row.get(9);
            if !exports_satisfy(&exports, &declaration.interfaces) {
                return Err(refuse("integration_import_unsatisfied", at));
            }
            let runtime: String = row.get(2);
            let resolved = if runtime == "host" {
                let requirement_type =
                    wamn_catalog::RequirementType::for_integration(&integration_type)
                        .ok_or_else(|| refuse("integration_version_unserved", &at))?;
                Some(crate::org_connection::ResolvedConnection {
                    requirement_type,
                    definition: row.get(3),
                    credential_handle: (integration_type != "gcs")
                        .then(|| format!("org:{}:{integration}", document.org)),
                    org_connection_name: Some(integration.clone()),
                })
            } else {
                None
            };
            bindings.push(Binding {
                application: application.clone(),
                alias: name,
                connection_id: row.get(0),
                integration_type,
                version: document.integrations[integration].version,
                generation: row.get(7),
                mode: declaration.mode,
                digest: row.get(5),
                exports,
                resolved,
            });
        }
    }
    Ok(bindings)
}

pub(crate) fn exports_satisfy(exports: &[String], required: &[String]) -> bool {
    required.iter().all(|interface| {
        exports
            .iter()
            .any(|export| interface_compatible(interface, export))
    })
}

pub(crate) fn interface_compatible(import: &str, export: &str) -> bool {
    use wit_parser::{PackageName, ParsedUsePath, parse_use_path};
    let (Ok(ParsedUsePath::Package(import, name)), Ok(ParsedUsePath::Package(export, export_name))) =
        (parse_use_path(import), parse_use_path(export))
    else {
        return false;
    };
    if import.namespace != export.namespace || import.name != export.name || name != export_name {
        return false;
    }
    match (import.version, export.version) {
        (Some(import), Some(export)) => {
            export >= import
                && PackageName::version_compat_track(&import)
                    == PackageName::version_compat_track(&export)
        }
        (None, None) => true,
        _ => false,
    }
}

/// Write the release's versions and names after its requirements exist.
pub async fn write(
    tx: &tokio_postgres::Transaction<'_>,
    env_key: &str,
    digest: &str,
    document: &EnvironmentDocument,
    bindings: &[Binding],
) -> anyhow::Result<()> {
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await?;
    let mut names = Vec::new();
    for (name, integration) in &document.integrations {
        let row=tx.query_one("SELECT c.id::text,c.integration_type FROM platform.connection c JOIN platform.integration i ON i.id=c.integration_id WHERE i.name=$1 AND i.status='active' AND c.status='active'",&[name]).await?;
        let id: String = row.get(0);
        tx.execute("INSERT INTO catalog.release_connection(env_key,manifest_digest,connection_id,version) VALUES ($1,$2,$3::text::uuid,$4) ON CONFLICT (env_key,manifest_digest,connection_id) DO UPDATE SET version=EXCLUDED.version WHERE catalog.release_connection.version IS DISTINCT FROM EXCLUDED.version", &[&env_key,&digest,&id,&integration.version]).await?;
    }
    for row in tx.query("SELECT component_digest,package_id,name,interfaces FROM catalog.connection_requirement WHERE env_key=$1", &[&env_key]).await? {
        let package: String=row.get(1);
        let Some(application)=wamn_catalog::application_for_package(&document.applications,&package) else {continue};
        let name: String=row.get(2);
        let Some(binding)=bindings.iter().find(|b| b.application==application && b.alias==name) else {continue};
        let required: Vec<String>=row.get(3);
        anyhow::ensure!(exports_satisfy(&binding.exports,&required),"integration_import_unsatisfied: {application}.{name}");
        let component: String=row.get(0);
        names.push(serde_json::json!({"component_digest":component,"package_id":package,"name":name}));
        tx.execute("INSERT INTO catalog.connection_binding(env_key,manifest_digest,component_digest,package_id,name,connection_id) SELECT $1,$2,$3,$4,$5,$6::text::uuid WHERE EXISTS(SELECT FROM catalog.releases r CROSS JOIN LATERAL jsonb_array_elements(convert_from(r.canonical_bytes,'UTF8')::jsonb->'components') c WHERE r.env_key=$1 AND r.manifest_digest=$2 AND c->>'package-id'=$4 AND c->'descriptor'->>'component-digest'=$3) ON CONFLICT (env_key,manifest_digest,component_digest,package_id,name) DO UPDATE SET connection_id=EXCLUDED.connection_id WHERE catalog.connection_binding.connection_id IS DISTINCT FROM EXCLUDED.connection_id", &[&env_key,&digest,&component,&package,&name,&binding.connection_id]).await?;
    }
    let names = serde_json::to_string(&names)?;
    tx.execute("DELETE FROM catalog.connection_binding b WHERE env_key=$1 AND manifest_digest=$2 AND NOT EXISTS(SELECT FROM jsonb_to_recordset($3::text::jsonb) AS desired(component_digest text,package_id text,name text) WHERE (desired.component_digest,desired.package_id,desired.name)=(b.component_digest,b.package_id,b.name))", &[&env_key,&digest,&names]).await?;
    let integrations: Vec<&str> = document.integrations.keys().map(String::as_str).collect();
    tx.execute("DELETE FROM catalog.release_connection r WHERE env_key=$1 AND manifest_digest=$2 AND NOT EXISTS(SELECT FROM platform.connection c JOIN platform.integration i ON i.id=c.integration_id WHERE c.id=r.connection_id AND i.name=ANY($3::text[]))", &[&env_key,&digest,&integrations]).await?;
    Ok(())
}

/// The project catalog identity keeps equal names in separate applications apart.
pub fn key(application: &str, alias: &str) -> String {
    format!("{application}/{alias}")
}

/// Refresh the environment projection under the caller's lifecycle lock.
pub async fn project_authority(
    system_url: &str,
    project_url: &str,
    document: &EnvironmentDocument,
) -> anyhow::Result<wamn_platform_identity::auth_changes::AuthDrops> {
    let mut system = super::analyse::connect(system_url).await?;
    let source = system.transaction().await?;
    source.batch_execute("SET LOCAL ROLE wamn_db_owner").await?;
    let mut project = super::analyse::connect(project_url).await?;
    let target = project.transaction().await?;
    crate::connection_configuration::use_role(
        &target,
        wamn_control_provision::WorkloadRoleFamily::Administration,
    )
    .await?;
    target
        .batch_execute(&wamn_control_provision::bind_platform_principal_sql(
            wamn_control_provision::PlatformComponent::Provisioning,
        ))
        .await?;
    let drops = wamn_platform_identity::connection_projection::project(
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

#[cfg(test)]
mod tests {
    use super::interface_compatible;
    #[test]
    fn imports_require_the_exported_interface_and_a_compatible_version() {
        assert!(interface_compatible(
            "acme:meter/read@1.1.0",
            "acme:meter/read@1.2.0"
        ));
        assert!(interface_compatible(
            "acme:meter/read@0.2.1",
            "acme:meter/read@0.2.3"
        ));
        for export in [
            "acme:meter/write@1.1.0",
            "other:meter/read@1.1.0",
            "acme:meter/read@2.0.0",
            "acme:meter/read@1.0.0",
        ] {
            assert!(!interface_compatible("acme:meter/read@1.1.0", export));
        }
        assert!(!interface_compatible(
            "acme:meter/read@0.2.0",
            "acme:meter/read@0.3.0"
        ));
    }
}
