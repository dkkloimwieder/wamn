//! The final delete transaction retains retry inputs until credential cleanup ends.

use anyhow::Context as _;
use wamn_control_registry::Triple;

/// Destroy credential files and remove the environment's registry rows last.
/// The caller holds the lifecycle lock and removes external resources first.
/// SQL changes roll back if file cleanup fails or this operation is cancelled.
/// Without a supplied file store, open the configured store when files exist.
///
/// # Errors
///
/// Returns an error if a database operation or credential file removal fails.
pub async fn delete_registry_rows(
    system: &mut tokio_postgres::Client,
    triple: &Triple,
    instance: &str,
    file_store: Option<&dyn wamn_credential_store::Store>,
) -> anyhow::Result<Vec<String>> {
    let (org, project, env) = (
        triple.org.as_str(),
        triple.project.as_str(),
        triple.env.as_str(),
    );
    let env_key = wamn_control_provision::project_env_key(org, project, env);
    let mut lines = Vec::new();
    let transaction = crate::provision_project_env::provisioning_transaction(system)
        .await
        .context("begin the row delete")?;
    transaction
        .query_one("SELECT set_config('app.env_key', $1, true)", &[&env_key])
        .await
        .context("claim the tenant")?;
    let credentials = transaction
        .query(
            wamn_control_registry::sql::delete_environment_connections_sql(),
            &[&org, &project, &env],
        )
        .await
        .context("delete the environment's connections")?;
    let files = super::plan::credential_files(
        credentials
            .iter()
            .map(|row| (row.get(1), row.get(2), row.get(3))),
    );
    if !files.is_empty() {
        let configured;
        let store = if let Some(store) = file_store {
            store
        } else {
            configured =
                wamn_credential_store::open("file").context("open the file credential store")?;
            configured.as_ref()
        };
        for (reference, version) in &files {
            store
                .destroy(reference, version)
                .await
                .with_context(|| format!("destroy credential file {reference}"))?;
        }
        lines.push(format!("destroyed {} credential files", files.len()));
    }
    let env_key_table: bool = transaction
        .query_one(
            "SELECT to_regclass('catalog.tenant_environments') IS NOT NULL",
            &[],
        )
        .await
        .context("probe catalog.tenant_environments")?
        .get(0);
    if env_key_table {
        transaction
            .execute(
                "DELETE FROM catalog.tenant_environments \
                  WHERE org = $1 AND project = $2 AND env = $3 AND instance_suffix = $4",
                &[&org, &project, &env, &instance],
            )
            .await
            .context("delete the catalog.tenant_environments row")?;
    }
    transaction
        .execute(
            "DELETE FROM wamn_authority.author_login_tenants \
              WHERE org_id=$1 AND project_id=$2 AND environment=$3",
            &[&org, &project, &env],
        )
        .await
        .context("delete the environment's control login mappings")?;
    transaction
        .execute(
            "DELETE FROM registry.project_envs WHERE org = $1 AND project = $2 AND env = $3",
            &[&org, &project, &env],
        )
        .await
        .context("delete the registry.project_envs row")?;
    transaction
        .commit()
        .await
        .context("commit the row delete")?;
    lines.push(format!(
        "deleted the control rows of tenant {env_key}, {} connection credentials and the environment row of {triple}",
        credentials.len()
    ));
    Ok(lines)
}
