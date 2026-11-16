//! Organization chart inputs come from the organization and its complete policy set.
use anyhow::Context as _;
use serde_json::{Value, json};
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

/// Apply the complete registered policy set while retaining the system database and declared logins.
pub async fn apply(
    registry_input: &str,
    namespace: &wamn_registry::OrgId,
    values: &mut Value,
) -> anyhow::Result<()> {
    crate::credentials::with_chart_lock(registry_input, namespace, async {
        let target = wamn_kubernetes::Target {
            kubeconfig: std::env::var_os("KUBECONFIG").map_or_default(std::path::PathBuf::from),
            context: String::new(),
            namespace: namespace.to_string(),
        };
        let installed = crate::platform::installed::InstalledPlatform::read(&target)
            .await?
            .context("platform is not installed")?;
        let reference = installed.values["platform"]["chart"]
            .as_str()
            .context("installed platform has no chart reference")?;
        let chart = crate::platform::artifact_plan::select(std::path::Path::new(reference))?;
        let name = wamn_registry::name::resource_suffix("org", namespace.as_str());
        if let Some(mut previous) =
            crate::platform::installed::release_values(&target, namespace.as_str(), &name).await?
        {
            for cluster in previous["clusters"]
                .take()
                .as_array_mut()
                .into_iter()
                .flatten()
                .filter(|cluster| cluster["database"] == "wamn_system")
            {
                values["clusters"]
                    .as_array_mut()
                    .context("org clusters")?
                    .push(cluster.take());
            }
        }
        let declarations = crate::roles::registered(registry_input, namespace).await?;
        values["managedRoles"] = crate::roles::values(&declarations)?;
        values["platform"] = json!({"chart":reference});
        let mut entry = crate::environment::entry(
            &chart.directory().join("platform/org"),
            namespace.as_str(),
            &name,
            std::mem::take(values),
        )?;
        let directory = tempfile::tempdir()?;
        let result =
            crate::platform::resources::apply_release(&target, &entry, directory.path()).await;
        *values = entry["values"].take();
        result?;
        crate::credentials::prepare(&target, &declarations).await
    })
    .await
}
