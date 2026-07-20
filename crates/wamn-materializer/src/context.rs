//! The event context — the JSON value conditions and registration
//! partition-key extractors evaluate over — and the tenant-scoping read.

use serde_json::{Map, Value, json};
use wamn_event_wire::{Envelope, Op};

/// Build the condition/extractor context from one envelope:
/// `{"op": "<insert|update|delete>", "old": {…}|null, "new": {…}|null}`.
///
/// The column maps pass through VERBATIM (pgoutput **text** representation —
/// values are JSON strings or `null`), so exact-decimal / >2^53 numbers arrive
/// as strings and the platform's no-float rule holds trivially. An unchanged
/// out-of-line TOAST column is ABSENT from the map (distinguishable from a
/// real NULL, which is present as `null`) — a condition over such a column
/// sees `null` either way in v1; the distinction becomes load-bearing only
/// with old-image conditions (l5i9.31).
pub fn event_context(envelope: &Envelope) -> Value {
    json!({
        "op": envelope.op.as_str(),
        "old": envelope.old.clone().map(Value::Object).unwrap_or(Value::Null),
        "new": envelope.new.clone().map(Value::Object).unwrap_or(Value::Null),
    })
}

/// The tenant an event belongs to, from the image that carries it. `None` =
/// the event cannot be tenant-scoped:
///
/// - a DELETE under REPLICA IDENTITY DEFAULT — the old image carries the key
///   column (`id`) ONLY, not `tenant_id` (the .17 design contract); or
/// - a table with no `tenant_id` column at all (hand-created, auto-included by
///   the schema-scoped publication).
///
/// The caller REFUSES such an event (alertable) rather than enqueue it under
/// the workload's own tenant — old-absent is cannot-evaluate, and a
/// cannot-scope enqueue would be a cross-tenant leak.
pub fn tenant_of(envelope: &Envelope) -> Option<&str> {
    let image: &Map<String, Value> = match envelope.op {
        Op::Insert | Op::Update => envelope.new.as_ref()?,
        // A DELETE's only image is the old key columns; under DEFAULT that is
        // never tenant-bearing, but read it anyway — if the entity later runs
        // REPLICA IDENTITY FULL (l5i9.31) the old image carries tenant_id and
        // deletes become scopable with zero change here.
        Op::Delete => envelope.old.as_ref()?,
    };
    image.get("tenant_id")?.as_str()
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
            table: "receipts".into(),
            lsn: 42,
            txid: 7,
            commit_ts: chrono_now(),
            causation: None,
        }
    }

    fn chrono_now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-07-19T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn context_is_op_old_new_with_null_for_absent_images() {
        let env = envelope(
            Op::Insert,
            None,
            Some(json!({"id": "7", "qty": "12.3400", "note": null})),
        );
        let ctx = event_context(&env);
        assert_eq!(ctx["op"], "insert");
        assert_eq!(ctx["old"], Value::Null);
        // pgoutput text representation passes through verbatim — the exact
        // decimal stays a string, a real NULL stays null.
        assert_eq!(ctx["new"]["qty"], "12.3400");
        assert_eq!(ctx["new"]["note"], Value::Null);
    }

    #[test]
    fn tenant_comes_from_the_new_image_for_insert_update() {
        let env = envelope(
            Op::Update,
            None,
            Some(json!({"id": "7", "tenant_id": "t1"})),
        );
        assert_eq!(tenant_of(&env), Some("t1"));
    }

    #[test]
    fn delete_under_default_identity_is_not_tenant_scopable() {
        // The old image of a DELETE carries the PK only (REPLICA IDENTITY
        // DEFAULT) — no tenant_id, so the event cannot be scoped.
        let env = envelope(Op::Delete, Some(json!({"id": "7"})), None);
        assert_eq!(tenant_of(&env), None);
    }

    #[test]
    fn delete_with_a_full_old_image_becomes_scopable() {
        // Forward-compat with the l5i9.31 per-entity FULL knob: a tenant-bearing
        // old image scopes the delete with zero change here.
        let env = envelope(
            Op::Delete,
            Some(json!({"id": "7", "tenant_id": "t1"})),
            None,
        );
        assert_eq!(tenant_of(&env), Some("t1"));
    }

    #[test]
    fn a_table_without_tenant_id_is_not_scopable() {
        let env = envelope(Op::Insert, None, Some(json!({"id": "7"})));
        assert_eq!(tenant_of(&env), None);
    }
}
