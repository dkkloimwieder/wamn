//! Route a JetStream delivery by its source package, entity and row operation.

use serde::Deserialize;

use super::{Component, bindings};
use bindings::exports::wasmcloud::nats::jetstream_handler::{Guest, MessageHandle};

#[derive(Deserialize)]
struct App {
    components: Vec<Sibling>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Sibling {
    name: String,
    data_change: Option<Handler>,
}

#[derive(Deserialize)]
struct Handler {
    models: Vec<Model>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Model {
    owner_package: String,
    entity: String,
    ops: Vec<String>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event {
    Cdc {
        package_id: String,
        entity: String,
        op: String,
    },
    Derived,
}

impl Guest for Component {
    async fn handle_message(handle: MessageHandle) -> Result<(), String> {
        let event = String::from_utf8(handle.message().body).map_err(|error| error.to_string())?;
        let Event::Cdc {
            package_id,
            entity,
            op,
        } = serde_json::from_str(&event).map_err(|error| error.to_string())?
        else {
            return Ok(());
        };
        let config = bindings::wasi::config::store::get("wamn.application")
            .map_err(|error| format!("read wamn.application: {error:?}"))?
            .ok_or_else(|| "ingress has no wamn.application".to_owned())?;
        let app: App = serde_json::from_str(&config).map_err(|error| error.to_string())?;
        for sibling in app.components {
            if sibling.data_change.is_some_and(|handler| {
                handler.models.iter().any(|model| {
                    model.owner_package == package_id
                        && model.entity == entity
                        && model.ops.contains(&op)
                })
            }) {
                return bindings::wamn::operation::operation::dispatch_data_change(
                    sibling.name,
                    event,
                )
                .await;
            }
        }
        Ok(())
    }
}
