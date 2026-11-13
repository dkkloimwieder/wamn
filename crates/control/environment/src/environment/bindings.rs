//! Resolve project integrations and write release-scoped environment bindings.

pub(super) mod plan;
pub(crate) use plan::exports_satisfy;
pub use plan::key;
use plan::{mode_satisfies, refuse};

use std::collections::BTreeMap;
use std::fmt;

use tokio_postgres::GenericClient;
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
impl From<tokio_postgres::Error> for BindingError {
    fn from(source: tokio_postgres::Error) -> Self {
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
    pub application: String,
    pub alias: String,
    pub connection_id: String,
    pub integration_type: String,
    pub exclusive: bool,
    pub version: i32,
    pub generation: i64,
    pub mode: ConnectionMode,
    pub digest: Option<String>,
    pub exports: Vec<String>,
    pub write_interfaces: Vec<String>,
    pub resolved: Option<crate::org_connection::ResolvedConnection>,
}

/// Read project authority without creating or changing a connection.
pub async fn analyse(
    system: &(impl GenericClient + Sync),
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
                .any(|integration| integration == *name)
        })
    }) {
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
    for (application, definition) in applications {
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
                exclusive: row.get(8),
                version: document.integrations[integration].version,
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

/// Write the release's versions and names after its requirements exist. A
/// changed row records the memory key of the release in `drops`.
pub async fn write(
    tx: &tokio_postgres::Transaction<'_>,
    env_key: &str,
    digest: &str,
    document: &EnvironmentDocument,
    bindings: &[Binding],
    drops: &mut wamn_broker::connection_changes::ConnectionDrops,
) -> anyhow::Result<()> {
    let mut changed = 0;
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await?;
    for binding in bindings {
        let active = tx.query_opt(
            "SELECT 1 FROM platform.connection c JOIN platform.integration i ON i.id=c.integration_id \
             WHERE c.id=$1::text::uuid AND c.status='active' AND i.status='active'",
            &[&binding.connection_id],
        ).await?.is_some();
        anyhow::ensure!(
            active,
            "connection projection {} is absent or inactive; run env connections",
            binding.connection_id
        );
        changed += tx.execute("INSERT INTO catalog.release_connection(env_key,manifest_digest,connection_id,version) VALUES ($1,$2,$3::text::uuid,$4) ON CONFLICT (env_key,manifest_digest,connection_id) DO NOTHING", &[&env_key,&digest,&binding.connection_id,&binding.version]).await?;
    }
    for row in tx
        .query(
            "SELECT q.component_digest,q.package_id,q.name,q.interface,q.mode \
         FROM catalog.connection_requirement q WHERE q.env_key=$1 AND EXISTS (\
         SELECT FROM catalog.releases r CROSS JOIN LATERAL \
         jsonb_array_elements(convert_from(r.canonical_bytes,'UTF8')::jsonb->'components') c \
         WHERE r.env_key=$1 AND r.manifest_digest=$2 AND c->>'package-id'=q.package_id \
         AND c->'descriptor'->>'component-digest'=q.component_digest)",
            &[&env_key, &digest],
        )
        .await?
    {
        let package: String = row.get(1);
        let application = document.application_for_package(&package).ok_or_else(|| {
            anyhow::anyhow!("release package {package} has no sole application owner")
        })?;
        let name: String = row.get(2);
        let binding = bindings
            .iter()
            .find(|b| b.application == application && b.alias == name)
            .ok_or_else(|| anyhow::anyhow!("connection_unbound: {application}.{name}"))?;
        let required: String = row.get(3);
        anyhow::ensure!(
            exports_satisfy(&binding.exports, std::slice::from_ref(&required)),
            "integration_import_unsatisfied: {application}.{name}"
        );
        let mode: String = row.get(4);
        anyhow::ensure!(
            mode != "read"
                || mode_satisfies(ConnectionMode::Read, &required, &binding.write_interfaces),
            "connection_read_imports_write_interface: {application}.{name}"
        );
        let component: String = row.get(0);
        changed += tx.execute("INSERT INTO catalog.connection_binding(env_key,manifest_digest,component_digest,package_id,name,connection_id) VALUES ($1,$2,$3,$4,$5,$6::text::uuid) ON CONFLICT (env_key,manifest_digest,component_digest,package_id,name) DO NOTHING", &[&env_key,&digest,&component,&package,&name,&binding.connection_id]).await?;
    }
    if changed > 0 {
        drops.release(env_key, digest);
    }
    Ok(())
}

/// Remove explicit retired releases after testing ownership, live hosts and pinned work.
/// The caller holds the operation grant lock and supplies a fresh host observation.
pub async fn contract(
    tx: &tokio_postgres::Transaction<'_>,
    env_key: &str,
    application: &str,
    declared: &wamn_catalog::EnvironmentApplications,
    retired: &[&str],
    live: &[&str],
    drops: &mut wamn_broker::connection_changes::ConnectionDrops,
) -> anyhow::Result<Vec<String>> {
    anyhow::ensure!(
        !live.contains(&"unlabelled"),
        "cannot retire bindings with unknown live releases"
    );
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await?;
    let mut removed = Vec::new();
    for digest in retired {
        if live.contains(digest) {
            continue;
        }
        let row = tx.query_one(
            "SELECT canonical_bytes FROM catalog.releases WHERE env_key=$1 AND manifest_digest=$2",
            &[&env_key, digest],
        ).await?;
        let (manifest, actual) = wamn_catalog::ServingManifest::from_canonical_bytes(row.get(0))?;
        anyhow::ensure!(
            actual.as_str() == *digest,
            "retired release digest mismatch"
        );
        let owners: Vec<_> = declared
            .iter()
            .filter(|(_, membership)| {
                !manifest.release.packages.is_empty()
                    && manifest
                        .release
                        .packages
                        .iter()
                        .all(|package| membership.packages.contains(package.package_id()))
            })
            .map(|(name, _)| name.as_str())
            .collect();
        anyhow::ensure!(
            owners == [application],
            "retired release {digest} has no sole owner {application}"
        );
        let needed: bool = tx
            .query_one(
                "SELECT EXISTS (SELECT FROM wamn_run.runs WHERE env_key=$1 AND manifest_digest=$2 \
             AND status NOT IN ('completed','failed','infrastructure-failure'))",
                &[&env_key, digest],
            )
            .await?
            .get(0);
        if needed {
            continue;
        }
        for table in ["catalog.connection_binding", "catalog.release_connection"] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE env_key=$1 AND manifest_digest=$2"),
                &[&env_key, digest],
            )
            .await?;
        }
        drops.release(env_key, digest);
        removed.push((*digest).to_owned());
    }
    Ok(removed)
}

/// Refresh shared connection projections during provisioning or explicit connection writes.
pub async fn project_authority(
    system_url: &str,
    project_url: &str,
    document: &EnvironmentDocument,
) -> anyhow::Result<wamn_broker::auth_changes::AuthDrops> {
    let mut system = wamn_package_control::database::connect(system_url).await?;
    let source = system.transaction().await?;
    let mut project = super::analyse::connect_project(project_url).await?;
    let target = project.transaction().await?;
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
