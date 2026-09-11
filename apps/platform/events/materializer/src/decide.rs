//! Pure selection of one fetched event for a registration delivery.

use std::collections::BTreeSet;

use serde_json::Value;
use wamn_event_reg::EventRegistration;
use wamn_event_wire::{Causation, DerivedEvent, Envelope};

use crate::condition::{CompiledCondition, ConditionOutcome, compile_condition};
use crate::context::{RowTenant, derived_event_context, event_context, row_tenant};
use crate::input::{derived_event_input, event_input};

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

/// Accept exactly one host-scoped `Nats-Msg-Id` matching a derived record.
///
/// Scope equality is checked before identity derivation, so bytes claiming a
/// foreign tenant/project/environment cannot borrow the local header identity.
pub fn verified_derived_source_event_id(
    tenant: &str,
    project: &str,
    environment: &str,
    event: &DerivedEvent,
    message_ids: &[&str],
) -> Option<VerifiedSourceEventId> {
    if event.tenant != tenant || event.project != project || event.environment != environment {
        return None;
    }
    let expected = wamn_event_wire::derived_msg_id(
        tenant,
        project,
        environment,
        &event.package_id,
        &event.entity,
        event.op,
        &event.dedup_id,
    );
    match message_ids {
        [actual] if *actual == expected => Some(VerifiedSourceEventId(expected)),
        _ => None,
    }
}

/// Normal non-delivery outcomes owned by registration filtering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    SourcePackageMismatch,
    EntityMismatch,
    OpMismatch,
    ForeignTenant,
    ConditionFalse,
}

/// Deterministic registration refusals that must remain operator-visible.
#[derive(Debug, Clone, PartialEq)]
pub enum RefuseReason {
    SourcePackageIdentityUnknown { source_package_id: String },
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
///
/// `resident_tenant` is the tenant read through the tenant-scoped catalog
/// credential after the event's stream identity has verified. Package rows
/// without a `tenant_id` use that trusted database-residency scope; a missing
/// or disagreeing scope remains unserviceable.
pub fn decide(
    registration: &EventRegistration,
    condition: Option<&CompiledCondition>,
    envelope: &Envelope,
    known_packages: &BTreeSet<String>,
    tenant: &str,
    resident_tenant: Option<&str>,
    max_depth: u32,
) -> Verdict {
    match envelope.package_id.as_str() {
        package_id if !known_packages.contains(package_id) => {
            return Verdict::Refuse(RefuseReason::SourcePackageIdentityUnknown {
                source_package_id: package_id.to_owned(),
            });
        }
        package_id if package_id != registration.source_package_id => {
            return Verdict::Skip(SkipReason::SourcePackageMismatch);
        }
        _ => {}
    }
    if envelope.entity != registration.entity {
        return Verdict::Skip(SkipReason::EntityMismatch);
    }
    if !registration.ops.contains(&envelope.op) {
        return Verdict::Skip(SkipReason::OpMismatch);
    }
    if resident_tenant != Some(tenant) {
        return Verdict::Refuse(RefuseReason::TenantUnscopable);
    }
    match row_tenant(envelope) {
        RowTenant::Absent => {}
        RowTenant::Tenant(event_tenant) if event_tenant != tenant => {
            return Verdict::Skip(SkipReason::ForeignTenant);
        }
        RowTenant::Tenant(_) => {}
        RowTenant::Unscopable => return Verdict::Refuse(RefuseReason::TenantUnscopable),
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

/// Decide whether one host-published derived event reaches the registration.
pub fn decide_derived(
    registration: &EventRegistration,
    condition: Option<&CompiledCondition>,
    event: &DerivedEvent,
    known_packages: &BTreeSet<String>,
    tenant: &str,
    max_depth: u32,
) -> Verdict {
    match event.package_id.as_str() {
        package_id if !known_packages.contains(package_id) => {
            return Verdict::Refuse(RefuseReason::SourcePackageIdentityUnknown {
                source_package_id: package_id.to_owned(),
            });
        }
        package_id if package_id != registration.source_package_id => {
            return Verdict::Skip(SkipReason::SourcePackageMismatch);
        }
        _ => {}
    }
    if event.entity != registration.entity.as_str() {
        return Verdict::Skip(SkipReason::EntityMismatch);
    }
    if !registration.ops.contains(&event.op) {
        return Verdict::Skip(SkipReason::OpMismatch);
    }
    if event.tenant != tenant {
        return Verdict::Skip(SkipReason::ForeignTenant);
    }
    if registration.condition.is_some() {
        let Some(condition) = condition else {
            return Verdict::Refuse(RefuseReason::ConditionError(
                "condition present but not compiled".into(),
            ));
        };
        if condition.references_old() {
            return Verdict::Refuse(RefuseReason::OldImageAbsent);
        }
        match condition.matches(&derived_event_context(event)) {
            Ok(true) => {}
            Ok(false) => return Verdict::Skip(SkipReason::ConditionFalse),
            Err(error) => return Verdict::Refuse(RefuseReason::ConditionError(error)),
        }
    }
    if event.causation.depth.saturating_add(1) > max_depth {
        return Verdict::Refuse(RefuseReason::DepthExceeded {
            parent: event.causation.clone(),
        });
    }
    Verdict::Deliver(derived_event_input(event))
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
            package_id: "cat".into(),
            source_package_id: "cat".into(),
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
            "package_id": "cat",
            "entity": "receipts",
            "table": "receipts",
            "lsn": 42,
            "txid": 7,
            "commit_ts": "2026-08-15T12:00:00Z"
        }))
        .unwrap()
    }

    fn known_packages() -> BTreeSet<String> {
        BTreeSet::from(["cat".to_owned(), "other".to_owned()])
    }

    #[test]
    fn matching_row_tenant_delivers_business_input_without_flow_or_run_identity() {
        assert_eq!(
            decide(
                &registration(None),
                None,
                &envelope(),
                &known_packages(),
                "t1",
                Some("t1"),
                16,
            ),
            Verdict::Deliver(json!({
                "event": "insert",
                "new": {"tenant_id": "t1", "status": "ready"}
            }))
        );
    }

    #[test]
    fn tenantless_package_row_delivers_under_verified_database_residency() {
        let mut tenantless = envelope();
        tenantless
            .new
            .as_mut()
            .expect("insert carries a new image")
            .remove("tenant_id");
        assert!(matches!(
            decide(
                &registration(None),
                None,
                &tenantless,
                &known_packages(),
                "t1",
                Some("t1"),
                16,
            ),
            Verdict::Deliver(_)
        ));
    }

    #[test]
    fn foreign_row_tenant_is_normal_filtration() {
        let mut foreign = envelope();
        foreign
            .new
            .as_mut()
            .expect("insert carries a new image")
            .insert("tenant_id".into(), Value::String("other".into()));
        assert_eq!(
            decide(
                &registration(None),
                None,
                &foreign,
                &known_packages(),
                "t1",
                Some("t1"),
                16,
            ),
            Verdict::Skip(SkipReason::ForeignTenant)
        );
    }

    #[test]
    fn unresolved_or_disagreeing_database_residency_refuses() {
        let mut tenantless = envelope();
        tenantless
            .new
            .as_mut()
            .expect("insert carries a new image")
            .remove("tenant_id");
        for resident_tenant in [None, Some("other")] {
            assert_eq!(
                decide(
                    &registration(None),
                    None,
                    &tenantless,
                    &known_packages(),
                    "t1",
                    resident_tenant,
                    16,
                ),
                Verdict::Refuse(RefuseReason::TenantUnscopable)
            );
        }
    }

    #[test]
    fn present_non_string_row_tenant_refuses_instead_of_using_residency() {
        for value in [Value::Null, json!(7)] {
            let mut invalid = envelope();
            invalid
                .new
                .as_mut()
                .expect("insert carries a new image")
                .insert("tenant_id".into(), value);
            assert_eq!(
                decide(
                    &registration(None),
                    None,
                    &invalid,
                    &known_packages(),
                    "t1",
                    Some("t1"),
                    16,
                ),
                Verdict::Refuse(RefuseReason::TenantUnscopable)
            );
        }
    }

    #[test]
    fn condition_and_tenant_guards_still_filter_before_delivery() {
        let registration = registration(Some("new.status == 'ready'"));
        let condition = serviceable(&registration).unwrap().unwrap();
        assert!(matches!(
            decide(
                &registration,
                Some(&condition),
                &envelope(),
                &known_packages(),
                "other",
                Some("other"),
                16,
            ),
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
            decide(
                &registration(None),
                None,
                &envelope,
                &known_packages(),
                "t1",
                Some("t1"),
                16,
            ),
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

    fn derived(depth: u32) -> DerivedEvent {
        DerivedEvent::new(
            "t1",
            "app",
            "dev",
            "cat",
            "receipts",
            Op::Insert,
            json!(["arbitrary", {"status": "ready"}]),
            "author:receipt:7",
            Causation {
                run: "delivery-7".into(),
                root: "delivery-1".into(),
                depth,
            },
        )
    }

    #[test]
    fn matching_derived_event_delivers_exact_arbitrary_payload() {
        assert_eq!(
            decide_derived(
                &registration(None),
                None,
                &derived(3),
                &known_packages(),
                "t1",
                16,
            ),
            Verdict::Deliver(json!(["arbitrary", {"status": "ready"}]))
        );
    }

    #[test]
    fn derived_event_keeps_registration_condition_and_depth_gates() {
        let registration = registration(Some("new[1].status == 'ready'"));
        let condition = serviceable(&registration).unwrap().unwrap();
        assert!(matches!(
            decide_derived(
                &registration,
                Some(&condition),
                &derived(3),
                &known_packages(),
                "t1",
                16,
            ),
            Verdict::Deliver(_)
        ));
        assert!(matches!(
            decide_derived(
                &registration,
                Some(&condition),
                &derived(16),
                &known_packages(),
                "t1",
                16,
            ),
            Verdict::Refuse(RefuseReason::DepthExceeded { .. })
        ));
    }

    #[test]
    fn derived_source_identity_requires_exact_scope_and_one_header() {
        let event = derived(0);
        let expected = wamn_event_wire::derived_msg_id(
            "t1",
            "app",
            "dev",
            &event.package_id,
            &event.entity,
            event.op,
            &event.dedup_id,
        );
        assert_eq!(
            verified_derived_source_event_id("t1", "app", "dev", &event, &[&expected])
                .unwrap()
                .as_str(),
            expected
        );
        assert!(
            verified_derived_source_event_id("other", "app", "dev", &event, &[&expected]).is_none()
        );
        assert!(
            verified_derived_source_event_id("t1", "app", "dev", &event, &[&expected, &expected])
                .is_none()
        );
    }

    #[test]
    fn derived_wire_identity_reaches_one_matching_registration_without_run_admission() {
        let encoded = serde_json::to_vec(&derived(3)).expect("derived event serializes");
        let event = DerivedEvent::from_slice(&encoded).expect("derived event wire decodes");
        let message_id = wamn_event_wire::derived_msg_id(
            "t1",
            "app",
            "dev",
            &event.package_id,
            &event.entity,
            event.op,
            &event.dedup_id,
        );
        let source = verified_derived_source_event_id("t1", "app", "dev", &event, &[&message_id])
            .expect("host-scoped source identity verifies");
        assert_eq!(source.as_str(), message_id);
        assert_eq!(
            decide_derived(
                &registration(None),
                None,
                &event,
                &known_packages(),
                "t1",
                16,
            ),
            Verdict::Deliver(json!(["arbitrary", {"status": "ready"}]))
        );
    }

    #[test]
    fn source_package_identity_has_match_known_skip_and_unknown_refusal_paths() {
        let mut registration = registration(None);
        registration.package_id = "overlay".into();
        let known = known_packages();

        let mut other = envelope();
        other.package_id = "other".into();
        assert_eq!(
            decide(&registration, None, &other, &known, "t1", Some("t1"), 16,),
            Verdict::Skip(SkipReason::SourcePackageMismatch)
        );

        let mut unknown = envelope();
        unknown.package_id = "unknown".into();
        assert_eq!(
            decide(&registration, None, &unknown, &known, "t1", Some("t1"), 16,),
            Verdict::Refuse(RefuseReason::SourcePackageIdentityUnknown {
                source_package_id: "unknown".into(),
            })
        );

        assert!(matches!(
            decide(
                &registration,
                None,
                &envelope(),
                &known,
                "t1",
                Some("t1"),
                16,
            ),
            Verdict::Deliver(_)
        ));
    }
}
