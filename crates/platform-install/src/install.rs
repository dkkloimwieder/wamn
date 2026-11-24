//! Order infrastructure and shared platform installation.

use std::path::Path;

use serde_json::{Value, json};
use wamn_kubernetes::Target;

use crate::platform_install::{install_infrastructure, install_platform};

/// Install the selected releases in dependency order.
pub async fn install(
    installer: &Target,
    charts: &Path,
    values: &Path,
    infrastructure: bool,
    application_chart: &str,
    registry_egress: Option<Value>,
) -> anyhow::Result<()> {
    if infrastructure {
        install_infrastructure(installer, charts, values).await?;
    }
    if let Some(registry_egress) = registry_egress {
        let mut overrides = json!({"applicationChart": application_chart});
        overrides["registryEgress"] = registry_egress;
        install_platform(installer, charts, values, &overrides).await?;
    }
    Ok(())
}
