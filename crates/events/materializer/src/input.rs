//! Author-visible input produced from one admitted CDC envelope.

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wamn_event_wire::Op;

    fn envelope(op: Op, old: Option<Value>, new: Option<Value>) -> Envelope {
        Envelope {
            op,
            old: old.map(|value| value.as_object().unwrap().clone()),
            new: new.map(|value| value.as_object().unwrap().clone()),
            entity: Some("receipts".into()),
            table: "receipts_v2".into(),
            lsn: 42,
            txid: 7,
            commit_ts: chrono::DateTime::parse_from_rfc3339("2026-07-19T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            causation: None,
        }
    }

    #[test]
    fn event_input_keeps_only_business_fields() {
        let input = event_input(&envelope(
            Op::Update,
            Some(json!({"id": "7", "status": "draft"})),
            Some(json!({"id": "7", "status": "shipped"})),
        ));
        assert_eq!(
            input,
            json!({
                "event": "update",
                "new": {"id": "7", "status": "shipped"},
                "old": {"id": "7", "status": "draft"}
            })
        );
        for absent in ["trigger", "entity", "table", "seq", "causation"] {
            assert!(input.get(absent).is_none(), "{absent} leaked into input");
        }
    }

    #[test]
    fn absent_images_are_omitted() {
        assert_eq!(
            event_input(&envelope(Op::Insert, None, Some(json!({"id": "7"})))),
            json!({"event": "insert", "new": {"id": "7"}})
        );
    }

    #[test]
    fn derived_input_is_the_arbitrary_payload_byte_semantics() {
        let event = DerivedEvent::new(
            "t1",
            "app",
            "dev",
            "receipts",
            Op::Delete,
            json!([1, {"nested": true}]),
            "d1",
            wamn_event_wire::Causation {
                run: "run-1".into(),
                root: "run-1".into(),
                depth: 0,
            },
        );
        assert_eq!(derived_event_input(&event), json!([1, {"nested": true}]));
    }
}
