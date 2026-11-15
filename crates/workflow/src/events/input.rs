//! Build workflow input from a source event.

use serde::Serialize;
use serde_json::{Map, Value};
use wamn_event_wire::{DerivedEvent, Envelope};

#[derive(Serialize)]
struct EventInput<'a> {
    event: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    new: Option<&'a Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    old: Option<&'a Map<String, Value>>,
}

/// Project one wire envelope into the component-facing event document.
pub fn event_input(envelope: &Envelope) -> Value {
    serde_json::to_value(EventInput {
        event: envelope.op.as_str(),
        new: envelope.new.as_ref(),
        old: envelope.old.as_ref(),
    })
    .expect("event input serializes")
}

/// Project a derived event to the exact arbitrary payload its author emitted.
pub fn derived_event_input(event: &DerivedEvent) -> Value {
    event.payload.clone()
}
