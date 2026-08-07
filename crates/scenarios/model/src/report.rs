use serde::{Deserialize, Serialize};

use crate::Outcome;

/// The canonical report emitted after a stored scenario suite is evaluated.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ScenarioReport {
    pub execution_id: String,
    /// Absolute virtual Unix epoch seconds used to replay every clock decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenario_epoch_secs: Option<u64>,
    pub flow_id: String,
    pub flow_version: i32,
    pub suite_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<ScenarioRefusal>,
    pub cases: Vec<CaseReport>,
}

impl ScenarioReport {
    /// Whether execution produced a compatible successful outcome.
    ///
    /// A typed refusal is successful because the authoritative runner proved
    /// before staging that this flow cannot be driven by the current artifact.
    pub fn passed(&self) -> bool {
        match self.refusal {
            Some(_) => self.cases.is_empty(),
            None => self.cases.iter().all(|case| case.outcome.passed()),
        }
    }
}

/// One stored case's durable run identity and evaluated outcome.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct CaseReport {
    pub case_id: String,
    pub run_id: String,
    pub outcome: Outcome,
}

/// A product-level reason a stored suite was not executed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "kebab-case"
)]
pub enum ScenarioRefusal {
    /// The compiled flowrunner cannot dispatch these sorted, unique node types.
    UndrivableNodes { node_types: Vec<String> },
    /// The exact immutable validated-draft row no longer matches admission pins.
    ValidatedDraftDrift,
    /// At least one exact active connection generation lacks draft-safe authority.
    DraftConnectionsDenied,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undrivable_report_round_trips_without_changing_normal_fields() {
        let report = ScenarioReport {
            execution_id: "exec-1".into(),
            scenario_epoch_secs: Some(1_700_000_000),
            flow_id: "flow-1".into(),
            flow_version: 2,
            suite_id: "suite-1".into(),
            refusal: Some(ScenarioRefusal::UndrivableNodes {
                node_types: vec!["external-node".into()],
            }),
            cases: Vec::new(),
        };

        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["execution-id"], "exec-1");
        assert_eq!(json["scenario-epoch-secs"], 1_700_000_000u64);
        assert_eq!(json["flow-id"], "flow-1");
        assert_eq!(json["flow-version"], 2);
        assert_eq!(json["suite-id"], "suite-1");
        assert_eq!(json["cases"], serde_json::json!([]));
        assert_eq!(json["refusal"]["kind"], "undrivable-nodes");
        assert_eq!(
            json["refusal"]["node-types"],
            serde_json::json!(["external-node"])
        );
        assert!(report.passed());
        assert_eq!(
            serde_json::from_value::<ScenarioReport>(json).unwrap(),
            report
        );

        let legacy = serde_json::json!({
            "execution-id": "old",
            "flow-id": "flow-1",
            "flow-version": 2,
            "suite-id": "suite-1",
            "cases": []
        });
        assert_eq!(
            serde_json::from_value::<ScenarioReport>(legacy)
                .unwrap()
                .scenario_epoch_secs,
            None
        );

        let mut contradictory = report.clone();
        contradictory.cases.push(CaseReport {
            case_id: "case-1".into(),
            run_id: "run-1".into(),
            outcome: Outcome {
                name: "case-1".into(),
                results: Vec::new(),
            },
        });
        assert!(!contradictory.passed());
    }
}
