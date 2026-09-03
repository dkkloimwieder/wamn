//! `blob-put` — write one object at a caller-supplied deterministic key.
//!
//! # The at-least-once rule this node exists to hold
//!
//! **The caller supplies the key. This node never generates one.** `put` is an
//! overwrite, so a redelivery of the same logical write lands on the same
//! object: an idempotent overwrite, not a duplicate. A node that minted its own
//! key — a UUID, a timestamp, a counter — would turn every redelivery into a
//! new object, and at-least-once delivery would silently accumulate garbage
//! that nothing reconciles.
//!
//! That is why the key is a required INPUT field with no default and no
//! fallback: there is no code path here that invents one.
//!
//! # Why the container is not named here
//!
//! The node names its own declared store ALIAS, from the wiring parameter. The
//! container is environment-owned, and a guest that could name it would need
//! the coordinate the confinement exists to keep from it.

wit_bindgen::generate!({
    world: "blob-put",
    path: "wit",
    generate_all,
});

use exports::wamn::node::handler::{Emission, Guest, NodeContext, NodeError};
use wamn::node::types::ErrorDetail;

/// One JSON-pointer wiring parameter.
///
/// Same contract as `transform`'s `pointer`: a RFC 6901 pointer, empty meaning
/// the whole document. Required, because a default would silently pick a
/// member name and reintroduce the coupling the pointer removes.
fn pointer_param<'a>(
    config: &'a serde_json::Value,
    name: &str,
) -> Result<&'a str, NodeError> {
    let pointer = config
        .get(name)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            terminal(
                "invalid_config",
                format!("{name} parameter must be a JSON pointer string"),
            )
        })?;
    if !pointer.is_empty() && !pointer.starts_with('/') {
        return Err(terminal(
            "invalid_config",
            format!("{name} {pointer:?} is not a JSON pointer"),
        ));
    }
    Ok(pointer)
}

struct Component;

impl Guest for Component {
    fn run(context: NodeContext, input: String) -> Result<Emission, NodeError> {
        let config = serde_json::from_str::<serde_json::Value>(&context.config)
            .map_err(|_| terminal("invalid_config", "config is not JSON"))?;
        let store_alias = config
            .get("store_alias")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| terminal("invalid_config", "store_alias parameter must be a string"))?
            .to_string();

        // WHERE the key and the body live in the payload is the WIRING's
        // business, not this node's. An edge carries an upstream payload
        // verbatim, so a node that insisted on the member names `key` and
        // `body` would only ever compose with a predecessor shaped to please
        // it. These pointers are the palette's existing mapping mechanism —
        // the same shape `transform` uses — so this node stays general and the
        // wirer says where to look.
        let key_field = pointer_param(&config, "key_field")?;
        let body_field = pointer_param(&config, "body_field")?;

        let input = serde_json::from_str::<serde_json::Value>(&input)
            .map_err(|_| invalid_input("input_not_json", "input is not JSON"))?;
        // Required, with no default and no generated fallback — see the module
        // docs. This is the whole at-least-once contract in one lookup.
        let key = input
            .pointer(key_field)
            .and_then(serde_json::Value::as_str)
            .filter(|key| !key.is_empty())
            .ok_or_else(|| {
                invalid_input(
                    "missing_key",
                    format!(
                        "key_field {key_field:?} resolves to no non-empty string: the caller \
                         supplies a deterministic object key, and this node never generates one"
                    ),
                )
            })?;
        let body = input
            .pointer(body_field)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                invalid_input(
                    "missing_body",
                    format!("body_field {body_field:?} resolves to no string"),
                )
            })?;

        let written = block_on(write_object(
            store_alias,
            key.to_string(),
            body.as_bytes().to_vec(),
        ))?;
        let payload = serde_json::to_string(&serde_json::json!({
            "container": written,
            "key": key,
        }))
        .map_err(|_| terminal("result_not_encodable", "result is not encodable"))?;
        Ok(Emission {
            payload,
            port: None,
        })
    }
}

/// Resolve the alias, write the body, and report the container written to.
async fn write_object(
    store_alias: String,
    key: String,
    body: Vec<u8>,
) -> Result<String, NodeError> {
    // The generated `wit_stream::new` is the only way a guest mints a
    // component-model stream; the vtable it needs is private to the bindings.
    let (mut writer, reader) = wit_stream::new::<u8>();
    let container = wasmcloud::blobstore::blobstore::get_container(store_alias)
        .await
        .map_err(|error| terminal("no_container", &format!("{error:?}")))?;
    let name = container
        .name()
        .await
        .map_err(|error| terminal("container_unnamed", &format!("{error:?}")))?;
    // The body is handed over BEFORE the write is awaited, and the writer is
    // dropped to signal end-of-stream — the host commits only on that clean
    // end, so a body abandoned here never reaches the store.
    writer.write_all(body).await;
    drop(writer);
    container
        .write_data(key, reader)
        .await
        .map_err(|error| terminal("write_failed", &format!("{error:?}")))?;
    Ok(name)
}

fn block_on<T: 'static>(future: impl std::future::Future<Output = T> + 'static) -> T {
    wit_bindgen::block_on(future)
}

fn invalid_input(code: &str, message: impl Into<String>) -> NodeError {
    NodeError::InvalidInput(ErrorDetail {
        message: message.into(),
        code: Some(code.to_string()),
    })
}

fn terminal(code: &str, message: impl Into<String>) -> NodeError {
    NodeError::Terminal(ErrorDetail {
        message: message.into(),
        code: Some(code.to_string()),
    })
}

export!(Component);
