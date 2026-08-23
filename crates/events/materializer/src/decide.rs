//! Pure selection of one fetched event for a registration delivery.

use serde_json::Value;
use wamn_event_reg::EventRegistration;
use wamn_event_wire::{Causation, Envelope};

use crate::condition::{CompiledCondition, ConditionOutcome, compile_condition};
use crate::context::{event_context, tenant_of};
use crate::input::event_input;

/// A source-event coordinate proven to match the delivered NATS identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedSourceEventId(String);

impl VerifiedSourceEventId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Accept exactly one `Nats-Msg-Id` matching the envelope's source coordinate.
pub fn verified_source_event_id(
    project: &str,
    environment: &str,
    envelope: &Envelope,
    message_ids: &[&str],
) -> Option<VerifiedSourceEventId> {
    let expected = wamn_event_wire::msg_id(project, environment, envelope.lsn);
    match message_ids {
        [actual] if *actual == expected => Some(VerifiedSourceEventId(expected)),
        _ => None,
    }
}

/// Normal non-delivery outcomes owned by registration filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    EntityMismatch,
    OpMismatch,
    ForeignTenant,
    ConditionFalse,
}

/// Deterministic registration refusals that must remain operator-visible.
#[derive(Debug, Clone, PartialEq)]
pub enum RefuseReason {
    DepthExceeded { parent: Causation },
    TenantUnscopable,
    OldImageAbsent,
    ConditionError(String),
}

/// One fetched event's registration decision.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Deliver(Value),
    Skip(SkipReason),
    Refuse(RefuseReason),
}

/// Why a registration cannot be served in this sweep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecideError {
    UnserviceableCondition(ConditionOutcome),
}

/// Compile a registration's optional filter once per sweep.
pub fn serviceable(
    registration: &EventRegistration,
) -> Result<Option<CompiledCondition>, DecideError> {
    match &registration.condition {
        None => Ok(None),
        Some(expression) => compile_condition(expression)
            .map(Some)
            .map_err(DecideError::UnserviceableCondition),
    }
}

/// Decide whether one envelope reaches the registration's wiring.
pub fn decide(
    registration: &EventRegistration,
    condition: Option<&CompiledCondition>,
    envelope: &Envelope,
    tenant: &str,
    max_depth: u32,
) -> Verdict {
    if envelope.entity.as_deref() != Some(registration.entity.as_str()) {
        return Verdict::Skip(SkipReason::EntityMismatch);
    }
    if !registration.ops.contains(&envelope.op) {
        return Verdict::Skip(SkipReason::OpMismatch);
    }
    match tenant_of(envelope) {
        None => return Verdict::Refuse(RefuseReason::TenantUnscopable),
        Some(event_tenant) if event_tenant != tenant => {
            return Verdict::Skip(SkipReason::ForeignTenant);
        }
        Some(_) => {}
    }
    if registration.condition.is_some() {
        let Some(condition) = condition else {
            return Verdict::Refuse(RefuseReason::ConditionError(
                "condition present but not compiled".into(),
            ));
        };
        if condition.references_old() && envelope.old.is_none() {
            return Verdict::Refuse(RefuseReason::OldImageAbsent);
        }
        match condition.matches(&event_context(envelope)) {
            Ok(true) => {}
            Ok(false) => return Verdict::Skip(SkipReason::ConditionFalse),
            Err(error) => return Verdict::Refuse(RefuseReason::ConditionError(error)),
        }
    }
    if let Some(parent) = &envelope.causation
        && parent.depth.saturating_add(1) > max_depth
    {
        return Verdict::Refuse(RefuseReason::DepthExceeded {
            parent: parent.clone(),
        });
    }
    Verdict::Deliver(event_input(envelope))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wamn_event_reg::RegistrationInput;
    use wamn_event_wire::Op;

    fn registration(condition: Option<&str>) -> EventRegistration {
        EventRegistration {
            schema_version: "0.1".into(),
            registration_id: "r1".into(),
            catalog_id: "cat".into(),
            flow_id: "legacy-flow".into(),
            entity: "receipts".into(),
            ops: vec![Op::Insert, Op::Update],
            input: RegistrationInput::Event,
            condition: condition.map(str::to_owned),
        }
    }

    fn envelope() -> Envelope {
        serde_json::from_value(json!({
            "op": "insert",
            "new": {"tenant_id": "t1", "status": "ready"},
            "entity": "receipts",
            "table": "receipts",
            "lsn": 42,
            "txid": 7,
            "commit_ts": "2026-08-15T12:00:00Z"
        }))
        .unwrap()
    }

    #[test]
    fn matching_event_delivers_business_input_without_flow_or_run_identity() {
        assert_eq!(
            decide(&registration(None), None, &envelope(), "t1", 16),
            Verdict::Deliver(json!({
                "event": "insert",
                "new": {"tenant_id": "t1", "status": "ready"}
            }))
        );
    }

    #[test]
    fn condition_and_tenant_guards_still_filter_before_delivery() {
        let registration = registration(Some("new.status == 'ready'"));
        let condition = serviceable(&registration).unwrap().unwrap();
        assert!(matches!(
            decide(&registration, Some(&condition), &envelope(), "other", 16),
            Verdict::Skip(SkipReason::ForeignTenant)
        ));
    }

    #[test]
    fn over_depth_event_refuses_without_minting_a_run() {
        let mut envelope = envelope();
        envelope.causation = Some(Causation {
            run: "delivery".into(),
            root: "root".into(),
            depth: 16,
        });
        assert!(matches!(
            decide(&registration(None), None, &envelope, "t1", 16),
            Verdict::Refuse(RefuseReason::DepthExceeded { .. })
        ));
    }

    #[test]
    fn source_identity_requires_one_exact_header() {
        let envelope = envelope();
        let expected = wamn_event_wire::msg_id("app", "dev", envelope.lsn);
        assert_eq!(
            verified_source_event_id("app", "dev", &envelope, &[&expected])
                .unwrap()
                .as_str(),
            expected
        );
        assert!(verified_source_event_id("app", "dev", &envelope, &[]).is_none());
    }
}
