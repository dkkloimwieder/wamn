//! Order infrastructure and shared platform installation.

use std::path::Path;

use serde_json::{Value, json};
use wamn_kubernetes::{Target, org_resources};

use crate::org_install::api_destinations;
use crate::platform_install::{install_infrastructure, install_platform};

/// Install the selected releases in dependency order.
pub async fn install(
    installer: &Target,
    charts: &Path,
    values: &Path,
    infrastructure: bool,
    app_chart: &str,
    registry_egress: Option<Value>,
) -> anyhow::Result<()> {
    if infrastructure {
        install_infrastructure(installer, charts, values).await?;
    }
    if let Some(registry_egress) = registry_egress {
        let api_server =
            api_destinations(org_resources::api_addresses(installer.client.client()).await?)?;
        let overrides = json!({
            "appChart": app_chart,
            "registryEgress": registry_egress,
            "apiServer": api_server,
        });
        install_platform(installer, charts, values, &overrides).await?;
    }
    Ok(())
}
