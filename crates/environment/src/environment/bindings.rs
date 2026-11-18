//! Resolve project integrations for application workload configuration.

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
    pub interface: String,
    pub components: std::collections::BTreeSet<String>,
    pub runtime_type: wamn_catalog::RuntimeType,
    pub target_type: String,
    pub connection_id: String,
    pub authority: Option<String>,
    pub integration_type: wamn_registry::integration_code::IntegrationCode,
    pub exclusive: bool,
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
    project: &mut sqlx::PgConnection,
    document: &EnvironmentDocument,
    packages: impl IntoIterator<Item = &PackageManifest>,
) -> Result<Vec<Binding>, BindingError> {
    let packages = packages.into_iter().collect::<Vec<_>>();
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
    let environment_target = sqlx::query_scalar::<_, Option<String>>(
        "SELECT target_id::text FROM registry.environments WHERE org=$1 AND env=$2",
    )
    .bind(&document.org)
    .bind(&document.env)
    .fetch_optional(&mut *system)
    .await?;
    if matches!(&environment_target, Some(None)) {
        return Err(refuse(
            "environment_target_missing",
            format!(
                "environment {}/{} has no target",
                document.org, document.env
            ),
        ));
    }
    let environment_target = environment_target.flatten();
    for (name, version) in document.integrations.iter().filter(|(name, _)| {
        applications.iter().any(|(_, definition)| {
            definition
                .connections
                .values()
                .any(|integration| integration.as_str() == *name)
        })
    }) {
        let connection=sqlx::query("SELECT c.id::text,c.integration_type,g.parameters,c.active_generation,c.target_id::text,c.authority FROM platform.connection c JOIN platform.connection_generation g ON (g.connection_id,g.generation)=(c.id,c.active_generation) WHERE c.name=$1 AND c.status='active'")
            .bind(name).fetch_optional(&mut *project).await?.ok_or_else(||refuse("connection_unavailable",name))?;
        let connection_target: &str = connection.get(4);
        let environment_target = environment_target
            .as_deref()
            .ok_or_else(|| refuse("connection_unavailable", name))?;
        if connection_target != environment_target {
            return Err(refuse(
                "connection_target_mismatch",
                format!(
                    "connection {name} target {connection_target} does not match environment {}/{} target {environment_target}",
                    document.org, document.env
                ),
            ));
        }
        let row=sqlx::query("SELECT $1::text AS id,t.code,t.runtime_type,$3::jsonb AS parameters,v.parameter_schema,v.digest,v.operations,$6::bigint AS generation,t.exclusive,v.exports,v.write_interfaces,destination.type FROM registry.integration_type t JOIN registry.target destination ON destination.id=$4::text::uuid AND destination.name=$5 AND destination.status='active' JOIN registry.environments e ON e.org=$7 AND e.env=$8 LEFT JOIN registry.integration_type_version v ON v.integration_type=t.code AND v.version=$9 WHERE t.code=$2")
            .bind(connection.get::<String,_>(0)).bind(connection.get::<String,_>(1)).bind(connection.get::<serde_json::Value,_>(2)).bind(connection_target).bind(&document.target).bind(connection.get::<i64,_>(3)).bind(&document.org).bind(&document.env).bind(version.version)
            .fetch_optional(&mut *system).await?.ok_or_else(||refuse("connection_unavailable",name))?;
        if row.get::<bool, _>(8) && row.get::<&str, _>(11) == "cloud" {
            return Err(refuse(
                "exclusive_connection_requires_local_target",
                format!("connection {name}: exclusive connections belong to local targets"),
            ));
        }
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
        if !wamn_connection::connections::administration::validates(&parameters, &schema) {
            return Err(refuse("connection_parameters_invalid", name));
        }
        selected.insert(name.as_str(), (row, connection.get::<Option<String>, _>(5)));
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
            let (row, authority) = selected
                .get(integration.as_str())
                .ok_or_else(|| refuse("integration_version_missing", &at))?;
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
                    org_connection_name: Some(integration.as_str().to_owned()),
                })
            } else {
                None
            };
            bindings.push(Binding {
                application: application.clone(),
                alias: name.to_string(),
                interface: declaration.interface,
                components: packages
                    .iter()
                    .filter(|p| definition.packages.contains(&p.package.id))
                    .flat_map(|p| {
                        p.components
                            .iter()
                            .filter(|(_, c)| c.connections.contains(&name))
                            .map(|(component, _)| format!("{}-{component}", p.package.id))
                    })
                    .collect(),
                runtime_type: if runtime == "host" {
                    wamn_catalog::RuntimeType::Host
                } else {
                    wamn_catalog::RuntimeType::Wasm
                },
                target_type: row.get(11),
                connection_id: row.get(0),
                authority: authority.clone(),
                integration_type,
                exclusive: row.get(8),
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
