//! Resolve project integrations and write release-targeted environment bindings.

use sqlx::Row as _;
pub(super) mod plan;
pub(crate) use plan::exports_satisfy;
pub use plan::key;
use plan::{mode_satisfies, refuse};

use std::collections::BTreeMap;
use std::fmt;

use wamn_catalog::manifest::{ConnectionDeclaration, ConnectionMode, PackageManifest};

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
impl From<sqlx::Error> for BindingError {
    fn from(source: sqlx::Error) -> Self {
        Self {
            code: "connection_database_error",
            context: "read connection authority".into(),
            source: Some(Box::new(source)),
        }
    }
}

/// One declared name resolved to an active environment connection and version.
#[derive(Debug, Clone)]
pub struct Binding {
    pub application: wamn_catalog::ApplicationName,
    pub alias: String,
    pub connection_id: String,
    pub integration_type: wamn_registry::integration_code::IntegrationCode,
    pub exclusive: bool,
    pub version: i32,
    pub generation: i64,
    pub mode: ConnectionMode,
    pub digest: Option<wamn_digest::Digest>,
    pub exports: Vec<String>,
    pub write_interfaces: Vec<String>,
    pub resolved: Option<crate::org_connection::ResolvedConnection>,
}

/// Read project authority without creating or changing a connection.
pub async fn analyse(
    system: &mut sqlx::PgConnection,
    document: &EnvironmentDocument,
    packages: &[PackageManifest],
) -> Result<Vec<Binding>, BindingError> {
    let mut selected = BTreeMap::new();
    // Only referenced integrations of the selected package closure need connections.
    let applications: Vec<_> = document
        .applications
        .iter()
        .filter(|(_, definition)| {
            definition.current != super::document::DeclaredRelease::None
                && packages
                    .iter()
                    .any(|package| definition.packages.contains(&package.package.id))
        })
        .collect();
    for (name, version) in document.integrations.iter().filter(|(name, _)| {
        applications.iter().any(|(_, definition)| {
            definition
                .connections
                .values()
                .any(|integration| &integration.name == *name)
        })
    }) {
        let row =
            sqlx::query(wamn_registry::sql::CONTROL_ENVIRONMENT_SRC_ENVIRONMENT_BINDINGS_ANALYSE)
                .bind(&document.org)
                .bind(&document.project)
                .bind(&document.env)
                .bind(&document.target)
                .bind(name)
                .bind(version.version)
                .fetch_optional(&mut *system)
                .await?
                .ok_or_else(|| refuse("connection_unavailable", name))?;
        let runtime: String = row.get(2);
        let schema: Option<serde_json::Value> = row.get(4);
        let schema = schema.ok_or_else(|| refuse("integration_version_unpublished", name))?;
        let digest: Option<wamn_digest::Digest> = row.get(5);
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
        if !wamn_connections::connections::administration::validates(&parameters, &schema) {
            return Err(refuse("connection_parameters_invalid", name));
        }
        selected.insert(name, row);
    }
    let mut bindings = Vec::new();
    for (application, definition) in applications {
        let mut declarations: BTreeMap<
            wamn_catalog::canonical_name::CanonicalName,
            ConnectionDeclaration,
        > = BTreeMap::new();
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
        for name in definition.connections.keys() {
            if !declarations.contains_key(name.as_str()) {
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
                .get(name.as_str())
                .ok_or_else(|| refuse("connection_unbound", &at))?;
            let row = selected
                .get(&integration.name)
                .ok_or_else(|| refuse("integration_version_missing", &at))?;
            let version = document.integrations[&integration.name].version;
            if integration
                .version
                .is_some_and(|requested| requested != version)
            {
                return Err(refuse("integration_version_missing", &at));
            }
            let integration_type: wamn_registry::integration_code::IntegrationCode = row.get(1);
            let exports: Vec<String> = row.get(9);
            if !exports_satisfy(&exports, std::slice::from_ref(&declaration.interface)) {
                return Err(refuse("integration_import_unsatisfied", at));
            }
            let write_interfaces: Vec<String> = row.get(10);
            if !mode_satisfies(declaration.mode, &declaration.interface, &write_interfaces) {
                return Err(refuse("connection_read_imports_write_interface", at));
            }
            let runtime: String = row.get(2);
            let resolved = if runtime == "host" {
                let requirement_type =
                    wamn_catalog::RequirementType::for_integration(&integration_type)
                        .ok_or_else(|| refuse("integration_version_unserved", &at))?;
                Some(crate::org_connection::ResolvedConnection {
                    requirement_type,
                    definition: row.get(3),
                    credential_handle: (integration_type.as_str() != "gcs")
                        .then(|| format!("org:{}:{integration}", document.org)),
                    org_connection_name: Some(integration.name.clone()),
                })
            } else {
                None
            };
            bindings.push(Binding {
                application: application.clone(),
                alias: name.to_string(),
                connection_id: row.get(0),
                integration_type,
                exclusive: row.get(8),
                version,
                generation: row.get(7),
                mode: declaration.mode,
                digest: row.get(5),
                exports,
                write_interfaces,
                resolved,
            });
        }
    }
    Ok(bindings)
}

/// Write the release's versions and names after its requirements exist.
/// The caller binds the transaction's principal before writing.
pub async fn write(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project_env_key: &wamn_registry::ProjectEnvKey,
    digest: &wamn_digest::Digest,
    bindings: &[Binding],
) -> anyhow::Result<()> {
    for binding in bindings {
        let active = sqlx::query("SELECT 1 FROM platform.connection c JOIN platform.integration i ON i.id=c.integration_id \
             WHERE c.id=$1::text::uuid AND c.status='active' AND i.status='active'").bind(&binding.connection_id).fetch_optional(&mut **tx).await?.is_some();
        anyhow::ensure!(
            active,
            "connection projection {} is absent or inactive; run env connections",
            binding.connection_id
        );
        sqlx::query("INSERT INTO catalog.release_connection(project_env_key,manifest_digest,connection_id,version) VALUES ($1,$2,$3::text::uuid,$4) ON CONFLICT (project_env_key,manifest_digest,connection_id) DO NOTHING").bind(project_env_key).bind(digest).bind(&binding.connection_id).bind(binding.version).execute(&mut **tx).await.map(|result| result.rows_affected())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::plan::interface_compatible;
    #[test]
    fn a_read_requirement_refuses_a_write_interface() {
        use wamn_catalog::manifest::ConnectionMode::{Read, Write};
        let writes = vec!["wamn:modbus/write@1.0.0".to_owned()];
        assert!(!super::mode_satisfies(
            Read,
            "wamn:modbus/write@1.0.0",
            &writes
        ));
        assert!(super::mode_satisfies(
            Write,
            "wamn:modbus/write@1.0.0",
            &writes
        ));
        assert!(super::mode_satisfies(
            Read,
            "wamn:modbus/read@1.0.0",
            &writes
        ));
    }

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
