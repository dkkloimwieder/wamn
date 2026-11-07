use super::{BTreeMap, Client};
use anyhow::Context as _;

pub(in super::super) fn revision_client(
    values: &serde_json::Value,
    host: &str,
) -> anyhow::Result<Option<Client>> {
    match (
        values["webClients"][host].as_str(),
        values["webDigests"][host].as_str(),
    ) {
        (None, None) => Ok(None),
        (Some(package), Some(web)) => Ok(Some(Client {
            package: package.to_owned(),
            web: web.to_owned(),
        })),
        _ => anyhow::bail!("revision must name both webClients and webDigests for host {host}"),
    }
}

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
