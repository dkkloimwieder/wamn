//! Order infrastructure and shared platform installation.

use std::path::Path;

use serde_json::{Value, json};

/// Install each shared release and wait for its required resources.
pub trait PlatformInstallation {
    fn install_infrastructure(
        &self,
        charts: &Path,
        values: &Path,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;

    fn install_platform(
        &self,
        charts: &Path,
        values: &Path,
        overrides: &Value,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
}

/// Install the selected releases in dependency order.
pub async fn install(
    installer: &impl PlatformInstallation,
    charts: &Path,
    values: &Path,
    infrastructure: bool,
    application_chart: &str,
    registry_egress: Option<Value>,
) -> anyhow::Result<()> {
    if infrastructure {
        installer.install_infrastructure(charts, values).await?;
    }
    if let Some(registry_egress) = registry_egress {
        let mut overrides = json!({"applicationChart": application_chart});
        overrides["registryEgress"] = registry_egress;
        installer
            .install_platform(charts, values, &overrides)
            .await?;
    }
    Ok(())
}
