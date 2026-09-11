//! The event context — the JSON value registration conditions evaluate over —
//! and the tenant-scoping read.
//!
//! **STATUS: FROZEN 0.1.0** (2026-07-19, wamn-l5i9.30). The context shape
//! [`event_context`] emits — `{"op", "old", "new"}` — is the frozen surface a
//! `wamn_event_reg` JMESPath condition evaluates against; it is
//! pinned by a golden test. Compatibility rule (the WIT-freeze discipline):
//! 0.1.x admits only additive or clarifying changes; any breaking change waits
//! for 0.2.

use serde_json::{Map, Value, json};
use wamn_event_wire::{DerivedEvent, Envelope, Op};

/// Row-level tenancy carried by one CDC image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowTenant<'a> {
    /// The image is present and has no `tenant_id`; database residency governs.
    Absent,
    /// The image carries a string tenant identity.
    Tenant(&'a str),
    /// The image is absent or carries a non-string tenant identity.
    Unscopable,
}

/// Build the condition context from one envelope:
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

/// Build the existing registration-condition context for a derived event.
///
/// Derived events deliberately have no CDC old/new images. Their arbitrary
/// payload occupies `new` while `old` remains null, preserving the frozen
/// `{op, old, new}` condition language without pretending the payload came
/// from WAL.
pub fn derived_event_context(event: &DerivedEvent) -> Value {
    json!({
        "op": event.op.as_str(),
        "old": Value::Null,
        "new": event.payload,
    })
}

/// The row-level tenant carried by an event image. [`RowTenant::Absent`] means
/// the package row relies on its separately verified database-residency scope:
///
/// - a DELETE under REPLICA IDENTITY DEFAULT — the old image carries the key
///   column (`id`) ONLY, not `tenant_id` (the .17 design contract); or
/// - a package application table with no `tenant_id` column.
///
/// The caller may accept `Absent` only after the tenant-scoped catalog
/// credential has proven the package's database residency. A present string
/// remains an additional equality guard; any other carrier is unscopable.
pub fn row_tenant(envelope: &Envelope) -> RowTenant<'_> {
    let image: &Map<String, Value> = match envelope.op {
        Op::Insert | Op::Update => match envelope.new.as_ref() {
            Some(image) => image,
            None => return RowTenant::Unscopable,
        },
        // A DELETE's only image is the old key columns; under DEFAULT that is
        // never tenant-bearing, but read it anyway — if the entity later runs
        // REPLICA IDENTITY FULL (l5i9.31) the old image carries tenant_id and
        // deletes become scopable with zero change here.
        Op::Delete => match envelope.old.as_ref() {
            Some(image) => image,
            None => return RowTenant::Unscopable,
        },
    };
    match image.get("tenant_id") {
        None => RowTenant::Absent,
        Some(Value::String(tenant)) => RowTenant::Tenant(tenant),
        Some(_) => RowTenant::Unscopable,
    }
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
            package_id: "receiving".into(),
            entity: "receipts".into(),
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
    fn frozen_context_shape_is_exactly_op_old_new() {
        // The freeze golden (wamn-l5i9.30): the condition context is
        // exactly {op, old, new}. A field rename/removal breaks THIS string.
        let env = envelope(
            Op::Update,
            Some(json!({"status": "draft"})),
            Some(json!({"status": "shipped"})),
        );
        assert_eq!(
            serde_json::to_string(&event_context(&env)).unwrap(),
            r#"{"new":{"status":"shipped"},"old":{"status":"draft"},"op":"update"}"#
        );
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
        assert_eq!(row_tenant(&env), RowTenant::Tenant("t1"));
    }

    #[test]
    fn delete_under_default_identity_has_no_row_tenant() {
        // The old image of a DELETE carries the PK only (REPLICA IDENTITY
        // DEFAULT), so database residency must supply the scope.
        let env = envelope(Op::Delete, Some(json!({"id": "7"})), None);
        assert_eq!(row_tenant(&env), RowTenant::Absent);
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
        assert_eq!(row_tenant(&env), RowTenant::Tenant("t1"));
    }

    #[test]
    fn a_package_row_may_omit_a_row_tenant() {
        let env = envelope(Op::Insert, None, Some(json!({"id": "7"})));
        assert_eq!(row_tenant(&env), RowTenant::Absent);
    }

    #[test]
    fn derived_context_preserves_arbitrary_payload_without_fabricating_images() {
        let event = DerivedEvent::new(
            "t1",
            "app",
            "dev",
            "receiving",
            "receipts",
            Op::Insert,
            json!(["arbitrary", 7]),
            "d1",
            wamn_event_wire::Causation {
                run: "run-1".into(),
                root: "run-1".into(),
                depth: 0,
            },
        );
        assert_eq!(
            derived_event_context(&event),
            json!({"op": "insert", "old": null, "new": ["arbitrary", 7]})
        );
    }
}
