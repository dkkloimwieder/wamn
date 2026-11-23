//! Stored results for fenced run transitions.

use crate::RunStatus;

/// Typed result of a fenced run terminalization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalizeResult {
    Terminalized,
    RunTerminal(RunStatus),
    FenceLost,
    CrossRunAuthority,
    NotFound,
}

impl TerminalizeResult {
    pub fn from_parts(code: &str, run_status: &str) -> Option<TerminalizeResult> {
        match code {
            "terminalized" => Some(TerminalizeResult::Terminalized),
            "run-terminal" => Some(TerminalizeResult::RunTerminal(RunStatus::from_sql(
                run_status,
            )?)),
            "fence-lost" => Some(TerminalizeResult::FenceLost),
            "cross-run-authority" => Some(TerminalizeResult::CrossRunAuthority),
            "not-found" => Some(TerminalizeResult::NotFound),
            _ => None,
        }
    }

    /// `FenceLost` is absolute: callers must stop without another store access.
    pub fn permits_access(self) -> bool {
        self != TerminalizeResult::FenceLost
    }
}
