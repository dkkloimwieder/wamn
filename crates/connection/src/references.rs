//! Refuse deletion before system authority or projected rows change.

use crate::administration::{ConnectionError, refuse, store_error};
use crate::connection_credentials::WriteCredentials;
use serde_json::Value;

/// A deployed workload retains authority for every connection in its config.
pub fn refuse_app_references(
    apps: &[wamn_app::app_config::AppConfig],
    id: &str,
) -> Result<(), ConnectionError> {
    if apps
        .iter()
        .flat_map(|app| &app.components)
        .flat_map(|component| &component.connections)
        .any(|binding| binding.connection_id == id)
    {
        return Err(refuse("referenced", "binding"));
    }
    Ok(())
}

/// Destroy every private object after the connection and projected rows disappear.
pub async fn destroy_credentials(
    store: &dyn WriteCredentials,
    credentials: &Value,
) -> Result<(), ConnectionError> {
    for credential in credentials.as_array().into_iter().flatten() {
        store
            .destroy(
                credential["ref"].as_str().expect("database reference"),
                credential["version"].as_str().expect("database version"),
            )
            .await
            .map_err(store_error)?;
    }
    Ok(())
}
