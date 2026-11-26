//! Resolve project integrations for application workload configuration.

pub(super) mod plan;
pub(crate) use plan::exports_satisfy;
use plan::{mode_satisfies, refuse};

use std::collections::BTreeMap;
use std::fmt;

use crate::app_chart::{binding::Binding, resolved_connection::ResolvedConnection};
use crate::env_sql_effects::PostgresSession;
use wamn_component_policy::PackageManifest;
use wamn_package::manifest::{ConnectionDeclaration, ConnectionMode};

use super::document::EnvDocument;

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
impl From<anyhow::Error> for BindingError {
    fn from(source: anyhow::Error) -> Self {
        Self {
            code: "connection_database_error",
            context: "read connection authority".into(),
            source: Some(source.into_boxed_dyn_error()),
        }
    }
}

/// Read project authority without creating or changing a connection.
pub(super) async fn analyse(
    system: &PostgresSession,
    project: &PostgresSession,
    document: &EnvDocument,
    packages: impl IntoIterator<Item = &PackageManifest>,
) -> Result<Vec<Binding>, BindingError> {
    let packages = packages.into_iter().collect::<Vec<_>>();
    let mut selected = BTreeMap::new();
    // Only referenced integrations of the selected package closure need connections.
    let apps: Vec<_> = document
        .apps
        .iter()
        .filter(|(_, definition)| {
            definition.current != super::document::DeclaredRelease::None
                && packages
                    .iter()
                    .any(|package| definition.packages.contains(&package.package.id))
        })
        .collect();
    let env_target = system.env_target(&document.triple()).await?;
    if matches!(&env_target, Some(None)) {
        return Err(refuse(
            "env_target_missing",
            format!(
                "environment {}/{} has no target",
                document.settings.org, document.settings.env
            ),
        ));
    }
    let env_target = env_target.flatten();
    for (name, version) in document.settings.integrations.iter().filter(|(name, _)| {
        apps.iter().any(|(_, definition)| {
            definition
                .connections
                .values()
                .any(|integration| integration.as_str() == *name)
        })
    }) {
        let connection = project
            .connection_binding(name)
            .await?
            .ok_or_else(|| refuse("connection_unavailable", name))?;
        let connection_target = connection.target.as_str();
        let env_target = env_target
            .as_deref()
            .ok_or_else(|| refuse("connection_unavailable", name))?;
        if connection_target != env_target {
            return Err(refuse(
                "connection_target_mismatch",
                format!(
                    "connection {name} target {connection_target} does not match environment {}/{} target {env_target}",
                    document.settings.org, document.settings.env
                ),
            ));
        }
        let row = system
            .integration_binding(
                &document.triple(),
                &document.settings.target,
                version.version,
                &connection,
            )
            .await?
            .ok_or_else(|| refuse("connection_unavailable", name))?;
        if row.exclusive && row.placement == "cloud" {
            return Err(refuse(
                "exclusive_connection_requires_local_target",
                format!("connection {name}: exclusive connections belong to local targets"),
            ));
        }
        let runtime = row.runtime.as_str();
        let schema = row
            .schema
            .as_ref()
            .ok_or_else(|| refuse("integration_version_unpublished", name))?;
        let digest = row
            .digest
            .as_deref()
            .map(str::parse::<wamn_digest::Digest>)
            .transpose()
            .map_err(anyhow::Error::from)?;
        if (runtime == "host" && version.version != 1)
            || (runtime == "wasm" && digest.is_none())
            || runtime == "native"
        {
            return Err(refuse("integration_version_unserved", name));
        }
        let mut parameters = connection.parameters.clone();
        let values = parameters
            .as_object_mut()
            .ok_or_else(|| refuse("connection_parameters_invalid", name))?;
        values.remove("auth");
        if !wamn_connection::administration::validates(&parameters, schema) {
            return Err(refuse("connection_parameters_invalid", name));
        }
        selected.insert(name.as_str(), (row, connection, digest));
    }
    let mut bindings = Vec::new();
    for (app, definition) in apps {
        let mut declarations: BTreeMap<
            wamn_package::canonical_name::CanonicalName,
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
                        format!("{app}.{name}"),
                    ));
                }
            }
        }
        for name in definition.connections.keys() {
            if !declarations.contains_key(name.as_str()) {
                return Err(refuse("connection_undeclared", format!("{app}.{name}")));
            }
        }
        for (name, declaration) in declarations {
            let at = format!("{app}.{name}");
            let integration = definition
                .connections
                .get(name.as_str())
                .ok_or_else(|| refuse("connection_unbound", &at))?;
            let (row, connection, digest) = selected
                .get(integration.as_str())
                .ok_or_else(|| refuse("integration_version_missing", &at))?;
            let integration_type: wamn_registry::integration_code::IntegrationCode =
                row.integration_type.parse().map_err(anyhow::Error::from)?;
            let exports = &row.exports;
            if !exports_satisfy(exports, std::slice::from_ref(&declaration.interface)) {
                return Err(refuse("integration_import_unsatisfied", at));
            }
            let write_interfaces = &row.write_interfaces;
            if !mode_satisfies(declaration.mode, &declaration.interface, write_interfaces) {
                return Err(refuse("connection_read_imports_write_interface", at));
            }
            let runtime = row.runtime.as_str();
            let resolved = if runtime == "host" {
                let requirement_type =
                    wamn_connection_type::RequirementType::for_integration(&integration_type)
                        .ok_or_else(|| refuse("integration_version_unserved", &at))?;
                Some(ResolvedConnection {
                    requirement_type,
                    definition: connection.parameters.clone(),
                    credential_handle: (integration_type.as_str() != "gcs")
                        .then(|| format!("org:{}:{integration}", document.settings.org)),
                    org_connection_name: Some(integration.as_str().to_owned()),
                })
            } else {
                None
            };
            bindings.push(Binding {
                app: app.clone(),
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
                    wamn_connection_type::RuntimeType::Host
                } else {
                    wamn_connection_type::RuntimeType::Wasm
                },
                target_type: row.placement.clone(),
                connection_id: connection.id.clone(),
                authority: connection.authority.clone(),
                integration_type,
                exclusive: row.exclusive,
                generation: connection.generation,
                mode: declaration.mode,
                digest: *digest,
                exports: exports.clone(),
                write_interfaces: write_interfaces.clone(),
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
        use wamn_package::manifest::ConnectionMode::{Read, Write};
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
