//! Scenario-only credential loading and capability adaptation.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context as _;
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;

/// Credentials selected specifically for one scenario-worker composition.
#[derive(Clone)]
pub struct ScenarioCredentials {
    plugin: Arc<WamnCredentials>,
}

impl std::fmt::Debug for ScenarioCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScenarioCredentials")
            .finish_non_exhaustive()
    }
}

impl ScenarioCredentials {
    /// Consume this adapter and return the runtime capability plugin.
    pub fn into_plugin(self) -> Arc<WamnCredentials> {
        self.plugin
    }
}

/// Load scenario credentials from an explicit product-scenario source.
///
/// An absent source produces an empty, fail-closed vault. A specified source
/// must exist and parse successfully; unlike an optional serving secret mount,
/// a missing scenario fixture is a scenario configuration error.
pub fn load_scenario_credentials(path: Option<&Path>) -> anyhow::Result<ScenarioCredentials> {
    let plugin = match path {
        Some(path) => {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("read scenario credentials file {}", path.display()))?;
            let projects =
                WamnCredentials::projects_from_json(&text).context("parse scenario credentials")?;
            WamnCredentials::from_projects(projects)
        }
        None => WamnCredentials::empty(),
    };
    Ok(ScenarioCredentials {
        plugin: Arc::new(plugin),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_source_is_an_explicit_empty_adapter() {
        let credentials = load_scenario_credentials(None).expect("empty adapter");
        let debug = format!("{credentials:?}");
        assert_eq!(debug, "ScenarioCredentials { .. }");
    }

    #[test]
    fn missing_explicit_source_is_an_error() {
        let error =
            load_scenario_credentials(Some(Path::new("/definitely/missing/scenario-creds.json")))
                .expect_err("explicitly selected missing source must fail");
        assert!(error.to_string().contains("read scenario credentials file"));
    }
}
