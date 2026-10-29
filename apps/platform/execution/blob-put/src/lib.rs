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
//! # The payload is the route envelope
//!
//! RULED `wamn-362o.42`: an edge carries the route envelope — an array of
//! request elements, each `{request_id, value}` or `{request_id, error}` — and the
//! platform has no fan-out, so this node writes ONE OBJECT PER ITEM VALUE,
//! resolving `key_field` and `body_field` against each request element's value, and
//! passes error request elements through untouched. It ENRICHES each value it wrote
//! with `stored: {container, key}` rather than replacing the payload, because
//! the route answers with what the last node emits and a caller is owed the
//! operation's result, not this node's result alone. A failed write fails
//! the emission as a whole; per request element outcome reporting is noted as future
//! work in `docs/architecture/execution.md#results-and-effects`.
//!
//! # Why the container is not named here
//!
//! The node names its own declared store ALIAS, from the wiring parameter. The
//! container is environment-owned, and a guest that could name it would need
//! the coordinate the confinement exists to keep from it.

// THIS NODE EXPORTS THE ASYNC CONTRACT, wamn:node/async-handler (RULED
// wamn-362o.46). Every wasmcloud:blobstore function is `async func` carrying
// streams, and the component model forbids a synchronously-lifted export from
// blocking on an async import: blob-put's first-ever execution, WMS cluster
// run 7, trapped with "cannot block a synchronous task before returning" on
// the blocking bridge that used to sit here. And the validator permits the
// `async` canonical option only on an `async func` type, so the lift needs
// the async-typed contract: same `run` shape, awaited. The router dispatches
// on the exported interface and drives it through call_async; admission
// admits the async lift on this contract alone. label-render, which imports
// nothing async, keeps `handler`.
#[allow(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.61 emits Vec::from_raw_parts with equal length and capacity"
)]
mod bindings {
    wit_bindgen::generate!({
        world: "wamn:blob-put/blob-put@0.1.0",
        path: [
            "../../../../crates/execution/workflow/router/wit",
            "../../../../crates/platform/runtime/wit/deps/wasmcloud-blobstore",
            "wit",
        ],
        generate_all,
        async: ["export:wamn:node/async-handler@0.1.0#run"],
    });
}

use bindings::exports::wamn::node::async_handler::{Emission, Guest, NodeContext, NodeError};
use bindings::wamn::node::types::ErrorDetail;

/// One JSON-pointer wiring parameter.
///
/// Same contract as `transform`'s `pointer`: a RFC 6901 pointer, empty meaning
/// the whole document. Required, because a default would silently pick a
/// member name and reintroduce the coupling the pointer removes.
fn pointer_param<'a>(config: &'a serde_json::Value, name: &str) -> Result<&'a str, NodeError> {
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
    async fn run(context: NodeContext, input: String) -> Result<Emission, NodeError> {
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

        let mut envelope = serde_json::from_str::<serde_json::Value>(&input)
            .map_err(|_| invalid_input("input_not_json", "input is not JSON"))?;
        let request_elements = envelope.as_array_mut().ok_or_else(|| {
            invalid_input(
                "input_not_envelope",
                "input must be the route envelope: a JSON array of request elements",
            )
        })?;
        for request_element in request_elements.iter_mut() {
            let Some(value) = request_element.get_mut("value") else {
                continue;
            };
            // Required, with no default and no generated fallback — see the
            // module docs. This is the whole at-least-once contract in one
            // lookup.
            let key = value
                .pointer(key_field)
                .and_then(serde_json::Value::as_str)
                .filter(|key| !key.is_empty())
                .ok_or_else(|| {
                    invalid_input(
                        "missing_key",
                        format!(
                            "key_field {key_field:?} resolves to no non-empty string in a request element's \
                             value: the caller supplies a deterministic object key, and this node \
                             never generates one"
                        ),
                    )
                })?
                .to_string();
            let body = value
                .pointer(body_field)
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    invalid_input(
                        "missing_body",
                        format!(
                            "body_field {body_field:?} resolves to no string in a request element's value"
                        ),
                    )
                })?
                .as_bytes()
                .to_vec();
            let written = write_object(store_alias.clone(), key.clone(), body).await?;
            let Some(record) = value.as_object_mut() else {
                return Err(invalid_input(
                    "item_value_not_object",
                    "a request element's value must be a JSON object",
                ));
            };
            record.insert(
                "stored".to_string(),
                serde_json::json!({ "container": written, "key": key }),
            );
        }
        let payload = serde_json::to_string(&envelope)
            .map_err(|_| terminal("result_not_encodable", "result is not encodable"))?;
        Ok(Emission {
            payload,
            port: None,
        })
    }
}

/// Resolve the alias, write the body, and report the container written to.
#[cfg(not(test))]
async fn write_object(
    store_alias: String,
    key: String,
    body: Vec<u8>,
) -> Result<String, NodeError> {
    // The generated `wit_stream::new` is the only way a guest mints a
    // component-model stream; the vtable it needs is private to the bindings.
    let (mut writer, reader) = bindings::wit_stream::new::<u8>();
    let container = bindings::wasmcloud::blobstore::blobstore::get_container(store_alias)
        .await
        .map_err(|error| terminal("no_container", format!("{error:?}")))?;
    let name = container
        .name()
        .await
        .map_err(|error| terminal("container_unnamed", format!("{error:?}")))?;
    // THE WRITE AND THE HAND-OVER RUN TOGETHER (wamn-362o.47). A component-
    // model stream write completes only as its reader consumes, and the
    // reader reaches the host inside `write-data`. Awaiting the body write
    // first -- what this node did on its first invocation -- waits on a
    // consumer that has not been handed the stream: "deadlock detected: event
    // loop cannot make further progress". So the two futures are driven
    // together; the writer still drops at end-of-body, which is the clean
    // end the host commits on, so a body abandoned here never reaches the
    // store.
    let fill = async move {
        writer.write_all(body).await;
        drop(writer);
    };
    let (written, ()) = join2(container.write_data(key, reader), fill).await;
    written.map_err(|error| terminal("write_failed", format!("{error:?}")))?;
    Ok(name)
}

/// Drive two futures to completion together. Dependency-free on purpose:
/// wit-bindgen's `async-spawn` would reach every guest in this workspace
/// through feature unification and move their digests for one node's need.
#[cfg(not(test))]
async fn join2<A, B>(a: A, b: B) -> (A::Output, B::Output)
where
    A: std::future::Future,
    B: std::future::Future,
{
    use std::pin::pin;
    use std::task::Poll;
    let mut a = pin!(a);
    let mut b = pin!(b);
    let mut a_out = None;
    let mut b_out = None;
    std::future::poll_fn(|cx| {
        if a_out.is_none()
            && let Poll::Ready(value) = a.as_mut().poll(cx)
        {
            a_out = Some(value);
        }
        if b_out.is_none()
            && let Poll::Ready(value) = b.as_mut().poll(cx)
        {
            b_out = Some(value);
        }
        if a_out.is_some() && b_out.is_some() {
            Poll::Ready((a_out.take().unwrap(), b_out.take().unwrap()))
        } else {
            Poll::Pending
        }
    })
    .await
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

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
use tests::write_object;

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::task::{Context, Poll, Waker};

    std::thread_local! {
        static WRITES: RefCell<Vec<(String, String, Vec<u8>)>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) async fn write_object(
        store: String,
        key: String,
        body: Vec<u8>,
    ) -> Result<String, NodeError> {
        WRITES.with_borrow_mut(|writes| writes.push((store.clone(), key, body)));
        Ok(store)
    }

    fn context() -> NodeContext {
        NodeContext {
            wiring_id: "test".into(),
            wiring_version: 1,
            node_id: "blob".into(),
            delivery_id: "delivery".into(),
            input_port: None,
            occurrence: 0,
            traceparent: None,
            tracestate: None,
            deadline_ms: None,
            config: serde_json::json!({
                "store_alias": "files", "key_field": "/key", "body_field": "/body"
            })
            .to_string(),
        }
    }

    fn run(value: &serde_json::Value) -> Result<Emission, NodeError> {
        let input = serde_json::json!([{ "request_id": "test", "value": value }]).to_string();
        let mut future = std::pin::pin!(Component::run(context(), input));
        let Poll::Ready(result) = future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        else {
            panic!("the test store must complete immediately");
        };
        result
    }

    #[test]
    fn empty_or_non_string_key_refuses_without_writing() {
        for key in [serde_json::json!(""), serde_json::json!(7)] {
            WRITES.with_borrow_mut(Vec::clear);
            let error = run(&serde_json::json!({ "key": key, "body": "content" })).unwrap_err();
            let NodeError::InvalidInput(detail) = error else {
                panic!("an invalid key must refuse as invalid input");
            };
            assert_eq!(detail.code.as_deref(), Some("missing_key"));
            WRITES.with_borrow(|writes| assert!(writes.is_empty()));
        }
    }

    #[test]
    fn missing_body_refuses_without_writing() {
        WRITES.with_borrow_mut(Vec::clear);
        let NodeError::InvalidInput(detail) =
            run(&serde_json::json!({ "key": "file" })).unwrap_err()
        else {
            panic!("a missing body must refuse as invalid input");
        };
        assert_eq!(detail.code.as_deref(), Some("missing_body"));
        WRITES.with_borrow(|writes| assert!(writes.is_empty()));
    }

    #[test]
    fn pointer_param_accepts_the_document_and_refuses_invalid_pointers() {
        assert_eq!(
            pointer_param(&serde_json::json!({ "key_field": "" }), "key_field").unwrap(),
            ""
        );
        for pointer in [serde_json::json!("key"), serde_json::json!(7)] {
            let config = serde_json::json!({ "key_field": pointer });
            let NodeError::Terminal(detail) = pointer_param(&config, "key_field").unwrap_err()
            else {
                panic!("an invalid pointer must refuse as invalid configuration");
            };
            assert_eq!(detail.code.as_deref(), Some("invalid_config"));
        }
    }
}
