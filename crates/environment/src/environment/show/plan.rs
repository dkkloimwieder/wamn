use anyhow::Context as _;
use std::collections::BTreeMap;

/// The applications one revision's values keep. A revision written before
/// the values carried them keeps none.
pub(in super::super) fn revision_applications(
    values: &serde_json::Value,
) -> anyhow::Result<wamn_catalog::EnvironmentApplications> {
    match values.get("applications") {
        None | Some(serde_json::Value::Null) => Ok(BTreeMap::new()),
        Some(applications) => serde_json::from_value(applications.clone())
            .context("read the applications of the Helm revision"),
    }
}
