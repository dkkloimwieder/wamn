//! The run-input envelope an evt firing persists (`runs.input_json`, 5.7 —
//! what a replay re-runs and the flow's trigger node reads).
//!
//! Shape (WORKING DRAFT until the l5i9.30 freeze, like every event-plane wire
//! shape): `{"trigger":"event", "entity"?, "table", "event", "seq", "payload",
//! "old"?, "causation":{run,root,depth}}` — the outbox firing's
//! `{trigger,table,event,seq,payload}` grammar (so a flow reads
//! `payload.<column>` the same way post-cutover), extended with the stable
//! entity id, the delete/update old image when present, and the CAUSATION
//! THREAD: the flow-runner reads `causation` at claim time and declares it on
//! the `wamn:runner/causation` channel, so this run's own writes carry
//! `depth = parent + 1` and the chain budget is real.
//!
//! `payload` is the row image the op is ABOUT: the new image for
//! insert/update, the old (key) image for delete. Values are pgoutput text
//! representation (strings/null) passed through verbatim — exact numerics
//! survive as strings, the no-float rule holds trivially.

use serde_json::{Value, json};
use wamn_event_wire::{Causation, Envelope, Op};

/// Mint the run input for one firing. `run_id` is the minted evt run id (the
/// causation stamp's `run`); `child` is the [`crate::child_causation`] result.
pub fn evt_input_json(envelope: &Envelope, stream_seq: u64, child: &Causation) -> String {
    let payload: Value = match envelope.op {
        Op::Insert | Op::Update => envelope
            .new
            .clone()
            .map(Value::Object)
            .unwrap_or(Value::Null),
        Op::Delete => envelope
            .old
            .clone()
            .map(Value::Object)
            .unwrap_or(Value::Null),
    };
    let mut input = json!({
        "trigger": "event",
        "table": envelope.table,
        "event": envelope.op.as_str(),
        "seq": stream_seq,
        "payload": payload,
        "causation": {
            "run": child.run,
            "root": child.root,
            "depth": child.depth,
        },
    });
    let obj = input.as_object_mut().expect("object literal");
    if let Some(entity) = &envelope.entity {
        obj.insert("entity".into(), Value::String(entity.clone()));
    }
    // The UPDATE old image (present only under the l5i9.31 FULL knob today —
    // DEFAULT emits no update old image) rides along for condition-parity
    // debugging; deletes already carry old AS the payload.
    if envelope.op == Op::Update
        && let Some(old) = &envelope.old
    {
        obj.insert("old".into(), Value::Object(old.clone()));
    }
    serde_json::to_string(&input).expect("input envelope serializes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn envelope(op: Op, old: Option<Value>, new: Option<Value>) -> Envelope {
        Envelope {
            op,
            old: old.map(|v| v.as_object().unwrap().clone()),
            new: new.map(|v| v.as_object().unwrap().clone()),
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
    fn insert_input_is_the_outbox_grammar_plus_entity_and_causation() {
        let env = envelope(
            Op::Insert,
            None,
            Some(json!({"id": "7", "qty": "12.3400", "tenant_id": "t1"})),
        );
        let child = Causation {
            run: "f1:evt:00000000000000000009".into(),
            root: "f1:evt:00000000000000000009".into(),
            depth: 0,
        };
        let input: Value = serde_json::from_str(&evt_input_json(&env, 9, &child)).unwrap();
        assert_eq!(input["trigger"], "event");
        assert_eq!(input["table"], "receipts_v2");
        assert_eq!(input["entity"], "receipts");
        assert_eq!(input["event"], "insert");
        assert_eq!(input["seq"], 9);
        // The row image is `payload` — the outbox grammar a flow already reads.
        assert_eq!(input["payload"]["qty"], "12.3400");
        // The causation thread the flow-runner declares at claim time.
        assert_eq!(input["causation"]["depth"], 0);
        assert_eq!(input["causation"]["run"], "f1:evt:00000000000000000009");
        assert!(input.get("old").is_none());
    }

    #[test]
    fn delete_input_carries_the_old_key_image_as_payload() {
        let env = envelope(Op::Delete, Some(json!({"id": "7"})), None);
        let child = Causation {
            run: "r".into(),
            root: "root".into(),
            depth: 3,
        };
        let input: Value = serde_json::from_str(&evt_input_json(&env, 11, &child)).unwrap();
        assert_eq!(input["event"], "delete");
        assert_eq!(input["payload"]["id"], "7");
        assert_eq!(input["causation"]["root"], "root");
        assert_eq!(input["causation"]["depth"], 3);
    }

    #[test]
    fn unmapped_envelope_omits_entity() {
        let mut env = envelope(Op::Insert, None, Some(json!({"id": "1"})));
        env.entity = None;
        let input: Value = serde_json::from_str(&evt_input_json(
            &env,
            1,
            &Causation {
                run: "r".into(),
                root: "r".into(),
                depth: 0,
            },
        ))
        .unwrap();
        assert!(input.get("entity").is_none());
        assert_eq!(input["table"], "receipts_v2");
    }
}
