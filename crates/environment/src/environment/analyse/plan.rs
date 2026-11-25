use super::EnvironmentDocument;
use wamn_registry::EnvironmentRow;

/// Resolve the project endpoint from the instance suffix in the environment row.
pub(crate) fn project_url(
    document: &EnvironmentDocument,
    row: &EnvironmentRow,
) -> anyhow::Result<String> {
    wamn_postgres::database::connection_for(&document.triple(), &row.instance_suffix).to_json()
}
