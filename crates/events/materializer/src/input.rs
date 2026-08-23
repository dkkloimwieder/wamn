//! Author-visible input produced from one admitted CDC envelope.

use serde::Serialize;
use serde_json::{Map, Value};
use wamn_event_wire::Envelope;

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
}
