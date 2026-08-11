//! Pure release-publication decisions shared by the effect-shell writers.

use std::fmt;

/// A release member selected during preflight.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReleaseFlow {
    pub flow_id: String,
    pub flow_version: i32,
}

/// Inputs checked before a publication transaction mutates anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationGuard<'a> {
    pub expected_base: Option<i32>,
    pub applied_version: Option<i32>,
    pub nonterminal_runs: i64,
    pub unresolved_sources: &'a [String],
}

/// Stable refusal names used by ctl and live gates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublicationError {
    StaleBase {
        expected: Option<i32>,
        applied: Option<i32>,
    },
    NonterminalRuns {
        count: i64,
    },
    UnresolvedSources {
        ids: Vec<String>,
    },
    DuplicateFlow {
        flow_id: String,
    },
    MissingRootPlan {
        flow_id: String,
        flow_version: i32,
    },
}

impl fmt::Display for PublicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleBase { expected, applied } => {
                write!(
                    formatter,
                    "catalog-release-stale-base: expected {expected:?}, applied {applied:?}"
                )
            }
            Self::NonterminalRuns { count } => {
                write!(
                    formatter,
                    "catalog-release-has-nonterminal-runs: {count} run(s)"
                )
            }
            Self::UnresolvedSources { ids } => {
                write!(
                    formatter,
                    "catalog-release-unresolved-sources: {}",
                    ids.join(", ")
                )
            }
            Self::DuplicateFlow { flow_id } => {
                write!(formatter, "catalog-release-duplicate-flow: {flow_id:?}")
            }
            Self::MissingRootPlan {
                flow_id,
                flow_version,
            } => write!(
                formatter,
                "missing-root-plan: flow {flow_id:?} version {flow_version} has no validated execution plan"
            ),
        }
    }
}

impl std::error::Error for PublicationError {}

/// Check all refusal conditions before DDL or release-row writes.
pub fn guard_publication(guard: &PublicationGuard<'_>) -> Result<(), PublicationError> {
    if !guard.unresolved_sources.is_empty() {
        return Err(PublicationError::UnresolvedSources {
            ids: guard.unresolved_sources.to_vec(),
        });
    }
    if guard.expected_base != guard.applied_version {
        return Err(PublicationError::StaleBase {
            expected: guard.expected_base,
            applied: guard.applied_version,
        });
    }
    if guard.nonterminal_runs != 0 {
        return Err(PublicationError::NonterminalRuns {
            count: guard.nonterminal_runs,
        });
    }
    Ok(())
}

/// Canonicalize membership and reject two versions of one flow in a release.
pub fn canonical_release_flows(
    mut members: Vec<ReleaseFlow>,
) -> Result<Vec<ReleaseFlow>, PublicationError> {
    members.sort();
    for pair in members.windows(2) {
        if pair[0].flow_id == pair[1].flow_id {
            return Err(PublicationError::DuplicateFlow {
                flow_id: pair[0].flow_id.clone(),
            });
        }
    }
    Ok(members)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publication_refuses_unresolved_stale_and_nonterminal_inputs() {
        let unresolved = vec!["auth-policy".to_string()];
        assert!(matches!(
            guard_publication(&PublicationGuard {
                expected_base: Some(1),
                applied_version: Some(1),
                nonterminal_runs: 0,
                unresolved_sources: &unresolved,
            }),
            Err(PublicationError::UnresolvedSources { .. })
        ));
        assert!(matches!(
            guard_publication(&PublicationGuard {
                expected_base: Some(1),
                applied_version: Some(2),
                nonterminal_runs: 0,
                unresolved_sources: &[],
            }),
            Err(PublicationError::StaleBase { .. })
        ));
        assert!(matches!(
            guard_publication(&PublicationGuard {
                expected_base: Some(2),
                applied_version: Some(2),
                nonterminal_runs: 1,
                unresolved_sources: &[],
            }),
            Err(PublicationError::NonterminalRuns { .. })
        ));
    }

    #[test]
    fn head_drift_after_preflight_is_refused() {
        let preflight_base = Some(7);
        let locked_after_competing_publish = Some(8);
        assert_eq!(
            guard_publication(&PublicationGuard {
                expected_base: preflight_base,
                applied_version: locked_after_competing_publish,
                nonterminal_runs: 0,
                unresolved_sources: &[],
            }),
            Err(PublicationError::StaleBase {
                expected: Some(7),
                applied: Some(8),
            })
        );
    }

    #[test]
    fn membership_is_canonical_and_one_version_per_flow() {
        let members = canonical_release_flows(vec![
            ReleaseFlow {
                flow_id: "z".into(),
                flow_version: 1,
            },
            ReleaseFlow {
                flow_id: "a".into(),
                flow_version: 2,
            },
        ])
        .unwrap();
        assert_eq!(members[0].flow_id, "a");
        assert!(matches!(
            canonical_release_flows(vec![
                ReleaseFlow {
                    flow_id: "a".into(),
                    flow_version: 1,
                },
                ReleaseFlow {
                    flow_id: "a".into(),
                    flow_version: 2,
                },
            ]),
            Err(PublicationError::DuplicateFlow { .. })
        ));
    }

    #[test]
    fn missing_root_plan_is_a_distinct_publication_refusal() {
        let error = PublicationError::MissingRootPlan {
            flow_id: "orders".into(),
            flow_version: 7,
        };

        assert_eq!(
            error.to_string(),
            "missing-root-plan: flow \"orders\" version 7 has no validated execution plan"
        );
    }
}
