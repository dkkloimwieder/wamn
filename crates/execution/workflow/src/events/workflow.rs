//! Match enabled workflows at their event sequence boundary.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::decide::{Verdict, decide, serviceable};
use wamn_event_wire::{Op, SourceEvent};
const MAX_CAUSATION_DEPTH: u32 = 16;

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
    pub type_: TriggerMatchErrorType,
    pub workflow_id: String,
    detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerMatchErrorType {
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
        project_env_key: &str,
    ) -> Result<Option<serde_json::Value>, TriggerMatchError> {
        if sequence <= self.enable_sequence {
            return Ok(None);
        }
        let condition = serviceable(self).map_err(|error| TriggerMatchError {
            type_: TriggerMatchErrorType::Condition,
            workflow_id: self.workflow_id.clone(),
            detail: format!("{error:?}"),
        })?;
        let verdict = match event {
            SourceEvent::Cdc(envelope) => decide(
                self,
                condition.as_ref(),
                envelope,
                packages,
                project_env_key,
                Some(project_env_key),
                MAX_CAUSATION_DEPTH,
            ),
            SourceEvent::Derived(event) => super::decide::decide_derived(
                self,
                condition.as_ref(),
                event,
                packages,
                project_env_key,
                MAX_CAUSATION_DEPTH,
            ),
        };
        match verdict {
            Verdict::Deliver(input) => Ok(Some(input)),
            Verdict::Skip(_) => Ok(None),
            Verdict::Refuse(reason) => Err(TriggerMatchError {
                type_: TriggerMatchErrorType::Event,
                workflow_id: self.workflow_id.clone(),
                detail: format!("{reason:?}"),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn workflow_matching_keeps_the_enable_boundary_and_all_filters() {
        let source = SourceEvent::decode(
            &serde_json::to_vec(&json!({
                "type": "cdc", "op": "update", "package_id": "orders", "entity": "order", "table": "orders",
                "identity": {"system_id": 1, "timeline": 1, "commit_lsn": 42, "row_ordinal": 0}, "txid": 7, "commit_ts": "2026-10-25T00:00:00Z",
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
                .matches(&source, 41, &packages, "project")
                .unwrap()
                .is_none()
        );
        assert!(
            trigger
                .matches(&source, 42, &packages, "project")
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
                    .matches(&source, 42, &packages, "project")
                    .unwrap()
                    .is_none()
            );
        }
        let second = EventTrigger {
            workflow_id: "audit".into(),
            ..trigger.clone()
        };
        assert_eq!(
            trigger.matches(&source, 42, &packages, "project").unwrap(),
            second.matches(&source, 42, &packages, "project").unwrap()
        );
        let SourceEvent::Cdc(mut missing_old) = source else {
            unreachable!()
        };
        missing_old.old = None;
        assert_eq!(
            trigger
                .matches(&SourceEvent::Cdc(missing_old), 42, &packages, "project")
                .unwrap_err()
                .type_,
            TriggerMatchErrorType::Event
        );
    }
}
