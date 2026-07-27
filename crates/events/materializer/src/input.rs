//! Synthesis of the business input emitted by an `event` entry.
//!
//! FLOW-SPEC rev18 §4.3 has one current shape:
//! `{"event": …, "new": …, "old": …}`, with absent images omitted. CDC and
//! admission metadata do not ride the author-visible payload: entity, table,
//! sequence, and trigger kind belong to trusted invocation context; causation
//! belongs to lineage columns.

use wamn_event_wire::{Causation, Envelope, Op};
use wamn_flow::{EventInput, RowEvent};

/// Mint the author-visible business input for one materialized event.
///
/// `stream_seq` and `child` remain inputs because the materializer decision
/// owns them, but their field homes are trusted persistence rather than this
/// payload. The admission rewrite persists those homes; this synthesizer
/// deliberately cannot serialize them into the event input.
pub fn evt_input_json(envelope: &Envelope, _stream_seq: u64, _child: &Causation) -> String {
    let input = EventInput {
        event: match envelope.op {
            Op::Insert => RowEvent::Insert,
            Op::Update => RowEvent::Update,
            Op::Delete => RowEvent::Delete,
        },
        new: envelope.new.clone(),
        old: envelope.old.clone(),
    };
    serde_json::to_string(&input).expect("event input serializes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

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

    fn child() -> Causation {
        Causation {
            run: "f1:evt:00000000000000000009".into(),
            root: "f1:evt:00000000000000000009".into(),
            depth: 0,
        }
    }

    #[test]
    fn current_event_input_golden_pins_every_business_field_home() {
        let event = envelope(Op::Insert, None, Some(json!({"id": "7", "qty": "12.3400"})));
        assert_eq!(
            evt_input_json(&event, 9, &child()),
            r#"{"event":"insert","new":{"id":"7","qty":"12.3400"}}"#
        );
    }

    #[test]
    fn update_carries_new_and_old_under_their_normative_names() {
        let event = envelope(
            Op::Update,
            Some(json!({"id": "7", "status": "draft"})),
            Some(json!({"id": "7", "status": "shipped"})),
        );
        let input: Value = serde_json::from_str(&evt_input_json(&event, 11, &child())).unwrap();
        assert_eq!(
            input,
            json!({
                "event": "update",
                "new": {"id": "7", "status": "shipped"},
                "old": {"id": "7", "status": "draft"}
            })
        );
    }

    #[test]
    fn delete_omits_new_and_keeps_old_as_old_not_payload() {
        let event = envelope(Op::Delete, Some(json!({"id": "7"})), None);
        assert_eq!(
            evt_input_json(&event, 11, &child()),
            r#"{"event":"delete","old":{"id":"7"}}"#
        );
    }

    #[test]
    fn absent_images_are_omitted_and_legacy_metadata_never_leaks_into_input() {
        let event = envelope(Op::Insert, None, Some(json!({"id": "7"})));
        let input: Value = serde_json::from_str(&evt_input_json(&event, 9, &child())).unwrap();
        for absent in [
            "old",
            "trigger",
            "entity",
            "table",
            "seq",
            "payload",
            "causation",
        ] {
            assert!(
                input.get(absent).is_none(),
                "{absent} moved into the business input"
            );
        }
        assert_ne!(input.get("old"), Some(&Value::Null));
    }
}
