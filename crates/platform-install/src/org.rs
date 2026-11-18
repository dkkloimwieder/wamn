//! Organization chart inputs come from the organization and its complete policy set.
use anyhow::Context as _;
use serde_json::{Value, json};
use sqlx::Connection as _;
use wamn_provision::{EnvPolicy, Org, Placement};

pub fn values(org: &Org, policies: &[EnvPolicy]) -> anyhow::Result<Value> {
    let mut clusters = Vec::new();
    if org.placement == Placement::Dedicated {
        for policy in policies {
            let owner = policy.owner();
            let name = wamn_registry::name::cluster(&org.id, owner);
            if clusters.iter().any(|entry: &Value| entry["name"] == name) {
                continue;
            }
            let policy = policies
                .iter()
                .find(|policy| &policy.name == owner)
                .context("recovery owner has no environment policy")?;
            clusters.push(json!({"name":name,"policy":policy}));
        }
    }
    Ok(json!({"org":org.id,"clusters":clusters}))
}

/// Apply the registered clusters and complete managed-role roster for an org.
pub async fn apply(
    target: &wamn_kubernetes::Target,
    chart: &std::path::Path,
    registry_input: &str,
    org: &wamn_registry::OrgId,
) -> anyhow::Result<()> {
    remove_roles(target, chart, registry_input, org, &[]).await
}

/// Reconcile the org chart with the requested logins marked absent.
pub async fn remove_roles(
    target: &wamn_kubernetes::Target,
    chart: &std::path::Path,
    registry_input: &str,
    org: &wamn_registry::OrgId,
    retired: &[wamn_identity_protocol::LoginName],
) -> anyhow::Result<()> {
    crate::credentials::with_chart_lock(registry_input, org, async {
        let mut connection = wamn_postgres::connect(registry_input).await?;
        let mut snapshot = connection.begin().await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            wamn_provision::bind_platform_principal_sql(
                wamn_provision::PlatformComponent::Provisioning,
            ),
        ))
        .execute(&mut *snapshot)
        .await?;
        let placement = wamn_registry::deployment::org_placement(&mut snapshot, org)
            .await?
            .context("org is not registered")?;
        let placement = match placement.placement_type.as_str() {
            "dedicated" => Placement::Dedicated,
            "pooled" => Placement::Pooled {
                pool: placement.pool_cluster.context("registered org pool")?,
            },
            _ => anyhow::bail!("invalid registered org placement"),
        };
        let policies = sqlx::query(sqlx::AssertSqlSafe(
            wamn_registry::sql::select_env_policies_sql(),
        ))
        .bind(org)
        .fetch_all(&mut *snapshot)
        .await?
        .iter()
        .map(wamn_provision::env_policies::env_policy_from_row)
        .collect::<anyhow::Result<Vec<_>>>()?;
        snapshot.commit().await?;
        let mut values = values(
            &Org {
                id: org.clone(),
                placement,
            },
            &policies,
        )?;
        let system = crate::platform::installation::organization_values(org)["clusters"][0].take();
        values["clusters"]
            .as_array_mut()
            .context("org clusters")?
            .push(system);
        let mut declarations = crate::roles::registered(registry_input, org).await?;
        for role in declarations
            .values_mut()
            .flatten()
            .filter(|role| retired.contains(&role.name))
        {
            role.ensure = "absent".into();
        }
        let system = crate::roles::platform(org, "wamn-pg", "wamn_system")?;
        values["loginSecrets"] = system
            .values()
            .flatten()
            .map(|role| json!({"name":role.password_secret.name,"username":role.name}))
            .collect::<Vec<_>>()
            .into();
        values["managedRoles"] = crate::roles::values(&declarations)?;
        let name = wamn_registry::name::resource_suffix("org", org.as_str());
        let directory = tempfile::tempdir()?;
        // Project login Secrets are submitted by the project chart.
        // Waiting for them here would put both chart upgrades in a cycle.
        let path = directory.path().join("org-values.yaml");
        std::fs::write(&path, serde_json::to_vec(&values)?)?;
        wamn_kubernetes::helm::recover_pending(target, &name, wamn_kubernetes::helm::Wait::None)
            .await?;
        wamn_kubernetes::helm::upgrade_without_wait(
            target,
            &chart.join("platform/org"),
            &name,
            &path,
            std::time::Duration::from_secs(600),
        )
        .await
    })
    .await
}
