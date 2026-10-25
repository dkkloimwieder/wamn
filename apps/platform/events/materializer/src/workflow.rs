//! Event workflow rows shared by the guest and host admission.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{Envelope, EventRegistration, MAX_CAUSATION_DEPTH, Op, Verdict, decide, serviceable};

/// A decoded event with its frozen source identity.
#[derive(Debug, Clone)]
pub enum SourceEvent {
    Cdc(Envelope),
    Derived(crate::DerivedEvent),
}

impl SourceEvent {
    pub fn decode(body: &[u8]) -> Result<Self, serde_json::Error> {
        let value: serde_json::Value = serde_json::from_slice(body)?;
        if value.get("format-version").is_some() {
            crate::DerivedEvent::from_slice(body).map(Self::Derived)
        } else {
            serde_json::from_value(value).map(Self::Cdc)
        }
    }

    pub fn identity(
        &self,
        tenant: &str,
        project: &str,
        environment: &str,
        headers: &[&str],
    ) -> Option<crate::VerifiedSourceEventId> {
        match self {
            Self::Cdc(envelope) => {
                crate::verified_source_event_id(project, environment, envelope, headers)
            }
            Self::Derived(event) => crate::verified_derived_source_event_id(
                tenant,
                project,
                environment,
                event,
                headers,
            ),
        }
    }

    pub fn source(&self) -> (&str, &str) {
        match self {
            Self::Cdc(envelope) => (&envelope.package_id, &envelope.entity),
            Self::Derived(event) => (&event.package_id, &event.entity),
        }
    }
}

/// An enabled event trigger of an environment application.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventTrigger {
    pub application: String,
    pub workflow_id: String,
    pub source_package: String,
    pub entity: String,
    pub ops: Vec<Op>,
    pub condition: Option<String>,
    pub enable_sequence: u64,
}

/// A stored condition or an event that cannot be evaluated safely.
#[derive(Debug)]
pub struct TriggerMatchError {
    pub kind: TriggerMatchErrorKind,
    pub workflow_id: String,
    detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerMatchErrorKind {
    Condition,
    Event,
}

impl std::fmt::Display for TriggerMatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "workflow {}: {}", self.workflow_id, self.detail)
    }
}

impl std::error::Error for TriggerMatchError {}

impl EventTrigger {
    /// Filter one published row change at the workflow's enable boundary.
    pub fn matches(
        &self,
        event: &SourceEvent,
        sequence: u64,
        packages: &BTreeSet<String>,
        tenant: &str,
    ) -> Result<Option<serde_json::Value>, TriggerMatchError> {
        if sequence <= self.enable_sequence {
            return Ok(None);
        }
        let registration = EventRegistration {
            schema_version: wamn_event_reg::SCHEMA_VERSION.to_owned(),
            registration_id: self.workflow_id.clone(),
            package_id: self.application.clone(),
            source_package_id: self.source_package.clone(),
            entity: self.entity.clone(),
            ops: self.ops.clone(),
            input: wamn_event_reg::RegistrationInput::Event,
            condition: self.condition.clone(),
        };
        let condition = serviceable(&registration).map_err(|error| TriggerMatchError {
            kind: TriggerMatchErrorKind::Condition,
            workflow_id: self.workflow_id.clone(),
            detail: format!("{error:?}"),
        })?;
        let verdict = match event {
            SourceEvent::Cdc(envelope) => decide(
                &registration,
                condition.as_ref(),
                envelope,
                packages,
                tenant,
                Some(tenant),
                MAX_CAUSATION_DEPTH,
            ),
            SourceEvent::Derived(event) => crate::decide_derived(
                &registration,
                condition.as_ref(),
                event,
                packages,
                tenant,
                MAX_CAUSATION_DEPTH,
            ),
        };
        match verdict {
            Verdict::Deliver(input) => Ok(Some(input)),
            Verdict::Skip(_) => Ok(None),
            Verdict::Refuse(reason) => Err(TriggerMatchError {
                kind: TriggerMatchErrorKind::Event,
                workflow_id: self.workflow_id.clone(),
                detail: format!("{reason:?}"),
            }),
        }
    }
}

/// The durable provisioned for one captured package and entity.
pub fn entity_durable(package: &str, entity: &str) -> String {
    format!(
        "entity_{}_{}_{}",
        package.len(),
        wamn_event_wire::subject_token(package),
        wamn_event_wire::subject_token(entity)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn workflow_matching_keeps_the_enable_boundary_and_all_filters() {
        let source = SourceEvent::decode(
            &serde_json::to_vec(&json!({
                "op": "update", "package_id": "orders", "entity": "order", "table": "orders",
                "lsn": 42, "txid": 7, "commit_ts": "2026-10-25T00:00:00Z",
                "old": {"status": "draft"}, "new": {"status": "open"}
            }))
            .unwrap(),
        )
        .unwrap();
        let packages = BTreeSet::from(["orders".to_owned(), "other".to_owned()]);
        let trigger = EventTrigger {
            application: "erp".into(),
            workflow_id: "notify".into(),
            source_package: "orders".into(),
            entity: "order".into(),
            ops: vec![Op::Update],
            condition: Some("old.status != new.status && new.status == 'open'".into()),
            enable_sequence: 41,
        };
        assert!(
            trigger
                .matches(&source, 41, &packages, "tenant")
                .unwrap()
                .is_none()
        );
        assert!(
            trigger
                .matches(&source, 42, &packages, "tenant")
                .unwrap()
                .is_some()
        );
        for changed in [
            EventTrigger {
                source_package: "other".into(),
                ..trigger.clone()
            },
            EventTrigger {
                entity: "other".into(),
                ..trigger.clone()
            },
            EventTrigger {
                ops: vec![Op::Insert],
                ..trigger.clone()
            },
            EventTrigger {
                condition: Some("new.status == 'draft'".into()),
                ..trigger.clone()
            },
        ] {
            assert!(
                changed
                    .matches(&source, 42, &packages, "tenant")
                    .unwrap()
                    .is_none()
            );
        }
        let second = EventTrigger {
            workflow_id: "audit".into(),
            ..trigger.clone()
        };
        assert_eq!(
            trigger.matches(&source, 42, &packages, "tenant").unwrap(),
            second.matches(&source, 42, &packages, "tenant").unwrap()
        );
        let SourceEvent::Cdc(mut missing_old) = source else {
            unreachable!()
        };
        missing_old.old = None;
        assert_eq!(
            trigger
                .matches(&SourceEvent::Cdc(missing_old), 42, &packages, "tenant")
                .unwrap_err()
                .kind,
            TriggerMatchErrorKind::Event
        );
    }
}
