//! Organization chart inputs come from the organization and its complete policy set.
use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_control_provision::{EnvPolicy, Org, Placement};
use wamn_registry::Env;

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

pub fn recovery_values(
    org: &Org,
    owner: &Env,
    policies: &[EnvPolicy],
    target: &str,
    target_time: Option<&str>,
) -> anyhow::Result<Value> {
    ensure!(
        org.placement == Placement::Dedicated,
        "a shared PostgreSQL cluster cannot be recovered as an organization cluster"
    );
    let source = wamn_registry::name::cluster(&org.id, owner);
    ensure!(
        source != target,
        "recovery must use a different cluster name"
    );
    let policy = policies
        .iter()
        .find(|policy| &policy.name == owner)
        .context("recovery owner has no environment policy")?;
    Ok(
        json!({"org":org.id,"clusters":[{"name":target,"policy":policy,"recovery":{"source":source,"targetTime":target_time}}]}),
    )
}

/// Apply the complete registered policy set while retaining the system database and declared logins.
pub async fn apply(
    registry_input: &str,
    namespace: &str,
    values: &mut Value,
) -> anyhow::Result<()> {
    crate::credentials::with_chart_lock(registry_input, namespace, async {
        let target = wamn_kubernetes::Target {
            kubeconfig: std::env::var_os("KUBECONFIG")
                .map(std::path::PathBuf::from)
                .unwrap_or_default(),
            context: String::new(),
            namespace: namespace.into(),
        };
        let installed = crate::platform::installed::InstalledPlatform::read(&target)
            .await?
            .context("platform is not installed")?;
        let reference = installed.values["platform"]["chart"]
            .as_str()
            .context("installed platform has no chart reference")?;
        let chart = crate::platform::artifact_plan::select(std::path::Path::new(reference))?;
        let name = wamn_registry::name::resource_suffix("org", namespace);
        if let Some(mut previous) =
            crate::platform::installed::release_values(&target, namespace, &name).await?
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
            namespace,
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
