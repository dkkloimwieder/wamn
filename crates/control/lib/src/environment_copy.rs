//! The two steps of the copy-environment saga that read the source
//! environment (`docs/plan/platform-ui.md` §5.3, `wamn-zua8.4`).
//!
//! The control login has no grant into a project database, so the worker
//! reads the source as `wamn_provisioner` (owner ruling of 2026-10-03).
//! `read-source` reads the head release of the source, its packages and the
//! connections it binds, and applies the replacements of the request.
//! `copy-roles` runs before `select-release`. It copies the authored roles
//! and their directly selected permissions, and the grant writes the closure
//! of each root from the release that the copy published.

use anyhow::{Context as _, bail};
use tokio_postgres::{Client, NoTls};
use wamn_catalog::{ReleaseClosures, RequirementType};
use wamn_control_provision::saga::{ConnectionReplacement, PackageReference, SourceRead};

use crate::environment_saga::{SourceConnection, apply_replacements};

/// The built-in role, which is never authored and holds no permission.
const ADMIN_ROLE: &str = "admin";

async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .context("connect to the source project database")?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            tracing::warn!(%error, "source database connection ended");
        }
    });
    Ok(client)
}

/// The tenant and head release of `environment`, the one environment of its
/// project database.
async fn source_head(client: &Client, environment: &str) -> anyhow::Result<(String, i32)> {
    let rows = client
        .query(
            "SELECT tenant_id, effective_release_id FROM catalog.effective_release_heads \
              WHERE environment = $1",
            &[&environment],
        )
        .await
        .context("read the head release of the source")?;
    match rows.as_slice() {
        [row] => Ok((row.get(0), row.get(1))),
        [] => bail!("the source environment {environment} has no head release"),
        _ => bail!(
            "the source environment {environment} has {} head releases, one per tenant",
            rows.len()
        ),
    }
}

/// Step 1, `read-source`: the head release of `source_env`, its packages,
/// and the connections it binds with the replacements applied.
pub async fn read_source(
    source_url: &str,
    source_env: &str,
    replacements: &[ConnectionReplacement],
) -> anyhow::Result<SourceRead> {
    let client = connect(source_url).await?;
    let (tenant, release) = source_head(&client, source_env).await?;
    let packages = client
        .query(
            "SELECT package_id, package_version FROM catalog.effective_release_packages \
              WHERE tenant_id = $1 AND effective_release_id = $2 ORDER BY package_id",
            &[&tenant, &release],
        )
        .await
        .context("read the packages of the source release")?
        .iter()
        .map(|row| PackageReference {
            package_id: row.get(0),
            version: row.get(1),
        })
        .collect();
    let rows = client
        .query(
            "SELECT DISTINCT b.instance_id, b.store_alias, i.requirement_type, \
                    g.definition_json::text, g.credential_set_handle \
               FROM catalog.connection_bindings b \
               JOIN catalog.connection_instances i \
                 ON i.tenant_id = b.tenant_id AND i.environment = b.environment \
                AND i.instance_id = b.instance_id \
               JOIN catalog.connection_generations g \
                 ON g.tenant_id = i.tenant_id AND g.environment = i.environment \
                AND g.instance_id = i.instance_id AND g.generation = i.active_generation \
              WHERE b.tenant_id = $1 AND b.effective_release_id = $2 \
                AND b.binding_status = 'active' \
              ORDER BY b.instance_id, b.store_alias",
            &[&tenant, &release],
        )
        .await
        .context("read the connections of the source release")?;
    let mut source = Vec::new();
    for row in rows {
        let instance_id: String = row.get(0);
        let type_name: String = row.get(2);
        let requirement_type: RequirementType = serde_json::from_value(serde_json::Value::String(
            type_name.clone(),
        ))
        .with_context(|| {
            format!(
                "the source connection {instance_id} has the type {type_name}, which \
                         bind-connection cannot bind"
            )
        })?;
        source.push(SourceConnection {
            definition: serde_json::from_str(&row.get::<_, String>(3))
                .with_context(|| format!("decode the definition of {instance_id}"))?,
            instance_id,
            alias: row.get(1),
            requirement_type,
            credential_handle: row.get(4),
        });
    }
    Ok(SourceRead {
        release,
        packages,
        connections: apply_replacements(source, replacements)?,
    })
}

/// `copy-roles`: the authored roles of `source_env` and their directly
/// selected permissions, in the tenant `tenant` of the target database. The
/// grant writes the closure of each root from `closures`. A role or a root
/// that the target holds already stays, so the step runs again after a
/// failure.
pub async fn copy_roles(
    source_url: &str,
    source_env: &str,
    target_url: &str,
    tenant: &str,
    closures: &ReleaseClosures,
) -> anyhow::Result<()> {
    let source = connect(source_url).await?;
    let (source_tenant, _) = source_head(&source, source_env).await?;
    let roles: Vec<String> = source
        .query(
            "SELECT name FROM app_system.roles WHERE tenant_id = $1 AND name <> $2 ORDER BY name",
            &[&source_tenant, &ADMIN_ROLE],
        )
        .await
        .context("read the roles of the source")?
        .iter()
        .map(|row| row.get(0))
        .collect();
    let roots: Vec<(String, String)> = source
        .query(
            "SELECT role_name, permission FROM app_system.permissions \
              WHERE tenant_id = $1 AND permission = required_by \
              ORDER BY role_name, permission",
            &[&source_tenant],
        )
        .await
        .context("read the permission roots of the source")?
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    let mut target = connect(target_url).await?;
    let transaction = target.transaction().await.context("begin the role copy")?;
    for role in &roles {
        crate::role_permissions::create_role(&transaction, tenant, role)
            .await
            .with_context(|| format!("create the role {role}"))?;
    }
    for (role, reference) in &roots {
        crate::role_permissions::grant_permission(&transaction, tenant, role, reference, closures)
            .await
            .with_context(|| format!("grant {reference} to the role {role}"))?;
    }
    transaction.commit().await.context("commit the role copy")
}
