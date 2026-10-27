//! Step 5 of `apply`: expand (docs/plan/platform-deploy.md §10.1 step 5, R2,
//! R22 (1), (5) to (7), epic decision D4).
//!
//! The policy name and the route host of the environment row are written
//! together, then the durability projection is reconciled from the declared
//! policy. With a release, the package stage runs (§10.2) and CDC is ensured
//! for its packages. Then every declared connection is ensured: its instance
//! is enabled at the generation of its definition, and the release's
//! components that declare its instance id as their store alias are bound to
//! it, keyed by the release digest. Nothing is retired: the live set still
//! reads its own bindings.

use anyhow::{Context as _, ensure};
#[cfg(test)]
use wamn_catalog::RequirementType;
use wamn_schema_control::BareSchemaName;

use super::Platform;
use super::analyse::{Analysis, connect};
use super::document::EnvironmentDocument;
use super::ensure::{RUN_SCHEMA, ensure_cdc, project_url};
use crate::bind_connection::{
    BindConnectionRequest, ConnectionInstance, bind_with_mode, ensure_instance,
};

/// Write the policy name and the route host where they differ.
const UPDATE_ROW_SQL: &str = "\
UPDATE registry.project_envs SET policy_name = $4, route_host = $5 \
 WHERE org = $1 AND project = $2 AND env = $3 \
   AND (policy_name, route_host) IS DISTINCT FROM ($4, $5)";

/// Expand the environment toward the document.
///
/// # Errors
///
/// When a write fails or the package stage refuses. Nothing is written to
/// Kubernetes before this step succeeds.
pub async fn expand(
    platform: &Platform,
    document: &EnvironmentDocument,
    analysis: &Analysis,
) -> anyhow::Result<std::collections::BTreeMap<String, String>> {
    let triple = document.triple();
    let tenant =
        wamn_control_provision::project_env_tenant(&document.org, &document.project, &document.env);
    let mut system = connect(&platform.system_database_url).await?;
    system
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("SET ROLE wamn_system")?;
    system
        .execute(
            UPDATE_ROW_SQL,
            &[
                &document.org,
                &document.project,
                &document.env,
                &document.policy,
                &document.route_host,
            ],
        )
        .await
        .context("write the policy name and the route host")?;
    let tx = system.transaction().await?;
    tx.batch_execute("SET LOCAL ROLE wamn_control").await?;
    let bindings = super::bindings::reserve(&tx, document, &analysis.packages).await?;
    tx.commit().await?;
    drop(system);

    // The durability projection of the declared policy (R2).
    let project_url = project_url(platform, &triple).await?;
    let drops =
        super::bindings::project_authority(&platform.system_database_url, &project_url, document)
            .await?;
    platform.auth_changes().await?.publish(drops).await;
    let source = crate::verification_policy::read_authoritative_environment_policy(
        &platform.system_database_url,
        &document.org,
        &document.policy,
        true,
    )
    .await?;
    let project = connect(&project_url).await?;
    crate::reconcile_run_plane::converge_environment_policy(
        &project,
        &BareSchemaName::new(RUN_SCHEMA.to_owned())?,
        &tenant,
        &document.env,
        &source,
        true,
    )
    .await
    .context("reconcile the durability projection")?;

    // The package stage, CDC of its packages, then the release's bindings,
    // which name the release cache row the stage wrote.
    let mut clients = std::collections::BTreeMap::new();
    if let Some(release) = &analysis.authorities.release {
        let analysed = analysis
            .authorities
            .project
            .as_ref()
            .map(|project| project.installed.clone())
            .unwrap_or_default();
        let manifests = super::stage::stage(
            platform,
            document,
            &analysed,
            &release.digest,
            &release.manifest,
            &project_url,
        )
        .await?;
        clients = super::clients::resolve(
            document,
            manifests.iter().filter_map(|manifest| {
                manifest
                    .client_package
                    .as_ref()
                    .map(|_| manifest.package.id.as_str())
            }),
        )?;
        ensure_cdc(platform, document, &manifests).await?;
    }
    let mut targets = Vec::new();
    if let Some(release) = &analysis.authorities.release {
        for component in &release.manifest.components {
            let application = wamn_catalog::application_for_package(
                &document.applications,
                &component.package_id,
            )
            .context("component has no application")?;
            for requirement in &component.descriptor.connections {
                let binding = bindings
                    .iter()
                    .find(|binding| {
                        binding.application == application
                            && binding.alias == requirement.store_alias
                    })
                    .context("connection_unbound")?;
                targets.push(ConnectionTarget {
                    instance_id: super::bindings::key(application, &binding.alias),
                    manifest_digest: release.digest.clone(),
                    component_digest: component.descriptor.component_digest.clone(),
                    store_alias: binding.alias.clone(),
                    package_id: component.package_id.clone(),
                    mode: binding.mode,
                });
            }
        }
    }
    let connections = bindings
        .into_iter()
        .map(|binding| {
            (
                super::bindings::key(&binding.application, &binding.alias),
                binding.resolved,
            )
        })
        .collect();
    materialize_connections(&project_url, document, &connections, &targets).await?;
    Ok(clients)
}

/// One release component's application-qualified connection binding.
#[derive(Debug, Clone)]
pub struct ConnectionTarget {
    pub instance_id: String,
    pub manifest_digest: String,
    pub component_digest: String,
    pub store_alias: String,
    pub package_id: String,
    pub mode: wamn_schema_generator::ConnectionMode,
}

/// Materialize one apply snapshot through the existing instance and bind paths.
pub async fn materialize_connections(
    project_url: &str,
    document: &EnvironmentDocument,
    connections: &std::collections::BTreeMap<String, crate::org_connection::ResolvedConnection>,
    targets: &[ConnectionTarget],
) -> anyhow::Result<()> {
    for (instance_id, declared) in connections {
        let instance = ConnectionInstance {
            database_url: project_url.to_owned(),
            org: document.org.clone(),
            project: document.project.clone(),
            environment: document.env.clone(),
            instance_id: instance_id.clone(),
            requirement_type: declared.requirement_type,
            definition: declared.definition.clone(),
            credential_handle: declared.credential_handle.clone(),
            org_connection_name: declared.org_connection_name.clone(),
        };
        ensure_instance(&instance)
            .await
            .with_context(|| format!("ensure connection {instance_id}"))?;
        for target in targets
            .iter()
            .filter(|target| target.instance_id == *instance_id)
        {
            bind_with_mode(
                &BindConnectionRequest {
                    database_url: instance.database_url.clone(),
                    org: instance.org.clone(),
                    project: instance.project.clone(),
                    environment: instance.environment.clone(),
                    instance_id: instance.instance_id.clone(),
                    requirement_type: instance.requirement_type,
                    definition: instance.definition.clone(),
                    credential_handle: instance.credential_handle.clone(),
                    org_connection_name: instance.org_connection_name.clone(),
                    manifest_digest: target.manifest_digest.clone(),
                    component_digest: target.component_digest.clone(),
                    store_alias: target.store_alias.clone(),
                },
                target.mode,
                &target.package_id,
            )
            .await
            .with_context(|| {
                format!(
                    "bind connection {instance_id} to {}",
                    target.component_digest
                )
            })?;
        }
    }
    let unbound: Vec<&str> = targets
        .iter()
        .filter(|target| !connections.contains_key(&target.instance_id))
        .map(|target| target.instance_id.as_str())
        .collect();
    ensure!(
        unbound.is_empty(),
        "the release requires the connections {unbound:?}, which the document does not declare"
    );
    Ok(())
}

/// The credential handle of a declared instance. Credentials are provisioned,
/// never declared: a definition that needs a handle names the host-held
/// credential set by its instance id.
#[cfg(test)]
fn credential_handle(
    requirement_type: RequirementType,
    definition: &serde_json::Value,
    instance_id: &str,
) -> Option<String> {
    requirement_type
        .check_credential_handle(definition, None)
        .is_err()
        .then(|| instance_id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_http_definition_takes_its_instance_id_as_the_handle() {
        let definition = json!({
            "primary-authority": "http://http-fixture:8080/", "tls-verification": "disabled"
        });
        let handle = credential_handle(RequirementType::Http, &definition, "erp");
        assert_eq!(handle.as_deref(), Some("erp"));
        RequirementType::Http
            .check_credential_handle(&definition, handle.as_deref())
            .expect("the instance id is a valid handle");
    }

    #[test]
    fn a_definition_that_needs_a_handle_takes_its_instance_id() {
        let definition = json!({"endpoint": "http://x", "container": "c", "prefix": "p"});
        let handle = credential_handle(RequirementType::Blobstore, &definition, "labels");
        assert!(
            RequirementType::Blobstore
                .check_credential_handle(&definition, handle.as_deref())
                .is_ok()
        );
    }
}
