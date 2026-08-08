//! The green-suite publish gate ([11.7], wamn-12g) — the pure decision.
//!
//! §11.7 asks for per-project rules such as "prod deploys require green suite",
//! with the results recorded in an audit log. This module owns the second half
//! of that sentence's precondition: given the suites a promotion touches and the
//! durable suite reports that exist, decide whether the promotion has PROVEN
//! green suites. Whether the rule applies at all is
//! `wamn_control_registry::resolve_publish_policy`.
//!
//! It lives beside [`crate::impact`] because that is where the suites come from:
//! the gate consumes exactly the `(tenant, flow_id, flow_version, suite_id)`
//! selectors [`crate::impact::suite_selectors`] flattens out of an
//! [`ImpactReport`](crate::impact::ImpactReport). It lives in a LIBRARY, not in
//! `wamn-ctl`, because the same decision must later back the authenticated
//! management transport (wamn-ftfc.33) — `services/scenario-worker` already
//! depends on this crate, so that consumer costs no new dependency edge and
//! cannot drift into a second, weaker rule.
//!
//! # What counts as evidence
//!
//! A suite is green only if a durable report says so about THE BYTES BEING
//! SHIPPED. Three things must hold together, and each has its own defect so a
//! refusal can say which one failed:
//!
//! 1. the report passed;
//! 2. its lineage is a RELEASE of the exact `(catalog_id, catalog_version,
//!    environment)` being promoted — a draft run proves nothing about a release;
//! 3. its recorded `artifact_hash` equals the artifact hash that release
//!    actually pins for that flow.
//!
//! (3) is the freshness rule, and it is deliberately a HASH COMPARISON rather
//! than a timestamp window. A hash match is definitionally a statement about the
//! current bytes; "the run was recent" is a guess about them. Edit a flow and
//! the pinned artifact hash moves, so yesterday's pass stops counting the moment
//! it stops being about today's code — with no clock, no skew, and no window to
//! tune.
//!
//! # Fail-closed
//!
//! Absent evidence is a REFUSAL, never a pass. No shipped producer writes
//! `wamn_run.authoring_suite_reports` yet (the suite-run backend is
//! wamn-ftfc.28/.33), so a project that turns this gate on today will find it
//! refuses until that lands. That is the honest posture for a change-control
//! gate: a gate that passes because it found no evidence is not a gate.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::impact::SuiteEdge;

/// The release a promotion is carrying — the lineage a report must be pinned to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ReleasePin {
    pub catalog_id: String,
    pub catalog_version: i32,
    /// The environment the suites RAN in (the promotion's source), not the
    /// environment being deployed into — evidence is produced upstream of the
    /// gate, so requiring the destination env would make the gate unsatisfiable
    /// by construction.
    pub environment: String,
}

/// The release-lineage half of one `wamn_run.authoring_suite_reports` row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ReleaseLineage {
    pub artifact_hash: String,
    pub catalog_id: String,
    pub catalog_version: i32,
    pub environment: String,
}

/// One durable suite report offered as evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SuiteReportRow {
    /// The suite identity this report is about.
    pub suite: SuiteEdge,
    /// `authoring_suite_reports.report_id` — the evidence pointer the audit
    /// ledger records for a pass.
    pub report_id: String,
    pub passed: bool,
    /// `None` when the report's lineage is a DRAFT run. A draft pass is a real
    /// result about a draft; it is not a statement about a release.
    pub release: Option<ReleaseLineage>,
}

/// Why one suite is not proven green. Each variant is a distinct operator
/// action, which is why this is a closed classification rather than a message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "defect", rename_all = "kebab-case")]
pub enum EvidenceDefect {
    /// No durable report exists for this suite at all.
    NoReport,
    /// Every report for this suite is a draft run — none is about a release.
    DraftOnly,
    /// A release report exists, but for a different release than the one being
    /// promoted.
    ForeignRelease {
        catalog_id: String,
        catalog_version: i32,
        environment: String,
    },
    /// The suite ran and FAILED against exactly this release.
    Failed,
    /// The release being promoted pins no flow artifact for this suite's flow,
    /// so freshness cannot be established.
    UnpinnedFlow,
    /// The report is about this release but about DIFFERENT flow bytes — the
    /// flow was edited and republished after the suite ran.
    StaleArtifact { recorded: String, current: String },
}

impl EvidenceDefect {
    /// A short stable spelling for logs and the audit ledger.
    pub fn as_str(&self) -> &'static str {
        match self {
            EvidenceDefect::NoReport => "no-report",
            EvidenceDefect::DraftOnly => "draft-only",
            EvidenceDefect::ForeignRelease { .. } => "foreign-release",
            EvidenceDefect::Failed => "failed",
            EvidenceDefect::UnpinnedFlow => "unpinned-flow",
            EvidenceDefect::StaleArtifact { .. } => "stale-artifact",
        }
    }
}

/// The verdict for one suite the promotion touches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SuiteFinding {
    pub suite: SuiteEdge,
    /// `None` when the suite is proven green.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub defect: Option<EvidenceDefect>,
    /// The report that proved it green — the audit ledger's evidence pointer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report_id: Option<String>,
}

impl SuiteFinding {
    /// `true` when this suite is proven green.
    pub fn is_green(&self) -> bool {
        self.defect.is_none()
    }
}

/// The whole gate verdict, one finding per selector in selector order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct PublishGateReport {
    /// Whether the policy demanded green suites at all.
    pub required: bool,
    pub findings: Vec<SuiteFinding>,
}

/// A promotion refused because its suites are not proven green.
///
/// A canonical struct error mirroring [`crate::impact::ImpactNotAcknowledged`],
/// naming every unproven suite and its defect so the operator learns the whole
/// list from one refusal rather than one suite per re-run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GreenSuiteNotProven {
    pub unproven: Vec<(SuiteEdge, EvidenceDefect)>,
}

impl std::fmt::Display for GreenSuiteNotProven {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "refusing this promotion: the publish policy requires green suites and {} suite{} \
             {} not proven green for the release being promoted:",
            self.unproven.len(),
            if self.unproven.len() == 1 { "" } else { "s" },
            if self.unproven.len() == 1 {
                "is"
            } else {
                "are"
            },
        )?;
        for (suite, defect) in &self.unproven {
            write!(
                f,
                "\n  - tenant {:?} flow {:?} v{} suite {:?} — {}",
                suite.tenant,
                suite.flow_id,
                suite.flow_version,
                suite.suite_id,
                defect.as_str(),
            )?;
            match defect {
                EvidenceDefect::StaleArtifact { recorded, current } => write!(
                    f,
                    " (ran against artifact {recorded:?}, release pins {current:?})"
                )?,
                EvidenceDefect::ForeignRelease {
                    catalog_id,
                    catalog_version,
                    environment,
                } => write!(
                    f,
                    " (report pins catalog {catalog_id:?} v{catalog_version} in {environment:?})"
                )?,
                _ => {}
            }
        }
        Ok(())
    }
}

impl std::error::Error for GreenSuiteNotProven {}

impl PublishGateReport {
    /// `true` when the gate lets the promotion proceed — either the policy did
    /// not demand green suites, or every touched suite is proven green.
    pub fn is_satisfied(&self) -> bool {
        !self.required || self.findings.iter().all(SuiteFinding::is_green)
    }

    /// The typed refusal for an unsatisfied gate
    /// ([`is_satisfied`](Self::is_satisfied) must be `false`).
    pub fn refusal(&self) -> GreenSuiteNotProven {
        GreenSuiteNotProven {
            unproven: self
                .findings
                .iter()
                .filter_map(|f| f.defect.clone().map(|d| (f.suite.clone(), d)))
                .collect(),
        }
    }

    /// The stable audit spelling of the outcome.
    pub fn decision(&self) -> &'static str {
        if !self.required {
            "not-required"
        } else if self.is_satisfied() {
            "passed"
        } else {
            "refused"
        }
    }

    /// A human-readable rendering, mirroring
    /// [`ImpactReport::render`](crate::impact::ImpactReport::render).
    pub fn render(&self) -> String {
        if !self.required {
            return "publish gate — not required by policy for this target env\n".to_string();
        }
        if self.findings.is_empty() {
            return "publish gate — required; no suites touched by this promotion\n".to_string();
        }
        let green = self.findings.iter().filter(|f| f.is_green()).count();
        let mut out = format!(
            "publish gate — required; {green}/{} suite(s) proven green\n",
            self.findings.len(),
        );
        for f in &self.findings {
            match &f.defect {
                None => out.push_str(&format!(
                    "  [green ] tenant {:?} flow {:?} v{} suite {:?} (report {:?})\n",
                    f.suite.tenant,
                    f.suite.flow_id,
                    f.suite.flow_version,
                    f.suite.suite_id,
                    f.report_id.as_deref().unwrap_or(""),
                )),
                Some(defect) => out.push_str(&format!(
                    "  [UNMET ] tenant {:?} flow {:?} v{} suite {:?} — {}\n",
                    f.suite.tenant,
                    f.suite.flow_id,
                    f.suite.flow_version,
                    f.suite.suite_id,
                    defect.as_str(),
                )),
            }
        }
        out
    }
}

/// Decide the gate for one promotion.
///
/// `selectors` are the suites the promotion touches (the flattened impact
/// report). `reports` is every durable report offered as evidence, in the
/// caller's preference order (newest first) — the first green one wins, and
/// when none is green the first candidate's defect is reported. `pin` is the
/// release being promoted, and `flow_artifact_hashes` maps each flow that
/// release pins to the artifact hash it pins, which is the freshness authority.
///
/// When `required` is `false` the findings are still computed, so the audit
/// ledger records what the gate WOULD have said for an ungated env.
pub fn evaluate(
    required: bool,
    selectors: &[SuiteEdge],
    reports: &[SuiteReportRow],
    pin: &ReleasePin,
    flow_artifact_hashes: &BTreeMap<String, String>,
) -> PublishGateReport {
    let findings = selectors
        .iter()
        .map(|suite| {
            let candidates: Vec<&SuiteReportRow> =
                reports.iter().filter(|r| r.suite == *suite).collect();
            let mut best_defect = None;
            for candidate in &candidates {
                match classify(candidate, pin, flow_artifact_hashes) {
                    None => {
                        return SuiteFinding {
                            suite: suite.clone(),
                            defect: None,
                            report_id: Some(candidate.report_id.clone()),
                        };
                    }
                    Some(defect) => best_defect.get_or_insert(defect),
                };
            }
            SuiteFinding {
                suite: suite.clone(),
                defect: Some(best_defect.unwrap_or(EvidenceDefect::NoReport)),
                report_id: None,
            }
        })
        .collect();
    PublishGateReport { required, findings }
}

/// Classify one candidate report against the release being promoted; `None`
/// means it proves the suite green.
fn classify(
    report: &SuiteReportRow,
    pin: &ReleasePin,
    flow_artifact_hashes: &BTreeMap<String, String>,
) -> Option<EvidenceDefect> {
    let Some(release) = &report.release else {
        return Some(EvidenceDefect::DraftOnly);
    };
    if release.catalog_id != pin.catalog_id
        || release.catalog_version != pin.catalog_version
        || release.environment != pin.environment
    {
        return Some(EvidenceDefect::ForeignRelease {
            catalog_id: release.catalog_id.clone(),
            catalog_version: release.catalog_version,
            environment: release.environment.clone(),
        });
    }
    if !report.passed {
        return Some(EvidenceDefect::Failed);
    }
    let Some(current) = flow_artifact_hashes.get(&report.suite.flow_id) else {
        return Some(EvidenceDefect::UnpinnedFlow);
    };
    if *current != release.artifact_hash {
        return Some(EvidenceDefect::StaleArtifact {
            recorded: release.artifact_hash.clone(),
            current: current.clone(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{EvidenceDefect, ReleaseLineage, ReleasePin, SuiteReportRow, evaluate};
    use crate::impact::SuiteEdge;

    fn suite(flow: &str, suite_id: &str) -> SuiteEdge {
        SuiteEdge {
            tenant: "acme".into(),
            flow_id: flow.into(),
            flow_version: 1,
            suite_id: suite_id.into(),
        }
    }

    fn pin() -> ReleasePin {
        ReleasePin {
            catalog_id: "ops".into(),
            catalog_version: 7,
            environment: "dev".into(),
        }
    }

    fn hashes(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(f, h)| ((*f).to_string(), (*h).to_string()))
            .collect()
    }

    fn green_report(flow: &str, suite_id: &str, artifact: &str) -> SuiteReportRow {
        SuiteReportRow {
            suite: suite(flow, suite_id),
            report_id: format!("rep-{suite_id}"),
            passed: true,
            release: Some(ReleaseLineage {
                artifact_hash: artifact.into(),
                catalog_id: "ops".into(),
                catalog_version: 7,
                environment: "dev".into(),
            }),
        }
    }

    /// The load-bearing refusal: a gated promotion with NO evidence at all must
    /// refuse. A gate that passes on an empty ledger is not a gate.
    #[test]
    fn required_gate_refuses_when_no_report_exists() {
        let report = evaluate(
            true,
            &[suite("f1", "s1")],
            &[],
            &pin(),
            &hashes(&[("f1", "hash-a")]),
        );
        assert!(!report.is_satisfied());
        assert_eq!(report.decision(), "refused");
        assert_eq!(
            report.findings[0].defect,
            Some(EvidenceDefect::NoReport),
            "a missing report must be a refusal, never a pass"
        );
        assert!(report.refusal().to_string().contains("no-report"));
    }

    #[test]
    fn required_gate_passes_on_a_fresh_release_pinned_pass() {
        let report = evaluate(
            true,
            &[suite("f1", "s1")],
            &[green_report("f1", "s1", "hash-a")],
            &pin(),
            &hashes(&[("f1", "hash-a")]),
        );
        assert!(report.is_satisfied());
        assert_eq!(report.decision(), "passed");
        assert_eq!(report.findings[0].report_id.as_deref(), Some("rep-s1"));
    }

    /// Freshness is a hash comparison: a pass recorded against superseded bytes
    /// is stale evidence even though it really did pass.
    #[test]
    fn stale_artifact_hash_is_not_evidence_about_the_shipped_bytes() {
        let report = evaluate(
            true,
            &[suite("f1", "s1")],
            &[green_report("f1", "s1", "hash-OLD")],
            &pin(),
            &hashes(&[("f1", "hash-NEW")]),
        );
        assert!(!report.is_satisfied());
        assert_eq!(
            report.findings[0].defect,
            Some(EvidenceDefect::StaleArtifact {
                recorded: "hash-OLD".into(),
                current: "hash-NEW".into(),
            })
        );
        let refusal = report.refusal().to_string();
        assert!(refusal.contains("hash-OLD") && refusal.contains("hash-NEW"));
    }

    #[test]
    fn a_draft_pass_is_not_evidence_about_a_release() {
        let mut draft = green_report("f1", "s1", "hash-a");
        draft.release = None;
        let report = evaluate(
            true,
            &[suite("f1", "s1")],
            &[draft],
            &pin(),
            &hashes(&[("f1", "hash-a")]),
        );
        assert!(!report.is_satisfied());
        assert_eq!(report.findings[0].defect, Some(EvidenceDefect::DraftOnly));
    }

    #[test]
    fn a_pass_from_another_release_does_not_transfer() {
        let mut foreign = green_report("f1", "s1", "hash-a");
        foreign.release.as_mut().expect("release").catalog_version = 6;
        let report = evaluate(
            true,
            &[suite("f1", "s1")],
            &[foreign],
            &pin(),
            &hashes(&[("f1", "hash-a")]),
        );
        assert!(!report.is_satisfied());
        assert_eq!(
            report.findings[0].defect,
            Some(EvidenceDefect::ForeignRelease {
                catalog_id: "ops".into(),
                catalog_version: 6,
                environment: "dev".into(),
            })
        );
    }

    #[test]
    fn a_recorded_failure_against_this_release_refuses() {
        let mut failed = green_report("f1", "s1", "hash-a");
        failed.passed = false;
        let report = evaluate(
            true,
            &[suite("f1", "s1")],
            &[failed],
            &pin(),
            &hashes(&[("f1", "hash-a")]),
        );
        assert!(!report.is_satisfied());
        assert_eq!(report.findings[0].defect, Some(EvidenceDefect::Failed));
    }

    /// Freshness cannot be established for a flow the release pins no artifact
    /// for, so the gate fails closed rather than skipping the check.
    #[test]
    fn a_flow_the_release_does_not_pin_fails_closed() {
        let report = evaluate(
            true,
            &[suite("f1", "s1")],
            &[green_report("f1", "s1", "hash-a")],
            &pin(),
            &hashes(&[("other", "hash-a")]),
        );
        assert!(!report.is_satisfied());
        assert_eq!(
            report.findings[0].defect,
            Some(EvidenceDefect::UnpinnedFlow)
        );
    }

    /// A green report anywhere in the candidate list wins over earlier defects,
    /// so a re-run after a failure is not shadowed by the failure.
    #[test]
    fn a_later_green_report_wins_over_an_earlier_defect() {
        let mut failed = green_report("f1", "s1", "hash-a");
        failed.passed = false;
        failed.report_id = "rep-failed".into();
        let report = evaluate(
            true,
            &[suite("f1", "s1")],
            &[failed, green_report("f1", "s1", "hash-a")],
            &pin(),
            &hashes(&[("f1", "hash-a")]),
        );
        assert!(report.is_satisfied());
        assert_eq!(report.findings[0].report_id.as_deref(), Some("rep-s1"));
    }

    /// Evidence for one suite must never satisfy another suite.
    #[test]
    fn evidence_does_not_transfer_between_suites() {
        let report = evaluate(
            true,
            &[suite("f1", "s1"), suite("f1", "s2")],
            &[green_report("f1", "s1", "hash-a")],
            &pin(),
            &hashes(&[("f1", "hash-a")]),
        );
        assert!(!report.is_satisfied());
        assert!(report.findings[0].is_green());
        assert_eq!(report.findings[1].defect, Some(EvidenceDefect::NoReport));
    }

    /// An ungated env still computes findings, so the ledger records what the
    /// gate would have said.
    #[test]
    fn an_unrequired_gate_is_satisfied_but_still_reports() {
        let report = evaluate(
            false,
            &[suite("f1", "s1")],
            &[],
            &pin(),
            &hashes(&[("f1", "hash-a")]),
        );
        assert!(report.is_satisfied());
        assert_eq!(report.decision(), "not-required");
        assert_eq!(report.findings[0].defect, Some(EvidenceDefect::NoReport));
    }

    #[test]
    fn defect_spellings_are_stable() {
        assert_eq!(EvidenceDefect::NoReport.as_str(), "no-report");
        assert_eq!(EvidenceDefect::DraftOnly.as_str(), "draft-only");
        assert_eq!(EvidenceDefect::Failed.as_str(), "failed");
        assert_eq!(EvidenceDefect::UnpinnedFlow.as_str(), "unpinned-flow");
        assert_eq!(
            EvidenceDefect::StaleArtifact {
                recorded: "a".into(),
                current: "b".into(),
            }
            .as_str(),
            "stale-artifact"
        );
        assert_eq!(
            EvidenceDefect::ForeignRelease {
                catalog_id: "c".into(),
                catalog_version: 1,
                environment: "dev".into(),
            }
            .as_str(),
            "foreign-release"
        );
    }
}
