use super::EnvDocument;
use wamn_registry::EnvRow;

/// Resolve the project endpoint from the instance suffix in the environment row.
pub(crate) fn project_url(document: &EnvDocument, row: &EnvRow) -> anyhow::Result<String> {
    wamn_postgres::database::connection_for(&document.triple(), &row.instance_suffix).to_json()
}
