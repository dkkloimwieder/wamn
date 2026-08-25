//! Pure release-publication decisions shared by the effect-shell writers.

use std::fmt;

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
}
