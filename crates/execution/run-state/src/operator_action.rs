//! Typed boundary and SQL for terminalizing one effect-uncertain run.

use std::fmt;
use std::str::FromStr;

/// The evidence basis recorded for one operator terminalization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperatorActionBasis {
    /// Evidence from the external system targeted by the effect.
    ExternalEvidence,
    /// Confirmation supplied by the effect counterparty.
    CounterpartyConfirmation,
    /// An explicit operator judgment when stronger evidence is unavailable.
    OperatorJudgment,
}

impl OperatorActionBasis {
    /// Stable persisted spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExternalEvidence => "external-evidence",
            Self::CounterpartyConfirmation => "counterparty-confirmation",
            Self::OperatorJudgment => "operator-judgment",
        }
    }
}

impl fmt::Display for OperatorActionBasis {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for OperatorActionBasis {
    type Err = InvalidOperatorActionBasis;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "external-evidence" => Ok(Self::ExternalEvidence),
            "counterparty-confirmation" => Ok(Self::CounterpartyConfirmation),
            "operator-judgment" => Ok(Self::OperatorJudgment),
            _ => Err(InvalidOperatorActionBasis {
                value: value.to_string(),
            }),
        }
    }
}

/// A value outside the frozen operator-action basis vocabulary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvalidOperatorActionBasis {
    value: String,
}

impl fmt::Display for InvalidOperatorActionBasis {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "unknown operator action basis {:?}; expected external-evidence, counterparty-confirmation, or operator-judgment",
            self.value
        )
    }
}

impl std::error::Error for InvalidOperatorActionBasis {}

/// Exact boundary result of one terminalization request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperatorTerminalizeResult {
    Terminalized,
    IdenticalRetry,
    RunNotFound,
    NotEffectUncertain,
    CorrelationConflict,
    RunStateInvariant,
}

impl OperatorTerminalizeResult {
    /// Stable command result spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Terminalized => "terminalized",
            Self::IdenticalRetry => "identical-retry",
            Self::RunNotFound => "run-not-found",
            Self::NotEffectUncertain => "not-effect-uncertain",
            Self::CorrelationConflict => "correlation-conflict",
            Self::RunStateInvariant => "run-state-invariant",
        }
    }
}

impl fmt::Display for OperatorTerminalizeResult {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Read an action matching either idempotency identity and lock it.
pub fn lock_operator_actions_sql() -> &'static str {
    "SELECT correlation_id, run_id, basis, evidence_ref, principal \
       FROM operator_run_actions \
      WHERE tenant_id = $1 AND (correlation_id = $2 OR run_id = $3) \
      ORDER BY created_at, action_id \
      FOR UPDATE"
}

/// Lock the exact run before reading any mutable projections.
pub fn lock_operator_run_sql() -> &'static str {
    "SELECT status, fail_kind FROM runs \
      WHERE tenant_id = $1 AND run_id = $2 FOR UPDATE"
}

/// Lock every immutable effect attempt belonging to the run.
pub fn lock_operator_effect_attempts_sql() -> &'static str {
    "SELECT frame_id, local_node_id, occurrence \
       FROM effect_attempts WHERE tenant_id = $1 AND run_id = $2 \
      ORDER BY frame_id, local_node_id, occurrence FOR UPDATE"
}

/// Lock any queue eligibility fact belonging to the run.
pub fn lock_operator_queue_fact_sql() -> &'static str {
    "SELECT run_id FROM run_queue \
      WHERE tenant_id = $1 AND run_id = $2 FOR UPDATE"
}

/// Append the immutable action before the terminal run projection is changed.
///
/// The prior-node columns are retained for history written before `node_runs`
/// was dropped and are written NULL: there is no longer a node projection to
/// attribute, and reconstructing one from deleted coordinates would fabricate
/// evidence. The all-NULL arm of `operator_run_actions_prior_node_check`
/// admits this shape.
pub fn insert_operator_action_sql() -> &'static str {
    "INSERT INTO operator_run_actions (\
         tenant_id, correlation_id, run_id, action_kind, basis, evidence_ref, \
         principal, principal_kind, prior_run_status, prior_started_node_frame_id, \
         prior_started_node_local_node_id, prior_started_node_occurrence, \
         prior_started_node_status) \
     VALUES ($1, $2, $3, 'terminalize-effect-uncertain', $4, $5, \
             $6, 'database-role', 'effect-uncertain', NULL, NULL, NULL, NULL) \
     RETURNING action_id::text"
}

/// Terminalize the run without rewriting its evidence or caller outcome.
pub fn terminalize_operator_run_sql() -> &'static str {
    "UPDATE runs \
        SET status = 'failed', \
            terminal_reason = 'operator-terminalized-effect-uncertain', \
            updated_at = now() \
      WHERE tenant_id = $1 AND run_id = $2 \
        AND status = 'effect-uncertain' AND fail_kind = 'effect-uncertain'"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basis_vocabulary_is_exact() {
        for (literal, expected) in [
            ("external-evidence", OperatorActionBasis::ExternalEvidence),
            (
                "counterparty-confirmation",
                OperatorActionBasis::CounterpartyConfirmation,
            ),
            ("operator-judgment", OperatorActionBasis::OperatorJudgment),
        ] {
            assert_eq!(literal.parse(), Ok(expected));
            assert_eq!(expected.as_str(), literal);
        }
        assert!("success".parse::<OperatorActionBasis>().is_err());
        assert!("".parse::<OperatorActionBasis>().is_err());
    }

    #[test]
    fn result_vocabulary_is_exact() {
        assert_eq!(
            [
                OperatorTerminalizeResult::Terminalized,
                OperatorTerminalizeResult::IdenticalRetry,
                OperatorTerminalizeResult::RunNotFound,
                OperatorTerminalizeResult::NotEffectUncertain,
                OperatorTerminalizeResult::CorrelationConflict,
                OperatorTerminalizeResult::RunStateInvariant,
            ]
            .map(OperatorTerminalizeResult::as_str),
            [
                "terminalized",
                "identical-retry",
                "run-not-found",
                "not-effect-uncertain",
                "correlation-conflict",
                "run-state-invariant",
            ]
        );
    }

    #[test]
    fn transaction_statements_pin_load_bearing_order_and_guards() {
        assert!(lock_operator_run_sql().ends_with("FOR UPDATE"));
        assert!(lock_operator_effect_attempts_sql().ends_with("FOR UPDATE"));
        assert!(lock_operator_queue_fact_sql().ends_with("FOR UPDATE"));
        assert!(lock_operator_actions_sql().contains("correlation_id = $2 OR run_id = $3"));

        let insert = insert_operator_action_sql();
        assert!(insert.contains("'terminalize-effect-uncertain'"));
        assert!(insert.contains("'database-role'"));
        assert!(insert.contains("'effect-uncertain'"));
        for retained in [
            "prior_started_node_frame_id",
            "prior_started_node_local_node_id",
            "prior_started_node_occurrence",
            "prior_started_node_status",
        ] {
            assert!(insert.contains(retained), "action drops {retained}");
        }
        assert!(insert.contains("'effect-uncertain', NULL, NULL, NULL, NULL)"));

        let run = terminalize_operator_run_sql();
        assert!(run.contains("status = 'effect-uncertain'"));
        assert!(run.contains("fail_kind = 'effect-uncertain'"));
        assert!(run.contains("operator-terminalized-effect-uncertain"));
        for preserved in [
            "caller_outcome",
            "state_json",
            "result_json",
            "input_json",
            "fail_kind = 'effect-uncertain',",
        ] {
            assert!(!run.contains(preserved), "run update rewrites {preserved}");
        }
    }
}
