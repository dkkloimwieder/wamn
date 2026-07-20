//! The pending-replica-identity-reconcile response warning for registration
//! writes (EVT-RI-ORCH, wamn-l5i9.66).
//!
//! A registration create/update is written on the API path under the `wamn_app`
//! role, which CANNOT `ALTER TABLE … REPLICA IDENTITY` (table ownership). So a
//! registration that newly needs the entity's OLD image — its condition reads
//! the root `old` ("changed-to") or it subscribes to `delete` — cannot itself
//! flip the entity's table to `REPLICA IDENTITY FULL`: the table stays at
//! DEFAULT, an old-image gap the periodic `reconcile-replica-identity` CronJob
//! (wamn-l5i9.65) closes within one cadence. This surface lets the write
//! response carry a WARNING so the caller sees the gap immediately, instead of
//! discovering it later via the materializer's old-image-absent refusal alerts.
//!
//! **The decision is not re-derived here.** [`pending_replica_identity_warning`]
//! keys on [`EventRegistration::requires_replica_identity_full`] — the SINGLE
//! predicate `wamn_migrate`'s `entities_requiring_full` / `pending_old_image_gap`
//! reconciler and the materializer's per-event old-absent guard also fold, so
//! the warning can never disagree with what the CronJob would actually flip. The
//! only live input is `table_is_full` (the entity table's
//! `pg_class.relreplident == 'f'`, which the guest reads — `wamn_app` may READ
//! `pg_class` even though it cannot `ALTER`).
//!
//! Reusing the whole `wamn_migrate` planner from the wasm guest was avoided on
//! purpose: `pending_old_image_gap` for a single registration+entity reduces
//! EXACTLY to `requires_replica_identity_full() && current != Full`, and keying
//! on the atomic predicate keeps the guest's dependency closure to
//! `wamn-event-reg` (the single detector) rather than dragging the migration
//! engine (`wamn-schema` → `wamn-registry`) into the component — the least
//! invasive factoring consistent with the pure-core layering.

use serde_json::{Value, json};

use wamn_event_reg::EventRegistration;

/// A non-fatal warning attached to a registration create/update response. An
/// enum (WIT-variant style) so a future warning is an additive variant, not a
/// stringly-typed code.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Warning {
    /// The written registration needs the entity's `REPLICA IDENTITY FULL` (it
    /// reads the root `old` image or subscribes to `delete`) but the entity's
    /// table is still at DEFAULT — an old-image gap a reconcile will close on
    /// the next cadence (wamn-l5i9.65).
    PendingReplicaIdentityReconcile { entity: String },
}

impl Warning {
    /// The stable machine code (the response envelope's `code`).
    pub fn code(&self) -> &'static str {
        match self {
            Warning::PendingReplicaIdentityReconcile { .. } => "pending-replica-identity-reconcile",
        }
    }

    /// The warning object shape — `{ code, message, entity }`, mirroring the
    /// gateway's `{ code, message }` error envelope.
    fn to_json(&self) -> Value {
        match self {
            Warning::PendingReplicaIdentityReconcile { entity } => json!({
                "code": self.code(),
                "message": format!(
                    "entity {entity:?} needs REPLICA IDENTITY FULL for the old image; \
                     a reconcile is pending and closes within one cadence"
                ),
                "entity": entity,
            }),
        }
    }
}

/// Whether a just-written registration opens an old-image gap. `table_is_full`
/// is the entity table's live `pg_class.relreplident == 'f'`. Returns the
/// warning when the registration needs the old image but the table is not yet
/// FULL; `None` otherwise (a registration that does not need the old image, or
/// an entity already at FULL, is not a gap).
pub fn pending_replica_identity_warning(
    reg: &EventRegistration,
    table_is_full: bool,
) -> Option<Warning> {
    (reg.requires_replica_identity_full() && !table_is_full)
        .then(|| Warning::PendingReplicaIdentityReconcile { entity: reg.entity.to_string() })
}

/// Attach an optional [`Warning`] to a registration write response as an
/// ADDITIVE `warnings` array. No warning → the row is returned UNCHANGED (a
/// consumer reading the registration fields is unaffected); a warning → a
/// `"warnings": [ … ]` sibling is added. A non-object response (never produced
/// by the registration builders, but total for safety) is returned untouched.
pub fn attach_warning(mut row: Value, warning: Option<Warning>) -> Value {
    let Some(w) = warning else {
        return row;
    };
    if let Value::Object(map) = &mut row {
        map.insert("warnings".to_string(), Value::Array(vec![w.to_json()]));
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_event_reg::{EventRegistration, Op, SCHEMA_VERSION};

    fn reg(ops: Vec<Op>, condition: Option<&str>) -> EventRegistration {
        EventRegistration {
            schema_version: SCHEMA_VERSION.to_string(),
            registration_id: "r".to_string(),
            catalog_id: "shop".to_string(),
            flow_id: "notify".to_string(),
            entity: "orders".into(),
            ops,
            condition: condition.map(str::to_string),
            partition_key: None,
        }
    }

    /// PRESENT: a delete subscription needs the old image; while the table is at
    /// DEFAULT the write opens a gap, so the warning is emitted and names the
    /// entity.
    #[test]
    fn replica_identity_warning_present_for_delete_on_default_table() {
        let w = pending_replica_identity_warning(&reg(vec![Op::Delete], None), false);
        assert_eq!(
            w,
            Some(Warning::PendingReplicaIdentityReconcile { entity: "orders".to_string() })
        );
        assert_eq!(w.unwrap().code(), "pending-replica-identity-reconcile");
    }

    /// PRESENT: an old-condition (changed-to) registration also needs the old
    /// image — the second half of `requires_replica_identity_full`.
    #[test]
    fn replica_identity_warning_present_for_old_condition() {
        let r = reg(vec![Op::Update], Some("new.status != old.status"));
        assert!(pending_replica_identity_warning(&r, false).is_some());
    }

    /// ABSENT: the entity table is already at FULL — no gap, so no warning.
    #[test]
    fn replica_identity_warning_absent_when_table_already_full() {
        assert_eq!(pending_replica_identity_warning(&reg(vec![Op::Delete], None), true), None);
    }

    /// ABSENT: an insert-only, new-only registration does not need the old image
    /// — no warning regardless of the table's identity.
    #[test]
    fn replica_identity_warning_absent_for_insert_only() {
        let r = reg(vec![Op::Insert], Some("new.status == 'new'"));
        assert_eq!(pending_replica_identity_warning(&r, false), None);
        assert_eq!(pending_replica_identity_warning(&r, true), None);
    }

    /// The envelope is ADDITIVE: a warning adds a `warnings` array and leaves the
    /// row fields intact; no warning leaves the row byte-for-byte unchanged.
    #[test]
    fn replica_identity_warning_envelope_attaches_and_omits() {
        let row = json!({ "registration_id": "r", "entity_id": "orders" });

        let with = attach_warning(
            row.clone(),
            Some(Warning::PendingReplicaIdentityReconcile { entity: "orders".to_string() }),
        );
        assert_eq!(with["registration_id"], "r", "the row fields survive");
        let warnings = with["warnings"].as_array().expect("warnings is an array");
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0]["code"], "pending-replica-identity-reconcile");
        assert_eq!(warnings[0]["entity"], "orders");

        let without = attach_warning(row.clone(), None);
        assert_eq!(without, row, "no warning → the row is unchanged");
        assert!(without.get("warnings").is_none());
    }
}
