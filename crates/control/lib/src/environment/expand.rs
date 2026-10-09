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
use wamn_catalog::RequirementType;
use wamn_schema_control::BareSchemaName;

use super::Platform;
use super::analyse::{Analysis, connect};
use super::document::EnvironmentDocument;
use super::ensure::{RUN_SCHEMA, ensure_cdc, project_url};
use crate::bind_connection::{BindConnectionRequest, ConnectionInstance, bind, ensure_instance};

/// Write the policy name and the route host where they differ.
const UPDATE_ROW_SQL: &str = "\
UPDATE registry.project_envs SET policy_name = $4, route_host = $5 \
 WHERE org = $1 AND project = $2 AND env = $3 \
   AND (policy_name, route_host) IS DISTINCT FROM ($4, $5)";

/// The release's components that declare a store alias, in the project
/// database.
const REQUIREMENTS_SQL: &str = "\
SELECT component_digest, store_alias FROM catalog.connection_requirements \
 WHERE tenant_id = $1 AND component_digest = ANY($2)";

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
) -> anyhow::Result<()> {
    let triple = document.triple();
    let tenant =
        wamn_control_provision::project_env_tenant(&document.org, &document.project, &document.env);
    let system = connect(&platform.system_database_url).await?;
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
    drop(system);

    // The durability projection of the declared policy (R2).
    let project_url = project_url(platform, &triple).await?;
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
    let mut targets = Vec::new();
    if let Some(release) = &analysis.authorities.release {
        let manifests =
            super::stage::stage(platform, document, analysis, release, &project_url).await?;
        ensure_cdc(platform, document, &manifests).await?;
        let components: Vec<String> = release
            .manifest
            .components
            .iter()
            .map(|component| component.digest.as_str().to_owned())
            .collect();
        project
            .query_one("SELECT set_config('app.tenant', $1, false)", &[&tenant])
            .await
            .context("claim the tenant")?;
        for row in project
            .query(REQUIREMENTS_SQL, &[&tenant, &components])
            .await
            .context("read the release's connection requirements")?
        {
            targets.push((
                release.digest.clone(),
                row.get::<_, String>(0),
                row.get::<_, String>(1),
            ));
        }
    }
    for (instance_id, declared) in &document.connections {
        let instance = ConnectionInstance {
            database_url: project_url.clone(),
            tenant: tenant.clone(),
            environment: document.env.clone(),
            instance_id: instance_id.clone(),
            requirement_type: declared.requirement_type,
            definition: declared.definition.clone(),
            credential_handle: credential_handle(
                declared.requirement_type,
                &declared.definition,
                instance_id,
            ),
        };
        let bound: Vec<_> = targets
            .iter()
            .filter(|(_, _, alias)| alias == instance_id)
            .collect();
        if bound.is_empty() {
            ensure_instance(&instance)
                .await
                .with_context(|| format!("ensure connection {instance_id}"))?;
            continue;
        }
        for (manifest_digest, component_digest, alias) in bound {
            bind(&BindConnectionRequest {
                database_url: instance.database_url.clone(),
                tenant: instance.tenant.clone(),
                environment: instance.environment.clone(),
                instance_id: instance.instance_id.clone(),
                requirement_type: instance.requirement_type,
                definition: instance.definition.clone(),
                credential_handle: instance.credential_handle.clone(),
                manifest_digest: manifest_digest.clone(),
                component_digest: component_digest.clone(),
                store_alias: alias.clone(),
            })
            .await
            .with_context(|| format!("bind connection {instance_id} to {component_digest}"))?;
        }
    }
    let unbound: Vec<&str> = targets
        .iter()
        .map(|(_, _, alias)| alias.as_str())
        .filter(|alias| !document.connections.contains_key(*alias))
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
